# Implementation dependencies

These entries cover the first local coordination slice. Versions and transitive
packages are pinned in `Cargo.lock`; the compiler is pinned in
`rust-toolchain.toml`. Dependency code is linked under its own licence; no implementation code
from another coordination tool is copied.

| Dependency | Purpose | Licence | Size / scope | Why use it; replacement path |
| --- | --- | --- | --- | --- |
| `serde` 1.0.229 | Typed, versioned JSON contracts and persisted events | MIT OR Apache-2.0 | Derive macros plus serialization traits; release footprint not measured yet | Hand-written parsing increases contract drift; another serializer could replace the adapter and derives. |
| `serde_json` 1.0.154, `preserve_order` | Bounded JSON messages and SQLite event/receipt encoding; `preserve_order` keeps a user's key order when hook installation rewrites their settings file | MIT OR Apache-2.0 | JSON parser/encoder. The feature links `indexmap` and `foldhash`, which were already in the lockfile through other dependencies; release footprint not measured yet | Keeps one encoding for all surfaces; replace behind the protocol and store adapters. |
| `rusqlite` 0.43.0, bundled | Local transactional event log and durable request receipts | MIT | Links SQLite through `libsqlite3-sys`; SQLite is public domain; release footprint not measured yet | Atomic event/outcome commits and recovery need a mature database; the store API is the replacement boundary. |
| `rmcp` 3.11.0 | Official MCP lifecycle, tools and bounded stdio transport | Apache-2.0 | Server/macros/stdio features; client/process transport only for tests; release footprint not measured yet | Avoid maintaining a partial MCP implementation; replace the MCP adapter with a conformant SDK. |
| `tokio` 1.54.1 | Async stdio, bounded socket service, timeouts and MCP runtime | MIT | Explicit runtime, I/O, network, sync, process and timer features; release footprint not measured yet | Provides cancellation and bounded waits; a different runtime can replace the outer adapters. |
| `schemars` 1.2.3 (through `rmcp`) | JSON Schema derived from typed tool parameters | MIT | SDK transitive dependency; release footprint not measured yet | Prevents hand-maintained schema drift; replace SDK schema generation if the adapter changes. |
| `rustix` 1.1.6 | Safe effective-user identity, no-follow file opening flags, deadline-bounded local socket I/O, and the daemon's sleep-inclusive clock | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | Unix process/filesystem/network/poll features for local sockets, bounded hook stdin, directory-relative no-follow Git observation, and process identity; the `time` feature (no added crate, lockfile unchanged) for `clock_gettime` with `CLOCK_BOOTTIME` on Linux and `CLOCK_MONOTONIC` on macOS, which `std::time::Instant` does not offer (`src/clock.rs`); release footprint not measured yet | Avoid platform constants and unsafe syscall wrappers; replace with equivalent standard-library APIs when available. |
| `tree-sitter` 0.27.2 | Incremental parser runtime used by `langspan-graph` to read the structure of source files | MIT | C runtime compiled at build time through `cc`; release footprint not measured yet | Writing parsers for four languages by hand is not credible; the crate boundary (`extract`, `diff`) is the replacement point for any other parser. |
| `tree-sitter-rust` 0.24.2, `tree-sitter-python` 0.25.0, `tree-sitter-typescript` 0.23.2, `tree-sitter-go` 0.25.0 | Grammars for the indexed languages | MIT | Generated C parsers, several megabytes of source each, compiled in; release footprint not measured yet | The official grammars for each language; each can be removed with its `Language` variant. |

Hook retry identities use a hand-written SHA-256 (`src/sha256.rs`, about 90 lines) instead of a crate. It names retries and identifies observed local content/process incarnations; it is not used for authentication or secrecy. It's pinned to FIPS 180-4 test vectors, and its derived IDs to vectors computed independently with Python `hashlib`. It can be replaced by any audited SHA-256 crate without changing the IDs.

The observer also uses a hand-written SHA-1 (`src/sha1.rs`, about 70 lines) for one purpose: computing the Git blob ID of a working file in a SHA-1 repository, to tell whether it equals the indexed blob without starting a Git process per file. It is not used for authentication, secrecy or integrity against an adversary. It's pinned to FIPS 180-4 test vectors and to a blob ID checked against `git hash-object`. It can be replaced by any SHA-1 crate without changing behaviour.

The dashboard (ADR 0013) adds no dependency. Its HTTP server is the standard
library's `TcpListener` serving two fixed `GET` routes with bounded request
heads, which needs neither an HTTP crate nor an async framework; its access
token reads `/dev/urandom` instead of a random-number crate; and a 15-line
Base64 encoder, pinned to the RFC 4648 test vectors, writes the SHA-256 hashes
in its content security policy. A hosted or multi-route server would be the
point to adopt a maintained HTTP crate.

None of the application uses above performs an outbound network request or
collects telemetry. Cargo downloads and dependency-advisory checks are build-time
network operations. A release-size measurement is still needed; research hook
sizes are not measurements of this application.

## Build and verification tools

`cargo-deny` 0.22.1 (MIT OR Apache-2.0) checks the locked dependency graph for
the four release targets against `deny.toml`: RustSec advisories (it replaced
`cargo-audit` 0.23.0 here), the licence allow-list, banned or wildcard
requirements, and that every crate comes from crates.io. The allow-list is
exactly what the tree uses today, found with `cargo deny check licenses`:
Apache-2.0, MIT, Unicode-3.0 (`unicode-ident`) and Zlib (`foldhash`). It is
installed separately and is not linked into LangSpan. Fetching the advisory
database is its one network access, an explicit verification step; another
RustSec-compatible checker could replace it.

`cargo-machete` 0.10.1 (MIT) reports dependencies a manifest names but no
source file uses; it reads the sources only and makes no network request.
`zizmor` 1.33.1 (MIT) audits `.github/` for workflow security problems; CI
installs it from PyPI with `pipx`, and its online audits query the GitHub API
with the job's read-only token.

GitHub Actions pins every action by full commit SHA, each resolved from the
action's official release tag: `actions/checkout` v7.0.1
(`3d3c42e5aac5ba805825da76410c181273ba90b1`, MIT) and `Swatinem/rust-cache`
v2.9.3 (`d1527c8f67ef9af357290475d4af4b0fab8f12e0`, LGPL-3.0, a build-time
tool only). The check workflow has no token permission by default and grants
each job only repository read access; it does not persist Git credentials, and
only runs on `main` save the cache.

The release workflow (`.github/workflows/release.yml`) runs only in the
public repository and adds build-time tools, none linked into LangSpan:
`cargo-about` 0.9.3 (MIT OR Apache-2.0) writes `THIRD-PARTY-LICENSES.html`
from `packaging/about.toml`, with the same licence allow-list as `deny.toml`;
`cargo-cyclonedx` 0.5.10 (Apache-2.0) writes a CycloneDX 1.5 SBOM per target.
Its actions, pinned the same way: `googleapis/release-please-action` v5.0.0
(`45996ed1f6d02564a971a2fa1b5860e934307cf7`, Apache-2.0),
`actions/upload-artifact` v7.0.2 (`cf430e030ddbb5b0abf93d22962f4752f3646cd9`,
MIT), `actions/download-artifact` v8.0.2
(`9000827ccba6bdab643e8b6fd33ac0654aef8333`, MIT),
`actions/attest-build-provenance` v4.2.2
(`4d101475d8b20a2381f78447822ac1eab6504dd8`, MIT) and `actions/attest` v4.2.2
(`1e69f48acb82d1966a394da916b4c1698aa569d6`, MIT). The Scorecard workflow adds
`ossf/scorecard-action` v2.6.0 (`23f22cd43608bae7a1f1fa2603d7314dee9538a5`,
Apache-2.0) and `github/codeql-action/upload-sarif` v4.39.5
(`915412ff23e8b7659517e607487dbeae018f2934`, MIT).

## Initial audit finding

The 24 November 2026 `cargo audit` run reported no vulnerability findings and one
maintenance warning (`cargo deny` reports the same advisory): [`RUSTSEC-2026-0543`](https://rustsec.org/advisories/RUSTSEC-2026-0543/),
`chrono` 0.4.45 is unmaintained and has no fixed upgrade. `cargo tree -i chrono
--edges normal` identifies `rmcp` and its `schemars` dependency as the introducing
packages. `deny.toml` fails on unmaintained crates anywhere in the tree and lists this one
advisory as its only ignore, with this reason, so it stays a recorded decision
rather than a silent warning; any other unmaintained crate fails CI. Follow the
upstream SDK/schema migration; the local coordination core uses injected monotonic
time and does not depend on chrono. The audit is not a zero-warning result.

M3 adds no Rust package or Cargo feature dependency. Its observer invokes the locally installed Git binary for bounded read-only operations; its process-incarnation probe is Linux-specific. The M5 orchestration harness uses Python's standard library and is a development tool, not a runtime dependency.
