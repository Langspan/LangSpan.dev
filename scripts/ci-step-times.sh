#!/usr/bin/env bash
# Prints one row per Linux or macOS job of the given Rust checks runs, with
# the times of the steps that cost the most, in seconds: the wait from the
# run's creation to the job's start, the whole job, restoring the build
# cache, installing the CI tools, and the scripts/check.sh step. Read-only:
# it calls `gh run view` and changes nothing on GitHub.
#
#   bash scripts/ci-step-times.sh RUN_ID...
#
# Columns (tab-separated): run, event, sha, job, conclusion, wait_s, job_s,
# cache_s, tools_s, check_s. A step that didn't run reads 0; a job that
# never started reads -. docs/evidence/ci/README.md keeps the rows measured.
set -euo pipefail
[ "$#" -gt 0 ] || { echo "usage: ci-step-times.sh RUN_ID..." >&2; exit 2; }
command -v gh >/dev/null || { echo "ci-step-times: gh is not on PATH; install it and sign in" >&2; exit 1; }

printf 'run\tevent\tsha\tjob\tconclusion\twait_s\tjob_s\tcache_s\ttools_s\tcheck_s\n'
for run in "$@"; do
  case $run in '' | *[!0-9]*) echo "ci-step-times: $run is not a run ID" >&2; exit 2 ;; esac
  gh run view "$run" --json databaseId,event,headSha,createdAt,jobs --jq '
    def t: fromdateiso8601;
    def secs($s): if $s and $s.startedAt and $s.completedAt and $s.conclusion != "skipped"
      then ($s.completedAt | t) - ($s.startedAt | t) else 0 end;
    def step($j; $re): secs([$j.steps[] | select(.name | test($re))] | first);
    . as $r | .jobs[] | select(.name == "Linux" or .name == "macOS") | . as $j
    | [$r.databaseId, $r.event, $r.headSha[0:8], .name, (.conclusion // "-"),
       (if .startedAt then (.startedAt | t) - ($r.createdAt | t) else "-" end),
       (if .startedAt and .completedAt then (.completedAt | t) - (.startedAt | t) else "-" end),
       step($j; "^Restore the build cache"), step($j; "^Install the (dependency|test runner)"),
       step($j; "^Check formatting")]
    | map(tostring) | join("\t")'
done
