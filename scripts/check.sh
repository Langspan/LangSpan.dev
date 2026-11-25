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
cargo test --workspace --locked
# The integration tests run the langspan binary, so cargo test has just
# built target/debug/langspan. No separate `cargo build`: its features
# resolve without the dev-dependencies, so it would relink the binary (and
# the next cargo test would relink it back) for no extra coverage. The
# release build below is the plain build.
LANGSPAN=target/debug/langspan bash scripts/demo-two-developers.sh
if [ "${LANGSPAN_CHECK_RELEASE:-true}" != false ]; then
  cargo build --workspace --release --locked
  LANGSPAN=target/release/langspan bash scripts/demo-two-developers.sh
fi
