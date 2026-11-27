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
set -eu
: "${CARGO_HOME:=${HOME}/.cargo}"
: "${RUSTUP_HOME:=${HOME}/.rustup}"
export CARGO_HOME RUSTUP_HOME
export NEXTEST_USER_CONFIG_FILE=none

# Build what the suite runs first, target/debug/langspan included: that same
# build installs the foreign home, and the run below builds nothing more.
cargo nextest run --workspace --locked --no-run "$@"

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
foreign cargo nextest run --workspace --locked "$@"
foreign cargo test --doc --workspace --locked
