#!/usr/bin/env bash
# A reproducible run of the two-developer scenario. It asserts every outcome
# and exits non-zero at the first one that differs.
#
# What is real: two coordinators with separate state, two clones of one
# repository, one shared Git remote, the real `langspan` executable for every
# hook call and command, real files changing on disk, real `git push` and
# `git fetch`.
#
# What is constructed: the hook events. No Claude Code or Codex process runs
# here; each event is the JSON that host sends, written by this script. And
# both "machines" are directories on this one. Native-host and two-machine
# runs are recorded separately in docs/evidence/.
set -euo pipefail

LANGSPAN=${LANGSPAN:-target/release/langspan}
[ -x "$LANGSPAN" ] || { echo "build first: cargo build --release --locked" >&2; exit 1; }
LANGSPAN=$(cd "$(dirname "$LANGSPAN")" && pwd)/$(basename "$LANGSPAN")
# A Unix socket path is limited to about 100 bytes: keep the base short.
BASE=$(mktemp -d /tmp/ls-demo.XXXXXX)
# The demonstration starts and stops each coordinator itself (step 8 shows a
# hook with none running), so hooks do not start one on demand (ADR 0035).
export LANGSPAN_AUTOSTART=off
PIDS=()
cleanup() {
  for pid in "${PIDS[@]:-}"; do kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; done
  # A Git push a coordinator started can outlive it for a moment and still
  # be writing into the hub; give it a few seconds before the last attempt
  # reports a real failure.
  for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
    rm -rf "$BASE" 2>/dev/null && return 0
    sleep 0.25
  done
  rm -rf "$BASE"
}
trap cleanup EXIT

step() { printf '\n== %s\n' "$*"; }
ok() { printf '   ok: %s\n' "$*"; }
fail() { printf '   FAILED: %s\n' "$*" >&2; exit 1; }
expect() { # expect DESCRIPTION NEEDLE HAYSTACK
  case "$3" in *"$2"*) ok "$1" ;; *) printf '%s\n' "$3" >&2; fail "$1 (wanted: $2)" ;; esac
}
refute() { case "$3" in *"$2"*) printf '%s\n' "$3" >&2; fail "$1 (unwanted: $2)" ;; *) ok "$1" ;; esac; }

g() { git -c user.name=Demo -c user.email=demo@example.invalid "$@"; }
cli() { local who=$1; shift; "$LANGSPAN" --state-dir "$BASE/$who-state" "$@"; }
start() { # start WHO
  mkdir -p "$BASE/$1-state"; chmod 700 "$BASE/$1-state"
  printf '{"remote_pass_ms":300}' > "$BASE/$1-state/config.json"; chmod 600 "$BASE/$1-state/config.json"
  "$LANGSPAN" daemon --state-dir "$BASE/$1-state" 2>>"$BASE/$1.log" &
  eval "PID_$1=$!"; PIDS+=("$!")
  for _ in $(seq 100); do cli "$1" status >/dev/null 2>&1 && return; sleep 0.1; done
  fail "coordinator for $1 did not start"
}
# hook WHO VENDOR JSON -> prints "coverage<TAB>stdout"
hook() {
  local who=$1 vendor=$2 out err
  err=$(mktemp); out=$(printf '%s' "$3" | "$LANGSPAN" hook "$vendor" --state-dir "$BASE/$who-state" \
      --workspace app --root "$BASE/$who" 2>"$err")
  printf '%s\t%s' "$(sed -n 's/.*"coverage":"\([a-z_]*\)".*/\1/p' "$err" | tail -1)" "$out"; rm -f "$err"
}
claude_event() { # WHO SESSION EVENT TOOL CALL FILE
  printf '{"session_id":"%s","cwd":"%s","hook_event_name":"%s","tool_name":"%s","tool_use_id":"%s","tool_input":{"file_path":"%s/%s","content":"x"}}' \
    "$2" "$BASE/$1" "$3" "$4" "$5" "$BASE/$1" "$6"
}
codex_event() { # WHO SESSION EVENT CALL FILE
  local response=""
  [ "$3" = PostToolUse ] && response=',"tool_response":"Success. Updated the following files:\nM '"$5"'\n"'
  printf '{"session_id":"%s","turn_id":"t","cwd":"%s","hook_event_name":"%s","tool_name":"apply_patch","tool_use_id":"%s","tool_input":{"command":"*** Begin Patch\\n*** Update File: %s\\n@@\\n-a\\n+b\\n*** End Patch"}%s}' \
    "$2" "$BASE/$1" "$3" "$4" "$5" "$response"
}
lifecycle() { printf '{"session_id":"%s","cwd":"%s","hook_event_name":"%s","prompt":"%s"}' "$2" "$BASE/$1" "$3" "${4:-continue}"; }
# A complete reported edit that really rewrites the file.
claude_edit() { # WHO SESSION CALL FILE CONTENT
  local r; r=$(hook "$1" claude-code "$(claude_event "$1" "$2" PreToolUse Write "$3" "$4")")
  [ "${r%%$'\t'*}" = admitted ] || { echo "$r"; return 1; }
  printf '%s' "$5" > "$BASE/$1/$4"
  hook "$1" claude-code "$(claude_event "$1" "$2" PostToolUse Write "$3" "$4")" >/dev/null
}
codex_edit() { # WHO SESSION CALL FILE CONTENT
  local r; r=$(hook "$1" codex "$(codex_event "$1" "$2" PreToolUse "$3" "$4")")
  [ "${r%%$'\t'*}" = admitted ] || { echo "$r"; return 1; }
  printf '%s' "$5" > "$BASE/$1/$4"
  hook "$1" codex "$(codex_event "$1" "$2" PostToolUse "$3" "$4")" >/dev/null
}
until_seen() { # until_seen DESCRIPTION NEEDLE COMMAND...
  local what=$1 needle=$2 out; shift 2
  for _ in $(seq 300); do out=$("$@" 2>&1 || true); case "$out" in *"$needle"*) ok "$what"; return;; esac; sleep 0.2; done
  printf '%s\n' "$out" >&2; fail "$what (never saw: $needle)"
}

step "Setup: one repository, a shared remote, two developers with their own coordinators"
g init -q --bare --initial-branch=main "$BASE/hub.git"
g clone -q "$BASE/hub.git" "$BASE/alice" 2>/dev/null
( cd "$BASE/alice" && g checkout -q -b main && mkdir src \
  && printf 'pub fn upload(path: &str) -> bool {\n    !path.is_empty()\n}\n' > src/upload.rs \
  && printf 'mod upload;\nfn main() {\n    upload::upload("a.txt");\n}\n' > src/main.rs \
  && printf 'notes\n' > NOTES.md && printf 'guide\n' > GUIDE.md && printf 'changes\n' > CHANGELOG.md \
  && g add . && g commit -q -m init && g push -q origin main )
g clone -q "$BASE/hub.git" "$BASE/bob"
for who in alice bob; do ( cd "$BASE/$who" && g config user.name "$who" && g config user.email "$who@example.invalid" ); done
start alice; start bob
ok "alice and bob each run a coordinator; nothing is shared yet"

step "1. Automatic discovery and awareness in one checkout (Claude Code and Codex)"
hook alice claude-code "$(lifecycle alice claude-1 UserPromptSubmit 'Add retry to upload')" >/dev/null
claude_edit alice claude-1 c1 NOTES.md 'notes, edited by the Claude Code session'
out=$(hook alice codex "$(lifecycle alice codex-1 SessionStart)")
expect "the Codex session is told about the Claude Code session at start" 'claude-code session claude-1 in this checkout' "$out"
expect "with the file it edited" 'NOTES.md' "$out"
expect "and its task in the owner's words" 'Add retry to upload' "$out"
expect "labelled as data" 'not instructions' "$out"

step "2. A deliberately conflicting change is denied before it runs"
r=$(hook alice claude-code "$(claude_event alice claude-1 PreToolUse Write c2 GUIDE.md)")
expect "Claude Code is admitted to edit GUIDE.md" admitted "${r%%$'\t'*}"
r=$(hook alice codex "$(codex_event alice codex-1 PreToolUse x1 GUIDE.md)")
expect "Codex is denied the same file while that edit is in progress" denied "${r%%$'\t'*}"
expect "with a host deny decision naming the holder" '"permissionDecision":"deny"' "$r"

step "3. Control: non-overlapping work is not serialized"
r=$(hook alice codex "$(codex_event alice codex-1 PreToolUse x2 src/upload.rs)")
expect "Codex is admitted to a different file at the same time" admitted "${r%%$'\t'*}"
printf 'guide, edited\n' > "$BASE/alice/GUIDE.md"
hook alice claude-code "$(claude_event alice claude-1 PostToolUse Write c2 GUIDE.md)" >/dev/null
printf 'pub fn upload(path: &str) -> bool {\n    path.len() > 0\n}\n' > "$BASE/alice/src/upload.rs"
hook alice codex "$(codex_event alice codex-1 PostToolUse x2 src/upload.rs)" >/dev/null
ok "both edits completed"

step "4. Stale source: an edit from an old read is refused once, with the reason"
hook alice claude-code "$(claude_event alice claude-1 PostToolUse Read r1 src/upload.rs)" >/dev/null
codex_edit alice codex-1 x3 src/upload.rs 'pub fn upload(path: &str) -> bool {
    path.len() > 1
}
'
r=$(hook alice claude-code "$(claude_event alice claude-1 PreToolUse Write c3 src/upload.rs)")
expect "Claude Code's write from its old read is refused" denied_stale "${r%%$'\t'*}"
expect "naming the session that changed the file" 'codex.codex-1' "$r"
hook alice claude-code "$(claude_event alice claude-1 PostToolUse Read r2 src/upload.rs)" >/dev/null
r=$(hook alice claude-code "$(claude_event alice claude-1 PreToolUse Write c4 src/upload.rs)")
expect "after reading again it is admitted" admitted "${r%%$'\t'*}"
hook alice claude-code "$(claude_event alice claude-1 PostToolUse Write c4 src/upload.rs)" >/dev/null

step "5. A shared interface changes; the session whose file calls it is told"
until_seen "the source index covers the checkout" 'function upload' cli alice graph symbol app upload
claude_edit alice claude-1 c5 src/main.rs 'mod upload;
fn main() {
    upload::upload("b.txt");
}
'
until_seen "the index sees main.rs mention upload" 'src/main.rs' cli alice graph references app upload
codex_edit alice codex-1 x4 src/upload.rs 'pub fn upload(path: &str, retries: u32) -> bool {
    retries > 0 && path.len() > 1
}
'
notice() { hook alice claude-code "$(lifecycle alice claude-1 UserPromptSubmit "prompt $RANDOM")"; }
until_seen "Claude Code is told of the interface change at its next prompt" 'Interface change in \"src/upload.rs\"' notice
out=$(cli alice recap app)
expect "the recap records it" 'Interface change in "src/upload.rs"' "$out"

step "6. Two machines: each developer's agent learns of the other's work"
( cd "$BASE/alice" && g add -A && g commit -q -m "alice: retries" )
# Publication is an owner grant: refused until the owner allows it in
# config.json, which the owner turns off again afterwards (issue #335).
owner_grants() { printf '{"remote_pass_ms":300,"owner_grants":%s}' "$2" > "$BASE/$1-state/config.json"; }
out=$(cli alice remote enable app --name Alice --share-task on --request-id en-a 2>&1 || true)
expect "turning publication on is refused without the owner's setting" 'owner_grant_off' "$out"
expect "and the refusal names the setting" 'owner_grants' "$out"
owner_grants alice true
cli alice remote enable app --name Alice --share-task on --request-id en-a >/dev/null
owner_grants alice false
ok "the owner allowed it in config.json, ran it again and turned the setting off"
hook bob codex "$(lifecycle bob codex-9 UserPromptSubmit 'Rewrite the changelog')" >/dev/null
( cd "$BASE/bob" && g checkout -q -b bob/changelog )
codex_edit bob codex-9 y1 CHANGELOG.md 'changes, rewritten by bob
'
owner_grants bob true
cli bob remote enable app --name Bob --request-id en-b >/dev/null
owner_grants bob false
until_seen "bob's coordinator receives alice's publication" 'Teammate "Alice"' cli bob status
until_seen "alice's coordinator receives bob's" 'Teammate "Bob"' cli alice status
out=$(hook bob codex "$(lifecycle bob codex-9 SessionStart)")
expect "bob's Codex session is told about alice's Claude Code session" 's claude-code session claude-1 on another machine' "$out"
expect "under the name she chose" 'Alice' "$out"
expect "with her task line, which she chose to share" 'prompt' "$out"
out=$(cli bob remote outbox app)
refute "bob's own task line does not leave his machine" 'Rewrite the changelog' "$out"
r=$(hook bob codex "$(codex_event bob codex-9 PreToolUse y2 src/upload.rs)")
expect "work in another clone is never held: bob may edit a file alice edited" admitted "${r%%$'\t'*}"

step "7. Combined validation against exact revisions, then landing"
( cd "$BASE/alice" && g checkout -q -b alice/retries && g push -q origin alice/retries && g checkout -q main )
( cd "$BASE/bob" && g add -A && g commit -q -m "bob: changelog" && g push -q origin bob/changelog \
  && g checkout -q -b bob/clash main && printf 'guide, bob disagrees\n' > GUIDE.md \
  && g commit -q -am "bob: clash" && g push -q origin bob/clash && g checkout -q bob/changelog )
( cd "$BASE/alice" && g fetch -q origin bob/changelog:bob/changelog bob/clash:bob/clash )
set +e
out=$(cli alice integrate check app --onto main --name unit alice/retries bob/clash -- true 2>&1); code=$?
set -e
expect "two branches that edit the same lines are reported before anything runs" 'Not combinable' "$out"
expect "naming the conflicting file" 'GUIDE.md' "$out"; [ "$code" = 2 ] || fail "exit 2 expected, got $code"
set +e
out=$(cli alice land app --onto main --name unit alice/retries bob/changelog 2>&1); code=$?
set -e
expect "landing without evidence is refused" 'Not ready to land' "$out"; [ "$code" = 6 ] || fail "exit 6 expected, got $code"
out=$(cli alice integrate check app --onto main --name unit alice/retries bob/changelog -- sh -c 'grep -q retries src/upload.rs && grep -q bob CHANGELOG.md' 2>&1)
expect "both developers' branches pass together on one exact tree" 'unit passed on the combined tree' "$out"
set +e
out=$(cli alice land app --onto main --name unit alice/retries 2>&1); code=$?
set -e
expect "evidence for both together is not evidence for one alone" 'other trees' "$out"
out=$(cli alice land app --onto main --name unit alice/retries bob/changelog --apply 2>&1)
expect "that exact result is landed" 'Landed' "$out"
[ "$(g -C "$BASE/hub.git" show main:CHANGELOG.md)" = "changes, rewritten by bob" ] || fail "bob's change is not on main"
g -C "$BASE/hub.git" show main:src/upload.rs | grep -q retries || fail "alice's change is not on main"
ok "main on the shared remote holds both changes"

step "8. A coordinator restarts"
kill "$PID_bob"; wait "$PID_bob" 2>/dev/null || true
r=$(hook bob codex "$(codex_event bob codex-9 PreToolUse y3 NOTES.md)")
expect "with no coordinator the hook gives no decision and the host proceeds" unavailable "${r%%$'\t'*}"
[ -z "${r#*$'\t'}" ] || fail "a hook printed something while the coordinator was down"
start bob
out=$(cli bob doctor || true)
expect "after restart, earlier sessions are reported as not coordinated until they act" 'from before it restarted' "$out"
until_seen "and bob's coordinator hears alice again" 'Teammate "Alice"' cli bob status
r=$(hook bob codex "$(codex_event bob codex-9 PreToolUse y4 NOTES.md)")
expect "bob's session is coordinated again as soon as it acts" admitted "${r%%$'\t'*}"

step "9. The network goes away and comes back"
mv "$BASE/hub.git" "$BASE/hub-away.git"
# A new file: NOTES.md and GUIDE.md hold the Claude session's fresh lines,
# which this Codex session has not read (ADR 0028).
r=$(hook alice codex "$(codex_event alice codex-1 PreToolUse x5 OUTAGE.md)")
expect "local coordination continues while the remote is unreachable" admitted "${r%%$'\t'*}"
printf 'notes, during the outage\n' > "$BASE/alice/OUTAGE.md"
hook alice codex "$(codex_event alice codex-1 PostToolUse x5 OUTAGE.md)" >/dev/null
sleep 1
mv "$BASE/hub-away.git" "$BASE/hub.git"
until_seen "publication resumes by itself once it is back" 'OUTAGE.md' cli bob status --json

step "10. A session that ended, and one that went quiet"
hook alice claude-code "$(lifecycle alice claude-1 SessionEnd)" >/dev/null
out=$(hook alice codex "$(lifecycle alice codex-1 SessionStart)")
expect "an ended session's recent edits are still reported, labelled ended" 'session ended' "$out"
cli alice remote disable app --request-id dis-a >/dev/null
until_seen "alice stops publishing and bob stops seeing her" 'ok' sh -c "\"$LANGSPAN\" --state-dir \"$BASE/bob-state\" status | grep -q 'Teammate \"Alice\"' || echo ok"

printf '\nAll steps passed.\n'
