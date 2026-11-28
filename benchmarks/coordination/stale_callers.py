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
# (Bash, for one) leaves a committed file the replay does not match, and
# the session's changed files name it.
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


def owners(sessions):
    """Who committed a checkout's files: "session 1's", or "the checkout
    of sessions 1 and 2 (shared)" when several sessions work in it."""
    if len(sessions) == 1:
        return "session %d's" % sessions[0]
    names = [str(index) for index in sessions]
    return "the checkout of sessions %s and %s (shared)" % (", ".join(names[:-1]), names[-1])


def stale_callers(plan, transcripts, base, committed, changed):
    """The stale-caller measure of one trial. PLAN has its `family` and
    `tasks`; TRANSCRIPTS, per session in order, the events of its transcript
    (None when there is none) and the paths its checkout is known by;
    BASE(path) is the fixture's text of a file and COMMITTED(session, path)
    the text the session committed, each None for no file; CHANGED(session)
    the paths the session added, changed or removed, None when they could
    not be listed.

    The change is the changer's first admitted write after which its helper
    requires the changed form's arguments. From then on, a dependent's file
    that names the helper in its old form is a stale caller, fixed at the
    admitted write that leaves it without one, and each admitted dependent
    write that leaves such a file is a stale write. `fixed_after_s` runs
    from the change to the last stale caller fixed: None when none was
    stale, or one still is (`unfixed`). `unknown` when a session's
    committed file, its changed Python files among them, is not what its
    replayed calls left, as after a write with Bash. Sessions known by the
    same paths share one checkout: their writes replay together in clock
    order, each still attributed to its own session, and the checkout is
    checked against that combined replay. There a file is the checkout's,
    not its writer's: a dependent's stale caller is fixed by whichever
    session's admitted write leaves it in the new form."""
    changer = next((index for index, task in enumerate(plan["tasks"], 1) if task["role"] == "changer"), None)
    if plan["family"] != "discovered_change" or changer is None:
        return {"status": "not_applicable", "reason": "the family has no interface change"}
    interface = plan["tasks"][changer - 1]["interface"]
    helper = module_path(interface["module"])
    # Sessions known by the same paths share one checkout (the shared arm).
    checkouts = {}
    for index, ((events, roots), task) in enumerate(zip(transcripts, plan["tasks"]), 1):
        found = writes(events, roots) if events is not None else None
        if found is None:
            return unknown("session %d has no transcript, or a write in it has no clock line" % index)
        touched = changed(index)
        if touched is None:
            return unknown("session %d's changed files could not be listed" % index)
        # A Python file is the only kind that can hold a caller.
        expected = {path for path in touched if path.endswith(".py")} | {module_path(task["module"])} \
            | ({helper} if index == changer else set())
        checkout = checkouts.setdefault(tuple(roots), {"sessions": [], "writes": [], "expected": set()})
        checkout["sessions"].append(index)
        checkout["writes"] += [(ns, index, path, tool, args) for ns, path, tool, args, admitted in found if admitted]
        checkout["expected"] |= expected
    timeline = []
    for key, checkout in checkouts.items():
        # Each session's writes in clock order, its peers' in the same checkout among them.
        files, sessions = {}, checkout["sessions"]
        for ns, index, path, tool, args in sorted(checkout["writes"], key=lambda write: write[:2]):
            files[path] = replay(files[path] if path in files else base(path), tool, args)
            if files[path] is None:
                return unknown("session %d's %s of %s replaces text that is not there" % (index, tool, path))
            timeline.append((ns, index, (key, path), files[path]))
        for path in sorted(set(files) | checkout["expected"]):
            if committed(sessions[0], path) != files.get(path, base(path)):
                return unknown("%s committed %s is not what %s Write and Edit calls left: it was written another way"
                               % (owners(sessions), path, "its" if len(sessions) == 1 else "their"))
    timeline.sort(key=lambda entry: entry[:2])
    change = next((ns for ns, index, (_, path), text in timeline if index == changer and path == helper
                   and (required_arguments(text, interface["name"]) or 0) >= interface["arity"]), None)
    if change is None:
        return {"status": "not_applicable", "reason": "the changer did not make the change"}

    def dependent(index):
        return plan["tasks"][index - 1]["role"] == "dependent"

    # A file is one (checkout, path): in a shared checkout a peer's write
    # leaves it stale or fixes it as much as its writer's does.
    items, now, ever, admitted, last_fix = [], set(), set(), 0, None

    def item(index, file, ns, kind):
        if len(items) < MAX_ITEMS:
            items.append({"session": index, "path": file[1], "after_s": (ns - change) / 1e9, "kind": kind})

    # Each file a dependent wrote before the change, as the last write before it left it.
    at_change = {}
    for ns, index, file, text in timeline:
        if ns < change and (dependent(index) or file in at_change):
            at_change[file] = (index, text)
    for file, (index, text) in sorted(at_change.items(), key=lambda entry: (entry[1][0], entry[0])):
        if stale(text, interface):
            now.add(file)
            item(index, file, change, "stale_at_change")
    ever |= now  # before a write that follows the change can fix one
    for ns, index, file, text in timeline:
        if ns < change:
            continue
        if stale(text, interface):
            if dependent(index):
                admitted += 1
                now.add(file)
                item(index, file, ns, "stale_write")
        elif file in now:
            now.discard(file)
            last_fix = ns
            item(index, file, ns, "fixed")
        ever |= now
    return {"status": "reported", "writes_admitted": admitted, "stale_files": len(ever), "unfixed": len(now),
            "fixed_after_s": (last_fix - change) / 1e9 if ever and not now else None, "items": items}
