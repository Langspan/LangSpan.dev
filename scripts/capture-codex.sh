#!/usr/bin/env bash
# Capture the hook events a real Codex sends to LangSpan at the end of a reply
# and on a first patch, for ADR 0027's evidence section
# (docs/adr/0027-turn-and-batch-events.md, issue A3).
#
# Usage: CODEX_HOME=DIR bash scripts/capture-codex.sh DIR
#
# The owner runs this. It never signs in, never reads a file under
# CODEX_HOME, and never passes a trust, approval or bypass option: it asks
# `codex login status` and stops, writing nothing, unless that reports a
# signed-in account. It then creates a throwaway repository under DIR,
# installs LangSpan's project-level Codex hooks there with
# LANGSPAN_CAPTURE_DIR set, and waits while the owner trusts the project and
# reviews the hooks in Codex's own `/hooks` screen. Then it runs:
#
#   turn 1  read notes.txt with a shell command, then an apply_patch whose
#           context does not apply (the first patch: a `snapshot`), and end
#           the reply;
#   peer    a Claude Code Write of notes.txt through `langspan hook
#           claude-code`, as session `capture-peer`;
#   turn 2  the same Codex session, resumed: patch one line of notes.txt
#           (refused once, naming the peer), reread and patch again;
#   turn 3  a new Codex session patches notes.txt while a CLI session holds it.
#
# Every Codex run uses `-s workspace-write` and the `exec` default approval
# policy, under which Codex never asks and a refused command fails. Raw events
# go to DIR/capture (0600), `langspan status` after each turn to DIR/*.json,
# and every coordinator event this run recorded, from its first, to
# DIR/events.txt. They contain local paths and model text: clean them before
# committing them under fixtures/codex-<version>/.
set -euo pipefail
# The hook writes raw events as they arrive: owner-only from the start.
umask 077

DIR=${1:?usage: CODEX_HOME=DIR bash scripts/capture-codex.sh DIR}
LANGSPAN=${LANGSPAN:-target/release/langspan}
[ -x "$LANGSPAN" ] || { echo "build first: cargo build --release --locked" >&2; exit 1; }
LANGSPAN=$(cd "$(dirname "$LANGSPAN")" && pwd)/$(basename "$LANGSPAN")
command -v codex >/dev/null || { echo "codex is not on PATH" >&2; exit 1; }
# A run of this script trusts a throwaway project in Codex's configuration:
# keep that out of the owner's own by naming a separate Codex home.
[ -n "${CODEX_HOME:-}" ] || {
  echo "set CODEX_HOME to a Codex home kept for captures, signed in there with \`codex login\`" >&2
  exit 1
}

# Signed in, or nothing is written. Only the status is read; it is not printed.
if ! status=$(codex login status 2>&1) || ! printf '%s\n' "$status" | grep -q '^Logged in'; then
  echo "codex is not signed in for CODEX_HOME. Run \`codex login\` yourself, then run this again." >&2
  exit 1
fi
"$LANGSPAN" status --json >/dev/null 2>&1 || { echo "start the coordinator first: $LANGSPAN daemon" >&2; exit 1; }

# Write every event after sequence $1 to $2, one `SEQ JSON` line each, a page
# at a time (a page holds at most 1,000 events and 48 KiB), and print the
# last sequence read, or $1 when there is none.
events_after() {
  local after=$1 page
  : > "$2"
  while :; do
    page=$("$LANGSPAN" events --after "$after" --limit 1000) || return 1
    [ -n "$page" ] || break
    printf '%s\n' "$page" >> "$2"
    after=${page##*$'\n'}
    after=${after%% *}
  done
  printf '%s\n' "$after"
}
# The coordinator outlives this run and holds older events: the capture saves
# only those after this sequence, read before the run writes anything.
START=$(events_after 0 /dev/null) || { echo "could not read the coordinator's events" >&2; exit 1; }

mkdir -p "$DIR"
DIR=$(cd "$DIR" && pwd -P)
# The physical path: Codex reports `/private/tmp`, not `/tmp`, on macOS.
REPO=$(cd "$(mktemp -d "$DIR/repo.XXXXXX")" && pwd -P)
CAPTURE="$DIR/capture"
# The path is written into a JSON string and a single-quoted shell word.
case "$CAPTURE" in
  *[\'\"\\\|\&]*) echo "choose a DIR without quotes, backslashes, | or &" >&2; exit 1 ;;
esac
# One run, one workspace and one set of sessions: a second run never reuses the first's.
RUN=$(basename "$REPO" | tr -c 'A-Za-z0-9\n' '-')
WORKSPACE="capture-$RUN"
mkdir -p "$CAPTURE"
chmod 700 "$CAPTURE"

git -C "$REPO" init --quiet -b main
printf 'alpha\nbeta\n' > "$REPO/notes.txt"
git -C "$REPO" add notes.txt
git -C "$REPO" -c user.name=capture -c user.email=capture@example.invalid commit --quiet -m init

"$LANGSPAN" hooks install codex --workspace "$WORKSPACE" --root "$REPO" --apply
# Set the capture directory in each hook command. The entries then no longer
# match LangSpan's exact shape, which is fine in a throwaway repository.
sed -i.bak "s|\"command\": \"'|\"command\": \"LANGSPAN_CAPTURE_DIR='$CAPTURE' '|" "$REPO/.codex/hooks.json"
rm "$REPO/.codex/hooks.json.bak"
grep -q LANGSPAN_CAPTURE_DIR "$REPO/.codex/hooks.json" || { echo "could not set LANGSPAN_CAPTURE_DIR in $REPO/.codex/hooks.json" >&2; exit 1; }

cat <<EOF
== trust the throwaway project and its hooks yourself
In another terminal, with the same CODEX_HOME, run:
  codex -s workspace-write -C '$REPO'
Trust the project when Codex asks, open /hooks, read each LangSpan hook's
command and trust it, then quit Codex. This script trusts nothing itself.
Press Enter here when that is done.
EOF
read -r _

snap() { "$LANGSPAN" "$1" --json "${@:3}" > "$DIR/$2.json" 2>&1 || echo "langspan $1 failed; see $DIR/$2.json"; }
turn() { codex exec "$@" || echo "the turn ended with an error; the events up to it are kept"; }

echo "== turn 1: read notes.txt, then a first patch that does not apply"
turn -s workspace-write -C "$REPO" "Run the shell command \`cat notes.txt\`. Then call apply_patch exactly once with this patch, which is expected to fail; do not retry it or change it:
*** Begin Patch
*** Update File: notes.txt
@@
-no such line
+omega
*** End Patch
Reply with one word: done."
snap status status-after-turn-1

echo "== peer: capture-peer (claude-code) writes notes.txt"
peer() {
  printf '{"session_id":"capture-peer-%s","cwd":"%s","hook_event_name":"%s","tool_name":"Write","tool_use_id":"capture-peer-write","tool_input":{"file_path":"%s/notes.txt","content":"alpha\\nbeta\\npeer\\n"}%s}' \
    "$RUN" "$REPO" "$1" "$REPO" "$2" |
    "$LANGSPAN" hook claude-code --workspace "$WORKSPACE" --root "$REPO" > "$DIR/peer-$1.json"
}
peer PreToolUse ""
printf 'alpha\nbeta\npeer\n' > "$REPO/notes.txt"
peer PostToolUse ',"tool_response":{"success":true}'

echo "== turn 2: the same session patches the file the peer changed"
turn -c sandbox_mode=workspace-write -C "$REPO" resume --last "Use apply_patch to change the line 'beta' in notes.txt to 'gamma'. If LangSpan refuses it, do what its message says and try once more. Reply with one word: done."
snap status status-after-turn-2

echo "== turn 3: a new session patches notes.txt while another session holds it"
"$LANGSPAN" session start "$WORKSPACE.holder" "$WORKSPACE" --request-id "$WORKSPACE-holder" >/dev/null
"$LANGSPAN" admit "$WORKSPACE.holder" notes.txt --ttl-ms 60000 --request-id "$WORKSPACE-hold" >/dev/null
turn -s workspace-write -C "$REPO" "Use apply_patch to change the line 'alpha' in notes.txt to 'delta'. If it is refused, do not retry. Reply with one word: done."
snap status status-after-turn-3
events_after "$START" "$DIR/events.txt" >/dev/null || echo "langspan events failed; $DIR/events.txt holds the pages read before it"

chmod 600 "$CAPTURE"/* "$DIR"/*.json "$DIR/events.txt" 2>/dev/null || true
echo "== $(find "$CAPTURE" -type f | wc -l | tr -d ' ') event(s) in $CAPTURE"
echo "Clean them (home paths, transcript_path, model text beyond the prompts) before committing them under fixtures/codex-<version>/."
