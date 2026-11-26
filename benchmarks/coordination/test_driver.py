"""The benchmark driver, end to end in dry-run mode.

Real temporary Git repositories and worktrees, real child processes, and in
the LangSpan arm the real langspan binary with its SQLite store and Unix
socket. No model runs: the agent is standin_agent.py. Set
LANGSPAN_BENCH_LANGSPAN to the binary (`cargo test` does, through
tests/benchmark_harness.rs); without it the LangSpan-arm tests fail.
"""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

import driver
import standin_agent
import tasks

HERE = Path(__file__).resolve().parent


def langspan_binary():
    value = os.environ.get("LANGSPAN_BENCH_LANGSPAN")
    if not value:
        raise AssertionError("set LANGSPAN_BENCH_LANGSPAN to the langspan binary built from this checkout")
    return Path(value)


class Temporary(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="langspan-k1-test-")
        self.root = Path(self.temp.name)

    def tearDown(self):
        self.temp.cleanup()


class DriverUnitTests(Temporary):
    def test_assignment_is_seeded_sized_and_shaped(self):
        for n in tasks.SESSION_COUNTS:
            rename = tasks.assign("rename_vs_new_caller", n, 7)
            self.assertEqual(rename, tasks.assign("rename_vs_new_caller", n, 7))
            self.assertEqual(len(rename), n)
            self.assertEqual([t["role"] for t in rename].count("rename"), 1)
            self.assertEqual(len({t["id"] for t in rename}), n)
            contradictory = tasks.assign("contradictory_defaults", n, 7)
            self.assertEqual(len(contradictory), n)
            pairs = [t["pair"] for t in contradictory if "pair" in t]
            self.assertEqual(len(pairs), n // 2 * 2)
            self.assertTrue(all(pairs.count(key) == 2 for key in pairs))
            self.assertEqual([t["role"] for t in contradictory].count("control"), n % 2)
        self.assertNotEqual([t["id"] for t in tasks.assign("rename_vs_new_caller", 5, 1)],
                            [t["id"] for t in tasks.assign("rename_vs_new_caller", 5, 2)])
        with self.assertRaises(ValueError):
            tasks.assign("rename_vs_new_caller", 3, 1)

    def test_claude_command_keeps_permission_prompts_and_limits_tools(self):
        for arm in driver.ARMS:
            argv = driver.claude_argv("claude", "do the task", arm)
            self.assertEqual(argv[:3], ["claude", "-p", "do the task"])
            self.assertEqual(argv[argv.index("--permission-mode") + 1], "default")
            self.assertFalse(set(argv) & set(driver.FORBIDDEN_CLAUDE_FLAGS))
            self.assertFalse(any("bypass" in a.lower() or "acceptedits" in a.lower() for a in argv))
            tools = argv[argv.index("--allowedTools") + 1:]
            self.assertIn("Edit(./**)", tools)
            self.assertNotIn("Bash", tools, "no unrestricted shell")
            self.assertEqual(any(t.startswith("Bash(langspan") for t in tools), arm == "langspan")

    def test_only_the_awareness_arm_names_peers_in_the_prompt(self):
        assigned = tasks.assign("rename_vs_new_caller", 5, 3)
        for arm in driver.ARMS:
            prompt = driver.prompt_for(arm, assigned, 2)
            self.assertIn(assigned[1]["prompt"], prompt)
            named = [peer["summary"] in prompt for i, peer in enumerate(assigned, 1) if i != 2]
            self.assertEqual(all(named), arm == "awareness_only")
            self.assertEqual(any(named), arm == "awareness_only")
            self.assertNotIn(assigned[1]["summary"], prompt.split("Task:")[0])

    def test_plan_only_executes_nothing(self):
        output = self.root / "never"
        result = subprocess.run([sys.executable, "-B", str(HERE / "driver.py"), "--arm", "worktrees",
                                 "--family", "contradictory_defaults", "--sessions", "5", "--seed", "1",
                                 "--output", str(output)], capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)
        plan = json.loads(result.stdout)
        self.assertEqual(plan["executed_commands"], 0)
        self.assertEqual(len(plan["sessions"]), 5)
        self.assertFalse(output.exists())

    def test_a_claude_configuration_with_langspan_hooks_is_refused_before_anything_runs(self):
        config = self.root / "claude-config"
        config.mkdir()
        (config / "settings.json").write_text(json.dumps({"hooks": {"PreToolUse": [{"matcher": "Write", "hooks": [
            {"type": "command", "command": "'/opt/bin/langspan' hook claude-code --state-dir '/s'"}]}]}}))
        output = self.root / "out"
        env = dict(os.environ, **{driver.CLAUDE_CONFIG_ENV: str(config)})
        result = subprocess.run([sys.executable, "-B", str(HERE / "driver.py"), "--arm", "worktrees",
                                 "--family", "rename_vs_new_caller", "--sessions", "2", "--seed", "1",
                                 "--agent", "claude", "--run", "--output", str(output)],
                                capture_output=True, text=True, timeout=30, env=env)
        self.assertEqual(result.returncode, 2)
        self.assertIn("LangSpan hooks", result.stderr)
        self.assertFalse(output.exists())
        # The skill alone is refused too.
        (config / "settings.json").write_text("{}")
        self.assertEqual(driver.user_level_langspan(config), [])
        (config / "skills" / "langspan").mkdir(parents=True)
        (config / "skills" / "langspan" / "SKILL.md").write_text("skill")
        self.assertEqual(len(driver.user_level_langspan(config)), 1)

    def test_a_claude_run_needs_the_configuration_directory_variable(self):
        env = {k: v for k, v in os.environ.items() if k != driver.CLAUDE_CONFIG_ENV}
        result = subprocess.run([sys.executable, "-B", str(HERE / "driver.py"), "--arm", "worktrees",
                                 "--family", "rename_vs_new_caller", "--sessions", "2", "--seed", "1",
                                 "--agent", "claude", "--run", "--output", str(self.root / "out")],
                                capture_output=True, text=True, timeout=30, env=env)
        self.assertEqual(result.returncode, 2)
        self.assertIn(driver.CLAUDE_CONFIG_ENV, result.stderr)
        self.assertFalse((self.root / "out").exists())

    def test_duplicate_work_is_a_change_to_a_unit_another_task_owns(self):
        work = self.root / "work"
        work.mkdir()
        repo, base, worktrees = driver.prepare_fixture(work, 2)
        plan = {"sessions": 2, "tasks": [dict(tasks.RENAME, role="rename"),
                                         dict(tasks.feature_task(*tasks.FEATURES[0]), role="new_caller")]}
        # Session 2 adds an alias, which is not the rename: no duplicate.
        config = worktrees[1] / "shelf" / "config.py"
        config.write_text(config.read_text() + "\nload_config = parse_config\n")
        driver.git("commit", "-qam", "alias", cwd=worktrees[1])
        self.assertEqual(driver.duplicates(plan, repo, base), [])
        # Session 2 now renames the function itself: that is session 1's task.
        config.write_text(config.read_text().replace("def parse_config", "def load_config"))
        driver.git("commit", "-qam", "rename too", cwd=worktrees[1])
        self.assertEqual(len(driver.duplicates(plan, repo, base)), 2)  # parse_config gone, load_config added

    def test_an_explicit_report_of_the_contradiction_is_accepted(self):
        checkout = self.root / "checkout"
        shutil.copytree(HERE / "fixture", checkout)
        key, old, a, b = tasks.CONTRADICTIONS[0]
        first, second = (dict(tasks.contradiction_task(key, old, side), role="contradiction") for side in (a, b))
        settings = checkout / "shelf" / "settings.py"
        settings.write_text(settings.read_text().replace('"page_size": 20,', '"page_size": 100,'))
        plan = {"family": "contradictory_defaults", "tasks": [first, second], "config_users": [],
                "integration": [{"session": 1, "merged": True}, {"session": 2, "merged": True}]}
        plan_path = self.root / "plan.json"
        plan_path.write_text(json.dumps(plan))

        def oracle():
            out = self.root / "verdict.json"
            done = subprocess.run([sys.executable, "-I", "-B", str(HERE / "oracle.py"), str(plan_path),
                                   str(checkout), str(out)], capture_output=True, text=True, timeout=120)
            return done.returncode, json.loads(out.read_text())

        code, verdict = oracle()
        self.assertEqual(code, 1)
        self.assertEqual(len(verdict["semantic_conflicts"]), 1, "list_page silently got 100")
        (checkout / "notes").mkdir()
        (checkout / "notes" / "conflict-s2.md").write_text("page_size: export wants 100, listing wants 10.\n")
        code, verdict = oracle()
        self.assertEqual((code, verdict["integrated_tests"]), (0, "passed"), verdict)
        self.assertEqual(verdict["contradictions_reported"], ["page_size"])
        self.assertEqual([t["accepted"] for t in verdict["tasks"]], [True, True])
        self.assertEqual([t["completed"] for t in verdict["tasks"]], [True, False])


class RefusalCollection(Temporary):
    def test_a_real_refusal_is_collected_with_what_followed(self):
        """Two stand-in sessions in one worktree: B edits a file after A read
        it, so LangSpan refuses A's edit as stale; A reads again and is
        admitted. The record's refusal comes from the real event log."""
        work = self.root / "work"
        work.mkdir()
        _, _, worktrees = driver.prepare_fixture(work, 1)
        state = Path(tempfile.mkdtemp(prefix="lsb-", dir="/tmp"))
        self.addCleanup(shutil.rmtree, state, True)
        driver.install_langspan(langspan_binary(), state, worktrees, work)
        coordinator = driver.Coordinator(langspan_binary(), state, self.root / "coordinator.log")
        try:
            settings = json.loads((worktrees[0] / ".claude" / "settings.local.json").read_text())
            target = (worktrees[0] / "shelf" / "report.py").resolve()
            with open(self.root / "hooks.jsonl", "w") as log:
                a, b = (standin_agent.Session(settings, log, str(worktrees[0].resolve())) for _ in range(2))
                for session in (a, b):
                    session.fire("SessionStart", source="startup")
                a.read(target)
                b.read(target)

                def edit(session, old, new):
                    def apply():
                        target.write_text(target.read_text().replace(old, new))
                    return session.edit(target, "Edit", {"file_path": str(target), "old_string": old,
                                                         "new_string": new, "replace_all": False}, apply)

                self.assertTrue(edit(b, "nobody", "no owner"))
                self.assertTrue(edit(a, "one-line summary", "short summary"))
            self.assertEqual(len(a.refusals), 1, "A's first attempt is refused, its retry admitted")
            items = driver.refusal_items(coordinator.events())
        finally:
            coordinator.stop()
        self.assertEqual(len(items), 1, items)
        self.assertEqual(items[0]["kind"], "stale_base")
        self.assertEqual(items[0]["session"], 1)
        self.assertEqual(items[0]["paths"], ["shelf/report.py"])
        self.assertEqual(items[0]["followed_by"], "admitted_later")


class DryRun(Temporary):
    """The whole pipeline for each arm and family, at N = 2 and N = 5."""

    def run_driver(self, arm, family, n, seed=11, mode="naive"):
        output = self.root / ("%s-%s-%d-%s" % (arm, family, n, mode))
        argv = [sys.executable, "-B", str(HERE / "driver.py"), "--arm", arm, "--family", family,
                "--sessions", str(n), "--seed", str(seed), "--standin-mode", mode, "--time-limit", "120",
                "--run", "--output", str(output)]
        if arm == "langspan":
            argv += ["--langspan", str(langspan_binary())]
        done = subprocess.run(argv, capture_output=True, text=True, timeout=600)
        record_path = output / "record.json"
        self.assertTrue(record_path.is_file(), done.stdout + done.stderr)
        record = json.loads(record_path.read_text())
        driver.validate(record, driver.load_schema())
        self.assertNotIn("error", record, record.get("error"))
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertEqual((output.stat().st_mode & 0o777), 0o700)
        return output, record

    def check_common(self, arm, family, n, output, record):
        self.assertEqual((record["arm"], record["family"], record["sessions"]), (arm, family, n))
        self.assertTrue(record["dry_run"])
        self.assertEqual(record["status"], "succeeded")
        self.assertEqual(record["tasks_total"], n)
        self.assertEqual(record["stops"], 0)
        self.assertEqual(record["questions"], 0)
        self.assertEqual(record["tokens"], {"status": "unknown", "total": None})
        self.assertEqual(record["versions"]["agent_cli"], "standin-agent")
        self.assertTrue(record["versions"]["git"].startswith("git version"))
        rows = [json.loads(line) for line in (output / "runner" / "runs.jsonl").read_text().splitlines()]
        self.assertEqual([r["record_type"] for r in rows], ["run_started", "run_finished"])
        self.assertEqual(rows[1]["arm"], arm)
        self.assertEqual([c["phase"] for c in rows[1]["commands"]], ["version"] * 2 + ["agent"] * n + ["check"])
        self.assertEqual(rows[1]["metrics"]["integrated_correctness"]["status"], record["integrated_tests"])
        prompts = json.loads((output / "plan.json").read_text())["prompts"]
        work = output / "work"
        for index in range(1, n + 1):
            settings = work / ("s%d" % index) / ".claude" / "settings.local.json"
            skill = work / ("s%d" % index) / ".claude" / "skills" / "langspan" / "SKILL.md"
            self.assertEqual(settings.is_file(), arm == "langspan")
            self.assertEqual(skill.is_file(), arm == "langspan")
            self.assertEqual("Other sessions are working" in prompts[index - 1], arm == "awareness_only")
            # Nothing of LangSpan's reaches a branch.
            tracked = driver.git("ls-tree", "-r", "--name-only", driver.branch(index), cwd=work / "repo").stdout
            self.assertNotIn(".claude", tracked)
        if arm == "langspan":
            refusals = record["refusals"]
            self.assertEqual(refusals["status"], "reported")
            self.assertEqual(refusals["count"], 0)
            self.assertEqual(refusals["sessions_registered"], n)
            self.assertGreaterEqual(refusals["admissions"], n)
            self.assertTrue((output / "events.jsonl").is_file())
            self.assertEqual([t["refusals"] for t in record["tasks"]], [0] * n)
        else:
            self.assertEqual(record["refusals"]["status"], "not_applicable")
            self.assertIsNone(record["refusals"]["count"])

    def check_naive(self, arm, family, n):
        output, record = self.run_driver(arm, family, n)
        self.check_common(arm, family, n, output, record)
        self.assertEqual(record["integrated_tests"], "failed")
        self.assertEqual(record["duplicate_work"], 0)
        if family == "rename_vs_new_caller":
            # Every new caller uses the old name, which the rename removed.
            self.assertEqual(record["merge_conflicts"], [])
            self.assertEqual(record["semantic_conflicts"], n - 1, record["semantic_conflict_details"])
            self.assertEqual(record["tasks_completed"], 1)
            self.assertEqual([t["completed"] for t in record["tasks"] if t["role"] == "rename"], [True])
        else:
            # Both sides of each pair change the same default: Git refuses the second.
            self.assertEqual(len(record["merge_conflicts"]), n // 2)
            self.assertTrue(all(c["files"] == ["shelf/settings.py"] for c in record["merge_conflicts"]))
            self.assertEqual(record["semantic_conflicts"], 0)
            self.assertEqual(record["tasks_completed"], n // 2 + n % 2)

    def check_coordinated(self, arm, family, n):
        output, record = self.run_driver(arm, family, n, mode="coordinated")
        self.check_common(arm, family, n, output, record)
        self.assertEqual(record["integrated_tests"], "passed", record)
        self.assertEqual((record["semantic_conflicts"], record["merge_conflicts"]), (0, []))
        self.assertEqual(record["tasks_completed"], n)
        self.assertEqual(record["fixture_tests"], "passed")

    def each(self, arm):
        for family in tasks.FAMILIES:
            for n in (2, 5):
                with self.subTest(family=family, n=n, mode="naive"):
                    self.check_naive(arm, family, n)
            with self.subTest(family=family, n=5, mode="coordinated"):
                self.check_coordinated(arm, family, 5)

    def test_dry_run_arm_worktrees(self):
        self.each("worktrees")

    def test_dry_run_arm_awareness_only(self):
        self.each("awareness_only")

    def test_dry_run_arm_langspan(self):
        self.each("langspan")
        # The largest size: ten sessions on one coordinator.
        for family in tasks.FAMILIES:
            with self.subTest(family=family, n=10, mode="naive"):
                self.check_naive("langspan", family, 10)


if __name__ == "__main__":
    unittest.main()
