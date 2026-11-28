# Fuzz targets

cargo-fuzz targets for the parsers that read outside input
([issue #338](https://github.com/Langspan/LangSpan.dev/issues/338)). This is a
package with its own workspace and `Cargo.lock`, so nothing here reaches the
main lockfile or a release build. Each target calls one parser and drops the
result: it fails only on a panic, an abort, a sanitizer report, a timeout or
running out of memory.

| Target | Parser |
| --- | --- |
| `socket_frame` | The coordinator socket's frame reader, `protocol::read_frame`, then the `Request` and `Response` decoders and the request redactor |
| `index_import` | An exported source index, read with `index_file::read_line`, `parse_header` and `parse_record` as `langspan index import` reads it |
| `test_output` | Test-name classification of cargo test, go test and pytest output, `test_output::read`; the first byte sets the run's exit status and a preferred name |
| `codex_patch` | The Codex host's `apply_patch` parser, `codex::patch_paths` and `codex::patch_edits` |
| `graph_rust`, `graph_python`, `graph_typescript`, `graph_tsx`, `graph_go`, `graph_java`, `graph_csharp` | `langspan_graph::extract` for each language, which runs the tree-sitter grammar's C code |

`corpus/<target>/` holds each target's committed seeds, taken from the
recorded fixtures in `fixtures/` where one exists.

## Running one

From the repository root, with a nightly toolchain and
`cargo install --locked cargo-fuzz --version 0.13.1`:

```sh
mkdir -p target/fuzz-corpus/socket_frame
cargo +nightly fuzz run socket_frame target/fuzz-corpus/socket_frame fuzz/corpus/socket_frame -- -max_total_time=60
```

libFuzzer writes the inputs it finds into the first directory, so name a
scratch one first to keep the committed seeds as they are. A crash is written
to `fuzz/artifacts/<target>/`; reproduce it with
`cargo +nightly fuzz run <target> fuzz/artifacts/<target>/<file>`. Once the
parser is fixed, add the input to that target's corpus.

The nightly job (`.github/workflows/fuzz.yml`) runs every target for three
minutes on a copy of its seeds; no pull request runs it, and you can start it
by hand with `gh workflow run fuzz.yml`. A crash fails the run, uploads
`fuzz/artifacts/` as the `fuzz-artifacts` artifact, and opens the issue
"Nightly fuzz run on main failed", labelled `bug`, with the artifact's link,
or comments on that issue while it is open.

`fuzz/Cargo.lock` also locks the main crate's dependencies, at the main
lockfile's versions. When those change, the job's lockfile check fails until
it is refreshed with
`cargo update --manifest-path fuzz/Cargo.toml --workspace` (and, for a
version that moved, `-p <crate> --precise <version>` to match `Cargo.lock`).

## Adding a target

Add `fuzz_targets/<name>.rs` and its `[[bin]]` entry in `Cargo.toml`, and at
least one seed in `corpus/<name>/`. The job runs every target
`cargo fuzz list` names. A private parser needs a public entry point first.
