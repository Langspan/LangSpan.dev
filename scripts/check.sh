#!/usr/bin/env sh
# The full local check, and what CI runs. Cheapest first, so a failure shows
# as early as it can.
#
# LANGSPAN_CHECK_LANE picks the lane (docs/development-process.md#which-tests-run-when):
# `full`, the default, runs everything; `fast`, a pull request's lane, runs
# format, dependency policy, lint and the fast tests (nextest profile
# ci-fast unless NEXTEST_PROFILE says otherwise), and leaves the other
# tests and the demonstration to the merge queue.
#
# LANGSPAN_CHECK_RELEASE=false skips the release build and the demonstration
# on it. CI sets that on pull requests and merge-queue commits, so every
# push to main still builds and exercises the release binary. Unset, both
# run in the full lane.
set -eu
lane=${LANGSPAN_CHECK_LANE:-full}
case $lane in
  full) ;;
  fast)
    : "${NEXTEST_PROFILE:=ci-fast}"
    export NEXTEST_PROFILE
    ;;
  *)
    echo "check.sh: LANGSPAN_CHECK_LANE is '$lane'; set it to fast or full" >&2
    exit 2
    ;;
esac

# Each lane ends here. It fails while a process whose arguments name the
# build's target directory still runs 5 s after the checks, such as a
# coordinator a test or the demonstration started and never stopped
# (issue #313). Both spellings of the directory count, as given and
# resolved, since a test names the binary by the path cargo gave it.
leftovers() {
  target=${CARGO_TARGET_DIR:-target}
  [ -d "$target" ] || return 0
  given=$(cd "$target" && pwd)/
  resolved=$(cd "$target" && pwd -P)/
  tries=0
  while :; do
    # The directories reach awk through the environment, so its own
    # arguments don't match.
    left=$(ps -ww -A -o pid= -o args= |
      G=$given R=$resolved awk 'index($0, ENVIRON["G"]) || index($0, ENVIRON["R"])')
    if [ -z "$left" ]; then return 0; fi
    tries=$((tries + 1))
    if [ "$tries" -ge 10 ]; then
      echo "check.sh: these processes started from $given are still running after the checks:" >&2
      printf '%s\n' "$left" >&2
      echo "check.sh: a test or the demonstration left them behind; stop them with kill PID, and fix the test that starts one without stopping it" >&2
      return 1
    fi
    sleep 0.5
  done
}

cargo fmt --all -- --check
cargo deny --locked check
# cargo-audit reads the RustSec database too, with the ignores deny.toml
# lists (the script checks each has a reason and a review date still ahead).
# Its warnings (unmaintained, unsound, yanked) fail, as cargo-deny's do.
audit_flags=$(python3 -I -B scripts/advisory-ignores.py --audit-flags)
# shellcheck disable=SC2086 # each flag and id is one word
cargo audit --deny warnings $audit_flags
cargo machete
cargo clippy --workspace --all-targets --locked -- -D warnings
# Code outside tests returns an error instead of panicking (issue #330);
# tests may still unwrap, so this pass leaves them out.
cargo clippy --workspace --lib --bins --locked -- -D warnings \
  -D clippy::unwrap_used -D clippy::expect_used -D clippy::panic \
  -D clippy::unreachable -D clippy::unimplemented
# A broken intra-doc link or any other rustdoc warning fails.
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --locked
# The suite under cargo-nextest, with HOME at a read-only home holding a
# foreign LangSpan install, so no test can depend on the machine's own
# (issue #108).
sh scripts/test-foreign-home.sh
if [ "$lane" = fast ]; then
  leftovers
  echo "check.sh: fast lane done; the other tests and the demonstration run in the merge queue"
  exit 0
fi
# The integration tests run the langspan binary, so the test build has just
# built target/debug/langspan. No separate `cargo build`: its features
# resolve without the dev-dependencies, so it would relink the binary (and
# the next test build would relink it back) for no extra coverage. The
# release build below is the plain build.
LANGSPAN=target/debug/langspan bash scripts/demo-two-developers.sh
if [ "${LANGSPAN_CHECK_RELEASE:-true}" != false ]; then
  cargo build --workspace --release --locked
  LANGSPAN=target/release/langspan bash scripts/demo-two-developers.sh
fi
leftovers
