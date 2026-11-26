"""The refuse-against-wait comparison (holder_wait.py) through the real
`langspan` binary and its hooks, with the stand-in pair: no model runs.
`cargo test` sets LANGSPAN_BENCH_LANGSPAN to the binary it built."""

import json
import os
from pathlib import Path
import signal
import tempfile
import unittest

import driver
import holder_wait


def langspan():
    value = os.environ.get("LANGSPAN_BENCH_LANGSPAN")
    if not value:
        raise AssertionError("set LANGSPAN_BENCH_LANGSPAN to the langspan binary built from this checkout")
    return value


@unittest.skipUnless(os.name == "posix", "the coordinator needs Unix sockets")
class HolderWaitStandin(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="langspan-holder-")
        self.out = Path(self.temp.name) / "out"

    def tearDown(self):
        self.temp.cleanup()

    def test_each_arm_keeps_one_record_per_run_with_its_policy_and_measures(self):
        for arm in holder_wait.ARMS:
            self.assertEqual(holder_wait.main(["--arm", arm, "--out", str(self.out), "--langspan", langspan()]), 0)
        runs = [json.loads(line) for line in (self.out / "runs.jsonl").read_text().splitlines()]
        self.assertEqual([run["arm"] for run in runs], list(holder_wait.ARMS))
        for run in runs:
            self.assertIsNone(run["error"], run)
            self.assertEqual(run["status"], "completed", run)
            self.assertTrue(run["oracle"]["accepted"], run)
            self.assertTrue(run["measures"]["held"], run)
        refuse, wait = runs
        self.assertEqual(refuse["policy"][1]["policy"], "refuse")
        self.assertEqual(wait["policy"][1], {"case": "conflict", "policy": "wait", "configurable": True,
                                             "wait_ms": 2000})
        # Refused at once: a conflict, then a retry after a new read.
        self.assertEqual(refuse["measures"]["waits"], 0)
        self.assertGreaterEqual(refuse["measures"]["refusals_by_kind"].get("conflict", 0), 1)
        # Waited for the holder, which wrote the file: a stale refusal, not
        # an admission, and then the retry.
        self.assertEqual(wait["measures"]["waits_by_result"], {"refused_stale": 1})
        self.assertEqual(wait["measures"]["refusals_by_kind"], {"stale_base": 1})
        for run in runs:
            self.assertGreaterEqual(run["measures"]["retries"], 1, run)
            self.assertTrue(run["measures"]["ms_to_next_admitted"], run)

        report = holder_wait.summarize(self.out)
        for arm in holder_wait.ARMS:
            self.assertEqual(report[arm]["runs"], 1)
            self.assertEqual(report[arm]["standin_runs"], 1)
            self.assertEqual(report[arm]["held_runs"], 1)
            self.assertEqual(report[arm]["stop_rate"], 0.0)
        self.assertIn("not a result", holder_wait.text(report))
        # A second invocation adds runs; it never replaces a kept one.
        holder_wait.main(["--arm", "langspan_wait", "--out", str(self.out), "--langspan", langspan()])
        self.assertEqual(holder_wait.summarize(self.out)["langspan_wait"]["runs"], 2)


class CoordinatorLifetime(unittest.TestCase):
    def test_an_interrupt_while_the_coordinator_starts_stops_it(self):
        """Issue #239: until the constructor returns, no caller holds the
        coordinator to stop it. A stand-in `langspan` whose `daemon` runs
        until killed and whose `status` interrupts this test process makes
        the constructor raise; the daemon must not outlive it."""
        with tempfile.TemporaryDirectory() as temp:
            pid_file = Path(temp, "daemon.pid")
            fake = Path(temp, "langspan")
            fake.write_text("#!/bin/sh\n"
                            "if [ \"$1\" = daemon ]; then echo $$ > '%s'; exec sleep 600; fi\n"
                            "while [ ! -s '%s' ]; do sleep 0.05; done\n"
                            "kill -INT $PPID; sleep 5\n" % (pid_file, pid_file))
            fake.chmod(0o700)
            with self.assertRaises(KeyboardInterrupt):
                driver.Coordinator(str(fake), Path(temp, "state"), Path(temp, "daemon.log"))
            pid = int(pid_file.read_text())
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                return
            os.kill(pid, signal.SIGKILL)  # This test leaves nothing running either.
            self.fail("daemon %d outlived the interrupt" % pid)


class CoordinatorSocketPath(unittest.TestCase):
    def test_a_long_state_folder_is_refused_naming_the_socket_path_length(self):
        with tempfile.TemporaryDirectory() as temp:
            state = Path(temp, "x" * 120, "state")
            length = len(os.fsencode(os.path.join(os.path.realpath(state), "daemon.sock")))
            with self.assertRaises(RuntimeError) as caught:
                driver.Coordinator("langspan-not-run", state, Path(temp, "daemon.log"))
            message = str(caught.exception)
            self.assertIn("socket path is %d bytes" % length, message)
            self.assertIn("over the %d" % driver.MAX_SOCKET_PATH, message)
            self.assertNotIn("did not start", message)
            self.assertFalse(Path(temp, "daemon.log").exists())


class HolderWaitSummary(unittest.TestCase):
    def test_a_broken_run_counts_and_the_stop_rate_is_of_the_runs_that_met_a_holder(self):
        with tempfile.TemporaryDirectory() as temp:
            measures = lambda held, stopped: {"held": held, "stopped": stopped, "retries": 1 - stopped,
                                              "refusals": 1, "waits": 0, "ms_to_next_admitted": [900]}
            runs = [
                {"arm": "langspan_refuse", "agent": "claude", "status": "completed", "elapsed_s": 10,
                 "measures": measures(True, 1)},
                {"arm": "langspan_refuse", "agent": "claude", "status": "completed", "elapsed_s": 20,
                 "measures": measures(True, 0)},
                {"arm": "langspan_refuse", "agent": "claude", "status": "completed", "elapsed_s": 30,
                 "measures": measures(False, 0)},
                {"arm": "langspan_refuse", "agent": "claude", "status": "error", "elapsed_s": 1,
                 "error": "RuntimeError: no coordinator"},
            ]
            Path(temp, "runs.jsonl").write_text("".join(json.dumps(run) + "\n" for run in runs))
            report = holder_wait.summarize(temp)
            row = report["langspan_refuse"]
            self.assertEqual(row["runs"], 4)
            self.assertEqual(row["by_status"], {"completed": 3, "error": 1})
            self.assertEqual(row["held_runs"], 2)
            self.assertEqual(row["stop_rate"], 0.5)
            self.assertEqual(row["median_elapsed_s"], 15)
            self.assertEqual(report["langspan_wait"]["runs"], 0)
            self.assertIsNone(report["langspan_wait"]["stop_rate"])


if __name__ == "__main__":
    unittest.main()
