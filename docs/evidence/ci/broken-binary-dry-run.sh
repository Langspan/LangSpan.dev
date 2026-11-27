#!/usr/bin/env bash
# Dry run of the release gate (issue #315), without a fork: run the build
# job's own "Version" and "Two-developer demonstration on the release
# binary" steps, read from .github/workflows/release.yml, against
#   1. the real release binary, which must pass both, and
#   2. a broken binary that answers --version correctly but crashes
#      (SIGSEGV) on `langspan hook`, which must pass "Version" and fail
#      the demonstration.
# Then it checks that `publish` needs `build`, so the failing step stops
# the release while it is still a draft.
#
#   cargo build --release --locked --bin langspan
#   bash docs/evidence/ci/broken-binary-dry-run.sh [path/to/langspan]
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
WORKFLOW="$ROOT/.github/workflows/release.yml"
REAL=$(cd "$(dirname "${1:-$ROOT/target/release/langspan}")" && pwd)/$(basename "${1:-langspan}")
[ -x "$REAL" ] || { echo "build first: cargo build --release --locked --bin langspan" >&2; exit 1; }

# The `run:` of the step named $1 in the build job, as GitHub runs it.
step_script() {
  awk -v want="      - name: $1" '
    $0 == want { inside = 1; next }
    inside && /^      - / { exit }
    inside && /^        run: \|$/ { block = 1; next }
    inside && /^        run: / { sub(/^        run: /, ""); print; exit }
    inside && block { if ($0 !~ /^          / && $0 != "") exit; sub(/^          /, ""); print }
  ' "$WORKFLOW"
}

TARGET=$(rustc -vV | sed -n 's/^host: //p')
TAG=v$("$REAL" --version | sed 's/^langspan //')
export TARGET TAG
WORK=$(mktemp -d /tmp/ls-dry.XXXXXX)
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/scripts"
cp "$ROOT/scripts/demo-two-developers.sh" "$WORK/scripts/"

# GitHub's default for `run:` on Linux and macOS: bash -e -o pipefail.
run_step() { # NAME
  local script; script=$(step_script "$1")
  [ -n "$script" ] || { echo "no step named '$1' in $WORKFLOW" >&2; exit 1; }
  (cd "$WORK" && bash --noprofile --norc -eo pipefail -c "$script") > "$WORK/step.log" 2>&1
}

try() { # LABEL BINARY EXPECT_VERSION EXPECT_DEMO
  mkdir -p "$WORK/target/$TARGET/release"
  cp "$2" "$WORK/target/$TARGET/release/langspan"
  local version=pass demo=pass
  run_step "Version" || version=fail
  tail -n 2 "$WORK/step.log" | sed 's/^/     | /'
  run_step "Two-developer demonstration on the release binary" || demo=fail
  grep -E 'FAILED|^== ' "$WORK/step.log" | tail -n 3 | sed 's/^/     | /'
  echo "$1: Version $version (want $3), demonstration $demo (want $4)"
  [ "$version" = "$3" ] && [ "$demo" = "$4" ] || { echo "dry run: unexpected result for $1" >&2; exit 1; }
}

echo "target $TARGET, tag $TAG, steps from .github/workflows/release.yml"
try "real binary" "$REAL" pass pass

cat > "$WORK/broken" <<EOF
#!/bin/sh
# Answers everything as the real binary does, except that \`hook\` crashes.
[ "\${1:-}" = hook ] && kill -SEGV \$\$
exec "$REAL" "\$@"
EOF
chmod +x "$WORK/broken"
try "broken binary (crashes on hook)" "$WORK/broken" pass fail

# The gate: publish waits for every build job, and one failure cancels the rest.
needs=$(awk '/^  publish:/ { p = 1 } p && /^    needs:/ { print; exit }' "$WORKFLOW")
case "$needs" in *build*) echo "publish $needs: a failed build job leaves the release a draft" ;;
  *) echo "dry run: publish no longer needs build ($needs)" >&2; exit 1 ;; esac
awk '/^  build:/ { b = 1 } b && /fail-fast:/ { print "build matrix" $0; exit }' "$WORKFLOW"
