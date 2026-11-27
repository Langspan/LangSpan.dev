"""The matrix of trials and the results document (matrix.py).

MatrixTests runs in process, except where a test says it verifies child
processes. MatrixDryRun runs real dry-run trials through driver.py, with
the langspan binary in LANGSPAN_BENCH_LANGSPAN for the LangSpan arm.
No model runs.
"""

import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest

import driver
import matrix

HERE = Path(__file__).resolve().parent


def record(arm, seed, integrated="passed", status="succeeded", change="made", tokens=(10, 20, 30, 40),
           repair="not_needed", repair_succeeded=None, writes=0, fixed=None, dry_run=True):
    """A version 8 record with the fields the summary and the report read.
    TOKENS is (input, cache_read, cache_creation, output), or None when the
    host reported none."""
    names = ("input", "cache_read", "cache_creation", "output")
    run_tokens = dict(zip(names, tokens)) if tokens else None
    repair_tokens = dict.fromkeys(names, 0) if repair == "not_needed" else (
        dict.fromkeys(names, 1) if repair == "ran" else None)
    run = {"tokens": run_tokens, "turns": 3 if tokens else None, "cost_usd": 0.5 if tokens else None}
    spent = {"tokens": repair_tokens, "turns": 0 if repair == "not_needed" else (1 if repair == "ran" else None),
             "cost_usd": 0 if repair == "not_needed" else (0.25 if repair == "ran" else None)}
    correct = True if repair == "not_needed" else (repair_succeeded if repair == "ran" else False)
    return {
        "schema_version": driver.SCHEMA_VERSION, "record_type": "coordination_run", "arm": arm,
        "family": "discovered_change", "sessions": 2, "seed": seed, "agent": "standin" if dry_run else "claude-code",
        "dry_run": dry_run, "status": status, "stagger_s": 0, "elapsed_s": 12.5, "turns": run["turns"],
        "interface_change": change, "integrated_tests": integrated if status != "refused" else "unknown",
        "tasks_total": 2, "tasks_accepted": 2 if integrated == "passed" else 1,
        "tool_check": {"status": "passed", "failed_sessions": []},
        "stale_callers": {"status": "reported", "writes_admitted": writes, "stale_files": writes,
                          "unfixed": 0, "fixed_after_s": fixed, "items": []},
        "context_added": {"status": "not_applicable", **dict.fromkeys(driver.CONTEXT_FIELDS)},
        "tokens": {"status": "reported" if tokens else "unknown", "total": run_tokens},
        "cost_usd": run["cost_usd"],
        "versions": {"agent_cli": "standin-agent", "requested_model": None, "models": [],
                     "langspan_commit": "abc123", "langspan_binary_sha256": "d" * 64, "git": "git version 2.50.0",
                     "python": "3.12.0"},
        "repair": {"status": repair, "succeeded": repair_succeeded, "integrated_tests":
                   ("passed" if repair_succeeded else "failed") if repair == "ran" else "unknown",
                   "tool_check": {"status": "passed", "unexpected": []} if repair == "ran" else None},
        "cost": {"run": run, "repair": spent, "total": driver.added_figures(run, spent), "tasks_accepted": 2,
                 "correct_result": correct, "cost_per_accepted_task": 0.5},
    }


class MatrixTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="langspan-matrix-test-")
        self.root = Path(self.temp.name)

    def tearDown(self):
        self.temp.cleanup()

    def test_the_wilson_interval_matches_known_values(self):
        for (k, n), (low, high) in {(0, 5): (0.0, 0.4345), (5, 5): (0.5655, 1.0), (3, 5): (0.2307, 0.8824),
                                    (1, 2): (0.0945, 0.9055)}.items():
            with self.subTest(k=k, n=n):
                found = matrix.wilson(k, n)
                self.assertAlmostEqual(found[0], low, places=4)
                self.assertAlmostEqual(found[1], high, places=4)
        self.assertIsNone(matrix.wilson(0, 0))

    def test_the_trial_list_rotates_the_arm_order_with_the_seed(self):
        trials = matrix.trials(["discovered_change"], [2, 5], [1, 2, 3, 4], list(driver.ARMS))
        self.assertEqual(len(trials), 2 * 4 * 3)
        orders = [[t["arm"] for t in trials if (t["sessions"], t["seed"]) == (2, seed)] for seed in (1, 2, 3, 4)]
        self.assertEqual(orders, [["worktrees", "awareness_only", "langspan"],
                                  ["awareness_only", "langspan", "worktrees"],
                                  ["langspan", "worktrees", "awareness_only"],
                                  ["worktrees", "awareness_only", "langspan"]])
        self.assertEqual(trials[0]["name"], "discovered_change-n2-s1-worktrees")
        self.assertEqual(len({t["name"] for t in trials}), len(trials))

    def test_plan_only_counts_the_trials_and_sessions_and_runs_nothing(self):
        # A child process: what is verified is the command line the owner runs.
        out = self.root / "out"
        done = subprocess.run([sys.executable, "-I", "-B", str(HERE / "matrix.py"), "run", "--plan-only",
                               "--families", "discovered_change", "--sessions", "2", "5", "--seeds", "1", "2", "3",
                               "4", "5", "--output", str(out)], capture_output=True, text=True, timeout=60)
        self.assertEqual(done.returncode, 0, done.stderr)
        plan = json.loads(done.stdout)
        self.assertEqual((plan["mode"], plan["executed_commands"], plan["trials"], plan["sessions"],
                          plan["max_repair_sessions"]), ("plan_only", 0, 30, 105, 30))
        self.assertEqual(len(plan["trial_list"]), 30)
        self.assertFalse(out.exists())

    def test_the_report_lists_every_trial_and_sums_tokens_per_condition(self):
        records = [
            record("langspan", 1, fixed=4.5), record("langspan", 2, tokens=(1, 2, 3, 4)),
            record("worktrees", 1, integrated="failed", repair="ran", repair_succeeded=True, writes=2),
            record("worktrees", 2, integrated="failed", status="refused", tokens=None, repair="not_run"),
            record("awareness_only", 1, integrated="failed", change="change_not_made", repair="not_run"),
        ]
        text = matrix.report([("discovered_change-n2-s%d-%s" % (r["seed"], r["arm"]), r) for r in records],
                             ["discovered_change-n2-s3-langspan"])
        self.assertTrue(text.splitlines()[0].startswith("# "))
        self.assertIn("Dry run", text.splitlines()[2])
        self.assertIn("No result here is a LangSpan advantage unless the counts above show it.", text)
        for name in ("discovered_change-n2-s1-langspan", "discovered_change-n2-s2-worktrees",
                     "discovered_change-n2-s1-awareness_only", "discovered_change-n2-s3-langspan"):
            self.assertIn(name, text)
        trial_rows = [line for line in text.splitlines() if line.startswith("| discovered_change-")]
        self.assertEqual(len(trial_rows), 5)
        refused = next(line for line in trial_rows if "s2-worktrees" in line)
        self.assertIn("| refused | refused |", refused)
        self.assertIn("change_not_made", next(line for line in trial_rows if "awareness_only" in line))
        # The incomplete trial is listed apart.
        self.assertIn("No record", text)
        conditions = {line.split(" | ")[4]: line for line in text.splitlines()
                      if line.startswith("| claude-code |") or line.startswith("| standin |")}
        # langspan: two trials, both passed; tokens summed over both.
        self.assertIn("| 2 of 2 (0.342 to 1.000) |", conditions["langspan"])
        self.assertIn("11 / 22 / 33 / 44", conditions["langspan"])
        self.assertIn("4.5", conditions["langspan"])
        # worktrees: one compared trial, which failed and was repaired.
        self.assertIn("| 0 of 1 (0.000 to 0.793) |", conditions["worktrees"])
        self.assertIn("11 / 21 / 31 / 41", conditions["worktrees"])
        self.assertIn("1 refused", conditions["worktrees"])
        self.assertIn("1 change_not_made", conditions["awareness_only"])
        self.assertIn("abc123", text)
        self.assertIn("Seeds: 1, 2", text)
        self.assertNotIn("$", text)
        self.assertNotIn("cost", text.lower())

    def test_a_live_report_does_not_say_dry_run(self):
        text = matrix.report([("t", record("langspan", 1, dry_run=False))], [])
        self.assertNotIn("Dry run", text)
        self.assertIn("claude-code", text)

    def test_the_report_refuses_an_older_record(self):
        trial = self.root / "out" / "discovered_change-n2-s1-langspan"
        trial.mkdir(parents=True)
        (trial / "record.json").write_text(json.dumps({"schema_version": 6}))
        done = subprocess.run([sys.executable, "-I", "-B", str(HERE / "matrix.py"), "report", str(self.root / "out"),
                               "--output", str(self.root / "results.md")], capture_output=True, text=True, timeout=60)
        self.assertEqual(done.returncode, 2)
        self.assertIn("schema version 6", done.stderr)
        self.assertFalse((self.root / "results.md").exists())

    def test_the_matrix_keeps_going_past_a_refused_trial(self):
        # Child processes: what is verified is that the matrix runs each
        # trial's driver and keeps every record. Without --langspan the
        # driver refuses the LangSpan arm before preparing anything.
        out = self.root / "out"
        done = subprocess.run([sys.executable, "-I", "-B", str(HERE / "matrix.py"), "run", "--families",
                               "discovered_change", "--sessions", "2", "--seeds", "1", "2", "--arms", "langspan",
                               "--output", str(out)], capture_output=True, text=True, timeout=120)
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        lines = [json.loads(line) for line in (out / "matrix.jsonl").read_text().splitlines()]
        self.assertEqual([(line["trial"], line["outcome"], line["exit_code"]) for line in lines],
                         [("discovered_change-n2-s1-langspan", "ran", 2), ("discovered_change-n2-s2-langspan", "ran", 2)])
        for line in lines:
            kept = json.loads((out / line["trial"] / "record.json").read_text())
            self.assertEqual(kept["status"], "refused")
        self.assertEqual((out.stat().st_mode & 0o777), 0o700)
        self.assertEqual(((out / "matrix.jsonl").stat().st_mode & 0o777), 0o600)

    def test_a_kept_record_is_reused_only_for_the_same_agent_and_schema(self):
        # In process: every case here is decided before any driver starts.
        live, dry = json.dumps(record("worktrees", 1, dry_run=False)), json.dumps(record("worktrees", 1))
        cases = [("standin", dry, "kept"), ("claude", live, "kept"), ("claude", dry, "agent_mismatch"),
                 ("standin", live, "agent_mismatch"), ("claude", "{", "unusable_record"),
                 ("standin", "[]", "unusable_record"), ("standin", json.dumps({"schema_version": 6}),
                                                         "unusable_record"),
                 ("standin", json.dumps(dict(record("worktrees", 1), agent=None)), "agent_mismatch")]
        for index, (agent, body, outcome) in enumerate(cases):
            with self.subTest(agent=agent, outcome=outcome, case=index):
                out = self.root / ("out%d" % index)
                (out / "discovered_change-n2-s1-worktrees").mkdir(parents=True)
                (out / "discovered_change-n2-s1-worktrees" / "record.json").write_text(body)
                planned = matrix.trials(["discovered_change"], [2], [1], ["worktrees"])
                missing = matrix.run_matrix(planned, out, agent, None)
                line = json.loads((out / "matrix.jsonl").read_text())
                self.assertEqual((line["outcome"], line["exit_code"], missing), (outcome, None, outcome != "kept"))
                self.assertFalse((out / "discovered_change-n2-s1-worktrees.driver.log").exists())


def live_processes(pids):
    """Those of PIDS that can still run, from one `ps` listing. A zombie
    can't; where PID 1 does not reap orphans it stays listed, so
    `kill(pid, 0)` alone would report it running."""
    listing = subprocess.run(["ps", "-A", "-o", "pid=,stat="], capture_output=True, text=True, check=True,
                             timeout=10).stdout
    running = {int(fields[0]) for fields in (line.split() for line in listing.splitlines())
               if len(fields) >= 2 and fields[0].isdigit() and not fields[1].startswith("Z")}
    return [pid for pid in pids if pid in running]


class StopTests(unittest.TestCase):
    """Child processes: what is verified is that stopping a trial stops the
    driver and the sessions it started in process groups of their own."""

    DRIVER = ("import subprocess, sys, time\n"
              "kid = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(600)'], "
              "start_new_session=True)\n"
              "open(sys.argv[1] + '.tmp', 'w').write('%d %d' % (__import__('os').getpid(), kid.pid))\n"
              "__import__('os').rename(sys.argv[1] + '.tmp', sys.argv[1])\n"
              "time.sleep(600)\n")

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="langspan-matrix-stop-")
        self.root = Path(self.temp.name)
        (self.root / "driver.py").write_text(self.DRIVER)
        self.pids = self.root / "pids"
        self.argv = [sys.executable, "-I", "-B", str(self.root / "driver.py"), str(self.pids)]

    def tearDown(self):
        for pid in self.started():
            try:
                os.kill(pid, signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass
        self.temp.cleanup()

    def started(self):
        return [int(pid) for pid in self.pids.read_text().split()] if self.pids.is_file() else []

    def wait_for_pids(self):
        deadline = time.monotonic() + 30
        while not self.pids.is_file():
            self.assertLess(time.monotonic(), deadline, "the stand-in driver did not start")
            time.sleep(.05)
        return self.started()

    def assert_gone(self, pids):
        deadline = time.monotonic() + 15
        while True:
            running = live_processes(pids)
            if not running:
                break
            self.assertLess(time.monotonic(), deadline, "process %d is still running" % running[0])
            time.sleep(.05)
        # Gone: tearDown must not signal these PIDs, which may be reused.
        self.pids.unlink()

    def test_a_zombie_counts_as_stopped_and_a_running_process_does_not(self):
        # Where PID 1 does not reap orphans, a stopped session stays a zombie
        # and `kill(pid, 0)` still succeeds. An unreaped child of this test
        # is the same zombie on every platform.
        child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(600)"])
        try:
            self.assertEqual(live_processes([child.pid]), [child.pid])
            child.kill()
            deadline = time.monotonic() + 15
            while live_processes([child.pid]):
                self.assertLess(time.monotonic(), deadline, "process %d did not exit" % child.pid)
                time.sleep(.05)
            os.kill(child.pid, 0)  # Still a zombie: not yet reaped.
            self.pids.write_text(str(child.pid))
            self.assert_gone([child.pid])
        finally:
            child.kill()
            child.wait()

    def test_a_timed_out_trial_stops_the_sessions_in_their_own_groups(self):
        self.assertIsNone(matrix.run_trial(self.argv, self.root / "log", timeout=2))
        self.assert_gone(self.started())

    def test_an_interrupted_or_terminated_matrix_stops_its_trial_and_exits(self):
        for signum in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
            with self.subTest(signal=signum):
                self.pids.unlink(missing_ok=True)
                code = "import sys; sys.path.insert(0, sys.argv[1]); import matrix; matrix.run_trial(%r, %r)" % (
                    self.argv, str(self.root / "log"))
                harness = subprocess.Popen([sys.executable, "-I", "-B", "-c", code, str(HERE)],
                                           stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                                           start_new_session=True)
                try:
                    pids = self.wait_for_pids()
                    os.kill(harness.pid, signum)
                    _, stderr = harness.communicate(timeout=60)
                finally:
                    if harness.poll() is None:
                        harness.kill()
                        harness.wait()
                self.assertIn(b"KeyboardInterrupt", stderr)
                self.assert_gone(pids)


class MatrixDryRun(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="langspan-matrix-dry-")
        self.root = Path(self.temp.name)

    def tearDown(self):
        self.temp.cleanup()

    def matrix(self, *args):
        return subprocess.run([sys.executable, "-B", str(HERE / "matrix.py"), *args], capture_output=True, text=True,
                              timeout=1800)

    def test_a_dry_run_matrix_keeps_every_trial_and_resumes(self):
        # Real dry-run trials (Git, child processes, the coordinator): what is
        # verified is the matrix run end to end, its resume and its report.
        langspan = os.environ.get("LANGSPAN_BENCH_LANGSPAN")
        if not langspan:
            raise AssertionError("set LANGSPAN_BENCH_LANGSPAN to the langspan binary built from this checkout")
        out = self.root / "out"
        common = ["--families", "discovered_change", "--sessions", "2", "--output", str(out)]
        done = self.matrix("run", *common, "--seeds", "1", "--langspan", langspan)
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        names = ["discovered_change-n2-s1-%s" % arm for arm in driver.ARMS]
        self.assertTrue(all((out / name / "record.json").is_file() for name in names))
        # Run again with a trial planted that has a directory and no record:
        # nothing is repeated and the planted trial is reported incomplete.
        (out / "discovered_change-n2-s2-worktrees").mkdir()
        done = self.matrix("run", *common, "--seeds", "1", "2", "--arms", "worktrees")
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
        lines = [json.loads(line) for line in (out / "matrix.jsonl").read_text().splitlines()]
        self.assertEqual([(line["trial"], line["outcome"]) for line in lines[3:]],
                         [("discovered_change-n2-s1-worktrees", "kept"),
                          ("discovered_change-n2-s2-worktrees", "incomplete")])
        report = self.root / "results.md"
        done = self.matrix("report", str(out), "--output", str(report))
        self.assertEqual(done.returncode, 0, done.stderr)
        text = report.read_text()
        rows = {arm: next(line for line in text.splitlines() if line.startswith("| discovered_change-n2-s1-" + arm))
                for arm in driver.ARMS}
        self.assertIn("| passed |", rows["langspan"])
        self.assertIn("| failed |", rows["worktrees"])
        self.assertIn("| failed |", rows["awareness_only"])
        self.assertIn("discovered_change-n2-s2-worktrees", text)
        self.assertNotIn("$", text)


if __name__ == "__main__":
    unittest.main()
