#!/usr/bin/env bash
# The result of one platform's Rust checks, for the check a ruleset requires
# (`Linux` or `macOS` in .github/workflows/check.yml). The full lane builds
# the suite once (the build job) and runs its tests in shards; this passes
# only when the build passed and every shard passed, or when no shard was
# meant to run: on a pull request, whose fast lane the build job runs
# whole, or for a change that touches no code.
#
# Usage: bash scripts/ci-shard-results.sh DIR
#
# Environment: EVENT (github.event_name), CODE (the build job's `code`
# output), BUILD and TESTS (the build job's and the shard matrix's result:
# success, failure, cancelled or skipped). DIR holds the shards' JUnit
# reports, one directory each (missing when no shard ran). Under GitHub
# Actions it writes the step outputs `flaky` and `failed`, the tests every
# shard names (scripts/flaky-tests.py, scripts/ci-failed-tests.sh), for
# the issues check.yml opens on main. The reports are this run's own output.
set -euo pipefail
dir=${1:?usage: ci-shard-results.sh DIR}
here=$(dirname "$0")

flaky="" failed=""
for junit in "$dir"/*/junit.xml; do
  [ -e "$junit" ] || continue
  flaky+=$(env -u GITHUB_OUTPUT -u GITHUB_STEP_SUMMARY python3 -I -B "$here/flaky-tests.py" "$junit")$'\n'
  failed+=$(env -u GITHUB_OUTPUT bash "$here/ci-failed-tests.sh" "$junit")$'\n'
done
flaky=$(printf '%s' "$flaky" | sed '/^$/d')
failed=$(printf '%s' "$failed" | sed '/^$/d')
if [ -n "${GITHUB_OUTPUT:-}" ]; then
  for name in flaky failed; do
    delimiter="END_$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')"
    printf '%s<<%s\n%s\n%s\n' "$name" "$delimiter" "${!name}" "$delimiter" >>"$GITHUB_OUTPUT"
  done
fi

if [ "${BUILD:-}" != success ]; then
  verdict="the build job ended with '${BUILD:-}', so no shard's result counts; its log says what failed"
elif [ "${TESTS:-}" = success ]; then
  verdict=""
elif [ "${TESTS:-}" = skipped ] && { [ "${EVENT:-}" = pull_request ] || [ "${CODE:-}" != true ]; }; then
  verdict=""
else
  verdict="the test shards ended with '${TESTS:-}'; open the failed shard's log, or the failed tests below"
fi
summary=${GITHUB_STEP_SUMMARY:-/dev/null}
if [ -z "$verdict" ]; then
  echo "ci-shard-results: passed (build ${BUILD}, shards ${TESTS})" | tee -a "$summary"
  exit 0
fi
{
  echo "ci-shard-results: failed: $verdict"
  if [ -n "$failed" ]; then printf 'Failed tests:\n%s\n' "$failed"; fi
} | tee -a "$summary" >&2
exit 1
