"""Fail when a test not named slow_ took over 10 s, from a cargo-nextest JUnit report.

Usage: python3 -I -B scripts/slow-tests.py JUNIT

A test that waits in real time sets the floor of the full lane's time
(issue #311). Make it wait on a condition, or shorten the product delay it
tests with a LANGSPAN_TEST_*_MS setting. A test that must stay slow is
named `slow_…`, with a comment saying why; the fast lane skips it and the
full lane still runs it (docs/development-process.md#slow-tests).

Prints the summed test time and the slowest tests. Exits 1 naming each test
over the limit whose name (its last `::` part) does not start with `slow_`.
The report is the CI job's own output, from tests in this repository.
"""

import os
import sys
import xml.etree.ElementTree as ElementTree

LIMIT_S = 10.0
SHOWN = 10
# nextest writes one report per run; this bounds what is read if a run
# produces something else at that path.
MAX_REPORT_BYTES = 64 * 1024 * 1024


def cases(path):
    """Each test case as (seconds, binary, name)."""
    size = os.path.getsize(path)
    if size > MAX_REPORT_BYTES:
        raise SystemExit(
            f"{path} is {size} bytes, over the {MAX_REPORT_BYTES}-byte bound; "
            "check what wrote it"
        )
    root = ElementTree.parse(path).getroot()
    return [
        (float(case.get("time") or 0), case.get("classname"), case.get("name"))
        for case in root.iter("testcase")
    ]


def tagged(name):
    return name.rsplit("::", 1)[-1].startswith("slow_")


def main(argv):
    if len(argv) != 2:
        raise SystemExit("usage: slow-tests.py JUNIT")
    found = cases(argv[1])
    found.sort(reverse=True)
    total = sum(seconds for seconds, _, _ in found)
    print(f"slow-tests.py: {len(found)} tests, {total:.1f} s summed; the slowest:")
    for seconds, binary, name in found[:SHOWN]:
        print(f"  {seconds:6.1f} s  {binary} {name}")
    over = [
        (seconds, binary, name)
        for seconds, binary, name in found
        if seconds > LIMIT_S and not tagged(name)
    ]
    for seconds, binary, name in over:
        print(
            f"slow-tests.py: {binary} {name} took {seconds:.1f} s, over {LIMIT_S:.0f} s; "
            "wait on the condition (tests/support/mod.rs) or shorten the delay it "
            "tests, or name it slow_… with a comment saying why",
            file=sys.stderr,
        )
    return 1 if over else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
