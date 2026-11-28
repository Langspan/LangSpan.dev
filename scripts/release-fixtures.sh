#!/usr/bin/env bash
# Write what a published release adds to the tests (issue #566): its golden
# stores and their state snapshots (ADR 0015), and its frozen event-type
# list with the generated table that names it. The release workflow runs it
# on the release commit and opens a pull request with the result; it can be
# run by hand the same way.
#
# Usage: bash scripts/release-fixtures.sh VERSION
#   VERSION  the released version, such as 0.10.0; its tag vVERSION must be
#            the commit checked out, so the list is the event types it reads.
#
# Writes tests/fixtures/store-VERSION/ and fixtures/event-types/VERSION.txt,
# and rewrites the generated docs tables. The row in RELEASES
# (tests/protocol.rs), with the capabilities the release's requests name, is
# left to the pull request's author: until it is added, the protocol tests
# fail naming it.
set -euo pipefail

[ $# -eq 1 ] || { echo "usage: $0 VERSION" >&2; exit 2; }
version=$1
root=$(git rev-parse --show-toplevel)
cd "$root"
tag=$(git rev-parse --verify --quiet "v$version^{commit}") || {
  echo "there is no tag v$version: run git fetch --tags, or name a released version" >&2
  exit 1
}
[ "$tag" = "$(git rev-parse HEAD)" ] || {
  echo "HEAD is not v$version: check out the release commit first" >&2
  exit 1
}
list=fixtures/event-types/$version.txt
[ ! -e "$list" ] || { echo "$list exists: frozen lists are never replaced" >&2; exit 1; }

bash scripts/golden-stores.sh "$version"
LANGSPAN_UPDATE_GOLDEN=1 cargo test --locked --test golden_stores \
  every_golden_store_replays_to_its_committed_state

# The sorted `type` of each event vector, one per line.
python3 -I -B - "$list" <<'EOF'
import glob, json, sys
types = sorted({json.load(open(path))["type"] for path in glob.glob("fixtures/*-event-v10.json")})
if not types:
    sys.exit("no fixtures/*-event-v10.json: run this in the repository root at the release tag")
with open(sys.argv[1], "x") as out:
    out.write("".join(f"{name}\n" for name in types))
EOF
python3 -I -B scripts/gen-docs.py
echo "wrote tests/fixtures/store-$version and $list; add the $version row to RELEASES in tests/protocol.rs"
