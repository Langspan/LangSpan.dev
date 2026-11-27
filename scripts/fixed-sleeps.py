"""Count the fixed sleeps of 200 ms or more in the Rust test code under DIR.

Usage: python3 -I -B scripts/fixed-sleeps.py DIR

A test that sleeps a fixed time and then asserts is too short on a loaded
runner, which fails the run, and wasted time on a fast one (issue #310).
Wait on the condition instead, with the helpers in tests/support/mod.rs.

A call `sleep(...)` counts unless its argument is a literal
`Duration::from_millis`, `from_micros` or `from_nanos` under 200 ms, such as
the step of a loop that polls toward a deadline. An argument the script
can't read as short (a constant, a sum, `from_secs`) counts.

scripts/fixed-sleeps.tsv is the allowlist: a path and the number of fixed
sleeps it may still hold. A file not listed may hold none. The check fails
when a file holds more than its count, and also when it holds fewer, so the
list only shrinks: lower the count, or remove the line at zero.
"""

import os
import re
import sys

LIMIT_MS = 200
ALLOWLIST = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fixed-sleeps.tsv")
# Test sources are small; this bounds what is read from one file.
MAX_FILE_BYTES = 4 * 1024 * 1024

CALL = re.compile(r"\bsleep\(")
SHORT = re.compile(
    r"^\s*(?:std::time::)?Duration::from_(millis|micros|nanos)\(\s*([0-9_]+)\s*\)\s*$"
)
COMMENT = re.compile(r"//[^\n]*")
PER_MS = {"millis": 1, "micros": 1_000, "nanos": 1_000_000}


def argument(text, start):
    """The text between the parenthesis at `start` and its match."""
    depth = 0
    for index in range(start, len(text)):
        if text[index] == "(":
            depth += 1
        elif text[index] == ")":
            depth -= 1
            if depth == 0:
                return text[start + 1 : index]
    return text[start + 1 :]


def is_short(arg):
    match = SHORT.match(arg)
    if not match:
        return False
    value = int(match.group(2).replace("_", ""))
    return value < LIMIT_MS * PER_MS[match.group(1)]


def fixed_sleeps(text):
    """The line numbers of the fixed sleeps in one file's text."""
    # A comment's length is kept, so line numbers stay right.
    code = COMMENT.sub(lambda match: " " * len(match.group(0)), text)
    lines = []
    for match in CALL.finditer(code):
        if not is_short(argument(code, match.end() - 1)):
            lines.append(code.count("\n", 0, match.start()) + 1)
    return lines


def allowlist():
    allowed = {}
    with open(ALLOWLIST, encoding="utf-8") as listing:
        for number, line in enumerate(listing, 1):
            line = line.rstrip("\n")
            if not line or line.startswith("#"):
                continue
            fields = line.split("\t")
            if len(fields) != 2 or not fields[1].isdigit():
                raise SystemExit(
                    f"{ALLOWLIST}:{number}: expected PATH<TAB>COUNT, found {line!r}"
                )
            allowed[fields[0]] = int(fields[1])
    return allowed


def scan(root):
    found = {}
    for directory, _, files in os.walk(root):
        for name in sorted(files):
            if not name.endswith(".rs"):
                continue
            path = os.path.join(directory, name)
            if os.path.getsize(path) > MAX_FILE_BYTES:
                raise SystemExit(f"{path} is over {MAX_FILE_BYTES} bytes; check what wrote it")
            with open(path, encoding="utf-8") as source:
                lines = fixed_sleeps(source.read())
            if lines:
                found[os.path.normpath(path)] = lines
    return found


def main(argv):
    if len(argv) != 2:
        raise SystemExit("usage: fixed-sleeps.py DIR")
    root = os.path.normpath(argv[1])
    found = scan(root)
    allowed = {
        os.path.normpath(path): count
        for path, count in allowlist().items()
        if os.path.normpath(path).startswith(root + os.sep)
    }
    problems = []
    for path in sorted(set(found) | set(allowed)):
        lines = found.get(path, [])
        ceiling = allowed.get(path, 0)
        if len(lines) > ceiling:
            at = ", ".join(str(line) for line in lines)
            problems.append(
                f"{path}: {len(lines)} fixed sleep(s) of {LIMIT_MS} ms or more "
                f"(lines {at}), {ceiling} allowed; wait on the condition with "
                "tests/support/mod.rs instead"
            )
        elif len(lines) < ceiling:
            fix = "remove its line" if not lines else f"lower its count to {len(lines)}"
            problems.append(
                f"{path}: {len(lines)} fixed sleep(s), fewer than the {ceiling} "
                f"scripts/fixed-sleeps.tsv allows; {fix} there"
            )
    total = sum(len(lines) for lines in found.values())
    for problem in problems:
        print(f"fixed-sleeps.py: {problem}", file=sys.stderr)
    if problems:
        return 1
    print(f"fixed-sleeps.py: {total} fixed sleep(s) in {root}, as scripts/fixed-sleeps.tsv allows")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
