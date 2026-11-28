"""The shared-checkout arm (issue #543): every session in one checkout,
committing its own files and pushing to one integration remote.

SharedUnit runs in process, except where a test says it verifies Git's own
behaviour. SharedDryRun runs real dry-run trials through driver.py with the
stand-in agent. No model runs.
"""

import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout

import driver
import matrix
import shared_checkout as shared
import tasks
from test_driver import langspan_binary
from test_matrix import record

HERE = Path(__file__).resolve().parent


def bash(call, command, output, is_error=False):
    """One Bash call and its result, as Claude Code's stream-json has them."""
    return [{"type": "assistant", "message": {"content": [
                {"type": "tool_use", "id": call, "name": "Bash", "input": {"command": command}}]}},
            {"type": "user", "message": {"content": [
                {"type": "tool_result", "tool_use_id": call, "is_error": is_error,
                 "content": [{"type": "text", "text": output}]}]}}]


REJECTED = (" ! [rejected]        main -> main (fetch first)\n"
            "error: failed to push some refs to '/tmp/origin.git'\n")


class SharedUnit(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="langspan-shared-test-")
        self.root = Path(self.temp.name)

    def tearDown(self):
        self.temp.cleanup()

    def test_push_counts_come_from_the_bash_calls_and_their_results(self):
        events = (bash("a", "git status", "On branch main")
                  + bash("b", "git push origin main", REJECTED, is_error=True)
                  + bash("c", "git pull --no-edit origin main && git push origin main", "main -> main")
                  + bash("d", "git commit -m 'push the rename' -- shelf/a.py", "1 file changed"))
        self.assertEqual(shared.push_counts(events), {"attempts": 2, "rejections": 1, "retries": 1, "landed": 1})
        self.assertEqual(shared.push_counts([]), {"attempts": 0, "rejections": 0, "retries": 0, "landed": 0})
        # A result given as plain text counts the same as one in blocks.
        plain = bash("e", "git push", "")
        plain[1]["message"]["content"][0]["content"] = REJECTED
        self.assertEqual(shared.push_counts(plain), {"attempts": 1, "rejections": 1, "retries": 0, "landed": 0})
        # A push that failed another way neither landed nor was rejected, so
        # the push after it is not a retry; a second landed push is no retry.
        other = (bash("f", "git push", "fatal: unable to access", is_error=True) + bash("g", "git push", "ok")
                 + bash("h", "git push", "Everything up-to-date"))
        self.assertEqual(shared.push_counts(other), {"attempts": 3, "rejections": 0, "retries": 0, "landed": 2})

    def test_a_push_routed_to_landing_counts_its_landing_calls_instead(self):
        # Pure parsing. LangSpan's hook refused the push before it ran, so
        # it is no attempt; each `langspan land --apply` is one, rejected when
        # the remote refused it, and the one after a rejected land a retry.
        land = "'/b/langspan' --state-dir '/tmp/s' land 'bench-s1' --onto main --name tests refs/heads/bench/s1 " \
               "--apply --rebase --check-again -- python3 -m unittest discover -s tests"
        events = (bash("a", "git push origin HEAD:main", "Hook PreToolUse:Bash denied this tool: " + shared.ROUTED
                       + " Run `...`", is_error=True)
                  + bash("b", land, 'Not landed: "main" moved under each of 3 attempts. Nothing was forced; try '
                                    'again later.', is_error=True)
                  + bash("c", land, "Landed 1a2b on origin/main, on check 3c4d for exactly its tree."))
        self.assertEqual(shared.push_counts(events), {"attempts": 2, "rejections": 1, "retries": 1, "landed": 1})
        # A landing that fails another way, a check, or a land without --apply is no rejection or no attempt.
        failed = bash("d", land, "Not landed: tests failed (exit 1) on the combined tree 5e6f.", is_error=True)
        self.assertEqual(shared.push_counts(failed), {"attempts": 1, "rejections": 0, "retries": 0, "landed": 0})
        for command in ("langspan land app --onto main --name t b --check-again -- true",
                        "langspan integrate check app --onto main --name t b -- true", "echo langspan land --apply"):
            with self.subTest(command=command):
                self.assertFalse(shared.runs_land(command))
        for command in (land, "langspan land app --onto main --name t b --apply --check-again -- true",
                        "LANGSPAN_STATE_DIR=/s /x/langspan land a --apply --onto main --name t b --check-again -- t"):
            with self.subTest(command=command):
                self.assertTrue(shared.runs_land(command))
        # The routed command, with the check's name and the package's tests filled in.
        reason = (shared.ROUTED + " main is the default branch of the remote ... Run `'/b/langspan' --state-dir "
                  "'/tmp/s' land 'bench-s1' --onto main --name CHECK refs/heads/bench/s1 --apply --rebase "
                  "--check-again -- TEST_COMMAND`, with CHECK a name for the check ...")
        self.assertEqual(shared.landing_command(reason), land)
        self.assertIsNone(shared.landing_command("LangSpan: push refused. ..."))

    def test_time_to_all_landed_needs_every_session_landed(self):
        # Pure arithmetic over the remote's update times.
        landed, unlanded = {"attempts": 2, "landed": 1}, {"attempts": 1, "landed": 0}
        start = "1970-01-01T00:00:10.500000+00:00"
        times = [15, 22]
        self.assertEqual(driver.landed_after([landed, landed], times, start), 11.5)
        # The remote's times are whole seconds: never below zero.
        self.assertEqual(driver.landed_after([landed], [10], start), 0)
        self.assertIsNone(driver.landed_after([landed, unlanded], times, start))
        self.assertIsNone(driver.landed_after([landed, None], times, start))
        self.assertIsNone(driver.landed_after([landed], [], start))
        self.assertIsNone(driver.landed_after([landed], times, None))

    def test_push_contention_gives_independent_tasks_that_every_arm_lands_by_pushing(self):
        # Pure planning: no Git or process runs.
        for n in tasks.SESSION_COUNTS:
            assigned = tasks.assign("push_contention", n, 3)
            owned = [path for task in assigned for path in task["owns"]]
            self.assertEqual((len(assigned), len(owned)), (n, len(set(owned))), n)
            for task in assigned:
                self.assertNotIn("trigger", task)
                self.assertFalse(task.get("leads"))
                self.assertNotIn("rename", task["details"])
        for arm in ("shared", "worktrees", "awareness_only"):
            plan = driver.make_plan(arm, "push_contention", 2, 1)
            self.assertEqual(plan["branches"], ["main", "main"], arm)
            for prompt in plan["prompts"]:
                self.assertIn("push to `origin main`", prompt)
                self.assertNotIn("Do not push", prompt)
            self.assertIn("Bash(git push:*)", driver.claude_argv("claude", "", arm, push=driver.lands_by_pushing(plan)))
        self.assertIn("own checkout", driver.make_plan("worktrees", "push_contention", 2, 1)["prompts"][0])
        plan = driver.make_plan("worktrees", "rename_vs_new_caller", 2, 1)
        self.assertFalse(driver.lands_by_pushing(plan))
        self.assertIn("Do not push", plan["prompts"][0])
        # The langspan arm keeps its worktrees, one branch each, and pushes
        # that branch to `origin main`, which LangSpan routes to landing.
        plan = driver.make_plan("langspan", "push_contention", 2, 1)
        self.assertTrue(driver.lands_by_pushing(plan))
        self.assertEqual((plan["isolation"], plan["branches"]), ("worktree", ["bench/s1", "bench/s2"]))
        for index, prompt in enumerate(plan["prompts"], 1):
            self.assertIn("on branch `bench/s%d`" % index, prompt)
            self.assertIn("push to `origin main` with `git push origin HEAD:main`", prompt)
        self.assertNotIn("HEAD:main", driver.make_plan("worktrees", "push_contention", 2, 1)["prompts"][0])
        # A real agent may run the landing command the routing names, and only with --apply's own prefix.
        plan.update(langspan="/b/lang span", state_dir="/tmp/it's")
        tools = driver.land_tools(plan)
        self.assertEqual(tools, ("Bash('/b/lang span' --state-dir '/tmp/it'\\''s' land:*)", "Bash(langspan land:*)"))
        argv = driver.claude_argv("claude", "", "langspan", push=True, land=tools, skill="s")
        self.assertTrue(set(tools) <= set(argv[argv.index("--allowedTools"):]))
        self.assertEqual(driver.land_tools(dict(driver.make_plan("worktrees", "push_contention", 2, 1),
                                                langspan="/b/langspan", state_dir="/tmp/s")), ())

    def test_only_a_command_that_runs_git_push_is_a_push_attempt(self):
        # Pure parsing: no shell or Git runs. Text that only mentions a push
        # (a quoted argument, a comment, another program's argument) is not
        # an attempt; each form that runs one is.
        mentions = [
            "git commit -m 'Retry git push after fixing tests' -- shelf/a.py",
            'git commit -m "git push" -- shelf/a.py',
            "echo git push",
            "git log --grep 'git push'",
            "git status # then git push",
            "git commit -m 'unbalanced",
            "git pushed",
            "gitpush",
            "git -c push.default=simple status",
            "",
        ]
        runs = [
            "git push",
            "git push origin main",
            "git pull --no-edit origin main && git push origin main",
            "git add a; git push",
            "git status || git push",
            "git commit -m 'x'\ngit push",
            "cd trunk && git push",
            "GIT_TRACE=1 git push",
            "git -C trunk push",
            "git -c user.name=b push origin main",
            "git --git-dir=.git push",
            "(git push)",
            "if true; then git push; fi",
            "env GIT_TRACE=1 git push",
            "bash -c 'git push origin main'",
            "/usr/bin/git push",
        ]
        for command in mentions:
            with self.subTest(command=command):
                self.assertFalse(shared.runs_git_push(command))
        for command in runs:
            with self.subTest(command=command):
                self.assertTrue(shared.runs_git_push(command))
        # The finding's own scenario, through the transcript count.
        events = bash("a", "git commit -m 'Retry git push after fixing tests' -- shelf/a.py", "1 file changed")
        self.assertEqual(shared.push_counts(events), {"attempts": 0, "rejections": 0, "retries": 0, "landed": 0})

    def test_a_comment_ends_at_its_newline_and_the_next_command_still_counts(self):
        # Pure parsing. A comment runs to the end of its line, not further:
        # the command on the next line is its own simple command.
        runs = [
            "git status --short # inspect before pushing\ngit push origin main",
            "git status #c\ngit push",
            "# first a comment\ngit push",
            "git status # it's unbalanced\ngit push",
            "git status #\n\ngit push",
            "git status # one\n# two\ngit push",
            "echo x #c\r\ngit push",
            "git status;#c\ngit push",
            "echo a#b; git push",
            "echo 'a'#b; git push",
        ]
        for command in runs:
            with self.subTest(command=command):
                self.assertTrue(shared.runs_git_push(command))
        self.assertEqual(shared.simple_commands("git status --short # inspect\ngit push origin main"),
                         [["git", "status", "--short"], ["git", "push", "origin", "main"]])
        # A comment on the last line, or one that names a push, still runs nothing.
        for command in ["git status # then git push", "git status # then\n# git push", "# git push",
                        "git status;# git push", "git status &&#git push"]:
            with self.subTest(command=command):
                self.assertFalse(shared.runs_git_push(command))
        # '#' inside a word or a quote is not a comment, as in the shell.
        self.assertEqual(shared.simple_commands("echo a#b; git push"), [["echo", "a#b"], ["git", "push"]])
        self.assertEqual(shared.simple_commands("echo 'a # b'\ngit push"), [["echo", "a # b"], ["git", "push"]])
        self.assertEqual(shared.simple_commands("echo '#' \"#\" \\#\ngit push"),
                         [["echo", "#", "#", "#"], ["git", "push"]])
        # The finding's own scenario, through the transcript count.
        events = bash("a", "git status --short # inspect before pushing\ngit push origin main", "main -> main")
        self.assertEqual(shared.push_counts(events), {"attempts": 1, "rejections": 0, "retries": 0, "landed": 1})

    def test_a_rejected_push_is_counted_and_retried_until_it_lands(self):
        # Real Git: what is verified is Git's own rejection and the retry.
        repo = self.root / "repo"
        repo.mkdir()
        (repo / "a.py").write_text("A = 1\n")
        (repo / "b.py").write_text("B = 1\n")
        driver.git("init", "-q", "-b", "main", cwd=repo)
        driver.git("add", "-A", cwd=repo)
        driver.git("commit", "-q", "-m", "Fixture", cwd=repo)
        first, second = shared.prepare(self.root, repo, ("trunk", "other"))
        (first / "a.py").write_text("A = 2\n")
        (second / "b.py").write_text("B = 2\n")
        sink = io.StringIO()
        with redirect_stdout(sink):
            self.assertTrue(shared.commit_and_push(["a.py"], 1, cwd=first))
            self.assertTrue(shared.commit_and_push(["b.py"], 2, cwd=second))
        events = driver.transcript_events(sink.getvalue())
        self.assertEqual(shared.push_counts(events), {"attempts": 3, "rejections": 1, "retries": 1, "landed": 2})
        # The bare remote times each update of main, one per landed push.
        self.assertEqual(len(shared.landing_times(self.root / "origin.git")), 2)
        landed = self.root / "landed"
        driver.git("clone", "-q", str(self.root / "origin.git"), str(landed), cwd=self.root)
        self.assertEqual(((landed / "a.py").read_text(), (landed / "b.py").read_text()), ("A = 2\n", "B = 2\n"))

    def test_the_shared_plan_puts_every_session_on_main_and_lets_it_push(self):
        plan = driver.make_plan("shared", "rename_vs_new_caller", 2, 1)
        self.assertEqual((plan["isolation"], plan["branches"]), ("shared", ["main", "main"]))
        for prompt in plan["prompts"]:
            self.assertIn("push to `origin main`", prompt)
            self.assertNotIn("Do not push", prompt)
        self.assertIn("Do not push", driver.make_plan("worktrees", "rename_vs_new_caller", 2, 1)["prompts"][0])
        for arm in driver.ARMS + driver.SHARED_ARMS:
            argv = driver.claude_argv("claude", "", arm, skill=driver.skill_placeholder(arm))
            self.assertEqual("Bash(git push:*)" in argv, arm == "shared", arm)
        self.assertEqual(driver.memory_directory("shared", "/w", 2), Path("/w/trunk"))

    def test_the_matrix_accepts_the_shared_arm_and_keeps_the_compared_default(self):
        names = [t["name"] for t in matrix.trials(["rename_vs_new_caller"], [2], [1], ["shared"])]
        self.assertEqual(names, ["rename_vs_new_caller-n2-s1-shared"])
        sink = io.StringIO()
        with redirect_stdout(sink):
            self.assertEqual(matrix.main(["run", "--plan-only", "--families", "rename_vs_new_caller", "--sessions",
                                          "2", "--seeds", "1", "--arms", "shared", "--output",
                                          str(self.root / "out")]), 0)
        self.assertEqual(json.loads(sink.getvalue())["trial_list"], names)
        # Without --arms the matrix runs the three compared arms, as before.
        sink = io.StringIO()
        with redirect_stdout(sink):
            matrix.main(["run", "--plan-only", "--families", "rename_vs_new_caller", "--sessions", "2",
                         "--seeds", "1", "--output", str(self.root / "out")])
        self.assertEqual([name.rsplit("-", 1)[1] for name in json.loads(sink.getvalue())["trial_list"]],
                         ["worktrees", "awareness_only", "langspan"])
        # push_contention runs in every arm, the langspan arm's included.
        argv = ["run", "--plan-only", "--families", "push_contention", "--sessions", "5", "--seeds", "1",
                "--output", str(self.root / "out")]
        sink = io.StringIO()
        with redirect_stdout(sink):
            self.assertEqual(matrix.main(argv + ["--arms", "shared", "worktrees", "langspan"]), 0)
        self.assertEqual(json.loads(sink.getvalue())["trials"], 3)

    def test_the_summary_and_report_show_push_attempts_and_rejections(self):
        pushed = [dict(record("shared", seed), pushes={"status": "reported", "attempts": attempts,
                                                       "rejections": rejected, "retries": rejected,
                                                       "landed_after_s": after})
                  for seed, attempts, rejected, after in ((1, 3, 1, 7.5), (2, 2, 0, None))]
        # A version 8 record has no pushes: not applicable, never zero.
        older = dict(record("worktrees", 1), schema_version=8)
        # A version 9 record has no retries or landing time: unknown, never zero.
        nine = dict(record("awareness_only", 1), schema_version=9,
                    pushes={"status": "reported", "attempts": 1, "rejections": 0})
        groups = {group["arm"]: group for group in driver.summarize(pushed + [older, nine])}
        self.assertEqual([groups["shared"][key] for key in ("push_attempts", "push_rejections", "push_retries")],
                         [5, 1, 1])
        self.assertEqual(groups["shared"]["landed_after_s"], [7.5, None])
        self.assertEqual((groups["worktrees"]["push_attempts"], groups["worktrees"]["push_rejections"]), (None, None))
        self.assertEqual((groups["awareness_only"]["push_attempts"], groups["awareness_only"]["push_retries"]),
                         (1, None))
        text = matrix.report([("t%d" % r["seed"], r) for r in pushed] + [("old", older), ("nine", nine)], [])
        for column in ("Push attempts", "Push rejections", "Push retries", "All landed after (s)"):
            self.assertIn(column, text)
        rows = {line.split(" | ")[0][2:]: line for line in text.splitlines()
                if line.startswith(("| t", "| old", "| nine"))}
        self.assertTrue(rows["t1"].endswith("| 3 | 1 | 1 | 7.5 |"), rows["t1"])
        self.assertTrue(rows["t2"].endswith("| 2 | 0 | 0 | not all landed |"), rows["t2"])
        self.assertTrue(rows["old"].endswith("| not applicable | not applicable | not applicable | not applicable |"),
                        rows["old"])
        self.assertTrue(rows["nine"].endswith("| 1 | 0 | unknown | unknown |"), rows["nine"])
        shared_row = next(line for line in text.splitlines() if " | shared | " in line and line.startswith("| standin"))
        self.assertTrue(shared_row.endswith("| 5 | 1 | 1 | 7.5, not all landed |"), shared_row)

    def test_the_report_and_summary_read_version_8_records(self):
        trial = self.root / "out" / "t"
        trial.mkdir(parents=True)
        (trial / "record.json").write_text(json.dumps(dict(record("worktrees", 1), schema_version=8)))
        named, _ = matrix.read_trials([self.root / "out"])
        self.assertEqual(len(named), 1)
        with redirect_stdout(io.StringIO()):
            self.assertEqual(driver.summarize_main([str(trial)]), 0)


class SharedDryRun(unittest.TestCase):
    """The whole pipeline in the shared arm: one checkout, real pushes to a
    real bare remote, real child processes."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="langspan-shared-dry-")
        self.root = Path(self.temp.name)

    def tearDown(self):
        self.temp.cleanup()

    def run_shared(self, n, family="rename_vs_new_caller", arm="shared"):
        output = self.root / ("%s-%d-%s" % (arm, n, family))
        argv = [sys.executable, "-B", str(HERE / "driver.py"), "--arm", arm, "--family", family, "--sessions",
                str(n), "--seed", "11", "--time-limit", "600", "--run", "--output", str(output)]
        if arm == "langspan":
            argv += ["--langspan", str(langspan_binary())]
        done = subprocess.run(argv, capture_output=True, text=True, timeout=900)
        record_path = output / "record.json"
        self.assertTrue(record_path.is_file(), done.stdout + done.stderr)
        found = json.loads(record_path.read_text())
        driver.validate(found, driver.load_schema())
        self.assertNotIn("error", found, found.get("error"))
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertEqual((found["arm"], found["isolation"], found["status"]),
                         (arm, driver.isolation(arm), "succeeded"))
        self.assertEqual(found["tool_check"], {"status": "passed", "failed_sessions": []})
        self.assertEqual(found["refusals"]["status"], "reported" if arm == "langspan" else "not_applicable")
        manifest = json.loads((output / "manifest.json").read_text())
        agents = next(iter(manifest["items"][0]["arms"].values()))["agents"]
        checkouts = {Path(a["cwd"]).resolve() for a in agents}
        self.assertEqual(checkouts, {(output / "work" / "trunk").resolve()} if arm == "shared" else
                         {(output / "work" / ("s%d" % i)).resolve() for i in range(1, n + 1)})
        for index, task in enumerate(found["tasks"], 1):
            self.assertEqual(task["branch"], driver.branch(index) if arm == "langspan" else "main")
            pushes = task["pushes"]
            self.assertGreaterEqual(pushes["attempts"], 1, task)
            self.assertLessEqual(pushes["rejections"], pushes["attempts"], task)
            self.assertLessEqual(pushes["retries"], pushes["rejections"], task)
        total = found["pushes"]
        self.assertEqual(total["status"], "reported")
        for key in ("attempts", "rejections", "retries"):
            self.assertEqual(total[key], sum(t["pushes"][key] for t in found["tasks"]), key)
        # The combined tree is what origin main holds: one commit per session past the fixture at least.
        log = driver.git("rev-list", "--count", "main", cwd=output / "work" / "origin.git").stdout
        self.assertGreaterEqual(int(log), n + 1)
        return found

    def test_dry_run_shared_arm_two_sessions(self):
        self.run_shared(2)

    def test_dry_run_shared_arm_five_sessions(self):
        self.run_shared(5)

    def test_dry_run_shared_arm_reports_stale_callers_from_every_session_s_writes(self):
        # The driver's real shared checkout and transcripts: each session's
        # checkout holds its peers' recorded writes, which are not writes
        # made another way.
        found = self.run_shared(2, "discovered_change")
        self.assertEqual(found["stale_callers"]["status"], "reported", found["stale_callers"])

    def test_dry_run_push_contention_lands_every_session_s_change_in_each_pushing_arm(self):
        # Real pushes at once to a real bare remote: what is verified is that
        # Git refuses all but one of a round, the sessions retry, and the
        # remote's own update times give the time to all landed.
        for arm in ("shared", "worktrees"):
            with self.subTest(arm=arm):
                found = self.run_shared(5, "push_contention", arm)
                self.assertEqual((found["integrated_tests"], found["tasks_accepted"]), ("passed", 5))
                self.assertEqual(found["repair"]["status"], "not_needed")
                for task in found["tasks"]:
                    self.assertEqual(task["pushes"]["landed"], 1, task)
                self.assertGreaterEqual(found["pushes"]["landed_after_s"], 0)

    def test_dry_run_push_contention_langspan_arm_lands_through_landing(self):
        # The real coordinator and hooks: what is verified is that LangSpan's
        # hook refuses each session's push to `origin main` with the landing
        # command, and that `langspan land` then lands every session's change
        # on the real bare remote.
        n = 5
        found = self.run_shared(n, "push_contention", "langspan")
        self.assertEqual((found["integrated_tests"], found["tasks_accepted"]), ("passed", n))
        self.assertEqual(found["repair"]["status"], "not_needed")
        output = self.root / ("langspan-%d-push_contention" % n)
        for task in found["tasks"]:
            self.assertEqual(task["pushes"]["landed"], 1, task)
            events = driver.transcript_events((output / task["transcript"]).read_text())
            calls = [block["input"]["command"] for event in events for block in shared.blocks(event)
                     if block.get("type") == "tool_use" and block.get("name") == "Bash"]
            results = [shared.result_text(block.get("content")) for event in events
                       for block in shared.blocks(event) if block.get("type") == "tool_result"]
            # Routed, never pushed directly: the push's result is the hook's
            # refusal, and the landing call after it runs the package's tests.
            self.assertIn("git push origin HEAD:main", calls)
            self.assertTrue(any(shared.ROUTED in text for text in results), results)
            lands = [call for call in calls if shared.runs_land(call)]
            self.assertTrue(lands and all("-- python3 -m unittest discover -s tests" in c for c in lands), calls)
        self.assertGreaterEqual(found["pushes"]["landed_after_s"], 0)
        self.assertEqual(found["tokens"], {"status": "unknown", "total": None})


if __name__ == "__main__":
    unittest.main()
