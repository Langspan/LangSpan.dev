"""Stale callers in a `discovered_change` trial, from its run records.

Every arm keeps each session's stream-json transcript, so the measure comes
from there and never from what an agent says: each Write or Edit call's
input, replayed over the fixture's file; whether its result was an error
(a refused write is not admitted); and the clock line the driver's tool
guard adds after each line of tool results (CLOCK_TYPE). Every session
runs on one machine, so one monotonic clock orders them. The oracle owns
what an old-form call (`references`) and the changed signature
(`required_arguments`) are. Nothing here reads a file or a clock.
"""

from pathlib import PurePosixPath

from oracle import references, required_arguments

CLOCK_TYPE = "langspan_bench_clock"
# The file tools whose input says what they wrote. A write any other way
# (Bash, for one) leaves a committed file the replay does not match.
TOOLS = ("Write", "Edit")
MAX_ITEMS = 200


def blocks(event):
    """The content blocks of a stream-json `assistant` or `user` event."""
    message = event.get("message") if isinstance(event, dict) else None
    content = message.get("content") if isinstance(message, dict) else None
    return [block for block in content if isinstance(block, dict)] if isinstance(content, list) else []


def clock_line(event, now_ns):
    """The line the tool guard writes after EVENT: the IDs of the tool
    results it carries and NOW_NS, or None when it carries none."""
    ids = [block["tool_use_id"] for block in blocks(event)
           if block.get("type") == "tool_result" and isinstance(block.get("tool_use_id"), str)]
    return {"type": CLOCK_TYPE, "tool_use_ids": ids, "monotonic_ns": now_ns} if ids else None


def relative(file_path, roots):
    """FILE_PATH relative to the first of ROOTS (a session's checkout) that
    holds it, or None for a path outside them or through `..`."""
    if not isinstance(file_path, str):
        return None
    path = PurePosixPath(file_path)
    for root in roots:
        try:
            inside = path.relative_to(root) if path.is_absolute() else path
        except ValueError:
            continue
        return None if ".." in inside.parts or not inside.parts else str(inside)
    return None


def writes(events, roots):
    """Each Write and Edit call in one session's EVENTS that a result
    answered, in the order its clock line gives: (monotonic_ns, path,
    tool, input, admitted). None when a result has no clock line. A call
    outside the session's checkout is left out."""
    calls, answered, clocks = {}, [], {}
    for event in events:
        kind = event.get("type")
        if kind == CLOCK_TYPE and type(event.get("monotonic_ns")) is int and isinstance(event.get("tool_use_ids"), list):
            for name in event["tool_use_ids"]:
                clocks.setdefault(name, event["monotonic_ns"])
        for block in blocks(event):
            name = block.get("id") if kind == "assistant" else block.get("tool_use_id")
            if not isinstance(name, str):
                continue
            if block.get("type") == "tool_use" and block.get("name") in TOOLS and isinstance(block.get("input"), dict):
                calls[name] = block
            elif kind == "user" and block.get("type") == "tool_result" and name in calls:
                answered.append((name, block.get("is_error") is not True))
    found = []
    for name, admitted in answered:
        if name not in clocks:
            return None
        call = calls[name]
        path = relative(call["input"].get("file_path"), roots)
        if path is not None:
            found.append((clocks[name], path, call["name"], call["input"], admitted))
    return sorted(found, key=lambda write: write[0])


def replay(before, tool, args):
    """The file's text after one Write or Edit of BEFORE, or None when the
    call cannot have made it (an Edit whose text is not there)."""
    if tool == "Write":
        return args.get("content") if isinstance(args.get("content"), str) else None
    old, new = args.get("old_string"), args.get("new_string")
    if not (isinstance(before, str) and isinstance(old, str) and isinstance(new, str) and old and old in before):
        return None
    return before.replace(old, new) if args.get("replace_all") is True else before.replace(old, new, 1)


def stale(text, interface):
    """Whether TEXT names the changed function in its old form: a call with
    fewer arguments than the change requires, or not a plain call. Text
    that does not parse names nothing, as in the oracle."""
    try:
        found = references(text, interface["name"]) if text is not None else None
    except (ValueError, RecursionError, MemoryError):
        # A NUL byte, or nesting deeper than the parser takes.
        return False
    return any(count is None or count < interface["arity"] for count in found or [])


def module_path(module):
    return module.replace(".", "/") + ".py"


def unknown(reason):
    return {"status": "unknown", "reason": reason}


def stale_callers(plan, transcripts, base, committed):
    """The stale-caller measure of one trial. PLAN has its `family` and
    `tasks`; TRANSCRIPTS, per session in order, the events of its transcript
    (None when there is none) and the paths its checkout is known by;
    BASE(path) is the fixture's text of a file and COMMITTED(session, path)
    the text the session committed, each None for no file.

    The change is the changer's first admitted write after which its helper
    requires the changed form's arguments. From then on, a dependent's file
    that names the helper in its old form is a stale caller, fixed at the
    admitted write that leaves it without one, and each admitted dependent
    write that leaves such a file is a stale write. `fixed_after_s` runs
    from the change to the last stale caller fixed: None when none was
    stale, or one still is (`unfixed`). `unknown` when a session's
    committed file is not what its replayed calls left, as after a write
    with Bash."""
    changer = next((index for index, task in enumerate(plan["tasks"], 1) if task["role"] == "changer"), None)
    if plan["family"] != "discovered_change" or changer is None:
        return {"status": "not_applicable", "reason": "the family has no interface change"}
    interface = plan["tasks"][changer - 1]["interface"]
    helper = module_path(interface["module"])
    timeline = []
    for index, ((events, roots), task) in enumerate(zip(transcripts, plan["tasks"]), 1):
        found = writes(events, roots) if events is not None else None
        if found is None:
            return unknown("session %d has no transcript, or a write in it has no clock line" % index)
        files = {}
        for ns, path, tool, args, admitted in found:
            if admitted:
                files[path] = replay(files[path] if path in files else base(path), tool, args)
                if files[path] is None:
                    return unknown("session %d's %s of %s replaces text that is not there" % (index, tool, path))
                timeline.append((ns, index, path, files[path]))
        for path in sorted(set(files) | {module_path(task["module"])} | ({helper} if index == changer else set())):
            if committed(index, path) != files.get(path, base(path)):
                return unknown("session %d's committed %s is not what its Write and Edit calls left: it was "
                               "written another way" % (index, path))
    timeline.sort(key=lambda entry: entry[:2])
    change = next((ns for ns, index, path, text in timeline if index == changer and path == helper
                   and (required_arguments(text, interface["name"]) or 0) >= interface["arity"]), None)
    if change is None:
        return {"status": "not_applicable", "reason": "the changer did not make the change"}
    dependents = [entry for entry in timeline if plan["tasks"][entry[1] - 1]["role"] == "dependent"]
    items, now, ever, admitted, last_fix = [], set(), set(), 0, None

    def item(index, path, ns, kind):
        if len(items) < MAX_ITEMS:
            items.append({"session": index, "path": path, "after_s": (ns - change) / 1e9, "kind": kind})

    at_change = {(index, path): text for ns, index, path, text in dependents if ns < change}
    for (index, path), text in sorted(at_change.items()):
        if stale(text, interface):
            now.add((index, path))
            item(index, path, change, "stale_at_change")
    for ns, index, path, text in dependents:
        if ns < change:
            continue
        if stale(text, interface):
            admitted += 1
            now.add((index, path))
            item(index, path, ns, "stale_write")
        elif (index, path) in now:
            now.discard((index, path))
            last_fix = ns
            item(index, path, ns, "fixed")
        ever |= now
    ever |= now  # the callers stale at the change, when no write follows it
    return {"status": "reported", "writes_admitted": admitted, "stale_files": len(ever), "unfixed": len(now),
            "fixed_after_s": (last_fix - change) / 1e9 if ever and not now else None, "items": items}
