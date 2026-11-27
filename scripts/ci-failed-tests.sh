#!/usr/bin/env bash
# Names the tests that failed every try in a cargo-nextest JUnit report, for
# the issue check.yml opens when the macOS job fails on main.
#
# Usage: bash scripts/ci-failed-tests.sh JUNIT
#
# Prints each failed test as "binary test", one per line, at most 50 (then
# a line saying how many more). Under GitHub Actions it also writes the list
# as the step output `failed` ($GITHUB_OUTPUT). A test that passed on a
# retry isn't listed (scripts/flaky-tests.py names those). With no report,
# as when the job failed or timed out before the tests ended, it prints
# nothing and exits 0: the issue then says no test was recorded.
#
# The report is the CI job's own output, from tests in this repository.
set -euo pipefail
junit=${1:?usage: ci-failed-tests.sh JUNIT}

names=$(python3 -I -B - "$junit" <<'PY'
import os
import sys
import xml.etree.ElementTree as ElementTree

path = sys.argv[1]
# nextest writes one report per run; this bounds what is read if a run
# produces something else at that path.
if not os.path.exists(path):
    sys.exit(0)
size = os.path.getsize(path)
if size > 64 * 1024 * 1024:
    sys.exit(f"{path} is {size} bytes, over the 64 MiB bound; check what wrote it")
failed = [
    f"{case.get('classname')} {case.get('name')}"
    for case in ElementTree.parse(path).getroot().iter("testcase")
    if case.find("failure") is not None or case.find("error") is not None
]
for name in failed[:50]:
    print(name)
if len(failed) > 50:
    print(f"and {len(failed) - 50} more; the run's JUnit report lists them all")
PY
)

list=${names:+$names$'\n'}
printf '%s' "$list"
if [ -n "${GITHUB_OUTPUT:-}" ]; then
  delimiter="FAILED_$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')"
  printf 'failed<<%s\n%s%s\n' "$delimiter" "$list" "$delimiter" >>"$GITHUB_OUTPUT"
fi
