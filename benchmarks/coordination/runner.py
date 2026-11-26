#!/usr/bin/env python3
"""Opt-in runner. Commands execute literally; no agent, tool, or installer is inferred."""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import re
import selectors
import signal
import subprocess
import sys
import time
import uuid

# The three K1 conditions come first. `no_tool` and `coarse_claims` are the
# v0 arm names, still accepted so earlier manifests keep validating; they are
# no longer part of the required comparison (see README.md, "Arms").
ARMS = ("worktrees", "awareness_only", "langspan", "no_tool", "coarse_claims", "comparable_tool")
REQUIRED_ARMS = set(ARMS[:3])
COVERAGE_STATES = (
    "admitted", "denied", "unavailable", "timeout", "unsupported",
    "late_observation", "unattributed", "unknown",
)
PHASES = (("versions", "version"), ("setup", "setup"), ("agents", "agent"), ("checks", "check"))
MAX_MANIFEST_BYTES = 1024 * 1024
MAX_EVIDENCE_BYTES = 256 * 1024
TERM_GRACE_SECONDS = .2
DRAIN_GRACE_SECONDS = .5


def utc_now():
    return datetime.now(timezone.utc).isoformat()


def digest(value):
    return hashlib.sha256(value).hexdigest()


def bounded_json(path, limit):
    with open(path, "rb") as source:
        body = source.read(limit + 1)
    if len(body) > limit:
        raise ValueError(str(path) + " exceeds its input bound")
    return json.loads(body), digest(body)


def object_fields(value, allowed, required, label):
    if not isinstance(value, dict) or not required <= value.keys() or value.keys() - allowed:
        raise ValueError(label + " has missing or unrecognized fields")


def identifier(value, label):
    if not isinstance(value, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,79}", value):
        raise ValueError(label + " must be a short ASCII identifier")


def relative_path(value, label):
    if not isinstance(value, str) or not value or "\0" in value:
        raise ValueError(label + " must be a nonempty relative path")
    path = Path(value)
    if path.is_absolute() or ".." in path.parts or value in (".", ""):
        raise ValueError(label + " must stay within the run directory")
    return path


def positive_number(value, maximum, label):
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value) or not 0 < value <= maximum:
        raise ValueError(label + " is outside its positive bound")


def load_manifest(path):
    path = Path(path).resolve()
    manifest, manifest_hash = bounded_json(path, MAX_MANIFEST_BYTES)
    object_fields(manifest, {"schema_version", "benchmark_id", "items"}, {"schema_version", "benchmark_id", "items"}, "manifest")
    if type(manifest["schema_version"]) is not int or manifest["schema_version"] != 1:
        raise ValueError("manifest schema_version must be 1")
    identifier(manifest["benchmark_id"], "benchmark_id")
    items = manifest["items"]
    if not isinstance(items, list) or not 1 <= len(items) <= 128:
        raise ValueError("manifest requires 1..128 items")
    item_ids = set()
    for item in items:
        object_fields(item, {"id", "description", "arms"}, {"id", "description", "arms"}, "item")
        identifier(item["id"], "item.id")
        if item["id"] in item_ids:
            raise ValueError("item IDs must be unique")
        item_ids.add(item["id"])
        if not isinstance(item["description"], str) or len(item["description"]) > 4096:
            raise ValueError("item.description must be a bounded string")
        arms = item["arms"]
        if not isinstance(arms, dict) or not arms or arms.keys() - set(ARMS):
            raise ValueError("arms must select " + ", ".join(ARMS))
        for arm_name, arm in arms.items():
            object_fields(arm, {"agents", "versions", "setup", "checks", "evidence_file"}, {"agents"}, arm_name)
            if "evidence_file" in arm:
                relative_path(arm["evidence_file"], "evidence_file")
            for field, phase in PHASES:
                commands = arm.setdefault(field, [])
                if not isinstance(commands, list) or len(commands) > 16 or field == "agents" and not commands:
                    raise ValueError(field + " must be a bounded command list; agents cannot be empty")
                names = set()
                for command in commands:
                    allowed = {"id", "argv", "cwd", "timeout_seconds", "max_output_bytes"}
                    if phase == "check":
                        allowed.add("assesses")
                    object_fields(command, allowed, {"id", "argv", "cwd"}, phase)
                    identifier(command["id"], "command.id")
                    if command["id"] in names:
                        raise ValueError("command IDs must be unique within each phase")
                    names.add(command["id"])
                    argv = command["argv"]
                    if not isinstance(argv, list) or not 1 <= len(argv) <= 256 or not all(isinstance(a, str) and "\0" not in a for a in argv) or not argv[0]:
                        raise ValueError("argv must be a nonempty array of literal strings without NUL")
                    if sum(len(a.encode("utf-8")) for a in argv) > 65536:
                        raise ValueError("argv exceeds 64 KiB")
                    if not isinstance(command["cwd"], str) or not command["cwd"] or len(command["cwd"]) > 4096:
                        raise ValueError("cwd must be an explicit bounded directory path")
                    cwd = (path.parent / command["cwd"]).resolve()
                    if not cwd.is_dir():
                        raise ValueError("cwd does not exist: " + str(cwd))
                    command["cwd"] = str(cwd)
                    command.setdefault("timeout_seconds", 600)
                    positive_number(command["timeout_seconds"], 3600, "timeout_seconds")
                    command.setdefault("max_output_bytes", 4 * 1024 * 1024)
                    if type(command["max_output_bytes"]) is not int or not 1 <= command["max_output_bytes"] <= 64 * 1024 * 1024:
                        raise ValueError("max_output_bytes must be 1..67108864 per output stream")
                    if command.get("assesses", "diagnostic") not in ("diagnostic", "integrated_correctness"):
                        raise ValueError("a check can assess diagnostic or integrated_correctness")
    return manifest, manifest_hash


def unknown_metrics():
    return {
        "coverage": {"status": "unknown", "observed_tool_calls": None,
                     "counts": {name: None for name in COVERAGE_STATES}, "evidence_refs": []},
        "human_interventions": {"status": "unknown", "count": None, "evidence_refs": []},
        "unnecessary_denials": {"status": "unknown", "count": None, "evidence_refs": []},
        "missed_stale_writes": {"status": "unknown", "count": None, "evidence_refs": []},
        "integrated_correctness": {"status": "unknown", "source": None, "evidence_refs": [], "check_ids": []},
        "awareness": {"status": "unknown", "received_before_decision": None,
                      "changed_approach": None, "human_relay": None,
                      "delay_ms": None, "evidence_refs": []},
    }


def private_file(path):
    return os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "wb")


def signal_group(process, signum):
    """Signal only the new process group created for this child."""
    try:
        os.killpg(process.pid, signum)
        return True
    except (ProcessLookupError, PermissionError):
        return False


def run_group(commands, phase, run_dir, output_dir, context):
    selector = selectors.DefaultSelector()
    handles = []
    interrupted = False
    try:
        for spec in commands:
            base = phase + "-" + spec["id"]
            logs = {stream: private_file(run_dir / (base + "." + stream)) for stream in ("stdout", "stderr")}
            started = time.monotonic_ns()
            record = {
                "id": spec["id"], "phase": phase, "assesses": spec.get("assesses", "diagnostic"),
                "executable": Path(spec["argv"][0]).name,
                "argv_sha256": digest(json.dumps(spec["argv"], ensure_ascii=False, separators=(",", ":")).encode()),
                "cwd": spec["cwd"], "timeout_seconds": spec["timeout_seconds"],
                "max_output_bytes_per_stream": spec["max_output_bytes"],
                "started_at": utc_now(), "elapsed_ns": 0, "exit_code": None, "signal": None,
                "status": "running", "error": None,
                "stdout_path": str((run_dir / (base + ".stdout")).relative_to(output_dir)),
                "stderr_path": str((run_dir / (base + ".stderr")).relative_to(output_dir)),
                "stdout_bytes": 0, "stderr_bytes": 0, "stdout_truncated": False, "stderr_truncated": False,
                "cleanup": {"sigterm_sent": False, "sigkill_sent": False,
                            "pipe_drain_incomplete": False, "process_still_running": False},
            }
            env = os.environ.copy()
            env.update({
                "LANGSPAN_BENCH_RUN_DIR": str(run_dir),
                "LANGSPAN_BENCH_RUN_ID": context["run_id"],
                "LANGSPAN_BENCH_ITEM_ID": context["item_id"],
                "LANGSPAN_BENCH_ARM": context["arm"],
                "LANGSPAN_BENCH_REPETITION": str(context["repetition"]),
            })
            handle = {"spec": spec, "record": record, "logs": logs, "started": started,
                      "process": None, "pipes": set(), "stop_at": None, "reason": None, "done": False}
            handles.append(handle)
            try:
                process = subprocess.Popen(
                    spec["argv"], cwd=spec["cwd"], env=env, stdin=subprocess.DEVNULL,
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, shell=False, start_new_session=True,
                )
                handle["process"] = process
                for stream in ("stdout", "stderr"):
                    pipe = getattr(process, stream)
                    os.set_blocking(pipe.fileno(), False)
                    selector.register(pipe, selectors.EVENT_READ, (handle, stream))
                    handle["pipes"].add(pipe)
            except OSError as error:
                record["status"] = "spawn_error"
                record["error"] = str(error)
                record["elapsed_ns"] = time.monotonic_ns() - started
                handle["done"] = True
                for log in logs.values():
                    log.close()

        def stop(handle, reason=None):
            if reason is not None and handle["reason"] is None:
                handle["reason"] = reason
            if handle["stop_at"] is None:
                handle["stop_at"] = time.monotonic()
                handle["record"]["cleanup"]["sigterm_sent"] = signal_group(handle["process"], signal.SIGTERM)

        def close_pipe(handle, pipe):
            selector.unregister(pipe)
            handle["pipes"].remove(pipe)
            pipe.close()

        while any(not handle["done"] for handle in handles):
            try:
                now = time.monotonic()
                for handle in handles:
                    if handle["done"]:
                        continue
                    process = handle["process"]
                    elapsed = (time.monotonic_ns() - handle["started"]) / 1e9
                    if elapsed >= handle["spec"]["timeout_seconds"] and process.poll() is None:
                        stop(handle, "timed_out")
                    if process.poll() is not None:
                        stop(handle)  # Also end descendants left after the leader exits.
                    if handle["stop_at"] is not None and now - handle["stop_at"] >= TERM_GRACE_SECONDS:
                        if not handle.get("kill_attempted"):
                            handle["record"]["cleanup"]["sigkill_sent"] = signal_group(process, signal.SIGKILL)
                            handle["kill_attempted"] = True
                        if now - handle["stop_at"] >= DRAIN_GRACE_SECONDS and handle["pipes"]:
                            handle["record"]["cleanup"]["pipe_drain_incomplete"] = True
                            for pipe in list(handle["pipes"]):
                                close_pipe(handle, pipe)
                    cleanup_deadline = handle["stop_at"] is not None and now - handle["stop_at"] >= DRAIN_GRACE_SECONDS
                    if not handle["pipes"] and (process.poll() is not None or cleanup_deadline):
                        returncode = process.returncode
                        record = handle["record"]
                        if not handle.get("kill_attempted"):
                            # Finish this owned group immediately. Never retain
                            # an exited leader's PID for signaling after another
                            # long-running command, when that PID could be reused.
                            record["cleanup"]["sigkill_sent"] |= signal_group(process, signal.SIGKILL)
                            handle["kill_attempted"] = True
                        handle["group_cleanup_attempted"] = True
                        record["cleanup"]["process_still_running"] = returncode is None
                        record["exit_code"] = returncode if returncode is not None and returncode >= 0 else None
                        record["signal"] = -returncode if returncode is not None and returncode < 0 else None
                        incomplete = returncode is None or record["cleanup"]["pipe_drain_incomplete"]
                        record["status"] = handle["reason"] or ("cleanup_incomplete" if incomplete else (
                            "succeeded" if returncode == 0 else "failed"))
                        record["elapsed_ns"] = time.monotonic_ns() - handle["started"]
                        record["finished_at"] = utc_now()
                        for log in handle["logs"].values():
                            log.close()
                        handle["done"] = True
                for key, _ in selector.select(.02):
                    handle, stream = key.data
                    try:
                        chunk = os.read(key.fd, 65536)
                    except BlockingIOError:
                        continue
                    if not chunk:
                        close_pipe(handle, key.fileobj)
                        continue
                    record = handle["record"]
                    capacity = handle["spec"]["max_output_bytes"] - record[stream + "_bytes"]
                    kept = chunk[:capacity]
                    handle["logs"][stream].write(kept)
                    record[stream + "_bytes"] += len(kept)
                    if len(chunk) > capacity:
                        record[stream + "_truncated"] = True
                        stop(handle, "output_limit")
            except KeyboardInterrupt:
                interrupted = True
                for handle in handles:
                    if not handle["done"]:
                        stop(handle, "interrupted")
        return [handle["record"] for handle in handles], interrupted
    finally:
        for handle in handles:
            process = handle["process"]
            if process is not None:
                if not handle.get("group_cleanup_attempted"):
                    sent = signal_group(process, signal.SIGKILL)
                    handle["record"]["cleanup"]["sigkill_sent"] |= sent
                try:
                    process.wait(timeout=.5)
                except subprocess.TimeoutExpired:
                    handle["record"]["cleanup"]["process_still_running"] = True
            for pipe in list(handle["pipes"]):
                pipe.close()
            for log in handle["logs"].values():
                log.close()
        selector.close()


def evidence_refs(value, run_dir):
    if not isinstance(value, list) or not 1 <= len(value) <= 32:
        raise ValueError("evidence_refs requires 1..32 retained files")
    resolved = []
    for name in value:
        path = (run_dir / relative_path(name, "evidence reference")).resolve()
        if not path.is_relative_to(run_dir) or not path.is_file():
            raise ValueError("evidence reference is missing or outside this run: " + str(name))
        resolved.append(str(path.relative_to(run_dir)))
    return resolved


def nonnegative_count(value, name):
    if type(value) is not int or value < 0:
        raise ValueError(name + " must be an explicitly measured nonnegative integer")


def assess(commands, arm, run_dir, setup_ok):
    metrics = unknown_metrics()
    errors = []
    checks = [command for command in commands if command["phase"] == "check" and command["assesses"] == "integrated_correctness"]
    if checks and setup_ok:
        outcome = "failed" if any(c["status"] == "failed" for c in checks) else (
            "passed" if all(c["status"] == "succeeded" for c in checks) else "unknown")
        metrics["integrated_correctness"].update(
            status=outcome, source="designated_check_exit_status", check_ids=[c["id"] for c in checks])
    if "evidence_file" not in arm:
        return metrics, errors
    try:
        path = (run_dir / arm["evidence_file"]).resolve()
        if not path.is_relative_to(run_dir):
            raise ValueError("assessment escaped the run directory")
        report, _ = bounded_json(path, MAX_EVIDENCE_BYTES)
        object_fields(report, set(metrics), set(), "assessment")
    except (OSError, ValueError) as error:
        errors.append(str(error))
        return metrics, errors
    for name, entry in report.items():
        try:
            if not isinstance(entry, dict):
                raise ValueError(name + " must be an object")
            refs = evidence_refs(entry.get("evidence_refs"), run_dir)
            if name == "coverage":
                object_fields(entry, {"observed_tool_calls", "counts", "evidence_refs"},
                              {"observed_tool_calls", "counts", "evidence_refs"}, name)
                nonnegative_count(entry["observed_tool_calls"], "observed_tool_calls")
                object_fields(entry["counts"], set(COVERAGE_STATES), set(COVERAGE_STATES), "coverage.counts")
                for key, count in entry["counts"].items():
                    nonnegative_count(count, key)
                metrics[name] = {"status": "reported", **entry, "evidence_refs": refs}
            elif name in ("human_interventions", "unnecessary_denials", "missed_stale_writes"):
                object_fields(entry, {"count", "evidence_refs"}, {"count", "evidence_refs"}, name)
                nonnegative_count(entry["count"], name)
                metrics[name] = {"status": "reported", **entry, "evidence_refs": refs}
            elif name == "integrated_correctness":
                object_fields(entry, {"status", "evidence_refs"}, {"status", "evidence_refs"}, name)
                if entry["status"] not in ("passed", "failed", "unknown"):
                    raise ValueError("integrated_correctness status is invalid")
                checked = metrics[name]["status"]
                if checked != "unknown" and entry["status"] != checked:
                    metrics[name]["status"] = "unknown"
                    raise ValueError("check and supplied correctness assessment disagree")
                metrics[name].update(status=entry["status"], source="retained_evidence", evidence_refs=refs)
            else:
                fields = {"received_before_decision", "changed_approach", "human_relay", "delay_ms", "evidence_refs"}
                object_fields(entry, fields, fields, name)
                for field in ("received_before_decision", "changed_approach", "human_relay"):
                    if type(entry[field]) is not bool:
                        raise ValueError(field + " must be an explicitly assessed boolean")
                if isinstance(entry["delay_ms"], bool) or not isinstance(entry["delay_ms"], (int, float)) or not math.isfinite(entry["delay_ms"]) or entry["delay_ms"] < 0:
                    raise ValueError("delay_ms must be a measured, finite nonnegative value")
                metrics[name] = {"status": "reported", **entry, "evidence_refs": refs}
        except (OSError, ValueError) as error:
            errors.append(name + ": " + str(error))
    return metrics, errors


def append_record(journal, record):
    journal.write((json.dumps(record, ensure_ascii=False, separators=(",", ":")) + "\n").encode())
    journal.flush()
    os.fsync(journal.fileno())


def run_manifest(manifest_path, output_dir, repetitions=1):
    if os.name != "posix":
        raise ValueError("live runs require POSIX process-group cleanup")
    if type(repetitions) is not int or not 1 <= repetitions <= 100:
        raise ValueError("repetitions must be 1..100")
    manifest, manifest_hash = load_manifest(manifest_path)
    output_dir = Path(output_dir).resolve()
    runner_dir = Path(__file__).resolve().parent
    repository = next((parent for parent in runner_dir.parents if (parent / ".git").exists()), None)
    if repository is not None and output_dir.is_relative_to(repository) and not output_dir.is_relative_to(runner_dir / "runs"):
        raise ValueError("keep private results outside the repository or under benchmarks/coordination/runs/")
    output_dir.mkdir(mode=0o700, parents=True, exist_ok=False)
    environment = {
        "system": platform.system(), "release": platform.release(), "machine": platform.machine(),
        "python": platform.python_version(), "cpu_count": os.cpu_count(),
        "monotonic_resolution_seconds": time.get_clock_info("monotonic").resolution,
        "runner_sha256": digest(Path(__file__).read_bytes()),
        "environment_values_recorded": False,
    }
    finals = []
    aborted = False
    with private_file(output_dir / "runs.jsonl") as journal:
        for repetition in range(1, repetitions + 1):
            for item in manifest["items"]:
                # Rotate the declared arm order across repetitions; retain it.
                names = list(item["arms"])
                offset = (repetition - 1) % len(names)
                names = names[offset:] + names[:offset]
                for order, name in enumerate(names, 1):
                    arm = item["arms"][name]
                    context = {
                        "schema_version": 1, "benchmark_id": manifest["benchmark_id"],
                        "manifest_sha256": manifest_hash, "item_id": item["id"], "arm": name,
                        "item_description": item["description"],
                        "repetition": repetition, "arm_order": order, "run_id": uuid.uuid4().hex,
                    }
                    started = time.monotonic_ns()
                    run_dir = output_dir / context["run_id"]
                    run_dir.mkdir(mode=0o700)
                    initial = {
                        **context, "record_type": "run_started", "started_at": utc_now(),
                        "environment": environment, "metrics": unknown_metrics(),
                        "missing_comparison_arms": sorted(REQUIRED_ARMS - item["arms"].keys()),
                    }
                    append_record(journal, initial)
                    commands = []
                    setup_ok = True
                    agent_elapsed = None
                    for field, phase in PHASES:
                        specs = arm[field]
                        if not specs or phase == "agent" and not setup_ok:
                            continue
                        phase_started = time.monotonic_ns()
                        groups = [specs] if phase == "agent" else [[spec] for spec in specs]
                        for group in groups:
                            records, interrupted = run_group(group, phase, run_dir, output_dir, context)
                            commands.extend(records)
                            if interrupted:
                                aborted = True
                                break
                            if phase == "setup" and any(r["status"] != "succeeded" for r in records):
                                setup_ok = False
                                break
                        if phase == "agent":
                            agent_elapsed = time.monotonic_ns() - phase_started
                        if aborted:
                            break
                    metrics, errors = assess(commands, arm, run_dir, setup_ok)
                    relevant = [c for c in commands if c["phase"] in ("agent", "setup")]
                    statuses = {c["status"] for c in relevant}
                    status = "interrupted" if aborted else (
                        "setup_failed" if not setup_ok else (
                            "timed_out" if "timed_out" in statuses else (
                                "failed" if statuses - {"succeeded"} else "succeeded")))
                    final = {
                        **initial, "record_type": "run_finished", "finished_at": utc_now(),
                        "elapsed_ns": time.monotonic_ns() - started,
                        "agent_phase_elapsed_ns": agent_elapsed, "status": status,
                        "commands": commands, "metrics": metrics, "evidence_errors": errors,
                        "versions_status": "captured" if arm["versions"] and
                            len([c for c in commands if c["phase"] == "version"]) == len(arm["versions"]) and all(
                                c["status"] == "succeeded" and c["stdout_bytes"] + c["stderr_bytes"] > 0
                                for c in commands if c["phase"] == "version") else "unknown",
                    }
                    append_record(journal, final)
                    finals.append(final)
                    if aborted:
                        break
                if aborted:
                    break
            if aborted:
                break
    summary = {
        "schema_version": 1, "benchmark_id": manifest["benchmark_id"],
        "finished_attempts": len(finals), "interrupted": aborted,
        "statuses": {name: sum(r["status"] == name for r in finals) for name in (
            "succeeded", "failed", "timed_out", "setup_failed", "interrupted")},
        "integrated_correctness": {name: sum(r["metrics"]["integrated_correctness"]["status"] == name for r in finals)
                                  for name in ("passed", "failed", "unknown")},
        "note": "Process completion is not model quality or product-proof acceptance. No missing assessment is zero.",
    }
    with private_file(output_dir / "summary.json") as output:
        output.write((json.dumps(summary, indent=2) + "\n").encode())
    return summary


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("--run", action="store_true", help="explicitly execute the owner-supplied commands")
    parser.add_argument("--output", type=Path, help="new private result directory; required with --run")
    parser.add_argument("--repetitions", type=int, default=1)
    args = parser.parse_args(argv)
    try:
        manifest, manifest_hash = load_manifest(args.manifest)
        if not 1 <= args.repetitions <= 100:
            raise ValueError("--repetitions must be 1..100")
        if not args.run:
            print(json.dumps({
                "mode": "validate_only", "executed_commands": 0,
                "benchmark_id": manifest["benchmark_id"], "manifest_sha256": manifest_hash,
                "repetitions": args.repetitions,
                "items": [{"id": item["id"], "arms": list(item["arms"]),
                           "missing_comparison_arms": sorted(REQUIRED_ARMS - item["arms"].keys())}
                          for item in manifest["items"]],
            }, indent=2))
            return 0
        if args.output is None:
            raise ValueError("--run requires --output pointing to a new directory")
        if hasattr(signal, "SIGTERM"):
            signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(KeyboardInterrupt()))
        summary = run_manifest(args.manifest, args.output, args.repetitions)
        print(json.dumps({"results": str(args.output.resolve() / "runs.jsonl"), **summary}, indent=2))
        return 130 if summary["interrupted"] else (
            1 if summary["statuses"]["failed"] or summary["statuses"]["timed_out"]
            or summary["statuses"]["setup_failed"] or summary["integrated_correctness"]["failed"] else 0)
    except (OSError, ValueError) as error:
        print(json.dumps({"error": str(error)}), file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
