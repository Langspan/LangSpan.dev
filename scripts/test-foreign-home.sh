#!/usr/bin/env sh
# Run the test suite against a home that is not the machine's own (issue
# #108). The home holds LangSpan's hooks and skill for every host, installed
# for another state directory, and is read-only. A test that reads the real
# user home or host configuration instead of a home of its own sees that
# foreign install and fails, and one that writes there fails too. The host
# location variables are unset, so they cannot point past it.
#
# The tests run under cargo-nextest (.config/nextest.toml; NEXTEST_PROFILE
# and NEXTEST_RETRIES choose the profile and retries), then the doc tests
# under `cargo test --doc`, which nextest does not run. Arguments are passed
# to `cargo nextest run`. The toolchain and the cargo cache keep their own
# locations, resolved before HOME moves. nextest's per-user configuration
# is not read, so the run is the same on every machine.
#
# CI's full lane builds once and tests in shards (check.yml), through
# LANGSPAN_TEST_STAGE: `archive` builds the suite into the nextest archive
# LANGSPAN_TEST_ARCHIVE and runs only the doc tests; `shard` extracts that
# archive into the checkout, at the paths the build used, and runs shard
# LANGSPAN_TEST_SHARD (`K/N`) of its tests, without the doc tests. The
# shard's tests come from scripts/ci-shards.py, which splits the measured
# test time evenly; nextest must select exactly the count it expects, or
# the shard fails. Unset, it builds and runs everything.
set -eu
: "${CARGO_HOME:=${HOME}/.cargo}"
: "${RUSTUP_HOME:=${HOME}/.rustup}"
export CARGO_HOME RUSTUP_HOME
export NEXTEST_USER_CONFIG_FILE=none
stage=${LANGSPAN_TEST_STAGE:-all}
case $stage in
  all) ;;
  archive | shard)
    archive=${LANGSPAN_TEST_ARCHIVE:-}
    if [ -z "$archive" ]; then
      echo "test-foreign-home.sh: the $stage stage needs LANGSPAN_TEST_ARCHIVE, the nextest archive's path" >&2
      exit 2
    fi
    if [ "$stage" = shard ] && [ -z "${LANGSPAN_TEST_SHARD:-}" ]; then
      echo "test-foreign-home.sh: the shard stage needs LANGSPAN_TEST_SHARD, the shard as K/N" >&2
      exit 2
    fi
    ;;
  *)
    echo "test-foreign-home.sh: LANGSPAN_TEST_STAGE is '$stage'; set it to all, archive or shard" >&2
    exit 2
    ;;
esac

# Build what the suite runs first, target/debug/langspan included: that same
# build installs the foreign home, and the run below builds nothing more. A
# shard extracts the build instead; the run then reads the extracted
# metadata, so the archive is unpacked once.
case $stage in
  all)
    cargo nextest run --workspace --locked --no-run "$@"
    set -- --workspace --locked "$@"
    ;;
  archive) cargo nextest archive --workspace --locked --archive-file "$archive" ;;
  shard)
    cargo nextest run --archive-file "$archive" --workspace-remap . \
      --extract-to . --extract-overwrite --no-run
    set -- --binaries-metadata target/nextest/binaries-metadata.json \
      --cargo-metadata target/nextest/cargo-metadata.json --workspace-remap . "$@"
    # Every shard lists the same archive and computes the same assignment.
    skip=
    if [ "${LANGSPAN_BENCH_DRY_RUNS:-}" = skip ]; then skip=--skip-dry-runs; fi
    cargo nextest list "$@" --message-format json >target/nextest/all-tests.json
    # shellcheck disable=SC2086 # $skip is one flag or none
    plan=$(python3 -I -B scripts/ci-shards.py assign $skip --shard "$LANGSPAN_TEST_SHARD" \
      target/nextest/all-tests.json)
    expected=$(printf '%s\n' "$plan" | sed -n 1p)
    filter=$(printf '%s\n' "$plan" | sed -n 2p)
    cargo nextest list "$@" -E "$filter" --message-format json >target/nextest/shard-tests.json
    selected=$(python3 -I -B scripts/ci-shards.py count target/nextest/shard-tests.json)
    if [ "$selected" != "$expected" ]; then
      echo "test-foreign-home.sh: nextest selects $selected tests for shard $LANGSPAN_TEST_SHARD, but scripts/ci-shards.py assigned it $expected; its filterset doesn't mean what the script expects, so fix the script" >&2
      exit 1
    fi
    set -- "$@" -E "$filter"
    ;;
esac

home=$(mktemp -d "${TMPDIR:-/tmp}/langspan-foreign-home.XXXXXX")
cleanup() {
  chmod -R u+w "$home" 2>/dev/null || true
  rm -rf "$home"
}
trap cleanup EXIT
trap 'exit 1' INT TERM
"${CARGO_TARGET_DIR:-target}/debug/langspan" install --home "$home" \
  --hosts claude-code,codex,cursor,antigravity --state-dir "$home/elsewhere" \
  --skill --apply >/dev/null
chmod -R a-w "$home"

foreign() {
  env -u CLAUDE_CONFIG_DIR -u CODEX_HOME -u GEMINI_CLI_HOME \
    -u LANGSPAN_STATE_DIR -u XDG_STATE_HOME -u XDG_CONFIG_HOME \
    -u CLAUDE_PROJECT_DIR \
    HOME="$home" "$@"
}
if [ "$stage" != archive ]; then foreign cargo nextest run "$@"; fi
if [ "$stage" != shard ]; then foreign cargo test --doc --workspace --locked; fi
