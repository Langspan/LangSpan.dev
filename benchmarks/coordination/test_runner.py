"""Runner mechanics using real local child processes, never mocked models."""

import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest

import runner


@unittest.skipUnless(os.name == "posix", "owned process-group cleanup requires POSIX")
class RunnerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="langspan-bench-test-")
        self.root = Path(self.temp.name)

    def tearDown(self):
        self.temp.cleanup()

    def command(self, name, source, *args, **extra):
        return {
            "id": name,
            "argv": [sys.executable, "-c", source, *args],
            "cwd": str(self.root),
            "timeout_seconds": 3,
            **extra,
        }

    def manifest(self, agents, **arm):
        path = self.root / "manifest.json"
        path.write_text(json.dumps({
            "schema_version": 1,
            "benchmark_id": "runner-mechanics",
            "items": [{
                "id": "real-process-fixture",
                "description": "Runner behavior only; no agent-quality evidence.",
                "arms": {"no_tool": {"agents": agents, **arm}},
            }],
        }))
        return path

    def run_benchmark(self, manifest, repetitions=1):
        output = self.root / "output"
        runner.run_manifest(manifest, output, repetitions)
        rows = [json.loads(line) for line in (output / "runs.jsonl").read_text().splitlines()]
        finals = [row for row in rows if row["record_type"] == "run_finished"]
        return output, rows, finals

    def test_success_captures_real_output_and_never_invents_coverage_or_correctness(self):
        manifest = self.manifest([
            self.command("a", "print('agent A output')"),
            self.command("b", "import sys; print('diagnostic B', file=sys.stderr)"),
        ])
        output, rows, finals = self.run_benchmark(manifest)
        self.assertEqual([r["record_type"] for r in rows], ["run_started", "run_finished"])
        self.assertEqual(len(finals), 1)
        result = finals[0]
        self.assertEqual(result["status"], "succeeded")
        self.assertEqual(len(result["commands"]), 2)
        self.assertEqual((output / result["commands"][0]["stdout_path"]).read_text(), "agent A output\n")
        self.assertEqual((output / result["commands"][1]["stderr_path"]).read_text(), "diagnostic B\n")
        self.assertGreater(result["elapsed_ns"], 0)
        self.assertEqual(result["metrics"]["coverage"]["status"], "unknown")
        self.assertIsNone(result["metrics"]["coverage"]["observed_tool_calls"])
        self.assertTrue(all(value is None for value in result["metrics"]["coverage"]["counts"].values()))
        self.assertIsNone(result["metrics"]["human_interventions"]["count"])
        self.assertEqual(result["metrics"]["integrated_correctness"]["status"], "unknown")

    def test_parallel_children_can_rendezvous_and_checks_run_after_both(self):
        rendezvous = (
            "from pathlib import Path; import sys,time; "
            "Path(sys.argv[1]).write_text('ready'); "
            "deadline=time.monotonic()+2\n"
            "while not Path(sys.argv[2]).exists():\n"
            " if time.monotonic()>deadline: raise SystemExit(7)\n"
            " time.sleep(.01)\n"
            "Path(sys.argv[3]).write_text('finished')"
        )
        manifest = self.manifest([
            self.command("a", rendezvous, "a.ready", "b.ready", "a.done"),
            self.command("b", rendezvous, "b.ready", "a.ready", "b.done"),
        ], checks=[
            self.command("integration", "from pathlib import Path; assert Path('a.done').exists() and Path('b.done').exists()", assesses="integrated_correctness")
        ])
        _, _, finals = self.run_benchmark(manifest)
        self.assertEqual(finals[0]["status"], "succeeded")
        self.assertEqual(finals[0]["metrics"]["integrated_correctness"]["status"], "passed")
        self.assertEqual([c["phase"] for c in finals[0]["commands"]], ["agent", "agent", "check"])

    def test_failures_and_all_repetitions_are_retained_and_checks_still_run(self):
        manifest = self.manifest([
            self.command("failure", "import sys; print('failed attempt'); sys.exit(9)")
        ], checks=[
            self.command("integration", "import sys; print('incorrect integrated state'); sys.exit(1)", assesses="integrated_correctness")
        ])
        _, rows, finals = self.run_benchmark(manifest, repetitions=2)
        self.assertEqual(len(rows), 4)
        self.assertEqual([r["repetition"] for r in finals], [1, 2])
        self.assertTrue(all(r["status"] == "failed" for r in finals))
        self.assertTrue(all(r["commands"][0]["exit_code"] == 9 for r in finals))
        self.assertTrue(all(r["metrics"]["integrated_correctness"]["status"] == "failed" for r in finals))

    def test_timeout_retains_partial_output_and_stops_own_descendants(self):
        child_pid = self.root / "child.pid"
        heartbeat = self.root / "heartbeat"
        descendant = (
            "from pathlib import Path; import os,time; "
            "Path('child.pid').write_text(str(os.getpid()))\n"
            "while True:\n"
            " with open('heartbeat','ab') as f: f.write(b'x')\n"
            " time.sleep(.01)\n"
        )
        parent = (
            "import subprocess,sys,time; "
            "subprocess.Popen([sys.executable,'-c',sys.argv[1]]); "
            "print('started before timeout',flush=True); time.sleep(60)"
        )
        # Include interpreter and descendant startup in the deadline; a
        # subsecond budget can expire before either fixture executes.
        manifest = self.manifest([
            self.command("timeout", parent, descendant, timeout_seconds=5)
        ], checks=[self.command("after", "print('check after timeout')")])
        try:
            output, _, finals = self.run_benchmark(manifest)
            command = finals[0]["commands"][0]
            self.assertEqual(command["status"], "timed_out")
            self.assertEqual(finals[0]["status"], "timed_out")
            self.assertIn("started before timeout", (output / command["stdout_path"]).read_text())
            self.assertEqual(finals[0]["commands"][-1]["phase"], "check")
            self.assertTrue(command["cleanup"]["sigterm_sent"])
            size = heartbeat.stat().st_size
            time.sleep(.15)
            self.assertEqual(heartbeat.stat().st_size, size, "descendant survived owned-group cleanup")
            self.assertEqual(finals[0]["metrics"]["integrated_correctness"]["status"], "unknown")
        finally:
            # Only the PID written by this fixture's own child is considered.
            if child_pid.exists():
                try:
                    os.kill(int(child_pid.read_text()), signal.SIGKILL)
                except ProcessLookupError:
                    pass

    def test_spawn_failure_is_a_record_and_output_limit_is_bounded(self):
        missing = self.command("missing", "")
        missing["argv"] = [str(self.root / "does-not-exist")]
        manifest = self.manifest([
            missing,
            self.command("noisy", "import sys; sys.stdout.write('x'*1000000); sys.stdout.flush()", max_output_bytes=1024),
        ])
        output, _, finals = self.run_benchmark(manifest)
        commands = finals[0]["commands"]
        self.assertEqual(commands[0]["status"], "spawn_error")
        self.assertEqual(commands[1]["status"], "output_limit")
        self.assertEqual((output / commands[1]["stdout_path"]).stat().st_size, 1024)
        self.assertTrue(commands[1]["stdout_truncated"])

    def test_argv_is_literal_and_dry_run_never_launches_a_command(self):
        marker = self.root / "should-not-exist"
        payload = "$(touch " + str(marker) + "); " + chr(96) + "touch " + str(marker) + chr(96)
        manifest = self.manifest([
            self.command("literal", "import sys; print(sys.argv[1])", payload)
        ])
        result = subprocess.run([sys.executable, str(Path(runner.__file__)), str(manifest)], capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((self.root / "output").exists())
        self.assertFalse(marker.exists())
        output, _, finals = self.run_benchmark(manifest)
        self.assertEqual((output / finals[0]["commands"][0]["stdout_path"]).read_text().strip(), payload)
        self.assertFalse(marker.exists())

    def test_explicit_evidence_can_report_counts_but_missing_evidence_stays_unknown(self):
        report = {
            "coverage": {
                "observed_tool_calls": 3,
                "counts": {name: (1 if name == "denied" else 0) for name in runner.COVERAGE_STATES},
                "evidence_refs": ["host-events.jsonl"],
            },
            "human_interventions": {"count": 0, "evidence_refs": ["human-log.jsonl"]},
        }
        source = (
            "from pathlib import Path; import os,json,sys; "
            "p=Path(os.environ['LANGSPAN_BENCH_RUN_DIR']); "
            "(p/'host-events.jsonl').write_text('fixture host evidence, not real host calls\\n'); "
            "(p/'human-log.jsonl').write_text('fixture intervention evidence\\n'); "
            "(p/'assessment.json').write_text(sys.argv[1])"
        )
        manifest = self.manifest([
            self.command("evidence", source, json.dumps(report)),
        ], evidence_file="assessment.json")
        _, _, finals = self.run_benchmark(manifest)
        self.assertEqual(finals[0]["metrics"]["coverage"]["status"], "reported")
        self.assertEqual(finals[0]["metrics"]["coverage"]["observed_tool_calls"], 3)
        self.assertEqual(finals[0]["metrics"]["human_interventions"]["count"], 0)
        self.assertEqual(finals[0]["metrics"]["integrated_correctness"]["status"], "unknown")

    def test_missing_referenced_evidence_does_not_turn_zero_into_observed_coverage(self):
        report = {"human_interventions": {"count": 0, "evidence_refs": ["missing-log"]}}
        source = "from pathlib import Path; import os,sys; (Path(os.environ['LANGSPAN_BENCH_RUN_DIR'])/'assessment.json').write_text(sys.argv[1])"
        manifest = self.manifest([self.command("bad-evidence", source, json.dumps(report))], evidence_file="assessment.json")
        _, _, finals = self.run_benchmark(manifest)
        self.assertEqual(finals[0]["metrics"]["human_interventions"]["status"], "unknown")
        self.assertIsNone(finals[0]["metrics"]["human_interventions"]["count"])
        self.assertTrue(finals[0]["evidence_errors"])

    def test_bad_manifest_is_rejected_before_creating_results_or_running_anything(self):
        marker = self.root / "ran"
        command = self.command("agent", "from pathlib import Path; Path('ran').touch()")
        manifest = self.manifest([command])
        document = json.loads(manifest.read_text())
        document["items"][0]["arms"]["no_tool"]["agents"][0]["argv"] = "python3 -c unsafe-string"
        manifest.write_text(json.dumps(document))
        with self.assertRaisesRegex(ValueError, "argv"):
            runner.run_manifest(manifest, self.root / "output")
        self.assertFalse(marker.exists())
        self.assertFalse((self.root / "output").exists())

    def test_versions_and_setup_are_retained_and_unknown_checks_are_not_correctness(self):
        manifest = self.manifest(
            [self.command("agent", "from pathlib import Path; assert Path('prepared').exists()")],
            versions=[self.command("python-version", "import sys; print(sys.version)")],
            setup=[self.command("prepare", "from pathlib import Path; Path('prepared').touch()")],
            checks=[self.command("diagnostic", "print('diagnostics are not an oracle')")],
        )
        output, _, finals = self.run_benchmark(manifest)
        row = finals[0]
        self.assertEqual(row["versions_status"], "captured")
        self.assertEqual([c["phase"] for c in row["commands"]], ["version", "setup", "agent", "check"])
        self.assertEqual(row["metrics"]["integrated_correctness"]["status"], "unknown")
        self.assertEqual((output.stat().st_mode & 0o777), 0o700)
        self.assertEqual(((output / "runs.jsonl").stat().st_mode & 0o777), 0o600)


if __name__ == "__main__":
    unittest.main()
