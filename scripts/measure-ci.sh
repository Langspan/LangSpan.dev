#!/usr/bin/env bash
# Measure the Rust checks (check.yml) from GitHub's run data: workflow
# completion per event, the merge-queue gate, the Linux and macOS job times, whether each job
# found its build cache (cold, a fallback entry, or its exact key), and how
# many dependency crates it compiled. Read-only: it calls `gh api` and
# changes nothing on GitHub.
#
#   bash scripts/measure-ci.sh [--since ISO] [--until ISO] [--logs N] [--rows DIR]
#
# --since/--until  bound the runs by creation time (UTC, e.g. 2026-11-27T04:06:11Z).
# --logs N         read the logs of the newest N pull-request and N merge-queue
#                  runs (default 10; 0 skips logs). Logs give cache and compile facts.
# --rows DIR       also write one row per run (runs.tsv) and per job (jobs.tsv).
#
# Only completed first attempts that succeeded or failed count; cancelled and
# rerun attempts are left out, and counted. A run that built nothing (a
# Markdown-only change) is counted apart from the lane medians. "Workflow
# completion" is the run's creation to its last update, so it includes
# queueing and the macOS job. The merge queue doesn't wait for macOS, so the
# "merge-queue gate" ends instead when the last required check (Linux,
# Workflows, and Content from its own workflow on the same commit) ends.
# Needs gh (signed in), jq and cargo (for the workspace's crate names).
set -euo pipefail

REPO=${MEASURE_CI_REPO:-Langspan/LangSpan.dev}
SINCE="" UNTIL="" LOGS=10 ROWS=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --since) SINCE=${2:?--since needs a time}; shift 2 ;;
    --until) UNTIL=${2:?--until needs a time}; shift 2 ;;
    --logs) LOGS=${2:?--logs needs a number}; shift 2 ;;
    --rows) ROWS=${2:?--rows needs a directory}; shift 2 ;;
    *) echo "measure-ci: unknown argument $1; see the usage at the top of $0" >&2; exit 2 ;;
  esac
done
case "$LOGS" in ''|*[!0-9]*) echo "measure-ci: --logs takes a whole number, not $LOGS" >&2; exit 2 ;; esac
for tool in gh jq cargo; do
  command -v "$tool" >/dev/null || { echo "measure-ci: $tool is not on PATH; install it and run again" >&2; exit 1; }
done

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

created="${SINCE:-*}..${UNTIL:-*}"
gh api --paginate "repos/$REPO/actions/workflows/check.yml/runs?per_page=100&created=$created" \
  --jq '.workflow_runs[]' | jq -s . > "$TMP/all.json"
jq '[.[] | select(.status == "completed" and .run_attempt == 1
                  and (.conclusion == "success" or .conclusion == "failure")
                  and (.event == "pull_request" or .event == "merge_group" or .event == "push"))]' \
  "$TMP/all.json" > "$TMP/runs.json"

# The Content workflow's merge-queue runs: Content is a required check that
# runs in its own workflow, matched to a Rust checks run by commit.
gh api --paginate "repos/$REPO/actions/workflows/content.yml/runs?per_page=100&created=$created" \
  --jq '.workflow_runs[]' | jq -s '[.[] | select(.status == "completed" and .run_attempt == 1
                                         and .event == "merge_group")]' > "$TMP/content.json"

# One row per job: the run's facts and the job's own times and steps.
: > "$TMP/jobs.jsonl"
for id in $(jq -r '.[].id' "$TMP/runs.json"); do
  gh api "repos/$REPO/actions/runs/$id/attempts/1/jobs?per_page=50" --jq '.jobs[]
    | select(.name == "Linux" or .name == "macOS" or .name == "Workflows")
    | {run: .run_id, job: .id, name, conclusion, started_at, completed_at,
       built: ([.steps[] | select((.name | startswith("Check formatting")) and (.conclusion == "success" or .conclusion == "failure"))] | length > 0),
       check_s: ([.steps[] | select(.name | startswith("Check formatting")) | select(.completed_at != null and .started_at != null)
                  | (.completed_at | fromdateiso8601) - (.started_at | fromdateiso8601)] | first)}' >> "$TMP/jobs.jsonl"
done

# The workspace's own crates; any other "Compiling" line is a dependency.
cargo metadata --no-deps --format-version 1 --locked | jq -r '.packages[].name' > "$TMP/workspace"

# Cargo colors its output. A gh that has --allow-escape-sequences rewrites
# escape characters without it, and the parser below needs them as sent; an
# older gh has no such flag and passes them through. The log is only parsed
# here, and the one line the report quotes has its control characters removed.
RAW=()
if gh api --help 2>&1 | grep -q -- '--allow-escape-sequences'; then RAW=(--allow-escape-sequences); fi

# Cache and compile facts from a job's log, within the build cache restore
# and the scripts/check.sh step only (installing tools compiles crates too).
log_facts() { # JOB_ID -> JSON
  if ! gh api ${RAW[@]+"${RAW[@]}"} "repos/$REPO/actions/jobs/$1/logs" > "$TMP/log" 2> "$TMP/log.err"; then
    echo "measure-ci: could not read the log of job $1 ($(head -c 200 "$TMP/log.err" | tr '\n' ' ')); its cache and compile facts are left out" >&2
    echo '{"log": false}'; return
  fi
  awk -v ws="$TMP/workspace" '
    BEGIN { while ((getline n < ws) > 0) own[n] = 1 }
    { sub(/^[0-9T:.\-]+Z /, ""); gsub(/\033\[[0-9;]*[A-Za-z]/, "") }
    /^##\[group\]Run / { step = $0 }
    step ~ /actions\/cache\/restore/ && /^  key: rust-v1-/ { key = $2 }
    step ~ /actions\/cache\/restore/ && /Cache restored from key: rust-v1-/ { got = $NF }
    step ~ /Run sh scripts\/check\.sh/ && /^ *Compiling [A-Za-z0-9_-]+ v/ {
      for (i = 1; i <= NF; i++) if ($i == "Compiling") { c = $(i + 1); break }
      if (c in own) w++; else { d++; if (first == "") first = $0 }
    }
    END {
      cache = got == "" ? "cold" : (got == key ? "exact" : "fallback")
      gsub(/\033\[[0-9;]*[A-Za-z]/, "", first); gsub(/[[:cntrl:]"\\`|]/, "", first)
      printf "{\"log\": true, \"cache\": \"%s\", \"deps_compiled\": %d, \"own_compiled\": %d, \"first_dep\": \"%s\"}\n", cache, d, w, first
    }' "$TMP/log"
}

: > "$TMP/facts.jsonl"
if [ "$LOGS" -gt 0 ]; then
  for event in pull_request merge_group; do
    for job in $(jq -rs --arg e "$event" --argjson n "$LOGS" --slurpfile runs "$TMP/runs.json" '
        ($runs[0] | map({key: (.id | tostring), value: .event}) | from_entries) as $ev
        | [.[] | select(.built and $ev[.run | tostring] == $e)] | sort_by(.run) | reverse
        | ([.[] | .run] | unique | sort | reverse | .[:$n]) as $keep
        | .[] | select(.run as $r | any($keep[]; . == $r)) | .job' "$TMP/jobs.jsonl"); do
      log_facts "$job" | jq -c --argjson j "$job" '. + {job: $j}' >> "$TMP/facts.jsonl"
    done
  done
fi

jq -n --slurpfile runs "$TMP/runs.json" --slurpfile all "$TMP/all.json" --slurpfile content "$TMP/content.json" \
  --slurpfile jobs <(jq -s . "$TMP/jobs.jsonl") --slurpfile facts <(jq -s . "$TMP/facts.jsonl") \
  --arg since "${SINCE:-the first run}" --arg until "${UNTIL:-now}" --arg repo "$REPO" -r '
  def t: fromdateiso8601;
  def median: sort | if length == 0 then null elif length % 2 == 1 then .[length / 2 | floor]
                     else (.[length / 2 - 1] + .[length / 2]) / 2 end;
  def p90: sort | if length == 0 then null else .[(length * 0.9 | ceil) - 1] end;
  def m: if . == null then "–" else (. / 60 * 10 | round / 10 | tostring) end;
  ($jobs[0]) as $J | ($facts[0] | map({key: (.job | tostring), value: .}) | from_entries) as $F
  | ($J | group_by(.run) | map({key: (.[0].run | tostring), value: (any(.[]; .built))}) | from_entries) as $built
  | ($content[0] | group_by(.head_sha) | map({key: .[0].head_sha, value: ([.[].updated_at | t] | min)}) | from_entries) as $C
  | ($J | group_by(.run) | map({key: (.[0].run | tostring),
       value: (. as $j | ["Linux", "Workflows"] | map(. as $n | [$j[] | select(.name == $n and .completed_at) | .completed_at | t] | first))})
     | from_entries) as $Req
  | ($runs[0] | map(. + {push_s: ((.updated_at | t) - (.created_at | t)),
                         built: ($built[.id | tostring] // false),
                         gate_s: ((.created_at | t) as $c
                                  | ($Req[.id | tostring] // [null]) + [$C[.head_sha]]
                                  | if any(.[]; . == null) then null else max - $c end)})) as $R
  | ($J | map(. + {job_s: (if .started_at and .completed_at then (.completed_at | t) - (.started_at | t) else null end),
                   event: (.run as $r | $R[] | select(.id == $r) | .event)}
              + ($F[.job | tostring] // {}))) as $JJ
  | "# Rust checks in \($repo), runs created from \($since) to \($until)",
    "",
    "Completed first attempts that passed or failed: \($R | length) of \($all[0] | length) runs (the rest were cancelled, reruns or other events).",
    "",
    "## Workflow completion (run created to run updated, queueing and macOS included), runs that built",
    "",
    "| Event | n | Median min | p90 min | Max min | Markdown-only runs left out |",
    "| --- | --- | --- | --- | --- | --- |",
    ( ["pull_request", "merge_group", "push"][] as $e
      | [$R[] | select(.event == $e and .built) | .push_s] as $s
      | "| \($e) | \($s | length) | \($s | median | m) | \($s | p90 | m) | \($s | max | m) | \([$R[] | select(.event == $e and (.built | not))] | length) |" ),
    "",
    "## Merge-queue gate (run created to the last required check: Linux, Workflows, Content), runs that built",
    "",
    "| Event | n | Median min | p90 min | Max min | Runs missing a required check result |",
    "| --- | --- | --- | --- | --- | --- |",
    ( [$R[] | select(.event == "merge_group" and .built)] as $q | [$q[] | .gate_s | select(. != null)] as $s
      | "| merge_group | \($s | length) | \($s | median | m) | \($s | p90 | m) | \($s | max | m) | \([$q[] | select(.gate_s == null)] | length) |" ),
    "",
    "## Job time (job started to job completed), jobs that built",
    "",
    "| Event | Job | Cache | n | Median job min | Median check.sh step min | Median dependency crates compiled |",
    "| --- | --- | --- | --- | --- | --- | --- |",
    ( $JJ | map(select(.built)) | group_by([.event, .name, (.cache // "no log read")])[]
      | "| \(.[0].event) | \(.[0].name) | \(.[0].cache // "no log read") | \(length) | \([.[].job_s | select(. != null)] | median | m) | \([.[].check_s | select(. != null)] | median | m) | \(if .[0].log then ([.[].deps_compiled] | median | tostring) else "–" end) |" ),
    "",
    "## Jobs whose log was read",
    "",
    "| Run | Event | Job | Cache | Dependency crates compiled | Workspace crates compiled | Job min | First dependency compiled |",
    "| --- | --- | --- | --- | --- | --- | --- | --- |",
    ( $JJ[] | select(.log) | "| \(.run) | \(.event) | \(.name) | \(.cache) | \(.deps_compiled) | \(.own_compiled) | \(.job_s | m) | \(if .first_dep == "" then "none" else "`\(.first_dep)`" end) |" )
' > "$TMP/report.md"

if [ -n "$ROWS" ]; then
  mkdir -p "$ROWS"
  jq -r '(["run", "event", "conclusion", "created_at", "run_started_at", "updated_at", "head_sha"] | @tsv),
         (.[] | [.id, .event, .conclusion, .created_at, .run_started_at, .updated_at, .head_sha] | @tsv)' \
    "$TMP/runs.json" > "$ROWS/runs.tsv"
  jq -r '(["run", "event", "conclusion", "created_at", "updated_at", "head_sha"] | @tsv),
         (.[] | [.id, .event, .conclusion, .created_at, .updated_at, .head_sha] | @tsv)' \
    "$TMP/content.json" > "$ROWS/content.tsv"
  jq -rs --slurpfile facts <(jq -s . "$TMP/facts.jsonl") '
    ($facts[0] | map({key: (.job | tostring), value: .}) | from_entries) as $F
    | (["run", "job", "name", "conclusion", "built", "started_at", "completed_at", "check_step_s", "cache", "deps_compiled", "own_compiled"] | @tsv),
      (.[] | ($F[.job | tostring] // {}) as $f
       | [.run, .job, .name, .conclusion, .built, .started_at, .completed_at, .check_s, $f.cache, $f.deps_compiled, $f.own_compiled] | @tsv)' \
    "$TMP/jobs.jsonl" > "$ROWS/jobs.tsv"
fi
cat "$TMP/report.md"
