#!/usr/bin/env sh
# The cargo targets a pull request's fast lane builds, one argument per line
# (scripts/check.sh passes them to scripts/test-foreign-home.sh). nextest
# builds every target it is given and only then applies the profile's
# filter, so without these the fast lane compiled all of the suite's test
# binaries and ran 67 of 148.
#
# What runs stays the `ci-fast` profile's filter in .config/nextest.toml;
# this only stops the build of what it leaves out. It prints `--lib` and
# `--bins`, a `--test` for each integration test of langspan-core and
# langspan-graph (the filter takes every test of those two crates), and a
# `--test` for each `binary_id(langspan::NAME)` the filter names. A `--test`
# name applies to every package in the workspace, so a langspan test that
# shares a name with one of the two crates' (tests/briefing.rs and
# crates/langspan-core/tests/briefing.rs) is built and then left out by the
# filter. Run from the repository root.
set -eu
printf '%s\n' --lib --bins
for file in crates/langspan-core/tests/*.rs crates/langspan-graph/tests/*.rs; do
  name=${file##*/}
  printf -- '--test\n%s\n' "${name%.rs}"
done
names=$(awk '/^\[profile\.ci-fast\]$/ { on = 1; next } /^\[/ { on = 0 } on' .config/nextest.toml |
  grep -o 'binary_id(langspan::[a-z0-9_]*)' | sed 's/^binary_id(langspan::\(.*\))$/\1/')
if [ -z "$names" ]; then
  echo "ci-fast-targets.sh: the ci-fast profile in .config/nextest.toml names no binary_id(langspan::NAME); run from the repository root, or fix this script if the filter changed form" >&2
  exit 1
fi
for name in $names; do
  printf -- '--test\n%s\n' "$name"
done
