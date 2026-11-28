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
from contextlib import redirect_stdout

import driver
import matrix
import shared_checkout as shared
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
        self.assertEqual(shared.push_counts(events), {"attempts": 2, "rejections": 1})
        self.assertEqual(shared.push_counts([]), {"attempts": 0, "rejections": 0})
        # A result given as plain text counts the same as one in blocks.
        plain = bash("e", "git push", "")
        plain[1]["message"]["content"][0]["content"] = REJECTED
        self.assertEqual(shared.push_counts(plain), {"attempts": 1, "rejections": 1})

    def test_a_rejected_push_is_counted_and_retried_until_it_lands(self):
        # Real Git: what is verified is Git's own rejection and the retry.
        repo = self.root / "repo"
        repo.mkdir()
        (repo / "a.py").write_text("A = 1\n")
        (repo / "b.py").write_text("B = 1\n")
        driver.git("init", "-q", "-b", "main", cwd=repo)
        driver.git("add", "-A", cwd=repo)
        driver.git("commit", "-q", "-m", "Fixture", cwd=repo)
        first = shared.prepare(self.root, repo)
        second = self.root / "other"
        driver.git("clone", "-q", str(self.root / "origin.git"), str(second), cwd=self.root)
        shared.identify(second)
        (first / "a.py").write_text("A = 2\n")
        (second / "b.py").write_text("B = 2\n")
        sink = io.StringIO()
        with redirect_stdout(sink):
            self.assertTrue(shared.commit_and_push(["a.py"], 1, cwd=first))
            self.assertTrue(shared.commit_and_push(["b.py"], 2, cwd=second))
        events = driver.transcript_events(sink.getvalue())
        self.assertEqual(shared.push_counts(events), {"attempts": 3, "rejections": 1})
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
            argv = driver.claude_argv("claude", "", arm)
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

    def test_the_summary_and_report_show_push_attempts_and_rejections(self):
        pushed = [dict(record("shared", seed), pushes={"status": "reported", "attempts": attempts,
                                                       "rejections": rejected})
                  for seed, attempts, rejected in ((1, 3, 1), (2, 2, 0))]
        # A version 8 record has no pushes: not applicable, never zero.
        older = dict(record("worktrees", 1), schema_version=8)
        groups = {group["arm"]: group for group in driver.summarize(pushed + [older])}
        self.assertEqual((groups["shared"]["push_attempts"], groups["shared"]["push_rejections"]), (5, 1))
        self.assertEqual((groups["worktrees"]["push_attempts"], groups["worktrees"]["push_rejections"]), (None, None))
        text = matrix.report([("t%d" % r["seed"], r) for r in pushed] + [("old", older)], [])
        self.assertIn("Push attempts", text)
        self.assertIn("Push rejections", text)
        rows = {line.split(" | ")[0][2:]: line for line in text.splitlines() if line.startswith("| t") or
                line.startswith("| old")}
        self.assertTrue(rows["t1"].endswith("| 3 | 1 |"), rows["t1"])
        self.assertTrue(rows["old"].endswith("| not applicable | not applicable |"), rows["old"])
        shared_row = next(line for line in text.splitlines() if " | shared | " in line and line.startswith("| standin"))
        self.assertTrue(shared_row.endswith("| 5 | 1 |"), shared_row)

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

    def run_shared(self, n):
        output = self.root / ("shared-%d" % n)
        done = subprocess.run([sys.executable, "-B", str(HERE / "driver.py"), "--arm", "shared", "--family",
                               "rename_vs_new_caller", "--sessions", str(n), "--seed", "11", "--time-limit", "600",
                               "--run", "--output", str(output)], capture_output=True, text=True, timeout=900)
        record_path = output / "record.json"
        self.assertTrue(record_path.is_file(), done.stdout + done.stderr)
        found = json.loads(record_path.read_text())
        driver.validate(found, driver.load_schema())
        self.assertNotIn("error", found, found.get("error"))
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertEqual((found["arm"], found["isolation"], found["status"]), ("shared", "shared", "succeeded"))
        self.assertEqual(found["tool_check"], {"status": "passed", "failed_sessions": []})
        self.assertEqual(found["refusals"]["status"], "not_applicable")
        manifest = json.loads((output / "manifest.json").read_text())
        agents = next(iter(manifest["items"][0]["arms"].values()))["agents"]
        self.assertEqual({Path(a["cwd"]).resolve() for a in agents}, {(output / "work" / "trunk").resolve()})
        for task in found["tasks"]:
            self.assertEqual(task["branch"], "main")
            pushes = task["pushes"]
            self.assertGreaterEqual(pushes["attempts"], 1, task)
            self.assertLessEqual(pushes["rejections"], pushes["attempts"], task)
        total = found["pushes"]
        self.assertEqual(total["status"], "reported")
        self.assertEqual(total["attempts"], sum(t["pushes"]["attempts"] for t in found["tasks"]))
        self.assertEqual(total["rejections"], sum(t["pushes"]["rejections"] for t in found["tasks"]))
        # The combined tree is what origin main holds: one commit per session past the fixture at least.
        log = driver.git("rev-list", "--count", "main", cwd=output / "work" / "origin.git").stdout
        self.assertGreaterEqual(int(log), n + 1)
        return found

    def test_dry_run_shared_arm_two_sessions(self):
        self.run_shared(2)

    def test_dry_run_shared_arm_five_sessions(self):
        self.run_shared(5)


if __name__ == "__main__":
    unittest.main()
