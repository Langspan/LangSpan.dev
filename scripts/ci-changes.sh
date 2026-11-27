#!/usr/bin/env bash
# Decides what a CI run's change touches (.github/workflows/check.yml), and
# writes `code` and `dry_runs` to $GITHUB_OUTPUT. Each job runs it for
# itself, so the Linux and macOS jobs start together.
#
# A pull request or merge-queue commit that changes only Markdown builds
# nothing: no test or build step reads a .md file. Other files under docs/
# count as code, since a test replays a recorded host session kept in
# docs/evidence/. The jobs still run and pass, so a required check is never
# left waiting, as it would be with a workflow-level paths filter. Pushes to
# main, manual runs and the nightly run always check.
#
# The benchmark harness's dry runs (tests/benchmark_harness.rs) took about
# 11 minutes of macOS's 18 (issue #286). They run only when the change
# touches a path in BENCH_PATHS: the harness, the briefing, hook and daemon
# code it drives, and what builds or runs the suite. The core, daemon and
# hook entries name directories, so code moved into a new module there
# still runs them (issue #341). A pull request is
# HEAD^1..HEAD. A merge-queue commit is its group's base..head, so the pull
# requests queued ahead count too. A push to main is HEAD^1..HEAD when it
# adds one commit (a squash merge), and otherwise runs them. Manual and
# nightly runs always run them. Anything unsure runs them.
#
# Environment: EVENT (github.event_name), BEFORE (github.event.before),
# BASE_SHA and HEAD_SHA (the merge group's), GITHUB_OUTPUT.
#
# A failed git command stops the script (set -e) instead of reading as
# "nothing changed".
set -eu

BENCH_PATHS='^benchmarks/
|^tests/benchmark_harness\.rs$
|^src/(daemon|hook)(/|\.rs$)
|^src/(claude_code|recent)\.rs$
|^crates/langspan-core/src/
|^(Cargo\.toml|Cargo\.lock|rust-toolchain\.toml)$
|^\.config/nextest\.toml$
|^\.github/workflows/check\.yml$
|^scripts/(check|test-foreign-home|ci-changes)\.sh$'

code=true
dry_runs=run
from=
to=HEAD
if [ "$EVENT" = pull_request ]; then
  from=HEAD^1
elif [ "$EVENT" = merge_group ] && [ -n "$BASE_SHA" ] && [ -n "$HEAD_SHA" ]; then
  from=$BASE_SHA
  to=$HEAD_SHA
elif [ "$EVENT" = push ] && [ "$(git rev-parse HEAD^1)" = "$BEFORE" ]; then
  from=HEAD^1
fi
if [ -n "$from" ]; then
  changed=$(git diff --name-only "$from" "$to")
  if [ "$EVENT" != push ] && [ -n "$changed" ] && ! printf '%s\n' "$changed" | grep -Ev '\.md$' | grep -q .; then
    code=false
  fi
  pattern=$(printf '%s' "$BENCH_PATHS" | tr -d '\n ')
  # grep exits 1 for no match; anything else (a bad pattern) stops the
  # script rather than skip the dry runs.
  matches=$(grep -Ec -- "$pattern" <<< "$changed") || [ $? -eq 1 ]
  if [ -n "$changed" ] && [ "$matches" -eq 0 ]; then
    dry_runs=skip
  fi
fi
echo "code=$code" >> "$GITHUB_OUTPUT"
echo "dry_runs=$dry_runs" >> "$GITHUB_OUTPUT"
echo "touches code: $code"
echo "benchmark dry runs: $dry_runs"
