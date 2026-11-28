#!/usr/bin/env bash
# The coordinator behind the Live view's screenshots (issue #491): a real
# daemon, three Claude Code sessions in one checkout driven through the real
# `langspan hook` executable, a write held for a stale read, the re-read and
# the save, then `langspan dashboard` against that daemon. What is
# constructed: the hook events (no Claude Code process runs). It prints the
# dashboard URL and keeps running until interrupted.
#
#   bash docs/evidence/dashboard-live/scenario.sh [LANGSPAN_BINARY]
set -euo pipefail
LANGSPAN=${1:-target/debug/langspan}
BASE=$(mktemp -d "${TMPDIR:-/tmp}/ls-live-XXXXXX")
PIDS=()
cleanup() { for pid in "${PIDS[@]:-}"; do kill "$pid" 2>/dev/null || true; done; rm -rf "$BASE"; }
trap cleanup EXIT
export LANGSPAN_AUTOSTART=off
STATE="$BASE/state" ROOT="$BASE/billing"
mkdir -p "$STATE" "$ROOT/src" "$ROOT/tests" "$ROOT/docs"; chmod 700 "$STATE"
g() { git -C "$ROOT" -c user.name=Demo -c user.email=demo@example.invalid "$@"; }
for f in src/invoice.rs src/totals.rs src/refund.rs tests/totals.rs docs/billing.md; do printf 'start\n' > "$ROOT/$f"; done
g init -q --initial-branch=feat/billing && g add . && g commit -q -m init
"$LANGSPAN" daemon --state-dir "$STATE" 2>>"$BASE/daemon.log" & PIDS+=("$!")
for _ in $(seq 100); do "$LANGSPAN" status --state-dir "$STATE" >/dev/null 2>&1 && break; sleep 0.1; done

hook() { printf '%s' "$1" | "$LANGSPAN" hook claude-code --state-dir "$STATE" --workspace billing --root "$ROOT" 2>&1 >/dev/null | sed -n 's/.*"coverage":"\([a-z_]*\)".*/\1/p' | tail -1; }
event() { # SESSION EVENT TOOL CALL FILE
  printf '{"session_id":"%s","cwd":"%s","hook_event_name":"%s","tool_name":"%s","tool_use_id":"%s","tool_input":{"file_path":"%s/%s","content":"x"}}' "$1" "$ROOT" "$2" "$3" "$4" "$ROOT" "$5"
}
prompt() { hook "$(printf '{"session_id":"%s","cwd":"%s","hook_event_name":"UserPromptSubmit","prompt":"%s"}' "$1" "$ROOT" "$2")" >/dev/null; }
n=0
read_file() { n=$((n + 1)); hook "$(event "$1" PostToolUse Read "r$n" "$2")" >/dev/null; sleep 0.4; }
edit() { # SESSION FILE -> admitted, or the refusal
  n=$((n + 1)); local r; r=$(hook "$(event "$1" PreToolUse Write "w$n" "$2")")
  if [ "$r" = admitted ]; then printf '%s %s\n' "$1" "$n" >> "$ROOT/$2"; hook "$(event "$1" PostToolUse Write "w$n" "$2")" >/dev/null; fi
  echo "$1 edit $2: $r" >&2; sleep 0.4
}
command() { # SESSION COMMAND
  n=$((n + 1)); local e; e=$(printf '{"session_id":"%s","cwd":"%s","hook_event_name":"%%s","tool_name":"Bash","tool_use_id":"b%s","tool_input":{"command":"%s"}}' "$1" "$ROOT" "$n" "$2")
  hook "$(printf "$e" PreToolUse)" >/dev/null; hook "$(printf "$e" PostToolUse)" >/dev/null; sleep 0.4
}

A=11111111-0000-4000-8000-00000000000a B=22222222-0000-4000-8000-00000000000b C=33333333-0000-4000-8000-00000000000c
prompt "$A" "#12 invoice totals"; prompt "$B" "#14 refund flow"; prompt "$C" "#15 billing docs"
read_file "$A" src/invoice.rs; read_file "$A" src/totals.rs
read_file "$B" src/refund.rs; read_file "$B" src/invoice.rs
read_file "$C" docs/billing.md
edit "$B" src/invoice.rs
edit "$C" docs/billing.md
edit "$A" src/invoice.rs   # held: changed since A read it
read_file "$A" src/invoice.rs
edit "$A" src/invoice.rs   # saved after re-reading
edit "$B" src/refund.rs
command "$C" "cargo test"
edit "$A" tests/totals.rs
read_file "$B" src/totals.rs
edit "$C" docs/billing.md

"$LANGSPAN" dashboard --listen 127.0.0.1:0 --state-dir "$STATE" & PIDS+=("$!")
wait
