#!/usr/bin/env bash
# Write the golden stores of one release (ADR 0015): run that release's own
# two-developer demonstration with that release's own build, and keep each
# coordinator's state file, compacted with VACUUM INTO.
#
# Usage: bash scripts/golden-stores.sh VERSION BINARY
#   VERSION  a released version, such as 0.4.0; its tag is vVERSION.
#   BINARY   `langspan` built from that tag with its lockfile, for example
#            cargo install --locked --git <this repository> --tag vVERSION \
#              --root DIR langspan
#
# Writes tests/fixtures/store-VERSION/alice.sqlite3 and bob.sqlite3, run from
# anywhere in the repository. An existing directory is never replaced. Then
# write each store's state snapshot: run `tests/golden_stores.rs` with
# LANGSPAN_UPDATE_GOLDEN=1 and review the new `<store>.state.json` files.
set -euo pipefail

[ $# -eq 2 ] || { echo "usage: $0 VERSION BINARY" >&2; exit 2; }
version=$1
binary=$2
[ -x "$binary" ] || { echo "$binary is not an executable: build release $version first" >&2; exit 1; }
reported=$("$binary" --version)
case "$reported" in
  *" $version" | *" $version "*) ;;
  *) echo "$binary reports \"$reported\", not $version: build it from tag v$version" >&2; exit 1 ;;
esac
command -v sqlite3 >/dev/null || { echo "sqlite3 is needed to compact the stores (VACUUM INTO)" >&2; exit 1; }
root=$(git rev-parse --show-toplevel)
out=$root/tests/fixtures/store-$version
[ ! -e "$out" ] || { echo "tests/fixtures/store-$version exists: golden stores are never replaced" >&2; exit 1; }

work=$(mktemp -d /tmp/ls-golden.XXXXXX)
trap 'rm -rf "$work"' EXIT
mkdir "$work/stores"

# The release's demonstration as tagged, with one addition: once its cleanup
# has stopped the coordinators, and before it removes their directories,
# each state file is compacted into the work directory.
git -C "$root" show "v$version:scripts/demo-two-developers.sh" > "$work/demo.sh"
awk -v keep="$work/stores" '
  { print }
  !done && /^  for pid in "\$\{PIDS\[@\]:-\}"; do kill/ {
    print "  for state in \"$BASE\"/*-state/state.sqlite3; do"
    print "    [ -f \"$state\" ] || continue"
    print "    who=$(basename \"$(dirname \"$state\")\")"
    print "    sqlite3 \"$state\" \"VACUUM INTO '\''" keep "/${who%-state}.sqlite3'\''\" || true"
    print "  done"
    done = 1
  }
  END { if (!done) exit 1 }
' "$work/demo.sh" > "$work/demo-keep.sh" || {
  echo "the demonstration of v$version has no cleanup to keep its stores from" >&2
  exit 1
}

LANGSPAN=$binary bash "$work/demo-keep.sh" > "$work/demo.log" 2>&1 || {
  tail -n 40 "$work/demo.log" >&2
  echo "the demonstration of v$version failed; no stores written" >&2
  exit 1
}
for who in alice bob; do
  [ -s "$work/stores/$who.sqlite3" ] || { echo "the demonstration left no $who store" >&2; exit 1; }
done
mkdir "$out"
mv "$work/stores/alice.sqlite3" "$work/stores/bob.sqlite3" "$out/"
echo "wrote tests/fixtures/store-$version: alice.sqlite3, bob.sqlite3 ($reported)"
