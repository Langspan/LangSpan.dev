"""The benchmark driver, end to end in dry-run mode.

Real temporary Git repositories and clones, real child processes, and in
the LangSpan arm the real langspan binary with its SQLite store, Unix
socket and Git-remote sharing. No model runs: the agent is
standin_agent.py. Set LANGSPAN_BENCH_LANGSPAN to the binary (`cargo test`
does, through tests/benchmark_harness.rs); without it the LangSpan-arm
tests fail.
"""

import json
import os
from pathlib import Path
import re
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time
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


def task_line(prompt):
    """The line LangSpan shares as a session's task, without its marker."""
    lines = [line for line in prompt.splitlines() if line.startswith("Task: ")]
    assert len(lines) == 1, prompt
    return lines[0][len("Task: "):]


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

    def test_claude_command_streams_its_transcript_keeps_prompts_and_limits_tools(self):
        for arm in driver.ARMS:
            argv = driver.claude_argv("claude", "do the task", arm)
            self.assertEqual(argv[:3], ["claude", "-p", "do the task"])
            self.assertEqual(argv[argv.index("--permission-mode") + 1], "default")
            # The whole transcript, one JSON object per line: -p needs --verbose for it.
            self.assertEqual(argv[argv.index("--output-format") + 1], "stream-json")
            self.assertIn("--verbose", argv)
            self.assertFalse(set(argv) & set(driver.FORBIDDEN_CLAUDE_FLAGS))
            self.assertFalse(any("bypass" in a.lower() or "acceptedits" in a.lower() for a in argv))
            tools = argv[argv.index("--allowedTools") + 1:]
            self.assertIn("Edit(./**)", tools)
            self.assertNotIn("Bash", tools, "no unrestricted shell")
            self.assertEqual(any(t.startswith("Bash(langspan") for t in tools), arm == "langspan")

    def test_each_task_line_names_what_its_peers_need_within_what_langspan_shares(self):
        """LangSpan shares one `Task:` line of at most 200 bytes. The peer
        that needs to know of a task must recognize it from that line alone,
        and no other task's line may set it off."""
        for family in tasks.FAMILIES:
            for n in tasks.SESSION_COUNTS:
                assigned = tasks.assign(family, n, 3)
                lines = [task_line(driver.prompt_for("langspan", family, assigned, i)) for i in range(1, n + 1)]
                for index, task in enumerate(assigned):
                    self.assertLessEqual(len(lines[index].encode()), 200, lines[index])
                    if "trigger" not in task:
                        continue
                    peers = [line for other, line in enumerate(lines) if other != index]
                    hits = [line for line in peers if re.search(task["trigger"], line)]
                    self.assertEqual(len(hits), 1, (task["id"], hits))
                    if family == "rename_vs_new_caller":
                        self.assertIn("load_config", hits[0])
                    else:
                        self.assertIn(task["key"], hits[0])

    def test_only_the_awareness_arm_gives_peers_task_text_and_every_prompt_states_success(self):
        for family in tasks.FAMILIES:
            assigned = tasks.assign(family, 5, 3)
            for arm in driver.ARMS:
                prompt = driver.prompt_for(arm, family, assigned, 2)
                self.assertIn(assigned[1]["task"], prompt)
                self.assertIn(assigned[1]["details"], prompt)
                self.assertIn(tasks.SUCCESS[family], prompt)
                peers = [peer for i, peer in enumerate(assigned, 1) if i != 2]
                given = [peer["task"] in prompt and peer["details"] in prompt for peer in peers]
                self.assertEqual(all(given), arm == "awareness_only", arm)
                self.assertEqual(any(peer["task"] in prompt for peer in peers), arm == "awareness_only", arm)

    def test_prompts_outside_the_awareness_arm_carry_no_peer_outcome(self):
        # The shared text (preamble, success definition) is the same for every
        # arm, so it must not reveal what a peer changes: a session learns that
        # only through its arm's peer-information channel.
        for family in tasks.FAMILIES:
            for sessions in tasks.SESSION_COUNTS:
                assigned = tasks.assign(family, sessions, 3)
                for arm in ("worktrees", "langspan"):
                    for index, task in enumerate(assigned, 1):
                        prompt = driver.prompt_for(arm, family, assigned, index)
                        shared = prompt.replace(task["task"], "").replace(task["details"], "")
                        if "trigger" in task:
                            self.assertIsNone(re.search(task["trigger"], shared), (family, arm, task["id"]))
                        if family == "rename_vs_new_caller" and task["id"] != "rename":
                            self.assertNotIn(tasks.NEW_NAME, prompt, (arm, task["id"]))

    def test_plan_only_executes_nothing_and_names_ignored_options(self):
        output = self.root / "never"
        result = subprocess.run([sys.executable, "-B", str(HERE / "driver.py"), "--arm", "worktrees",
                                 "--family", "contradictory_defaults", "--sessions", "5", "--seed", "1",
                                 "--langspan", "/nonexistent/langspan", "--output", str(output)],
                                capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)
        plan = json.loads(result.stdout)
        self.assertEqual(plan["executed_commands"], 0)
        self.assertEqual(len(plan["sessions"]), 5)
        self.assertEqual(plan["ignored_options"], ["--langspan"])
        self.assertFalse(output.exists())
        self.assertEqual(driver.make_plan("langspan", "rename_vs_new_caller", 2, 1, agent="claude",
                                          standin_mode="ignore", langspan_given=True)["ignored_options"],
                         ["--standin-mode"])

    def run_claude_preflight(self, config, env_extra):
        output = self.root / "out"
        env = {k: v for k, v in os.environ.items() if k != driver.CLAUDE_CONFIG_ENV}
        env.update(env_extra)
        result = subprocess.run([sys.executable, "-B", str(HERE / "driver.py"), "--arm", "worktrees",
                                 "--family", "rename_vs_new_caller", "--sessions", "2", "--seed", "1",
                                 "--agent", "claude", "--run", "--output", str(output)],
                                capture_output=True, text=True, timeout=30, env=env)
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        record = json.loads((output / "record.json").read_text())
        driver.validate(record, driver.load_schema())
        self.assertEqual(record["status"], "refused")
        self.assertTrue((output / "plan.json").is_file())
        self.assertFalse((output / "work").exists(), "nothing was prepared")
        self.assertEqual((record["integrated_tests"], record["tasks_accepted"]), ("unknown", None))
        return result, record

    def test_a_claude_configuration_with_langspan_entries_is_refused_named_by_kind_and_recorded(self):
        config = self.root / "claude-config"
        config.mkdir()
        command = "'/opt/bin/langspan' hook claude-code --state-dir '/private/state'"
        (config / "settings.json").write_text(json.dumps({"hooks": {"PreToolUse": [{"matcher": "Write", "hooks": [
            {"type": "command", "command": command}]}]}}))
        (config / "skills" / "langspan").mkdir(parents=True)
        (config / "skills" / "langspan" / "SKILL.md").write_text("skill")
        (config / ".claude.json").write_text(json.dumps({"mcpServers": {"coordination": {
            "command": "/opt/bin/langspan", "args": ["mcp", "--state-dir", "/private/state"]}}}))
        result, record = self.run_claude_preflight(config, {driver.CLAUDE_CONFIG_ENV: str(config)})
        for words in ("hook in settings.json", "skill in skills/langspan", "MCP server in .claude.json"):
            self.assertIn(words, result.stderr)
            self.assertIn(words, record["error"])
        for private in ("/opt/bin", "--state-dir", "/private/state", "hook claude-code"):
            self.assertNotIn(private, result.stderr)
            self.assertNotIn(private, record["error"])
        # Each kind alone is refused; a configuration without them is not.
        (config / ".claude.json").write_text(json.dumps({"mcpServers": {"docs": {"command": "docs-server"}}}))
        (config / "settings.json").write_text("{}")
        self.assertEqual(driver.user_level_langspan(config), ["skill in skills/langspan"])
        shutil.rmtree(config / "skills")
        self.assertEqual(driver.user_level_langspan(config), [])
        (config / ".claude.json").write_text(json.dumps({"projects": {"/x": {"mcpServers": {"langspan": {
            "command": "anything"}}}}}))
        self.assertEqual(driver.user_level_langspan(config), ["MCP server in .claude.json"])

    def test_a_claude_run_without_the_configuration_variable_is_refused_and_recorded(self):
        result, record = self.run_claude_preflight(None, {})
        self.assertIn(driver.CLAUDE_CONFIG_ENV, result.stderr)
        self.assertIn(driver.CLAUDE_CONFIG_ENV, record["error"])

    def test_the_record_keeps_the_requested_model_apart_from_reported_ones(self):
        plan = driver.make_plan("worktrees", "rename_vs_new_caller", 2, 1, agent="claude", model="model-x")
        self.assertEqual(plan["model"], "model-x")
        plan.update(run_id="0" * 32, output=str(self.root), plan_path=str(self.root / "plan.json"))
        record = driver.build_record(plan, None, self.root / "runner", None, None, "t", 0.0, status="refused",
                                     error="refused for the test")
        driver.validate(record, driver.load_schema())
        self.assertEqual(record["versions"]["requested_model"], "model-x")
        self.assertEqual(record["versions"]["models"], [])

    def test_a_langspan_arm_claude_session_uses_its_own_coordinator(self):
        """The session's bare `langspan status` and `langspan brief` must
        reach the coordinator its hooks use, not a default state."""
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        langspan = bin_dir / "langspan"
        langspan.write_text("#!/bin/sh\nexit 0\n")
        langspan.chmod(0o755)
        seen = self.root / "seen.txt"
        claude = self.root / "fake-claude"
        claude.write_text('#!/bin/sh\nprintf "%%s\\n%%s\\n" "$LANGSPAN_STATE_DIR" "$(command -v langspan)" > %s\n'
                          % seen)
        claude.chmod(0o755)
        config = self.root / "claude-config"
        config.mkdir()
        states = [str(self.root / "lsb-1"), str(self.root / "lsb-2")]
        plan = driver.make_plan("langspan", "rename_vs_new_caller", 2, 1, agent="claude")
        plan.update(langspan=str(langspan), state_dirs=states)
        plan_path = self.root / "plan.json"
        plan_path.write_text(json.dumps(plan))
        env = dict(os.environ, **{driver.CLAUDE_CONFIG_ENV: str(config), driver.CLAUDE_BIN_ENV: str(claude),
                                  "LANGSPAN_STATE_DIR": str(self.root / "operator-state")})
        done = subprocess.run([sys.executable, "-I", "-B", str(HERE / "driver.py"), "session", str(plan_path), "2"],
                              capture_output=True, text=True, timeout=30, env=env)
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(seen.read_text().splitlines(), [states[1], str(langspan)])

    def test_clones_share_nothing_but_what_the_arm_allows(self):
        for arm in driver.ARMS:
            work = self.root / arm
            work.mkdir()
            _, _, clones = driver.prepare_fixture(work, 2, arm)
            (clones[1] / "shelf" / "report.py").write_text("# peer work\n")
            driver.git("commit", "-qam", "peer-secret-commit", cwd=clones[1])
            seen = driver.git("log", "--all", "--oneline", cwd=clones[0]).stdout
            self.assertNotIn("peer-secret-commit", seen, arm)
            refs = driver.git("for-each-ref", "--format=%(refname)", cwd=clones[0]).stdout.split()
            self.assertFalse([ref for ref in refs if "bench/s2" in ref], (arm, refs))
            common = [driver.git("rev-parse", "--path-format=absolute", "--git-common-dir", cwd=clone).stdout
                      for clone in clones]
            self.assertNotEqual(common[0], common[1])
            remotes = driver.git("remote", cwd=clones[0]).stdout.split()
            # Only the LangSpan arm keeps a remote: LangSpan's sharing goes
            # through it, and no session can push a branch there.
            self.assertEqual(remotes, ["origin"] if arm == "langspan" else [], arm)

    def test_duplicate_work_is_a_change_to_a_unit_another_task_owns(self):
        work = self.root / "work"
        work.mkdir()
        repo, base, clones = driver.prepare_fixture(work, 2, "worktrees")
        plan = {"sessions": 2, "work": str(work), "tasks": [dict(tasks.RENAME, role="rename"),
                                                            dict(tasks.feature_task(*tasks.FEATURES[0]), role="new_caller")]}
        # Session 2 adds an alias, which is not the rename: no duplicate.
        config = clones[1] / "shelf" / "config.py"
        config.write_text(config.read_text() + "\nload_config = parse_config\n")
        driver.git("commit", "-qam", "alias", cwd=clones[1])
        driver.collect_branches(plan, repo)
        self.assertEqual(driver.duplicates(plan, repo, base), [])
        # Session 2 now renames the function itself: that is session 1's task.
        config.write_text(config.read_text().replace("def parse_config", "def load_config"))
        driver.git("commit", "-qam", "rename too", cwd=clones[1])
        driver.collect_branches(plan, repo)
        self.assertEqual(len(driver.duplicates(plan, repo, base)), 2)  # parse_config gone, load_config added

    def oracle(self, plan, checkout):
        plan_path = self.root / "plan.json"
        plan_path.write_text(json.dumps(plan))
        out = self.root / "verdict.json"
        done = subprocess.run([sys.executable, "-I", "-B", str(HERE / "oracle.py"), str(plan_path),
                               str(checkout), str(out)], capture_output=True, text=True, timeout=120)
        return done.returncode, json.loads(out.read_text())

    def test_the_contradiction_oracle_accepts_exactly_the_reconciliation_the_tasks_define(self):
        key, old, a, b = tasks.CONTRADICTIONS[0]
        first, second = (dict(tasks.contradiction_task(key, old, side), role="contradiction") for side in (a, b))
        plan = {"family": "contradictory_defaults", "tasks": [first, second], "config_users": [],
                "integration": [{"session": 1, "merged": True}, {"session": 2, "merged": True}]}

        def tree(default, export_value, ui_value, note=None):
            checkout = self.root / "checkout"
            shutil.rmtree(checkout, ignore_errors=True)
            shutil.copytree(HERE / "fixture", checkout)
            settings = checkout / "shelf" / "settings.py"
            settings.write_text(settings.read_text().replace('"page_size": 20,', '"page_size": %d,' % default))
            for module, value in (("export", export_value), ("ui", ui_value)):
                if value is not None:
                    source = checkout / "shelf" / ("%s.py" % module)
                    source.write_text(source.read_text().replace('DEFAULTS["page_size"]', str(value)))
            if note:
                (checkout / "notes").mkdir()
                (checkout / "notes" / "s1.md").write_text(note)
            return self.oracle(plan, checkout)

        # One side changed the shared default: the other side silently lost.
        code, verdict = tree(100, None, None)
        self.assertEqual((code, verdict["integrated_tests"]), (1, "failed"))
        self.assertEqual([t["completed"] for t in verdict["tasks"]], [True, False])
        self.assertEqual([t["accepted"] for t in verdict["tasks"]], [False, False])
        self.assertEqual(len(verdict["semantic_conflicts"]), 1)
        # A note naming both values is kept for review; it is not the reconciliation.
        code, verdict = tree(100, None, None, note="page_size: export wants 100, listing wants 10.\n")
        self.assertEqual((code, verdict["integrated_tests"]), (1, "failed"))
        self.assertNotIn("contradictions_reported", verdict)
        # Both functions get their values but the shared default moved: not the reconciliation.
        code, verdict = tree(100, None, 10)
        self.assertEqual([t["completed"] for t in verdict["tasks"]], [True, True])
        self.assertEqual([t["accepted"] for t in verdict["tasks"]], [False, False])
        self.assertEqual(code, 1)
        # Each side sets its own value and the default stays: accepted.
        code, verdict = tree(20, 100, 10)
        self.assertEqual((code, verdict["integrated_tests"]), (0, "passed"), verdict)
        self.assertEqual([t["accepted"] for t in verdict["tasks"]], [True, True])

    def test_the_rename_oracle_needs_the_new_caller_on_the_new_name(self):
        feature = dict(tasks.feature_task(*tasks.FEATURES[0]), role="new_caller")
        plan = {"family": "rename_vs_new_caller", "tasks": [dict(tasks.RENAME, role="rename"), feature],
                "config_users": list(tasks.CONFIG_USERS),
                "integration": [{"session": 1, "merged": True}, {"session": 2, "merged": True}]}

        def tree(caller_uses, alias=False):
            checkout = self.root / "checkout"
            shutil.rmtree(checkout, ignore_errors=True)
            shutil.copytree(HERE / "fixture", checkout)
            for name in tasks.CONFIG_USERS:
                path = checkout / name
                path.write_text(path.read_text().replace("parse_config", "load_config"))
            if alias:
                config = checkout / "shelf" / "config.py"
                config.write_text(config.read_text() + "\n\nparse_config = load_config\n")
            op = feature["standin"]["follow" if caller_uses == "load_config" else "ignore"][0]
            (checkout / op["path"]).write_text(op["content"])
            return self.oracle(plan, checkout)

        code, verdict = tree("parse_config")
        self.assertEqual((code, verdict["integrated_tests"]), (1, "failed"))
        self.assertEqual([t["accepted"] for t in verdict["tasks"]], [True, False])
        code, verdict = tree("parse_config", alias=True)
        self.assertEqual([t["accepted"] for t in verdict["tasks"]], [False, False], "an alias is not the rename")
        code, verdict = tree("load_config")
        self.assertEqual((code, verdict["integrated_tests"]), (0, "passed"), verdict)
        self.assertEqual([t["accepted"] for t in verdict["tasks"]], [True, True])

    def test_permission_denials_are_split_into_outside_the_clone_and_not_allowed(self):
        root = self.root / "work" / "s1"
        root.mkdir(parents=True)
        peer = str(self.root / "work" / "s2" / "shelf" / "config.py")
        denials = [
            {"tool_name": "Read", "tool_input": {"file_path": peer}},
            {"tool_name": "Grep", "tool_input": {"pattern": "x", "path": "../s2"}},
            {"tool_name": "Bash", "tool_input": {"command": "cat ../s2/shelf/ui.py"}},
            {"tool_name": "Bash", "tool_input": {"command": "git -C %s log" % (self.root / "work" / "s2")}},
            {"tool_name": "Bash", "tool_input": {"command": "sed -i s/a/b/ shelf/ui.py"}},
            {"tool_name": "Bash", "tool_input": {"command": "langspan status 2>&1 | head"}},
            {"tool_name": "Read", "tool_input": {"file_path": str(root / "shelf" / "ui.py")}},
            {"tool_name": "Write", "tool_input": {}},
            "not an object",
        ]
        kinds = [driver.classify_denial(denial, root) for denial in denials]
        self.assertEqual(kinds, ["outside_clone"] * 4 + ["not_allowed"] * 5)
        self.assertEqual(driver.denial_counts(denials, root), {"total": 9, "outside_clone": 4, "not_allowed": 5})
        self.assertIsNone(driver.denial_counts(None, root))

    def test_a_session_transcript_gives_its_result_cost_duration_and_turns(self):
        lines = [
            {"type": "system", "subtype": "init", "model": "model-x"},
            {"type": "assistant", "message": {"content": [{"type": "text", "text": "Done?"}]}},
            {"type": "result", "subtype": "success", "is_error": False, "result": "Done.", "num_turns": 7,
             "duration_ms": 41000, "total_cost_usd": 0.25, "permission_denials": [],
             "usage": {"input_tokens": 3, "output_tokens": 4}},
        ]
        text = "\n".join(json.dumps(line) for line in lines) + "\n"
        result = driver.session_result(text)
        self.assertEqual(result["result"], "Done.")
        self.assertEqual(driver.session_figures(result), {"cost_usd": 0.25, "duration_ms": 41000, "turns": 7})
        self.assertEqual(driver.session_figures({"num_turns": -1, "total_cost_usd": "x"}),
                         {"cost_usd": None, "duration_ms": None, "turns": None})
        # A transcript cut off before its result has none.
        self.assertIsNone(driver.session_result("\n".join(json.dumps(line) for line in lines[:2])))

    def test_coordinator_scan_failures_and_recoveries_reach_the_record(self):
        events = {1: [{"seq": 3, "event": {"type": "scan_failed", "failure": {
                      "workspace_id": "bench-s1", "detail": "Git repository metadata changed during observation",
                      "at_ms": 1, "generation": 1}}},
                      {"seq": 4, "event": {"type": "scan_recovered", "workspace_id": "bench-s1"}},
                      {"seq": 5, "event": {"type": "lease_granted"}}],
                  2: []}
        scans = driver.scan_summary(events, "langspan")
        self.assertEqual((scans["status"], scans["failed"], scans["recovered"]), ("reported", 1, 1))
        self.assertEqual(scans["items"], [
            {"session": 1, "seq": 3, "type": "scan_failed",
             "detail": "Git repository metadata changed during observation"},
            {"session": 1, "seq": 4, "type": "scan_recovered", "detail": None}])
        self.assertEqual(driver.scan_summary(None, "worktrees")["status"], "not_applicable")
        self.assertEqual(driver.scan_summary(None, "langspan")["status"], "unknown")

    def test_the_hook_log_passes_a_refusal_through_even_when_it_cannot_log(self):
        command = "cat >/dev/null; printf '{\"decision\":\"deny\"}'; echo why >&2; exit 2"
        payload = json.dumps({"hook_event_name": "PreToolUse", "tool_use_id": "call-1", "session_id": "s"})
        for log in (self.root / "hooks.jsonl", self.root / "missing" / "hooks.jsonl"):
            done = subprocess.run([sys.executable, "-I", "-B", str(HERE / "driver.py"), "hook-log", str(log), command],
                                  input=payload, capture_output=True, text=True, timeout=30)
            self.assertEqual((done.returncode, done.stdout), (2, '{"decision":"deny"}'), log)
            self.assertIn("why", done.stderr)
        entry = json.loads((self.root / "hooks.jsonl").read_text())
        self.assertEqual((entry["event"], entry["tool_use_id"], entry["exit_code"]), ("PreToolUse", "call-1", 2))
        self.assertEqual(entry["stdout"], '{"decision":"deny"}')
        self.assertEqual(os.stat(self.root / "hooks.jsonl").st_mode & 0o777, 0o600)

    def test_notes_and_merge_logs_survive_a_failed_merge(self):
        work = self.root / "work"
        work.mkdir()
        run_dir = self.root / "run"
        run_dir.mkdir()
        repo, base, clones = driver.prepare_fixture(work, 2, "worktrees")
        key, old, a, b = tasks.CONTRADICTIONS[0]
        assigned = [dict(tasks.contradiction_task(key, old, side), role="contradiction") for side in (a, b)]
        for index, (clone, value) in enumerate(zip(clones, (100, 10)), 1):
            settings = clone / "shelf" / "settings.py"
            settings.write_text(settings.read_text().replace('"page_size": 20,', '"page_size": %d,' % value))
            (clone / "notes").mkdir()
            (clone / "notes" / ("s%d.md" % index)).write_text("session %d wanted page_size %d\n" % (index, value))
        plan = {"family": "contradictory_defaults", "sessions": 2, "tasks": assigned, "config_users": [],
                "work": str(work), "repo": str(repo), "base": base}
        plan_path = self.root / "plan.json"
        plan_path.write_text(json.dumps(plan))
        done = subprocess.run([sys.executable, "-I", "-B", str(HERE / "driver.py"), "integrate", str(plan_path)],
                              capture_output=True, text=True, timeout=300,
                              env=dict(os.environ, LANGSPAN_BENCH_RUN_DIR=str(run_dir)))
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
        integrated = json.loads((self.root / "integration.json").read_text())
        self.assertEqual([e["merged"] for e in integrated["integration"]], [True, False])
        # Session 2's branch never merged, and its note is still kept.
        self.assertEqual((run_dir / "notes" / "s2" / "s2.md").read_text(), "session 2 wanted page_size 10\n")
        self.assertEqual((run_dir / "notes" / "s1" / "s1.md").read_text(), "session 1 wanted page_size 100\n")
        log = (run_dir / "merge-s2.log").read_text()
        self.assertIn("CONFLICT", log)
        self.assertIn("<<<<<<<", log, "the conflicting hunks before the merge was aborted")
        self.assertIn("Merge made", (run_dir / "merge-s1.log").read_text())
        self.assertEqual(integrated["notes"], ["notes/s1/s1.md", "notes/s2/s2.md"])


class StandinHooks(Temporary):
    """How the stand-in runs a hook: to completion within its own bound,
    whatever the host timeout, and never leaving one running."""

    def session(self, command, timeout):
        settings = {"hooks": {"PreToolUse": [{"matcher": "*", "hooks": [
            {"type": "command", "command": command, "timeout": timeout}]}]}}
        log = open(self.root / "standin.jsonl", "w", encoding="utf-8")
        self.addCleanup(log.close)
        return standin_agent.Session(settings, log, str(self.root)), log

    def test_a_hook_slower_than_its_host_timeout_still_decides_the_call(self):
        # On a loaded machine a hook can start late. Its decision is what the
        # dry run measures, so the edit waits for it, and the log says it was late.
        deny = json.dumps({"hookSpecificOutput": {"permissionDecision": "deny"}})
        session, log = self.session("sleep 2; printf '%%s' %s" % shlex.quote(deny), 1)
        denied = session.fire("PreToolUse", "Write", {"file_path": "x"}, "edit-1")
        log.flush()
        entry = json.loads((self.root / "standin.jsonl").read_text().splitlines()[0])
        self.assertTrue(denied, entry)
        self.assertEqual(entry["decision"], "deny", entry)
        self.assertTrue(entry["late"], entry)
        self.assertGreaterEqual(entry["elapsed_ms"], 2000, entry)
        self.assertEqual(session.late_hooks, 1)

    def test_a_hook_past_the_standin_bound_leaves_no_process_running(self):
        # A hook the stand-in gives up on must not go on acting, for example
        # by reading a file after the edit it was asked about was applied.
        # The child's heartbeat shows whether it still acts. Its PID alone
        # does not: where PID 1 does not reap orphans, a killed child stays
        # a zombie and `kill(pid, 0)` still succeeds.
        heartbeat, group = self.root / "heartbeat", self.root / "group"
        command = "echo $$ > %s; while :; do printf x >> %s; sleep 0.05; done & wait" % (
            shlex.quote(str(group)), shlex.quote(str(heartbeat)))
        session, _ = self.session(command, 1)
        original = standin_agent.HOOK_SECONDS
        standin_agent.HOOK_SECONDS = 2
        try:
            denied = session.fire("PreToolUse", "Write", {"file_path": "x"}, "edit-1")
            self.assertFalse(denied)
            self.assertGreater(heartbeat.stat().st_size, 0, "the hook's child never ran")
            self.assert_stopped(heartbeat)
        finally:
            standin_agent.HOOK_SECONDS = original
            self.kill_groups(group)

    def kill_groups(self, group_file, *groups):
        """Kill GROUPS and the hook's group named in GROUP_FILE, so a failed
        test leaves nothing running."""
        if group_file.exists() and group_file.read_text().strip():
            groups += (int(group_file.read_text()),)
        for group in groups:
            try:
                os.killpg(group, signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass

    def assert_stopped(self, heartbeat):
        """HEARTBEAT stops growing within a bound, and stays the same for a
        second after that, many times the child's 0.05 s beat."""
        deadline = time.monotonic() + 10
        size = heartbeat.stat().st_size if heartbeat.exists() else 0
        while True:
            time.sleep(1)
            now = heartbeat.stat().st_size if heartbeat.exists() else 0
            if now == size:
                return
            self.assertLess(time.monotonic(), deadline, "the hook's child is still writing %s" % heartbeat)
            size = now

    def test_terminating_the_standin_stops_the_hook_it_is_running(self):
        # The runner ends an attempt by signaling the stand-in's process
        # group. Each hook runs in a group of its own, so the stand-in must
        # end the running hook's group before it exits.
        heartbeat = self.root / "heartbeat"
        started = self.root / "started"
        command = "echo $$ > %s; while :; do printf x >> %s; sleep 0.05; done" % (
            shlex.quote(str(started)), shlex.quote(str(heartbeat)))
        settings = {"hooks": {"SessionStart": [{"hooks": [{"type": "command", "command": command}]}]}}
        (self.root / ".claude").mkdir()
        (self.root / ".claude" / "settings.local.json").write_text(json.dumps(settings))
        plan = {"sessions": 1, "prompts": ["p"],
                "tasks": [{"task": "t", "details": "d", "standin": {"ignore": [], "follow": []}}]}
        (self.root / "plan.json").write_text(json.dumps(plan))
        env = dict(os.environ, LANGSPAN_BENCH_RUN_DIR=str(self.root))
        standin = subprocess.Popen([sys.executable, str(HERE / "standin_agent.py"), "plan.json", "1", "ignore"],
                                   cwd=self.root, env=env, stdout=subprocess.DEVNULL,
                                   stderr=subprocess.DEVNULL, start_new_session=True)
        try:
            deadline = time.monotonic() + 10
            while not (started.exists() and started.read_text().strip() and heartbeat.exists()):
                self.assertLess(time.monotonic(), deadline, "the hook never started")
                time.sleep(0.05)
            os.killpg(standin.pid, signal.SIGTERM)
            standin.wait(timeout=10)
            self.assert_stopped(heartbeat)
        finally:
            self.kill_groups(started, standin.pid)
            standin.wait()


class RefusalCollection(Temporary):
    def test_a_real_refusal_is_collected_with_what_followed(self):
        """Two stand-in sessions in one clone: B edits a file after A read
        it, so LangSpan refuses A's edit as stale; A reads again and is
        admitted. The record's refusal comes from the real event log."""
        work = self.root / "work"
        work.mkdir()
        _, _, clones = driver.prepare_fixture(work, 1, "langspan")
        state = Path(tempfile.mkdtemp(prefix="lsb-", dir="/tmp"))
        self.addCleanup(shutil.rmtree, state, True)
        driver.install_hooks(langspan_binary(), state, 1, clones[0], self.root / "hook-log.jsonl")
        coordinator = driver.Coordinator(langspan_binary(), state, self.root / "coordinator.log")
        try:
            settings = json.loads((clones[0] / ".claude" / "settings.local.json").read_text())
            target = (clones[0] / "shelf" / "report.py").resolve()
            with open(self.root / "hooks.jsonl", "w") as log:
                a, b = (standin_agent.Session(settings, log, str(clones[0].resolve())) for _ in range(2))
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
        # The pass-through hook log kept what LangSpan said, and when.
        logged = [json.loads(line) for line in (self.root / "hook-log.jsonl").read_text().splitlines()]
        self.assertIn("PreToolUse", {entry["event"] for entry in logged})
        refused = [entry for entry in logged if '"deny"' in entry["stdout"]]
        self.assertEqual(len(refused), 1, logged)
        self.assertTrue(refused[0]["tool_use_id"].startswith("edit-"))
        self.assertTrue(all(entry["at"] and entry["elapsed_ms"] >= 0 for entry in logged))

    def test_a_pre_tool_hook_that_starts_late_refuses_nothing(self):
        """Issue #200: on a loaded runner a PreToolUse hook can start after
        its 5-second host timeout. The edit waits for it, so the hook reads
        the version the session read and LangSpan admits the edit. Before,
        the stand-in applied the edit at the timeout and the hook, still
        running, read the edited file and was refused as stale."""
        work = self.root / "work"
        work.mkdir()
        _, _, clones = driver.prepare_fixture(work, 1, "langspan")
        state = Path(tempfile.mkdtemp(prefix="lsb-", dir="/tmp"))
        self.addCleanup(shutil.rmtree, state, True)
        driver.install_hooks(langspan_binary(), state, 1, clones[0], self.root / "hook-log.jsonl")
        coordinator = driver.Coordinator(langspan_binary(), state, self.root / "coordinator.log")
        try:
            settings = json.loads((clones[0] / ".claude" / "settings.local.json").read_text())
            # LangSpan's hooks start late, behind the hook-log wrapper, as
            # a slow start of the hook process on a loaded runner makes
            # them: PreToolUse past its 5 s timeout, PostToolUse within it.
            for event, delay in (("PreToolUse", 6), ("PostToolUse", 4)):
                for entry in settings["hooks"][event]:
                    for hook in entry["hooks"]:
                        self.assertEqual(hook["timeout"], 5, hook)
                        *wrapper, inner = shlex.split(hook["command"])
                        hook["command"] = " ".join(shlex.quote(part)
                                                   for part in wrapper + ["sleep %d; %s" % (delay, inner)])
            target = (clones[0] / "shelf" / "report.py").resolve()
            with open(self.root / "hooks.jsonl", "w") as log:
                session = standin_agent.Session(settings, log, str(clones[0].resolve()))
                session.fire("SessionStart", source="startup")
                session.read(target)
                edited = session.edit(target, "Edit", {"file_path": str(target), "old_string": "nobody",
                                                       "new_string": "no owner", "replace_all": False},
                                      lambda: target.write_text(target.read_text().replace("nobody", "no owner")))
            # Read the log once LangSpan has decided the call, however late.
            decided = ("lease_granted", "write_refused")
            deadline = time.monotonic() + 60
            while True:
                events = coordinator.events()
                if any(record.get("event", {}).get("type") in decided for record in events):
                    break
                self.assertLess(time.monotonic(), deadline, "LangSpan never decided the edit")
                time.sleep(0.1)
        finally:
            coordinator.stop()
        seen = "stand-in: %s\nevents: %s" % ((self.root / "hooks.jsonl").read_text(),
                                             [record.get("event", {}).get("type") for record in events])
        self.assertTrue(edited, seen)
        self.assertEqual(session.refusals, [], seen)
        self.assertEqual(driver.refusal_items(events), [], seen)
        self.assertIn("lease_granted", [record.get("event", {}).get("type") for record in events], seen)
        self.assertGreaterEqual(session.late_hooks, 1, seen)
        self.assertIn("no owner", target.read_text())


class DryRun(Temporary):
    """The whole pipeline for each arm, family and stand-in mode."""

    def run_driver(self, arm, family, n, mode, seed=11):
        output = self.root / ("%s-%s-%d-%s" % (arm, family, n, mode))
        argv = [sys.executable, "-B", str(HERE / "driver.py"), "--arm", arm, "--family", family,
                "--sessions", str(n), "--seed", str(seed), "--standin-mode", mode, "--time-limit", "180",
                "--run", "--output", str(output)]
        if arm == "langspan":
            argv += ["--langspan", str(langspan_binary())]
        done = subprocess.run(argv, capture_output=True, text=True, timeout=900)
        record_path = output / "record.json"
        self.assertTrue(record_path.is_file(), done.stdout + done.stderr)
        record = json.loads(record_path.read_text())
        driver.validate(record, driver.load_schema())
        self.assertNotIn("error", record, record.get("error"))
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertEqual((output.stat().st_mode & 0o777), 0o700)
        self.check_common(arm, family, n, mode, output, record)
        return record

    def check_common(self, arm, family, n, mode, output, record):
        self.assertEqual((record["arm"], record["family"], record["sessions"]), (arm, family, n))
        self.assertTrue(record["dry_run"])
        self.assertEqual(record["standin_mode"], mode)
        self.assertEqual(record["status"], "succeeded")
        self.assertEqual(record["tasks_total"], n)
        self.assertEqual(record["stops"], 0)
        self.assertEqual(record["questions"], 0)
        self.assertEqual(record["permission_denials"], {"total": 0, "outside_clone": 0, "not_allowed": 0})
        self.assertEqual(record["tokens"], {"status": "unknown", "total": None})
        self.assertIsNone(record["cost_usd"])
        self.assertEqual(record["ignored_options"], [])
        self.assertIsNone(record["versions"]["requested_model"])
        self.assertEqual(record["versions"]["agent_cli"], "standin-agent")
        self.assertTrue(record["versions"]["git"].startswith("git version"))
        rows = [json.loads(line) for line in (output / "runner" / "runs.jsonl").read_text().splitlines()]
        self.assertEqual([r["record_type"] for r in rows], ["run_started", "run_finished"])
        self.assertEqual([c["phase"] for c in rows[1]["commands"]], ["version"] * 2 + ["agent"] * n + ["check"])
        self.assertEqual(rows[1]["metrics"]["integrated_correctness"]["status"], record["integrated_tests"])
        prompts = json.loads((output / "plan.json").read_text())["prompts"]
        work = output / "work"
        for index, task in enumerate(record["tasks"], 1):
            self.assertEqual((task["turns"], task["cost_usd"]), (1, None))
            transcript = output / task["transcript"]
            self.assertEqual(driver.session_result(transcript.read_text())["type"], "result")
            clone = work / ("s%d" % index)
            settings = clone / ".claude" / "settings.local.json"
            skill = clone / ".claude" / "skills" / "langspan" / "SKILL.md"
            self.assertEqual(settings.is_file(), arm == "langspan")
            self.assertEqual(skill.is_file(), arm == "langspan")
            self.assertEqual("Other sessions are working" in prompts[index - 1], arm == "awareness_only")
            # Nothing of LangSpan's reaches a branch.
            tracked = driver.git("ls-tree", "-r", "--name-only", driver.branch(index), cwd=work / "repo").stdout
            self.assertNotIn(".claude", tracked)
            self.assertEqual(task["hook_log"] is not None, arm == "langspan")
        if arm == "langspan":
            refusals = record["refusals"]
            self.assertEqual(refusals["status"], "reported")
            self.assertEqual(refusals["count"], 0, self.refusal_report(output, record))
            self.assertEqual(refusals["sessions_registered"], n)
            self.assertGreaterEqual(refusals["admissions"], n)
            self.assertEqual(record["coordinator_scans"]["status"], "reported")
            self.assertEqual(len(record["evidence"]["events"]), n)
            self.assertTrue(all((output / path).is_file() for path in record["evidence"]["events"]))
            self.assertEqual([t["refusals"] for t in record["tasks"]], [0] * n)
        else:
            self.assertEqual(record["refusals"]["status"], "not_applicable")
            self.assertIsNone(record["refusals"]["count"])
            self.assertEqual(record["coordinator_scans"]["status"], "not_applicable")
            self.assertEqual(record["evidence"]["events"], [])

    def refusal_report(self, output, record):
        """What a failed refusal check saw: each refusal, what LangSpan told
        the refused session, and that session's hooks slower than the host
        timeout."""
        lines = ["refusals: %s" % json.dumps(record["refusals"]["items"])]
        for index, task in enumerate(record["tasks"], 1):
            if not task["refusals"]:
                continue
            lines.append("session %d (%s, %s):" % (index, task["role"], task["task_id"]))
            log = output / task["hook_log"] if task["hook_log"] else None
            for line in log.read_text().splitlines() if log and log.is_file() else []:
                entry = json.loads(line)
                if '"deny"' in entry.get("stdout", ""):
                    lines.append("  %s at %s: %s" % (entry.get("event"), entry.get("at"), entry["stdout"][:1500]))
                elif entry.get("elapsed_ms", 0) > 5000:
                    lines.append("  %s %s at %s took %s ms" % (entry.get("event"), entry.get("tool"),
                                                              entry.get("at"), entry["elapsed_ms"]))
        return "\n".join(lines)

    def informed(self, output_record):
        return [t["standin_informed"] for t in output_record["tasks"]]

    def check_uninformed(self, record, family, n):
        """What sessions that know nothing of their peers produce."""
        self.assertEqual(record["integrated_tests"], "failed")
        self.assertEqual(record["duplicate_work"], 0)
        if family == "rename_vs_new_caller":
            # Every new caller uses the old name, which the rename removed.
            self.assertEqual(record["merge_conflicts"], [])
            self.assertEqual(record["semantic_conflicts"], n - 1, record["semantic_conflict_details"])
            self.assertEqual(record["tasks_accepted"], 1)
            self.assertEqual([t["accepted"] for t in record["tasks"] if t["role"] == "rename"], [True])
        else:
            # Both sides of each pair change the same default: Git refuses the second.
            self.assertEqual(len(record["merge_conflicts"]), n // 2)
            self.assertTrue(all(c["files"] == ["shelf/settings.py"] for c in record["merge_conflicts"]))
            self.assertEqual(record["semantic_conflicts"], 0)
            # A pair whose first side merged is still not accepted: only the control task is.
            self.assertEqual(record["tasks_accepted"], n % 2)
            self.assertEqual(sum(bool(t["completed"]) for t in record["tasks"]), n // 2 + n % 2)

    def check_arm(self, arm, sizes, modes=("ignore", "follow")):
        informs = arm != "worktrees"
        for family in tasks.FAMILIES:
            for n in sizes:
                for mode in modes:
                    with self.subTest(family=family, n=n, mode=mode):
                        record = self.run_driver(arm, family, n, mode)
                        triggered = [t["role"] in ("new_caller", "contradiction") for t in record["tasks"]]
                        if mode == "follow" and informs:
                            # The information its arm gave let every session reconcile.
                            self.assertEqual(self.informed(record), triggered)
                            self.assertEqual(record["integrated_tests"], "passed", record)
                            self.assertEqual((record["semantic_conflicts"], record["merge_conflicts"]), (0, []))
                            self.assertEqual(record["tasks_accepted"], n)
                            self.assertEqual(record["fixture_tests"], "passed")
                        else:
                            self.assertEqual(self.informed(record), [False] * n)
                            self.check_uninformed(record, family, n)

    def test_dry_run_arm_worktrees(self):
        self.check_arm("worktrees", (2, 5))

    def test_dry_run_arm_awareness_only(self):
        self.check_arm("awareness_only", (2, 5))

    def test_dry_run_arm_langspan_ignore(self):
        # Ten sessions: ten coordinators sharing through one remote.
        self.check_arm("langspan", (2, 5, 10), modes=("ignore",))

    def test_dry_run_arm_langspan_follow(self):
        # Not N = 10: a LangSpan briefing lists at most eight sessions from
        # other clones, so a session with nine peers can miss the one it
        # needs (README, "What each arm provides").
        self.check_arm("langspan", (2, 5), modes=("follow",))

    def test_a_dry_run_records_an_option_its_arm_ignores(self):
        output = self.root / "ignored"
        done = subprocess.run([sys.executable, "-B", str(HERE / "driver.py"), "--arm", "worktrees",
                               "--family", "rename_vs_new_caller", "--sessions", "2", "--seed", "1",
                               "--langspan", str(langspan_binary()), "--run", "--output", str(output)],
                              capture_output=True, text=True, timeout=300)
        record = json.loads((output / "record.json").read_text())
        driver.validate(record, driver.load_schema())
        self.assertEqual(record["ignored_options"], ["--langspan"], done.stderr)
        self.assertIsNone(record["versions"]["langspan_binary_sha256"])
        self.assertEqual(json.loads((output / "plan.json").read_text())["ignored_options"], ["--langspan"])


if __name__ == "__main__":
    unittest.main()
