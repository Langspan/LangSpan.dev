# Build from source

From this repository, use Rust's installed toolchain manager and a C compiler for
bundled SQLite and Tree-sitter grammars. The repository pins Rust 1.100.0 and
commits the dependency lockfile.

```sh
cargo build --workspace --release --locked
./target/release/langspan --help
```

For development checks:

```sh
cargo install --locked cargo-deny --version 0.22.1
cargo install --locked cargo-machete --version 0.10.1
sh scripts/check.sh
```

That script checks formatting, lints, runs the core and real SQLite/socket/CLI/MCP tests,
builds the release binary, and checks dependencies against `deny.toml`: advisories, licences, bans and sources. The integration
suite needs permission to bind a Unix socket. The
[validation record](../IMPLEMENTATION_STATUS.md) distinguishes observed passes,
blocked runs, and host acceptance. Build and test passes are recorded on Linux
and macOS arm64. Process probing is Linux-only; Windows transport is not implemented. Exact commands live in [engineering practice](../engineering.md).
