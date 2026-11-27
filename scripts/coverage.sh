#!/usr/bin/env bash
# Line coverage of the workspace, which the nightly coverage job
# (.github/workflows/coverage.yml) measures and holds to a floor
# (scripts/coverage-report.py; docs/development-process.md#coverage).
#
# The tests run once under cargo-llvm-cov and cargo-nextest, with the
# benchmark harness's dry runs skipped. Tests stop coordinators with
# SIGKILL, which never runs the profile runtime's exit hook, so the profile
# is written continuously (`%c`): without it `src/daemon.rs` measured 2.7%,
# not about 89%. That needs the counters page-aligned on macOS and
# relocated at run time on Linux, hence RUSTFLAGS below.
#
# `cargo llvm-cov report` covers only the root crate of this workspace, so
# llvm-cov exports the counts itself over every test binary nextest lists,
# plus the langspan binary the tests start: production.lcov over the
# integration-test binaries (their libraries have no #[cfg(test)] code),
# all.lcov over every one.
#
# Needs cargo-llvm-cov and cargo-nextest (scripts/install-ci-tools.sh) and
# rustup's llvm-tools component. Usage: bash scripts/coverage.sh [DIR]
# Writes DIR (default target/coverage): the two exports and report.md, the
# per-file table. Fails when a test fails or coverage is under the floor.
set -euo pipefail

out=${1:-target/coverage}
host=$(rustc -vV | sed -n 's/^host: //p')
tools="$(rustc --print sysroot)/lib/rustlib/$host/bin"
export LLVM_COV="$tools/llvm-cov" LLVM_PROFDATA="$tools/llvm-profdata"
if [ ! -x "$LLVM_COV" ] || [ ! -x "$LLVM_PROFDATA" ]; then
  echo "coverage.sh: no llvm-cov in $tools; run: rustup component add llvm-tools" >&2
  exit 1
fi
export LLVM_PROFILE_FILE_NAME='langspan-%c%16m.profraw'
case "$(uname -s)" in
  Darwin)
    for section in cnts data bits; do
      RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-Wl,-sectalign,__DATA,__llvm_prf_$section,0x4000"
    done
    ;;
  *) RUSTFLAGS="${RUSTFLAGS:-} -C llvm-args=-runtime-counter-relocation" ;;
esac
export RUSTFLAGS
export LANGSPAN_BENCH_DRY_RUNS=skip
export NEXTEST_USER_CONFIG_FILE=none

cargo llvm-cov clean --workspace
status=0
cargo llvm-cov nextest --workspace --locked --no-report || status=$?

# The same build environment, so nextest lists the binaries just built.
eval "$(cargo llvm-cov show-env --export-prefix)"
target=${CARGO_LLVM_COV_TARGET_DIR:-$PWD/target/llvm-cov-target}
mkdir -p "$out"
"$LLVM_PROFDATA" merge -sparse "$target"/*.profraw -o "$out/langspan.profdata"
CARGO_TARGET_DIR=$target cargo nextest list --workspace --locked \
  --list-type binaries-only --message-format json >"$out/binaries.json"

# export FILE KIND...: the lcov export over the binaries of those kinds.
export_lcov() {
  local file=$1
  shift
  local objects=(-object "$target/debug/langspan")
  while IFS= read -r binary; do
    objects+=(-object "$binary")
  done < <(python3 -I -c '
import json, sys
kinds = sys.argv[2:]
for b in json.load(open(sys.argv[1]))["rust-binaries"].values():
    if b["kind"] in kinds:
        print(b["binary-path"])
' "$out/binaries.json" "$@")
  "$LLVM_COV" export -format=lcov -instr-profile "$out/langspan.profdata" \
    -ignore-filename-regex '/(\.cargo|\.rustup|rustc)/' "${objects[@]}" >"$out/$file"
}
export_lcov production.lcov test
export_lcov all.lcov test lib bin proc-macro

report=0
python3 -I -B scripts/coverage-report.py . "$out" >"$out/report.md" || report=$?
cat "$out/report.md"
if [ "$status" -ne 0 ]; then
  echo "coverage.sh: a test failed (exit $status), so the figures above are low" >&2
  exit "$status"
fi
exit "$report"
