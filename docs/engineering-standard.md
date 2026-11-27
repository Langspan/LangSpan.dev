# Engineering standard

The design principles LangSpan's code follows, and the check that enforces
each one. [Engineering practice](engineering.md) owns the commands and the
workflow; this page says what each principle means here and how CI holds it.

## Standards are enforced in CI

A rule that matters has a check: a test, a lint or a script that CI runs. A
rule with no check is listed as a **convention**, with the reason no check
exists, and reviewers hold it. `tests/readme_and_agent_docs.rs` fails when
this page names a script or test file that doesn't exist, or a row names
neither a file nor a convention.

## Principles

| Principle | In this codebase | Check |
| --- | --- | --- |
| DRY | Each rule, formula and wire format has one owner. Interfaces call the same application operations instead of copying them. | Convention: no tool tells one rule's second copy from code that only looks alike, and the frozen protocol shapes repeat on purpose. Golden files (`tests/cli_transcript.rs`) catch two outputs drifting apart. |
| SOLID, adapted to Rust | Modules and traits, not class hierarchies. A port is a trait or a function the pure core takes as an argument; an adapter implements it. A module has one reason to change. | `crates/langspan-core/tests/architecture.rs` keeps the core free of effect adapters. |
| Separation of concerns | The pure core decides; adapters do the I/O, git, the database and external CLIs. A `// layer:` marker names a module's layer in the `langspan` crate. | `crates/langspan-core/tests/architecture.rs`, and the module-dependency ratchet in `scripts/ratchets.py` (no new cycle, no new outward edge). |
| High cohesion, low coupling | A file holds one area; a function does one thing. Splitting is by concern, not by line count. | `scripts/ratchets.py` (file size, function length, module edges), run by `tests/ratchets.rs`. |
| Information hiding | Items are private or `pub(crate)` unless another crate needs them. | The public-surface ratchet in `scripts/ratchets.py`. |
| Design by contract | Input is parsed into typed structures at the edge, with every size bounded first. Wire formats are versioned schemas with golden vectors. | `tests/parser_properties.rs`, `crates/langspan-core/tests/properties.rs`, `tests/command_vectors.rs`, `tests/event_vectors.rs`, `tests/outcome_vectors.rs`, `tests/protocol.rs`. |
| Fail fast | Code driven by input returns an error that says what failed and what to do; it never panics. Stored data that doesn't parse stops the load instead of being reset. | The panic lints in `scripts/check.sh` (`clippy::unwrap_used`, `expect_used`, `panic` and others), and `tests/store.rs`. |
| Least privilege | Unix sockets or loopback only; state files `0600` and directories `0700`; no credentials read or forwarded. | `tests/daemon.rs`, `tests/spool.rs` and `tests/notify.rs` assert the modes; `scripts/test-foreign-home.sh` fails a test that reads the machine's own home. |
| Defense in depth | A safety control denies when it fails, and supply-chain gates overlap: two advisory scanners, a licence and source policy, and a content check. | `deny.toml` with `cargo deny`, `cargo audit` with `scripts/advisory-ignores.py`, `scripts/content-check.py`, and zizmor in `.github/workflows/check.yml`. |
| KISS | The least code that solves the problem. | Convention: no tool judges simplicity. The ratchets bound size, not clarity. |
| YAGNI | No abstraction before its second use, no option nobody asked for. | `cargo machete` in `scripts/check.sh` catches unused dependencies; the rest is a convention, because no tool sees an unused option as unused. |

## What must not be split

These are single on purpose. A change that splits one needs an ADR.

- `decide` and `apply` in `langspan-core` are the only entry points: every
  command is decided by `decide`, and every state change goes through
  `apply`.
- `Event` and `State` are single types. Current state is derived from the
  one event stream, so a second event type would be a second history.
- The store's write path: `commit_with_snapshot` in `src/store.rs` applies
  the events to a copy of the state and writes them in one transaction.
- The frozen protocol shape functions in `src/protocol.rs`
  (`encode_for`, `earlier_shape` and the `without_*` functions). Versions 3
  to 10 answer in their own shape; changing one breaks a client of that
  version.

Convention: no check counts these, because a second entry point can appear
under any name. Reviewers hold it, and the golden vectors above catch a
changed shape.
