#!/usr/bin/env bash
# Capture the hook events a real Cursor CLI sends to LangSpan, so they can
# replace the constructed fixtures in fixtures/cursor-constructed
# (docs/adr/0032-cursor-adapter.md, "Taking a real capture").
#
# Usage: bash scripts/capture-cursor.sh DIR [--trust]
#
# The owner runs this. It never signs in and never reads Cursor's account
# files: it asks `cursor-agent status` and stops unless that reports a
# signed-in account. It runs two short model turns in a new throwaway
# repository under DIR, through LangSpan's project-level Cursor hooks with
# LANGSPAN_CAPTURE_DIR set, and leaves the raw events in DIR/capture (0600).
# They contain local paths and may contain the account email: clean them
# before committing anything.
#
# Cursor runs project hooks only in a trusted workspace. Either trust the
# repository this script prints by running `cursor-agent` in it once, or pass
# --trust to have this script pass `--trust` for that one directory.
set -euo pipefail
# The hook writes raw events as they arrive: owner-only from the start.
umask 077

DIR=${1:?usage: bash scripts/capture-cursor.sh DIR [--trust]}
TRUST=${2:-}
LANGSPAN=${LANGSPAN:-target/release/langspan}
[ -x "$LANGSPAN" ] || { echo "build first: cargo build --release --locked" >&2; exit 1; }
LANGSPAN=$(cd "$(dirname "$LANGSPAN")" && pwd)/$(basename "$LANGSPAN")
command -v cursor-agent >/dev/null || { echo "cursor-agent is not on PATH" >&2; exit 1; }

# Signed in, or nothing runs. Only the status line is read; it is not printed.
if ! cursor-agent status 2>&1 | grep -q 'Logged in'; then
  echo "cursor-agent is not signed in. Run \`cursor-agent login\` yourself, then run this again." >&2
  exit 1
fi

mkdir -p "$DIR"
DIR=$(cd "$DIR" && pwd)
REPO=$(cd "$(mktemp -d "$DIR/repo.XXXXXX")" && pwd -P)
CAPTURE="$DIR/capture"
# The path is written into a JSON string and a single-quoted shell word.
case "$CAPTURE" in
  *[\'\"\\\|\&]*) echo "choose a DIR without quotes, backslashes, | or &" >&2; exit 1 ;;
esac
# One run, one workspace and one holder: a second run never reuses the first's.
RUN=$(basename "$REPO" | tr -c 'A-Za-z0-9\n' '-')
WORKSPACE="capture-$RUN"
mkdir -p "$CAPTURE"
chmod 700 "$CAPTURE"
"$LANGSPAN" status --json >/dev/null 2>&1 || { echo "start the coordinator first: $LANGSPAN daemon" >&2; exit 1; }

git -C "$REPO" init --quiet -b main
printf 'alpha\nbeta\n' > "$REPO/notes.txt"
git -C "$REPO" add notes.txt
git -C "$REPO" -c user.name=capture -c user.email=capture@example.invalid commit --quiet -m init

"$LANGSPAN" hooks install cursor --workspace "$WORKSPACE" --root "$REPO" --apply
# Set the capture directory in each hook command. The entries then no longer
# match LangSpan's exact shape, which is fine in a throwaway repository.
sed -i.bak "s|\"LANGSPAN_MANAGED=cursor:v1 |\"LANGSPAN_CAPTURE_DIR='$CAPTURE' LANGSPAN_MANAGED=cursor:v1 |" "$REPO/.cursor/hooks.json"
rm "$REPO/.cursor/hooks.json.bak"
# Only `ls` runs without a prompt, and only in this repository.
printf '{"permissions":{"allow":["Shell(ls)"],"deny":[]}}\n' > "$REPO/.cursor/cli.json"

FLAGS=(-p --output-format text)
if [ "$TRUST" = --trust ]; then
  FLAGS+=(--trust)
else
  echo "If Cursor reports 'Workspace Trust Required', run \`cursor-agent\` in $REPO once and trust it, or rerun with --trust."
fi

turn() { (cd "$REPO" && cursor-agent "${FLAGS[@]}" "$1") || echo "the turn ended with an error; the events up to it are kept"; }

echo "== turn 1: read, edit one line, run ls"
turn "Read notes.txt, then change the line 'beta' to 'gamma' in it, then run the shell command ls. Reply with one word: done."

echo "== turn 2: the same file, held by another session"
"$LANGSPAN" workspace add "$WORKSPACE" "$REPO" --request-id "$WORKSPACE-workspace" >/dev/null
"$LANGSPAN" session start "$WORKSPACE.holder" "$WORKSPACE" --request-id "$WORKSPACE-holder" >/dev/null
"$LANGSPAN" admit "$WORKSPACE.holder" notes.txt --ttl-ms 60000 --request-id "$WORKSPACE-hold" >/dev/null
turn "Change the line 'alpha' in notes.txt to 'delta'. Reply with one word: done."

chmod 600 "$CAPTURE"/* 2>/dev/null || true
echo "== $(find "$CAPTURE" -type f | wc -l | tr -d ' ') event(s) in $CAPTURE"
echo "Clean them (home paths, user_email, transcript_path, model text) before committing them under fixtures/cursor-cli-<version>/."
