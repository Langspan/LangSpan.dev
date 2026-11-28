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
# CI splits the full lane in two, so the suite is built once per platform
# and its tests run in parallel shards (.github/workflows/check.yml):
# `build` runs everything `full` does except the tests themselves, building
# them into the nextest archive LANGSPAN_TEST_ARCHIVE instead; `shard` runs
# only the tests of that archive that the partition LANGSPAN_TEST_PARTITION
# (`time:K/N`: shard K of N, by measured test time; scripts/ci-shards.py)
# selects.
#
# LANGSPAN_CHECK_RELEASE=false skips the release build and the demonstration
# on it. CI sets that except on the nightly run; nothing else reads the
# release binary, and the release workflow builds its own.
# Unset, both run in the full lane.
#
# LANGSPAN_CHECK_PORTABLE=false skips the checks whose result doesn't depend
# on the platform: format, dependency policy, advisories, unused
# dependencies and the docs. CI's macOS job sets it, since Linux runs them
# on the same commit; lint, the build and the tests still run.
set -eu
lane=${LANGSPAN_CHECK_LANE:-full}
portable=${LANGSPAN_CHECK_PORTABLE:-true}
case $portable in
  true | false) ;;
  *)
    echo "check.sh: LANGSPAN_CHECK_PORTABLE is '$portable'; set it to true or false" >&2
    exit 2
    ;;
esac
case $lane in
  full) ;;
  fast)
    : "${NEXTEST_PROFILE:=ci-fast}"
    export NEXTEST_PROFILE
    ;;
  build | shard)
    if [ -z "${LANGSPAN_TEST_ARCHIVE:-}" ]; then
      echo "check.sh: the $lane lane needs LANGSPAN_TEST_ARCHIVE, the nextest archive's path" >&2
      exit 2
    fi
    if [ "$lane" = build ]; then LANGSPAN_TEST_STAGE=archive; else LANGSPAN_TEST_STAGE=shard; fi
    export LANGSPAN_TEST_STAGE
    ;;
  *)
    echo "check.sh: LANGSPAN_CHECK_LANE is '$lane'; set it to fast, full, build or shard" >&2
    exit 2
    ;;
esac
partition=${LANGSPAN_TEST_PARTITION:-}
if [ "$lane" = shard ]; then
  case $partition in
    time:[1-9]*/[1-9]*) ;;
    *)
      echo "check.sh: LANGSPAN_TEST_PARTITION is '$partition'; the shard lane needs time:K/N" >&2
      exit 2
      ;;
  esac
fi

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

# A shard runs its part of the tests the build lane archived, under the
# same foreign home, and nothing else: the build lane ran the other checks.
if [ "$lane" = shard ]; then
  LANGSPAN_TEST_SHARD=${partition#time:} sh scripts/test-foreign-home.sh
  leftovers
  exit 0
fi

if [ "$portable" = true ]; then
  cargo fmt --all -- --check
  cargo deny --locked check
  # cargo-audit reads the RustSec database too, with the ignores deny.toml
  # lists (the script checks each has a reason and a review date still ahead).
  # Its warnings (unmaintained, unsound, yanked) fail, as cargo-deny's do.
  audit_flags=$(python3 -I -B scripts/advisory-ignores.py --audit-flags)
  # shellcheck disable=SC2086 # each flag and id is one word
  cargo audit --deny warnings $audit_flags
  # The advisory dry run (check.yml's advisory_dry_run input): the same
  # audit on a lockfile with a known advisory, so the lane fails here as it
  # would on a new one, without one on main.
  if [ "${LANGSPAN_ADVISORY_DRY_RUN:-}" = 1 ]; then
    # shellcheck disable=SC2086 # each flag and id is one word
    cargo audit --deny warnings $audit_flags --file tests/fixtures/advisory-dry-run/known-advisory.lock
  fi
  cargo machete
fi
cargo clippy --workspace --all-targets --locked -- -D warnings
# Code outside tests returns an error instead of panicking (issue #330);
# tests may still unwrap, so this pass leaves them out.
cargo clippy --workspace --lib --bins --locked -- -D warnings \
  -D clippy::unwrap_used -D clippy::expect_used -D clippy::panic \
  -D clippy::unreachable -D clippy::unimplemented
# A broken intra-doc link or any other rustdoc warning fails.
if [ "$portable" = true ]; then
  RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --locked
fi
# The suite under cargo-nextest, with HOME at a read-only home holding a
# foreign LangSpan install, so no test can depend on the machine's own
# (issue #108). The build lane only builds the suite into its archive here,
# and runs the doc tests. The fast lane builds only the test targets its
# filter can select (scripts/ci-fast-targets.sh).
if [ "$lane" = fast ]; then
  # An assignment, so the script's failure stops the check.
  targets=$(sh scripts/ci-fast-targets.sh)
  # shellcheck disable=SC2086 # one argument per line, none with a space
  sh scripts/test-foreign-home.sh $targets
else
  sh scripts/test-foreign-home.sh
fi
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
