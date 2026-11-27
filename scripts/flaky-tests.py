"""Name the tests that passed only on retry in a cargo-nextest JUnit report.

Usage: python3 -I -B scripts/flaky-tests.py JUNIT

nextest records a try that failed before a later try passed as a
<flakyFailure> element of the test case. A test that failed every try has a
<failure> element instead, and it fails the run on its own; it is not
listed here.

Prints each flaky test as "binary test", one per line. When GitHub Actions
runs it, it also writes a section to the job summary ($GITHUB_STEP_SUMMARY)
and the list as the step output `flaky` ($GITHUB_OUTPUT), which an issue on
main is made from (.github/workflows/check.yml). It exits 0 whether or not
any test was flaky; a missing report is said in the summary, since the step
that ran the tests has failed already.

The report is the CI job's own output, from tests in this repository.
"""

import os
import secrets
import sys
import xml.etree.ElementTree as ElementTree

# nextest writes one report per run; this bounds what is read if a run
# produces something else at that path.
MAX_REPORT_BYTES = 64 * 1024 * 1024


def flaky_tests(path):
    """Returns the flaky tests' names, or None when there is no report."""
    try:
        size = os.path.getsize(path)
    except FileNotFoundError:
        return None
    if size > MAX_REPORT_BYTES:
        raise SystemExit(
            f"{path} is {size} bytes, over the {MAX_REPORT_BYTES}-byte bound; "
            "check what wrote it"
        )
    root = ElementTree.parse(path).getroot()
    return [
        f"{case.get('classname')} {case.get('name')}"
        for case in root.iter("testcase")
        if case.find("flakyFailure") is not None and case.find("failure") is None
    ]


def append(variable, text):
    path = os.environ.get(variable)
    if path:
        with open(path, "a", encoding="utf-8") as out:
            out.write(text)


def main(argv):
    if len(argv) != 2:
        raise SystemExit("usage: flaky-tests.py JUNIT")
    path = argv[1]
    names = flaky_tests(path)
    if names is None:
        summary = (
            f"### Tests that passed only on retry\n\nNo JUnit report at `{path}`: "
            "the tests did not run, so this run names no flaky test.\n"
        )
        names = []
    elif names:
        lines = "".join(f"- `{name}`\n" for name in names)
        summary = (
            "### Tests that passed only on retry\n\n"
            f"{len(names)} test(s) failed a try and then passed, so the run "
            "passed. Each needs a deterministic fix the same day "
            "(docs/development-process.md).\n\n"
            f"{lines}"
        )
    else:
        summary = "### Tests that passed only on retry\n\nNo test passed only on retry.\n"
    for name in names:
        print(name)
    append("GITHUB_STEP_SUMMARY", summary)
    delimiter = f"FLAKY_{secrets.token_hex(16)}"
    append("GITHUB_OUTPUT", f"flaky<<{delimiter}\n" + "".join(f"{n}\n" for n in names) + f"{delimiter}\n")


if __name__ == "__main__":
    main(sys.argv)
