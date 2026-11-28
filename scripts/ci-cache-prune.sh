#!/usr/bin/env bash
# Readies main's build cache for saving (.github/workflows/check.yml), after
# its checks passed or failed. Writes save=false to $GITHUB_OUTPUT when the
# run built nothing (BUILT below): a check that failed before the test
# build (format, or a new advisory in the nightly run) leaves a target
# directory of nothing this run compiled, and an entry, once saved, is
# never replaced. Otherwise
# drops what the next run rebuilds anyway (the workspace's own crates, the
# release build, test reports) and leaves the size check to
# scripts/ci-cache-size.sh, with the same CACHE_PATHS and LIMIT_MB.
#
# Usage: bash scripts/ci-cache-prune.sh [BUILT]
#
# BUILT, a path or glob under the target directory, names what this run's
# checks make (default debug/langspan, which the test build makes); the
# Linux lint job, which builds no tests, names langspan-core's lint output.
set -euo pipefail

: "${GITHUB_OUTPUT:?set GITHUB_OUTPUT to the step output file}"
target=${CARGO_TARGET_DIR:-target}
built=$target/${1:-debug/langspan}
# The test build makes target/debug/langspan (scripts/test-foreign-home.sh),
# clippy liblangspan_core-*.rmeta under target/debug/build/langspan-core/
# (Cargo's build-directory layout on the pinned toolchain), and a restored
# entry never holds either: the clean below removes them first.
if ! compgen -G "$built" >/dev/null; then
  echo "build cache: this run built nothing it caches ($built is missing), so it saves nothing; the next run on main that builds it saves instead"
  echo "save=false" >> "$GITHUB_OUTPUT"
  exit 0
fi
cargo clean --locked -p langspan -p langspan-core -p langspan-graph
rm -rf "$target/release" "$target/nextest"
bash "$(dirname "$0")/ci-cache-size.sh"
