#!/usr/bin/env sh
# The full local check, and what CI runs. Cheapest first, so a failure shows
# as early as it can.
#
# LANGSPAN_CHECK_RELEASE=false skips the release build and the demonstration
# on it. CI sets that on pull requests only, so every push to main still
# builds and exercises the release binary. Unset, both run.
set -eu
cargo fmt --all -- --check
cargo deny --locked check
cargo machete
cargo clippy --workspace --all-targets --locked -- -D warnings
# The whole suite under cargo-nextest, with HOME at a read-only home holding
# a foreign LangSpan install, so no test can depend on the machine's own
# (issue #108).
sh scripts/test-foreign-home.sh
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
