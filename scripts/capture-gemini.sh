#!/usr/bin/env bash
# Capture the hook events a real Gemini CLI session sends to LangSpan, for
# issue A6 (#14) and ADR 0033's evidence section
# (docs/adr/0033-gemini-cli-adapter.md). The events replace the hand-written
# files in fixtures/gemini-cli-contract/ as fixtures/gemini-cli-<version>/.
#
# Usage: GEMINI_CLI_HOME=DIR bash scripts/capture-gemini.sh OUT
#
# A person runs this, at a terminal. It never signs in, never reads a file
# under GEMINI_CLI_HOME, and never passes an approval, policy, allowed-tools
# or trust option. Gemini CLI has no sign-in status command, so it asks
# `gemini -p` for one word, in the default approval mode, from
# GEMINI_CLI_HOME, and stops, writing nothing, unless that answers. It then
# creates a throwaway repository under OUT, installs LangSpan's project-level
# Gemini CLI hooks there with LANGSPAN_CAPTURE_DIR set, and starts Gemini CLI
# interactively in that repository with `--approval-mode default`. The person
# trusts the folder and the new hooks, types the prompts this script prints,
# and approves each edit by hand with "Allow once". Between prompts they run
# OUT/step.sh in a second terminal. A relative GEMINI_CLI_HOME,
# LANGSPAN_STATE_DIR or XDG_STATE_HOME is made absolute first, and the steps
# carry the state directory, so they reach the same coordinator from anywhere:
#
#   prompt 1  a question with no tool: the BeforeAgent briefing names the
#             peer's task line;
#   hold      a CLI session holds notes.txt for 60 s;
#   prompt 2  replace on the held file: a live denial, whose reason the model
#             reports;
#   prompt 3  read_file notes.txt;
#   peer      a Claude Code peer writes notes.txt;
#   prompt 4  replace from the old read: refused once as stale, then the same
#             call again, approved by hand, goes through and its AfterTool
#             completes the admission;
#   /quit     SessionEnd, if Gemini CLI sends it.
#
# Raw events go to OUT/capture (0600), `langspan status` after each step to
# OUT/status-*.json, and at the end every coordinator event recorded since
# the script started, page after page, to OUT/events.txt (one line each: its
# sequence, then the event as JSON). They contain local paths and model text:
# clean them before committing them under fixtures/gemini-cli-<version>/.
set -euo pipefail
# The hook writes raw events as they arrive: owner-only from the start.
umask 077

DIR=${1:?usage: GEMINI_CLI_HOME=DIR bash scripts/capture-gemini.sh OUT}
LANGSPAN=${LANGSPAN:-target/release/langspan}
[ -x "$LANGSPAN" ] || { echo "build first: cargo build --release --locked" >&2; exit 1; }
LANGSPAN=$(cd "$(dirname "$LANGSPAN")" && pwd)/$(basename "$LANGSPAN")
command -v gemini >/dev/null || { echo "gemini is not on PATH" >&2; exit 1; }
# Gemini CLI asks to trust the throwaway folder and records that in its home:
# keep it out of the person's own by naming a separate Gemini CLI home.
if [ -z "${GEMINI_CLI_HOME:-}" ] || [ ! -d "$GEMINI_CLI_HOME" ]; then
  echo "set GEMINI_CLI_HOME to an existing directory kept for captures, and sign in there by running \`gemini\` yourself" >&2
  exit 1
fi
# Gemini CLI, its hooks in the throwaway repository and the steps run from
# other directories: a relative home or state directory would name another
# one there. Make each absolute, from this directory, before anything moves.
absolute() { case "$1" in /*) printf '%s' "$1" ;; *) printf '%s/%s' "$PWD" "$1" ;; esac; }
for name in GEMINI_CLI_HOME LANGSPAN_STATE_DIR XDG_STATE_HOME; do
  if [ -n "${!name:-}" ]; then
    export "$name=$(absolute "${!name}")"
  fi
done

# Signed in, or nothing is written. Gemini CLI has no status command: a
# one-word prompt, in the default approval mode, in which no tool runs
# without a person, stands in for one. Its answer is checked, not printed.
if ! answer=$(cd "$GEMINI_CLI_HOME" && gemini --approval-mode default -p 'Reply with the single word: ready' 2>&1 </dev/null) ||
  ! printf '%s\n' "$answer" | grep -qi 'ready'; then
  echo "gemini is not signed in for GEMINI_CLI_HOME: a one-word prompt did not answer. Run \`gemini\` there yourself to sign in or see why, then run this again." >&2
  exit 1
fi
"$LANGSPAN" status --json >/dev/null 2>&1 || { echo "start the coordinator first: $LANGSPAN daemon" >&2; exit 1; }

# `langspan events` reads oldest first, a bounded page at a time: one line
# per event, its sequence and then the event as JSON. Print the pages after
# sequence $1 up to the last event, and that last sequence on fd 3.
pages() {
  local after=$1 page
  while :; do
    page=$("$LANGSPAN" events --after "$after" --limit 1000) || { echo "langspan events --after $after failed: $page" >&2; return 1; }
    [ -n "$page" ] || break
    printf '%s\n' "$page"
    after=$(printf '%s\n' "$page" | tail -n 1)
    after=${after%% *}
    case "$after" in
      '' | *[!0-9]*) echo "langspan events printed a line without a sequence" >&2; return 1 ;;
    esac
  done
  printf '%s' "$after" >&3
}
# Every event this capture records comes after this one.
START=$(pages 0 3>&1 >/dev/null) || exit 1

mkdir -p "$DIR"
DIR=$(cd "$DIR" && pwd -P)
# The physical path: hosts report `/private/tmp`, not `/tmp`, on macOS.
REPO=$(cd "$(mktemp -d "$DIR/repo.XXXXXX")" && pwd -P)
CAPTURE="$DIR/capture"
# The path is written into a JSON string and a single-quoted shell word.
case "$CAPTURE" in
  *[\'\"\\\|\&]*) echo "choose an OUT without quotes, backslashes, | or &" >&2; exit 1 ;;
esac
# The steps run in a second terminal, without this environment: they name
# the same state directory, so they reach the coordinator checked above.
STATE_ENV=
for name in LANGSPAN_STATE_DIR XDG_STATE_HOME; do
  if [ -n "${!name:-}" ]; then
    case "${!name}" in
      *[\'\\]*) echo "choose a $name without quotes or backslashes" >&2; exit 1 ;;
    esac
    STATE_ENV="${STATE_ENV}export $name='${!name}'
"
  fi
done
# One run, one workspace and one set of sessions: a second run never reuses the first's.
RUN=$(basename "$REPO" | tr -c 'A-Za-z0-9\n' '-')
WORKSPACE="capture-$RUN"
mkdir -p "$CAPTURE"
chmod 700 "$CAPTURE"

git -C "$REPO" init --quiet -b main
printf 'alpha\nbeta\n' > "$REPO/notes.txt"
git -C "$REPO" add notes.txt
git -C "$REPO" -c user.name=capture -c user.email=capture@example.invalid commit --quiet -m init

"$LANGSPAN" hooks install gemini-cli --workspace "$WORKSPACE" --root "$REPO" --apply
# Set the capture directory in each hook command. The entries then no longer
# match LangSpan's exact shape, which is fine in a throwaway repository.
sed -i.bak "s|\"command\": \"'|\"command\": \"LANGSPAN_CAPTURE_DIR='$CAPTURE' '|" "$REPO/.gemini/settings.json"
rm "$REPO/.gemini/settings.json.bak"
grep -q LANGSPAN_CAPTURE_DIR "$REPO/.gemini/settings.json" || { echo "could not set LANGSPAN_CAPTURE_DIR in $REPO/.gemini/settings.json" >&2; exit 1; }
# The coordinator states a refusal on stdout: show it, do not drop it.
added=$("$LANGSPAN" workspace add "$WORKSPACE" "$REPO" --request-id "$WORKSPACE-workspace") ||
  { echo "langspan workspace add failed: $added" >&2; exit 1; }

# The peer: a Claude Code session whose task line the briefing should carry.
cat > "$DIR/peer.sh" <<EOF
#!/usr/bin/env bash
# One Claude Code hook event from the peer session capture-peer-$RUN.
set -euo pipefail
umask 077
$STATE_ENV
printf '{"session_id":"capture-peer-$RUN","cwd":"$REPO","hook_event_name":"%s"%s}' "\$1" "\$2" |
  '$LANGSPAN' hook claude-code --workspace '$WORKSPACE' --root '$REPO'
EOF
WRITE=',"tool_name":"Write","tool_use_id":"capture-peer-write","tool_input":{"file_path":"'"$REPO"'/notes.txt","content":"alpha\nbeta\npeer\n"}'
bash "$DIR/peer.sh" UserPromptSubmit ',"prompt":"capture-peer task: append a peer line to notes.txt"' > "$DIR/peer-prompt.json"

# The steps the person runs in a second terminal, between prompts.
cat > "$DIR/step.sh" <<EOF
#!/usr/bin/env bash
# Usage: bash '$DIR/step.sh' hold|peer|snap NAME|events
set -euo pipefail
umask 077
$STATE_ENV
LANGSPAN='$LANGSPAN'
$(declare -f pages)
snap() { '$LANGSPAN' status --json > '$DIR/'"status-\$1.json" 2>&1 || echo "langspan status failed; see $DIR/status-\$1.json"; }
case "\${1:-}" in
  hold)
    started=\$('$LANGSPAN' session start '$WORKSPACE.holder' '$WORKSPACE' --request-id '$WORKSPACE-holder') ||
      { echo "langspan session start failed: \$started" >&2; exit 1; }
    # A new request each time: a second hold is a new admission, not a replay.
    '$LANGSPAN' admit '$WORKSPACE.holder' notes.txt --ttl-ms 60000 --request-id "$WORKSPACE-hold-\$(date +%s)-\$\$" > '$DIR/hold.txt' ||
      { echo "langspan admit failed: \$(cat '$DIR/hold.txt')" >&2; exit 1; }
    snap after-hold
    echo "notes.txt is held for 60 s from now. Send prompt 2 in Gemini CLI at once."
    sleep 62
    echo "The hold has lapsed. Go on to prompt 3." ;;
  peer)
    bash '$DIR/peer.sh' PreToolUse '$WRITE' > '$DIR/peer-PreToolUse.json'
    printf 'alpha\nbeta\npeer\n' > '$REPO/notes.txt'
    bash '$DIR/peer.sh' PostToolUse '$WRITE,"tool_response":{"success":true}' > '$DIR/peer-PostToolUse.json'
    snap after-peer
    echo "The peer wrote notes.txt. Go on to prompt 4." ;;
  snap) snap "\${2:?usage: step.sh snap NAME}" ;;
  events)
    # This capture's events, every page of them: from the sequence recorded
    # before setup, not the coordinator's oldest.
    pages '$START' >'$DIR/events.txt' 3>/dev/null
    echo "Saved the events after sequence $START to $DIR/events.txt." ;;
  *) echo "usage: bash '$DIR/step.sh' hold|peer|snap NAME|events" >&2; exit 2 ;;
esac
EOF

cat <<EOF

== Gemini CLI starts next, in $REPO, in its default approval mode.
Keep a second terminal open for the steps. Approve nothing automatically:
whenever Gemini CLI asks to run a tool, choose "Allow once", never an
"always" or session-wide choice, and never switch approval mode (Shift+Tab).

0. When Gemini CLI asks, trust this folder, and accept its notice about the
   project's new hooks after reading the LangSpan commands it lists. If it
   asks to restart, quit and run this script's last line again:
     cd '$REPO' && gemini --approval-mode default

1. Type:
     Which other sessions does LangSpan say are working in this repository? Quote any task line exactly. Do not call any tool.
   Expected: the answer quotes "capture-peer task: append a peer line to notes.txt".

2. In the second terminal:  bash '$DIR/step.sh' hold
   Then, within 60 s, type:
     Use the replace tool to change the line alpha in notes.txt to delta. If it is refused, do not retry; tell me the reason you were given, word for word.
   If Gemini CLI asks to approve the replace, choose "Allow once".
   Expected: LangSpan denies it, and the answer gives the holder's reason.
   Wait until the second terminal says the hold has lapsed.

3. Type:
     Read notes.txt with the read_file tool and show me its contents. Do not change it.

4. In the second terminal:  bash '$DIR/step.sh' peer
   Then type:
     Use the replace tool to change the line beta in notes.txt to gamma, from the contents you read. If LangSpan refuses it, do what its message says and make the same replace call once more.
   Approve each replace with "Allow once".
   Expected: the first is refused as stale, and the retry goes through.
   In the second terminal:  bash '$DIR/step.sh' snap after-prompt-4

5. Type /quit. This script then saves langspan status and events.

Press Enter to start Gemini CLI.
EOF
# Gemini CLI runs interactively: a person types the prompts at a terminal.
[ -t 0 ] || { echo "run this at a terminal: Gemini CLI needs one for the prompts" >&2; exit 1; }
read -r _

(cd "$REPO" && gemini --approval-mode default) || echo "Gemini CLI ended with an error; the events up to it are kept"

"$LANGSPAN" status --json > "$DIR/status-after-quit.json" 2>&1 || echo "langspan status failed; see $DIR/status-after-quit.json"
bash "$DIR/step.sh" events || echo "langspan events failed; run bash '$DIR/step.sh' events again"
chmod 600 "$CAPTURE"/* "$DIR"/*.json "$DIR"/*.txt 2>/dev/null || true
echo "== $(find "$CAPTURE" -type f | wc -l | tr -d ' ') event(s) in $CAPTURE"
echo "Clean them (home paths, transcript_path, model text beyond the prompts) before committing them under fixtures/gemini-cli-<version>/."
