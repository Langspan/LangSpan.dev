# ADR 0008: A derived source index and interface-change notices

**Status:** Implementation decision for the owner-directed coordination work,
24 November 2026. The earlier build contract made symbol work conditional on
the product benchmark; the owner's later direction supersedes that
ordering. No benchmark result yet shows this index improving an outcome.

## Context

File-level awareness cannot say "you changed a function my file calls". Two
sessions can edit different files and still break each other. Answering that
needs to know what each file defines and what each file mentions.

## Decision

**Extraction.** A new crate, `langspan-graph`, parses one file's bytes with
Tree-sitter and returns the symbols it defines (name, kind, line range,
signature text up to the body, whether exported by the language's own rule),
the names it mentions and what it imports. Rust, Python, TypeScript/JavaScript
(including TSX/JSX) and Go, chosen by file extension. It does no I/O. Inputs
over 1 MiB are refused; a file with a syntax error is indexed as far as it
parsed and marked partial.

**Everything is a name match.** Nothing resolves scopes, types, modules or
imports. A mention of `open` matches every symbol called `open`. Every
surface says so: the CLI output, the MCP tool descriptions and the briefing
text. This is the honest description of what a structural index without a
type checker knows, and it over-reports rather than under-reports.

**Storage.** A second SQLite file, `graph.sqlite3`, beside the event store.
It is derived: deleting it loses nothing, and a file from another schema
version is dropped and rebuilt. Rows are keyed by workspace and path and
carry the digest of the exact content they came from. Each worktree is
therefore indexed as it is on disk, uncommitted changes included, which is
the revision-aware overlay: the index never describes a version other than
the one whose digest it names. No separate graph engine: a few indexed tables
answer every query here.

**Updating.** After each complete Git scan, the daemon hands the indexer the
supported files and their digests. The indexer, the index's only writer, runs
on its own thread, removes files that are gone, and parses files whose digest
differs from the indexed one: at most 200 per pass, within 2 s, only if the
file on disk still has exactly the scanned digest. The rest wait for the next
scan.

**Queries**, all read-only and bounded to 200 results, with index coverage in
every answer:

| CLI | MCP | Returns |
| --- | --- | --- |
| `graph symbol WORKSPACE NAME [--detail]` | `langspan_symbol` (`detail`) | Definitions of that name; with detail, what is around each (below) |
| `graph references WORKSPACE NAME` | `langspan_references` | Mentions of that name |
| `graph impact WORKSPACE PATH` | `langspan_impact` | What the file exports, and other files mentioning those names |
| `graph blind-spots WORKSPACE` | `langspan_blind_spots` | The files the index cannot answer for, by reason (below) |

**Symbol lookup with detail (added 25 November 2026, roadmap D1).** The
indexer also stores, per definition, `body_digest`: the SHA-256 of the
definition's own bytes (its Tree-sitter node, without leading comments or
attributes), so an edit elsewhere in the file changes the file's `digest`
but not this one. It stores every call site `calls()` finds (ADR 0029) in a
`calls` table: workspace, path, the innermost definition whose lines hold
the call, callee name, line and argument count, replaced with the file's
other rows. The schema marker went from 2 to 3, so an older file is dropped
and rebuilt. A lookup with `detail` adds to each definition:

- `calls`: the distinct names called within its lines, with a count and the
  first line, first call first, at most 32;
- `callers`: how many call sites outside the definition have its name as
  callee, and the first 8 as `path:line` in path order. A call of the
  definition within itself is not counted;
- `notes`: remembered notes whose paths include its file, newest first, at
  most 4, with their ID and text. A workspace-wide note is not included;
- `last_check`: of the 8 newest conclusive check records of the workspace
  (ADR 0007), the newest whose tree holds the file's indexed content at that
  path (name, `passed` or `failed`, tree). The daemon reads the file again,
  only if it still has the indexed digest, computes its raw Git blob ID and
  compares it with one read-only `git ls-tree` per record, within 300 ms in
  all. A file Git stores through a clean filter never matches. Absent when
  none matched: absence claims nothing.

The answer carries `resolution: "name"`: calls and callers are matched by
name, never resolved to a definition. No source text other than signatures
and note text is returned. Definitions that would take the response past one
64 KiB frame are left out, and `truncated` says so. Without `detail` the
request and the answer are byte for byte what they were (golden vectors
`fixtures/graph-symbol-detail-*-v3.json`; test
`a_detailed_lookup_adds_calls_callers_digest_notes_and_check_and_is_rebuilt`
in `tests/graph.rs`).

Coverage is: supported files in the last scan, how many are indexed at their
current content, how many only partly parsed, and whether any scan exists.
An answer's results and its coverage are read from one SQLite snapshot, so
the coverage never counts a file the results were read without (added 25
November 2026, after CI caught a query pairing results read before the
indexer's next commit with coverage read after it). Test:
`coverage_describes_the_same_snapshot_as_the_results_it_comes_with` in
`src/graph.rs`.

**Blind spots (added 25 November 2026, roadmap D8).** Coverage counts say how
many files are partial, not which, so an empty answer could not be told from
one the index was unable to give. `graph blind-spots` (MCP
`langspan_blind_spots`) lists, each in path order and capped at `--limit`
(1 to 200, default 50) so a capped list is the same on every call, with full
counts in `totals`:

- `partial`: indexed with a syntax error or a bound reached, with `since_ms`,
  the wall-clock time its indexed versions became partial (kept across further
  partial versions, cleared by a clean parse);
- `never_clean`: indexed but never parsed cleanly, so it has no interface
  baseline and no notice can come from it;
- `waiting`: its scanned content is not indexed yet;
- `too_large`: over the index's 1 MiB bound, or over the scan's 4 MiB bound;
- `unscanned`: left out by the scan's file, byte or time bound;
- `unsupported_languages`: other source files the scan listed, by extension,
  from a fixed list of about 40 programming-language extensions
  (documentation, data and images are not counted).

Every answer's `coverage` gains `blind_in_scope`: supported files that are
partial, not indexed at their scanned content, or not read by the last scan.
Every query matches names across the whole workspace, so each of those files
could hold a match the answer lacks; the field is the count of them, omitted
when zero so earlier answers are byte for byte unchanged. Paths only; nothing
guesses what a blind file contains. With no complete scan the answer has
`scanned: false`, and the last scan failure, if one is recorded, is in
`scan_failure`. Lists past one 64 KiB frame are cut to the longest common
prefix that fits, with `truncated`.

To answer from one snapshot, the index keeps what the last complete scan did
not read: up to 4,096 such supported-language paths with the reason, the full
counts, and the language counts (tables `unread` and `scan_summary`; the paths
are rewritten only when they change). A file between 1 and 4 MiB is marked
too large for its scanned digest, so it is not retried every pass and no
longer takes one of a pass's 200 places, which could stall indexing behind
200 such files. A complete scan with no supported file now counts as
`scanned`. The schema marker went from 3 to 4, so an older file is dropped
and rebuilt. Past 4,096 unread files the counts still hold but those paths
are not named, and `blind_in_scope` may count one of them twice if the index
also holds an earlier row for it. Tests: `blind_spots_name_partial_never_clean_and_waiting_files`,
`a_file_over_the_parse_bound_is_too_large_and_not_waiting`,
`a_capped_list_is_the_same_on_every_call`,
`impact_on_a_file_mentioned_only_by_a_partial_file_counts_one_blind_file` and
`files_the_scan_did_not_read_and_other_languages_come_from_a_real_scan` in
`src/graph.rs`; `blind_spots_name_the_files_the_index_cannot_answer_for` in
`tests/graph.rs`; `stdio_blind_spots_names_a_partial_file_through_the_daemon`
in `tests/mcp.rs`; golden vectors `fixtures/graph-blind-spots-*-v3.json`.

**Interface-change notices.** When a file is re-indexed, its exported symbols
are compared with the previous version by name and kind. A removed symbol or
a changed signature produces a notice with the files that mention the name.
A session whose own or read files are among them is told at its next prompt,
with the before and after signatures and the session that last reported
editing the file. Bodies are not compared: a change of behaviour behind an
unchanged signature is not reported. A rename is reported as a removal; no
link to the new name is claimed. A first indexing reports nothing, and a
version that parsed only partly is not trusted to say a symbol is gone.

Signatures are compared in a normalised form and shown as written (added 24
November 2026, backlog item 15). The comparison removes whitespace next to
punctuation and drops a trailing comma before a closing bracket or at the end,
so a rustfmt, black, prettier or gofmt rewrap of a parameter list, a magic
trailing comma, a rewrapped `where` clause or spacing inside braces is not a
changed signature, while whitespace between two words, and any change to a
name, type or default, still is. The same rule holds for all four languages,
since in each a trailing comma in those lists is optional and spacing around
punctuation means nothing. One trailing comma is kept: the only comma of a
parenthesised group that is not a parameter or argument list (a parenthesis
not right after a name or a closing `>`), because it makes a one-element
tuple: Python's `(1,)` and `(1)`, or Rust's `(u32,)` and `(u32)`, differ.
Tests:
`a_formatter_rewrap_of_a_signature_is_not_a_signature_change_and_a_parameter_change_is`
and `a_one_element_tuple_losing_its_comma_is_a_signature_change_and_a_wrapped_one_is_not`
in `crates/langspan-graph/tests/extract.rs`.

**Code briefing at a prompt (added later).** At `UserPromptSubmit`, for both
hosts, the hook process takes candidate names from the prompt: ASCII words of
3 to 64 letters, digits and underscores not starting with a digit, a fixed
list of common words skipped, distinct, at most 12. Code-shaped words (an
underscore, a digit, an inner capital, a call, a qualified path such as
`a::b` or `a.b`, or backticks) are taken before plain words. Only the first
16 KiB of the prompt is read. The names go to the daemon in one read-only
command, `graph_context`, which the core bounds (1 to 12 distinct names, 1 to
3 definitions each) and the daemon answers from the index like the other
queries: never an event, a receipt or a log line. For each name with at
least one definition it returns up to 3 definitions and up to 5 distinct
files that mention the name, with coverage. The hook adds a section after the
peer briefing, under a header saying it comes from the source index, is
matched by name and not resolved, and is data, not instructions. The code
section is sized first, up to 1,000 bytes, and the peer briefing gets the
rest of the 2,400-byte cap; a name that does not fit is counted. It is sent
at every prompt that names a defined symbol, whether or not peer news
changed, and is not remembered as delivered. Nothing is added when the index
has no complete scan or nothing matched, and any failure adds nothing.
`--code-briefing off` on the hook, or on `hooks install`, turns it off.

## Consequences and limits

- **Dependencies.** Tree-sitter and four grammars are compiled C, linked into
  the binary. Rows are in [the dependency notes](../dependencies.md).
- The index lags the working tree by one scan plus one indexing pass. With N
  workspaces a scan comes round about every N × 2 s.
- The scan versions at most 4,096 files per workspace. A larger repository is
  scanned partially (ADR 0003): changed files first, then clean ones in path
  order. The index holds the files the scan versioned; a file a partial scan
  left out keeps what was last indexed for it and is not reported removed.
- Impact is one level: files mentioning the names a file exports. It does not
  follow chains, and import targets are stored but not yet used to narrow
  matches.
- Notices cover one physical workspace. A teammate on another machine or in
  another worktree is not yet told of an interface change.
- False positives are expected for common names. False negatives occur for
  dynamic uses, reflection, string-built names, macros and generated code.
- Formatter changes the normalised comparison does not absorb still read as
  a changed signature: prettier adding parentheses around a lone arrow
  parameter, a change of quote style in a default string, or a comment inside
  a parameter list. Conversely, spacing inside a string default is ignored.
- Other languages are not indexed and are not counted in coverage.
- The code briefing matches words, not meaning. A prompt about "the upload
  flow" gets the definitions of every symbol named `upload`, including a
  module and a function of that name. A plain English word that is also a
  symbol name (`start`, `open`) matches too. A name outside the first 12
  candidates, or past the first 16 KiB, is not looked up.
- The code briefing is not counted as delivered, so a session that repeats a
  name at every prompt is told every time, at up to 1,000 bytes of context
  each time.
- Repository source is parsed by C code generated from grammars. A parser bug
  is a memory-safety risk in the daemon; see the threat model.
