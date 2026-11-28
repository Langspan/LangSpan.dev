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
(including TSX/JSX), Go, Java and C#, chosen by file extension. It does no I/O. Inputs
over 1 MiB are refused; a file with a syntax error is indexed as far as it
parsed and marked partial.

**Java** (`.java`, added 25 November 2026 for roadmap D6,
[issue #55](https://github.com/Langspan/LangSpan.dev/issues/55)). Classes,
records and enums are types; interfaces and annotation types are interfaces;
methods, constructors and annotation elements are methods, since Java has no
free functions; enum
constants and `static final` fields (every interface field) are constants, one
per declarator; a `module-info.java` module is a module. Packages are not
symbols. The export rule is what another package can reach: `public` or
`protected`; an interface's or annotation type's members unless `private`;
every enum constant and module. Package-private and `private` are not
exported, nor is a method of an anonymous class or of an enum constant's body.
Each declaration is judged alone, so a `public` method of a package-private
class is exported, as a `pub fn` in a private Rust module is. A definition
starts after the annotations and comments that lead its modifiers, and its
signature leaves those annotations out: adding `@Override` or `@Deprecated`
changes no caller. A constant's signature is its modifiers, type, name and
any array dimensions after the name (`int IDS[][]`). An annotation element's
signature ends before its `default` value. An
import is recorded as written after `import` or `import static`, with any
`.*`. References are identifiers and type names. Calls are method calls,
member calls (`a.f(x)`, `this.f()`, `super.f()`) and constructor calls
(`new T(x)`, named by the type's last identifier); `this(x)` and `super(x)`
are not. Tests: the Java tests in `crates/langspan-graph/tests/extract.rs`
and `calls.rs`, and
`a_java_repository_is_indexed_and_a_changed_method_is_told_to_its_callers_session`
in `tests/graph.rs`.

**C#** (`.cs`, added 26 November 2026 for roadmap D6,
[issue #55](https://github.com/Langspan/LangSpan.dev/issues/55)). Classes,
structs, records, enums and delegates are types; interfaces are interfaces;
methods, constructors and properties are methods, since C# has no free
functions; enum members and `const` or `static readonly` fields are
constants, one per declarator. Namespaces, block or file-scoped, are not
symbols, and a namespace's name is not a mention. The export rule is what
another assembly can reach: `public`, `protected` and `protected internal`.
`internal`, `private protected`, `private`, `file` and no modifier are not
exported, since a type defaults to `internal` and a member to `private`. An
interface's members are exported unless `private` or `internal`, and every
enum member is. An explicit interface implementation (`void IPlacer.Cancel()`)
is not exported: callers reach it through the interface's own member. Each
declaration is judged alone, as in Java. A definition starts after the
attributes and comments that lead it, and its signature leaves those
attributes out, so adding `[Obsolete]` changes no caller. A constructor's
signature ends before its `: base(...)` or `: this(...)` initializer. A
constant's signature is its modifiers, type and name, without the value. A
property's signature is its declaration and its accessors without their bodies
or an initial value (`public int Count { get; private set; }`), and `{ get; }`
for an expression-bodied property, so removing a setter is an interface
change. A method's or constructor's block or `=> expression` is its body for
a rename (D7); a property has none, so a renamed property is linked only by
its lines. An import is recorded as written after `using`, `using static` or
an alias's `=`. References are identifiers. Calls are method calls, member
calls (`a.F(x)`, `a?.F(x)`, `F<T>(x)`) and constructor calls (`new T(x)`,
named by the type's last identifier); a target-typed `new(x)`, an object
initializer without parentheses and a `: base(x)` initializer are not.
Tests: the C# tests in `crates/langspan-graph/tests/extract.rs`, `calls.rs`
and `renames.rs`; in `tests/graph.rs`,
`a_csharp_repository_is_indexed_and_a_changed_method_is_told_to_its_callers_session`
and
`a_committed_csharp_rename_in_another_worktree_is_told_until_it_is_merged`;
`a_csharp_method_call_that_no_longer_fits_is_told_at_the_write` in
`tests/break_alerts.rs`; and
`stdio_check_write_reports_a_peers_csharp_signature_and_body_changes` in
`tests/mcp.rs`.

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
| `graph symbol WORKSPACE NAME [--detail] [--rank [--session ID]]` | `langspan_symbol` (`detail`, `rank`, `session_id`) | Definitions of that name; with detail, what is around each (below); ranked, the code briefing's order with each one's dependents (below) |
| `graph references WORKSPACE NAME` | `langspan_references` | Mentions of that name |
| `graph impact WORKSPACE PATH [--depth N]` | `langspan_impact` (`depth`) | What the file exports, and other files mentioning those names; with a depth of 2 or 3, those files level by level (below) |
| `graph blind-spots WORKSPACE` | `langspan_blind_spots` | The files the index cannot answer for, by reason (below) |
| `graph history WORKSPACE NAME` | `langspan_history` | The recorded interface changes of that name, from the event log (below) |

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

Lookup answers' `coverage` gains `blind_in_scope`: supported files that are
partial, not indexed at their scanned content, or not read by the last scan.
Every query matches names across the whole workspace, so each of those files
could hold a match the answer lacks; the field is the count of them, omitted
when zero. Earlier builds read coverage with `deny_unknown_fields`, and the
protocol has no negotiation, so an older CLI or a long-running MCP adapter
would fail to read any answer that carried the field. As with symbol
`detail`, the client asks: `graph_symbol`, `graph_references` and
`graph_impact` take `blind_in_scope: true` (sent by this build's CLI and MCP
tools), and `graph_blind_spots` always gets it. Every other answer that
carries coverage (code context, call breaks, the dashboard) and every request
without the flag gets the shape earlier builds know, the field absent; the
daemon applies this in one place before it replies. Bumping the protocol
version instead would have broken every command of an older client against
this daemon, not only these. A request with the flag sent to an earlier
daemon is refused as an unknown field, like `detail`; restart the daemon
after upgrading. Test: `blind_spots_name_the_files_the_index_cannot_answer_for`
(`tests/graph.rs`) reads every such answer with a strict copy of the earlier
coverage, over the daemon's socket; it failed before the change. Paths only; nothing
guesses what a blind file contains. With no complete scan of the workspace's
current registration ([ADR 0025](0025-distribution-lifecycle-and-store-beyond-the-first-release.md))
the answer has `scanned: false`, and the last scan failure, if one is recorded, is in
`scan_failure`. Lists past one 64 KiB frame are cut to the longest common
prefix that fits, with `truncated`.

To answer from one snapshot, the index keeps what the last complete scan did
not read: up to 4,096 such supported-language paths with the reason, the full
counts, and the language counts (tables `unread` and `scan_summary`; the paths
are rewritten only when they change). A file between 1 and 4 MiB is marked
too large for its scanned digest, so it is not retried every pass and no
longer takes one of a pass's 200 places, which could stall indexing behind
200 such files. The mark is set only after the file's bytes, read up to the
scan's 4 MiB bound, are confirmed to be that digest: a file scanned small that
grew before the pass ran is left for the next scan, so its scanned version is
not skipped if it returns (`a_small_scanned_version_is_not_marked_too_large_by_a_later_large_file`). A complete scan with no supported file now counts as
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

**Impact level by level (added 25 November 2026, roadmap D2).**
`graph_impact` takes `depth`, 1 to 3 (`MAX_IMPACT_DEPTH`); anything else is
refused with "depth must be between 1 and 3". Above 1 the answer adds
`levels`: level 1 is the files mentioning a name the queried file exports,
and level n+1 the files mentioning a name a level-n file exports. Each file
appears once, at its first level, with `via` (the exported name it mentions)
and `from` (the previous-level file; the queried file for level 1), so a
cycle never brings the queried file or an earlier level back. Files are
reached in the previous level's order, names in name order. Bounds: 200
files across all levels (`MAX_RESULTS`), 50 distinct exported names expanded
per level (`NAMES_PER_LEVEL`), and the answer must fit one 64 KiB frame;
a level any of these cut, and every level after a frame cut, is `truncated`.
The levels are read in the same snapshot as the exports, references and
coverage (`with_coverage`). They may take 500 ms (`IMPACT_BUDGET`): the daemon
answers one request at a time, so hooks behind a long query wait; past it the levels finished so far and the one
in progress are returned, the last `truncated`. The answer then also has
`resolution: "name"`, and the CLI says "Found by name, not resolved; each
level adds false positives". An empty level is no claim that nothing is
affected: with blind or lagging files in `coverage`, the CLI says so. At
depth 1 the field is left out of the request and `levels` and `resolution`
out of the answer, which is byte-for-byte the earlier one, and the request
is the one earlier builds send. Nothing is stored. Tests:
`impact_levels_follow_a_caller_of_a_caller_and_name_the_way`,
`a_file_appears_once_at_its_first_level`,
`impact_levels_expand_at_most_fifty_names_per_level_and_say_so`,
`impact_levels_stop_at_two_hundred_files_in_total`,
`a_passed_deadline_returns_the_levels_so_far_marked_truncated` and
`levels_and_coverage_come_from_one_snapshot` in `src/graph.rs`;
`impact_levels_with_long_paths_are_cut_to_one_frame_and_say_so` in
`src/daemon.rs`;
`impact_by_level_reaches_a_caller_of_a_caller_and_depth_one_is_unchanged`
in `tests/graph.rs`; `stdio_impact_with_depth_returns_levels_by_name` in
`tests/mcp.rs`; the request vectors in `tests/protocol.rs`.

**Paths between symbols and a repository map (added 26 November 2026,
roadmap D3).** Two read-only queries use four edge kinds, each with a
`resolution`. The kinds are `defines` (file to symbol), `calls` (a
definition to the names called within its lines, from the `calls` table),
`imports` (file to target as written) and `mentions` (file to name, the
`refs` table that the earlier queries use).

- **Import resolution.** `langspan_graph::resolve_import` resolves an import
  to a workspace file by path alone: the target as written, the importing
  file's path and the paths the last complete scan listed. It covers
  TypeScript and JavaScript relative specifiers (as written, the `.ts` file a
  `.js` name is emitted from, each script extension, then `index.*`), Python
  relative modules from the importer's package and absolute ones under each
  directory above it, and Rust `crate::` (from the nearest `lib.rs` or
  `main.rs` above), `super::`, `self::` and bare paths, longest module
  prefix first. A resolved import has `resolution: "path"`. Go, Java and C#
  name packages and namespaces, not files, and no configuration file,
  manifest, alias or search path is read, so those imports stay as written.
  The issue proposed an `imports_resolved` column; resolution runs at query
  time instead, against the current file list. A stored column would go
  stale when the target file appears or disappears after the importer was
  indexed, and the index schema stays version 4. Nothing new is stored.
- **`graph path WORKSPACE FROM TO [--max-hops N]` (`graph_path`,
  `langspan_path`).** A breadth-first search from every function, method or
  type named `FROM`, over the calls within each definition's lines, to one
  named `TO`. A call of a name reaches the definitions of that name in files
  the calling file imports by path, when there are any (`resolution:
  "path"`); otherwise it reaches every function, method or type of the name
  (`"name"`). The imported files' definitions are read on their own, so
  other files' definitions of a common name never push one of them past the
  bound. More than one reached is `ambiguous`, and each is a separate
  chain. A module, constant or interface of the name is never reached, so
  `mod service;` is no second `service`. A type's step follows every call in
  its lines, its methods' among them, since a constructor call is not told
  apart from the rest. Each definition is visited once, at
  its first hop, and every shortest way into it is kept, so a cycle ends and
  equally short chains are all found. The answer lists at most 5 chains
  (`MAX_PATHS`), sorted by their steps. `N` is 1 to 6 (`MAX_PATH_HOPS`,
  default 4); anything else is refused with "max_hops must be between 1 and
  6". Each step names its file, the call's line (the definition's line on
  the last step), the definition, its `callee`, its `resolution` and
  `ambiguous`. The search visits at most 10,000 edges (`MAX_PATH_EDGES`): one
  per definition a call reaches, and one per called name that reaches none.
  It may take 500 ms (`PATH_BUDGET`), checked before each called name is
  read, and the answer must fit one frame.
  Past any of these, or with more than 5 chains, it returns what it found
  with `truncated: true` and the `edges_visited` count. A name of more than
  128 bytes or with whitespace is refused.
- **`graph map WORKSPACE [--depth D]` (`graph_map`, `langspan_map`).** A
  file's area is its module path cut to its first `D + 1` parts (`D` is 1 to
  3, `MAX_MAP_DEPTH`, default 1). The module path is the file's path without
  its last extension (and without `.d.ts` for a declaration file), or its
  directory for a Go file (a package is a directory) and for `mod.rs`,
  `lib.rs`, `main.rs`, `index.*` and `__init__.py`. So `src/api.ts` and
  `src/api/handler.ts` are both `src/api` at depth 1, which a plain directory
  cut could not give for the first, while `src/api.v1.ts` and `src/api.v2.ts`
  stay two areas. Each area has its file
  count, exported definitions and languages. Edges between two different
  areas are counted per kind: `imports` resolved by path, and `calls` by name
  of a name with exactly one function, method or type definition (a common
  name would otherwise draw edges to every area defining it). At most 64
  areas in path order (`MAX_MAP_AREAS`) and 256 edges by count
  (`MAX_MAP_EDGES`). It reads at most 200,000 import rows and as many call
  rows (`MAP_ROWS`); past any bound, or the frame, it is `truncated`.

Both queries read one snapshot with their coverage (`with_coverage`), so a
workspace without a complete scan answers `coverage.scanned: false`. Both
always send `blind_in_scope`. Neither type-checks, resolves a receiver, or
sees dynamic dispatch, macros or generated code. Tests: the resolution tests
in `crates/langspan-graph/tests/resolve.rs`;
`more_shortest_chains_than_the_limit_are_cut_and_said_to_be`,
`a_cycle_is_visited_once_and_the_start_can_be_the_end`,
`a_chain_longer_than_the_hops_asked_for_is_not_found` and
`a_map_area_is_a_module_path_cut_to_its_depth` in `src/graph.rs`;
`a_call_chain_from_handler_to_save_is_found_in_each_of_four_languages`,
`a_call_chain_is_printed_one_step_per_line_with_how_each_was_found`,
`a_chain_through_a_name_with_two_definitions_is_marked_ambiguous`,
`a_relative_typescript_import_resolves_by_path_and_the_map_counts_its_edges`,
`max_hops_outside_one_to_six_is_refused_with_the_allowed_range` and
`a_path_search_capped_at_its_edge_bound_says_truncated` in
`tests/graph.rs`; `stdio_path_and_map_follow_calls_and_imports` in
`tests/mcp.rs`; the golden vectors `fixtures/graph-path-*-v4.json` and
`fixtures/graph-map-*-v4.json`. Not yet called on a host.

**Interface-change notices.** When a file is re-indexed, its exported symbols
are compared with the previous version by name and kind. A removed symbol or
a changed signature produces a notice with the files that mention the name.
A session whose own or read files are among them is told at its next prompt,
with the before and after signatures and the session that last reported
editing the file. Bodies are not compared: a change of behaviour behind an
unchanged signature is not reported. A rename is reported as a removal; no
link to the new name is claimed, unless nothing but the name changed (added
25 November 2026, roadmap B10): the only exported function or method of a
name went and the only one of a new name came in the same file transition,
with the same kind, the same signature apart from the name and the same body
digest. That is one `renamed` change under the old name, with the new name
in `after_name`, told as "function "scale" was renamed to "zoom": …"
([ADR 0029](0029-break-alerts-at-write-time.md) has the rule and its limits).
A removal now also carries the signature it had. A first indexing reports nothing, and a
version that parsed only partly is not trusted to say a symbol is gone.

Rename tracking (added 26 November 2026, roadmap D7) widens the link and
records why it was made. A rename's `evidence` is `same_body` (the rule
above), `same_signature` (a changed body, but the same signature apart from
the name and the same first and last line; each baseline definition now keeps
its lines in two nullable columns, so an index from before stays and tells
its next such change as a removal and an addition) or `file_moved`. A
removed definition's candidates are the added ones of its kind with the same
signature apart from the name and its body digest or its lines; a pair is
linked only when each is the other's one candidate. The notice says the
evidence: "function "fetch_user" was renamed to "load_user" (same body)".
A file moved whole is found as Git finds an exact rename: in one scan, a
removed path whose indexed content parsed cleanly and a path new to the index
with exactly that content and the same language, neither content shared by
another removed or new path. Its rows move to the new path and are not parsed
again, and one notice at the new path lists each export as `renamed` with
`file_moved`, the same name and its old path in `before_path`. A file with no
exports lists its private definitions instead, marked `private`: history only,
told to no dependent and never held, so that the file's notes and history
follow it too. A file with no definitions at all, such as an import-only
module, is listed as itself: one private change of kind `file`, named by its
file name. A file changed
as well as moved stays a removal and a first indexing: similarity is not
scored. The coordinator moves notes on the old path to the new one, and
`graph history --path NEW` includes what was recorded under earlier paths,
each only while the file was there: the moves are followed in event order, so
a later file at a path this one left is never part of its history. The store
reads the newest 1,000 moves through a partial index, whatever a page's
cursor, so every page follows the same lineage.
A notice's request ID includes the index pass that found it, so a move or
content change that repeats an earlier one (`a → b → a → b`) is recorded
again, and a redelivery of one notice is still a replay.
`graph symbol OLD --renamed` follows recorded renames of one kind to the
newest name. A
rename across files other than such a move is never inferred.
Earlier builds read interface changes and history entries with
`deny_unknown_fields`, and status, briefings, recaps, history and the
dashboard all carry them, so a per-command flag like `blind_in_scope` would
not cover every answer. The wire protocol is version 4 instead: version 3
with `before_name`, `evidence`, `before_path` and `private` in interface
changes and `after_name`, `evidence` and `before_path` in history entries.
Requests are the same in both. The daemon answers a version 3 request in
version 3, without those fields and without private changes. The shape is
chosen in one place, the protocol adapter, as the reply is encoded, so the
pure core keeps one serialized form. A client of this build sends
version 4, and when a daemon of an earlier build refuses it with
`unsupported_version`, which that build does before reading the command, it
sends the same request once in version 3. Tests:
`a_protocol_3_client_reads_answers_after_a_rename_and_a_file_move`
(`tests/graph.rs`) reads status, a briefing, two histories, a recap and the
dashboard over the daemon's socket with strict copies of the earlier
structures (it failed before the change);
`v4_rename_history_response_and_its_version_3_form_match_the_golden_vectors`
and `a_version_3_reply_leaves_out_rename_fields_and_private_changes`
(`tests/protocol.rs`); and
`a_daemon_of_an_earlier_build_is_asked_again_in_its_version`
(`src/client.rs`). Version 5 (roadmap H9) later added `refusal_outcomes` to
a report's counts in the same way; a client now asks an earlier daemon again
in the version it answers in, 4 or 3
([ADR 0019](0019-person-facing-surfaces-and-usage-accounting.md)).

Signatures are compared in a normalised form and shown as written (added 24
November 2026, backlog item 15). The comparison removes whitespace next to
punctuation and drops a trailing comma before a closing bracket or at the end,
so a rustfmt, black, prettier or gofmt rewrap of a parameter list, a magic
trailing comma, a rewrapped `where` clause or spacing inside braces is not a
changed signature, while whitespace between two words, and any change to a
name, type or default, still is. The same rule holds for every indexed
language, since in each a trailing comma in those lists is optional or, in
Java's and C#'s parameter lists, not allowed, and spacing around punctuation
means nothing. A google-java-format or `dotnet format` rewrap of a parameter
list is therefore not a changed signature, and a changed parameter type is. One trailing comma is kept: the only comma of a
parenthesised group that is not a parameter or argument list (a parenthesis
not right after a name or a closing `>`), because it makes a one-element
tuple: Python's `(1,)` and `(1)`, or Rust's `(u32,)` and `(u32)`, differ.
Tests:
`a_formatter_rewrap_of_a_signature_is_not_a_signature_change_and_a_parameter_change_is`
and `a_one_element_tuple_losing_its_comma_is_a_signature_change_and_a_wrapped_one_is_not`
in `crates/langspan-graph/tests/extract.rs`.

Several definitions can share a name and kind in one file, such as a `text`
method on each of two types; the comparison takes their signatures as a set
with repeats (added 25 November 2026, issue #118). When a definition is added
and every earlier signature is still there, the change is
`definition_added`; when one goes and every remaining signature was there
before, it is `definition_removed`. Either carries the signatures that came
or went, each shown once, and two counts, `before_count` and `after_count`
(at most 65,535), so the notice reads "a second method "text" was added: …
(now 2 definitions with that name)" or "one of 2 methods "text" was
removed: … (now 1 definition with that name)", never a signature "changed"
to itself. Reordering same-named definitions is no change. Any other
difference is a changed signature, as before, with every signature of that
name before and after; losing the last definition is a removal. Older
notices have no counts and read as they did. A store holding one of these
changes does not open in an earlier build
([ADR 0015](0015-version-tolerance.md)); the protocol stays at version 3.
Tests: `a_second_same_named_definition_is_an_addition_and_losing_one_is_a_removal`
in `crates/langspan-graph/tests/extract.rs`,
`a_second_same_named_definition_is_told_as_added_and_losing_it_as_removed`
in `tests/graph.rs`, and `crates/langspan-core/tests/interface_counts.rs`.

**A definition's history (added 25 November 2026, roadmap D4).** The index
keeps only a file's current version and baseline, but every notice is an
`interface_changed` event in the append-only log. `graph history WORKSPACE
NAME [--path PATH] [--limit N] [--before SEQ]` (MCP `langspan_history`, op
`history`) reads them back for one name, newest first, at most 50 entries
(default 10). It is answered by the daemon's store from the log, not by the
index, and decided by the core (`crates/langspan-core/src/history.rs`): it
records nothing and delivers nothing. Each entry is one change of that name
in one notice: `seq`, `age_ms`, `change`, `kind`, `path`, `before`, `after`,
`by_session`, `task`, `notes`, `messages` and `reverted`.

- `by_session` is what the notice already holds: the last session that
  reported editing the file before the change was indexed. Every answer's
  `attribution` says it is a correlation, not proof of authorship.
- `task` is that session's newest `session_prompted` summary before the
  change. A session with prompt summaries off records none, so none is shown.
- `notes` and `messages`, up to 4 each, are `remembered` and `message_sent`
  events of the workspace whose paths include the file or whose text names
  it, placed within 30 minutes either side of the change. Placing them needs
  a time, which those events did not carry: they now record `at` (the
  daemon's clock and generation) as an optional field, so earlier records
  still replay and are simply never placed. Two times of one generation are
  compared on its clock; of two generations, in wall time through each
  generation's `clock_anchored` event, as `today` does. A note with a later
  `forgotten` event is not shown.
- `reverted` is true when `after` equals the `before` or `after` of an
  earlier recorded change of the same name and kind in the same file: the
  change put back a signature the definition had. Only the events one answer
  reads are compared.

Two notice changes serve it. A new exported name is now recorded as `added`,
with its signature in `after` and no parameter counts; it is never news to a
dependent, so replay does not hold it in the briefing state (briefings,
recap, break alerts and the dashboard are unchanged), and a notice of
additions alone does not replace the file's held notice. A removal now
carries the removed signatures in `before`, so a definition removed and then
added back is marked reverted; parameter counts stay on signature changes
only (ADR 0029). The signatures that come or go are joined as a notice shows
several, as many as fit its 256 bytes, so they never make the core refuse
the notice and the changes in it. As with `definition_added`, a store
holding an `added` change, or a note or message with `at`, does not open in
an earlier build ([ADR 0015](0015-version-tolerance.md)); the protocol stays
at version 3.

The store reads the newest 20,000 events of the five kinds a history uses
before `before_seq` through a partial index, `events_history` (created when
a store opens, like the generation-marker index), the notes, forgettings and
messages after that point when paging, and every clock anchor. Whether a note
it read was forgotten is looked up by the note's ID through a second partial
index, `events_forgotten`, never within those capped reads, so a forgotten
note's text does not come back however many events follow it. Likewise
whether a message it read was withdrawn is looked up by the message's ID
through `events_retracted`, and a withdrawn message is left out. An answer is
cut, a whole event's entries at a time and oldest first, with `truncated`
and a `next_seq` to pass as `before_seq`: past the limit, past 28 KiB (the
MCP result carries it twice, and the adapter trims again against one frame,
as recall does), or when the log holds older events than those read. Events
have no retention policy; when one is set, history follows it. Body-only
changes, changes before LangSpan watched the repository and commit messages
are out of scope, and Git is not read. Tests: the history tests at the end
of `crates/langspan-core/tests/recap.rs` over constructed logs, and
`a_definitions_history_shows_who_changed_it_why_and_that_it_was_reverted` and
`history_requests_match_the_committed_golden_vector`
(`fixtures/history-request-v3.json`) in `tests/graph.rs`, with a real daemon,
index and repository changed across several scans, the CLI and the MCP tool;
`history_reads_only_the_events_it_uses` in `src/store.rs` checks that the
query plans use the partial indexes, and
`paging_into_old_history_never_shows_a_forgotten_note` there pages into a
change whose note is forgotten after 20,000 following messages, and
`history_never_shows_a_withdrawn_message` does the same for a message
withdrawn straight after the change and after 20,000 following messages. Not yet called
on a native host.

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

**A ranked code briefing (added later, roadmap C2).** The hook sends
`graph_context` with `rank: true`, its own session ID and up to 12
qualifiers from the same 16 KiB of the prompt: relative paths
(`src/store/upload.rs`), source file names (`upload.rs`) and the leading
parts of qualified names (`store` in `store::upload` or `store.upload`). The
qualifiers are used for that query only and, like the names, never recorded.
The core ranks each name's definitions by fixed rules, each outranking the
next, with no learned weight:

1. A file the asking session edited or read (its touched paths, held leases
   and recorded source versions, ADR 0005).
2. A file a live peer is editing: a peer counted as in a briefing (this
   generation, the same workspace or a linked worktree of its repository,
   not exited, not ended, not the asker's parent), holding the file or
   having edited it within the 30-minute briefing horizon. The most recent
   such peer is named.
3. An exported definition.
4. A file whose path the prompt names: a qualifier equal to a directory on
   the path, the file name or its stem, or a whole-component run of the path.
5. Path order, then line.

Each definition in the answer carries `rank_reasons` (`own_file` with
`edited`, `peer_editing` with `session_id`, `exported`,
`qualified_in_prompt`) and `dependents`: up to 5 files other than its own
that mention its name, each with its count of `mentions`, ranked by rules 1,
2 and 4, then by most mentions, then by path. `dependent_count` and the
name's `definition_count` count what was left out. The daemon ranks among
the first 256 definitions in path order and the first 256 in the files the
session or a peer works on, taken by rules 1 to 3 and then path order, so a
peer's file of many same-name definitions never crowds out the session's
own; and among the 256 most-mentioning files and every file they work on.
Past that a definition or file is counted, never ranked. A code-context
answer that would exceed one 64 KiB frame is cut until it fits: first each
definition's dependents, one fewer at a time, then each name's mentioning
files, then each name's lowest-ranked definitions down to one, then the last
names. `dependent_count` and `definition_count` keep their values, and a name
that loses a definition or a file is `truncated`. The
section puts the top definition first with a short reason, such as `(you
edited this file)`, and `used by` with its ranked dependents and their
counts; later definitions follow as `also`. A line is built from whole parts
within 500 bytes: a dependent or definition that does not fit is counted (`and
3 more file(s)`, `2 more definition(s) not shown`), never cut, and a name
that does not fit is counted on the closing line as before. Paths and
signatures are capped at 160 bytes each. Without `rank`, `graph_context`
answers exactly as before. `graph symbol --rank [--session ID]` and
`langspan_symbol` with `rank` and `session_id` give the same order outside a
prompt, with up to 200 definitions in one frame. Nothing is resolved: a
dependent mentions the name and may mean another symbol of it.

**Another worktree's in-flight signatures (added later, roadmap C7).** The
observer keeps, for each file that differs from the worktree's `HEAD`, the
blob ID that `HEAD` holds for it, if any. After each indexing pass the indexer
compares each such file, once it is indexed at its scanned content, with that
blob: it reads the blob raw with `git cat-file blob` (no filters, within the
1 MiB parse bound and the pass's deadline, at most 200 reads per pass) and
parses it like any file. The exported symbols of the working version that
the `HEAD` version lacks, or exports with a different signature, are the
file's in-flight set. Signatures are compared by their tokens as a reformat
is ([ADR 0011](0011-shell-writes.md)): spacing between tokens aside, but a
string literal's whitespace counts. Every indexed language is split into
words, Go, Java and C# by the TypeScript rules, so removing spacing never joins
two tokens (`chan int` is not `chanint`). Every outcome of reading a file
for the index or for this comparison is terminal for that version or tried
again with a bound. A `HEAD` blob over the parse bound, or one that does not
parse completely, says nothing and is remembered until the file or `HEAD`
changes. A working file or blob that could not be read for now (removed,
changed, not a regular file or not readable since the scan, or a timeout or
missing object in Git) is tried again after a wait that doubles from one
pass up to 32, and given up on for that version after 10 failures. Versions
not tried yet are read before any retry, so files that keep failing never
take a pass's budget from the others. The retries live in `graph.sqlite3`
(`retries`, with each workspace's pass count in `passes`). A
file new since `HEAD` puts all its
exports in it. The set lives in `graph.sqlite3` (`inflight` and
`inflight_files`, the latter with the indexed digest and `HEAD` blob it was
computed at), added without a schema change, so an existing index computes
it at its next pass. An in-flight symbol that the interface diff from the
`HEAD` version finds renamed (roadmap D7) keeps its old name
(`inflight.renamed_from`, added the same way). Files that mention either
name are told, and the line names both, so a peer still calling the old
name learns of the rename before its next write as well as at it. A file found clean again, or gone, has none. A partial
parse at either version says nothing. When the daemon answers `Brief`, it
reads at most 256 in-flight symbols of the other worktrees of the asking
workspace's repository, in worktree, path and name order, and compares them
with the asking workspace's index. Each briefing reads on from where the
session's last one found nothing to offer, or from just before the first
symbol it offered, and wraps to the first symbol after the last. So a
symbol past the first 256 is reached once those before it are told or not
mentioned, and no one is read again before the others have had their turn.
The place is kept in the daemon's memory only; a restart starts again from
the first symbol. A symbol counts when one of the session's own
files (edited, held, declared or read) mentions its name, or imports a path
whose last part is the name or the file's module name: the file name without
its extension, or the directory for Go and for `mod.rs`, `lib.rs`, `main.rs`,
`index.*` and `__init__.py`. A symbol also counts when the session's own
copy of the file that defines it is one of those files: a session writing a
new caller reads the definition before any file of its own mentions the name
(issue #194). Symbols already told to the session are left
out, and at most 32 go to the core in a prepared command that only the
daemon builds, like `WorkspaceRegisterVerified`. The core names the worktree
root and the session that last reported editing the file, and selects at most
3 within 600 bytes by `inflight_cost`, never more than what an adapter
renders. It selects none for a briefing that carries a message, a withdrawal,
an uncoordinated change or a check result: those take the briefing's room
and are each delivered once, so the in-flight lines wait, counted. A
delivered briefing records the keys of the signatures it told
(`InflightDelivered`), at most 1,024 per session, so each signature is told
once and a changed one again. A session told more than 1,024 signatures
that are all still in flight forgets the oldest, which is told again when
the window next reaches it. `graph inflight WORKSPACE` answers the same
comparison for any file of the workspace, read-only, from the first 128
symbols of each other worktree and 256 in all. Neither a worktree nor
an asking workspace without a complete scan contributes anything; an empty
answer still carries the asking workspace's coverage. A briefing whose
comparison met such a worktree, or whose own workspace has none, says in one
line that the index has not finished scanning every worktree yet, instead of
saying nothing (issue #262); the line is not news by itself, so it rides with
a briefing delivered for another reason. It is the briefing's
`inflight_partial`, which protocol version 8 added; clients of version 7
or earlier are not sent it. Its answer keeps the
leading items that fit one response frame and is marked `truncated` when any
are left out. Tests:
`another_worktrees_uncommitted_signature_is_told_to_the_session_whose_file_mentions_it`,
`in_flight_lines_keep_to_their_share_and_the_rest_are_counted_and_told_later`,
`an_empty_inflight_answer_still_reports_the_asking_workspaces_coverage`,
`a_changed_string_default_is_in_flight_but_respacing_is_not`,
`a_go_channel_type_joined_into_one_name_is_in_flight`,
`every_inflight_signature_past_the_briefing_window_is_told_once`,
`inflight_signatures_behind_unmentioned_ones_are_told_in_a_later_briefing`,
`many_long_inflight_notices_are_trimmed_to_one_response_frame` and
`a_head_version_that_could_not_be_read_is_compared_on_a_later_pass`
in `tests/graph.rs`, the read-outcome and starvation tests after
`an_oversized_head_blob_is_remembered_for_that_version_and_not_read_again`
in `src/graph.rs`,
`a_codex_session_is_told_a_signature_claude_code_has_in_flight_in_a_linked_worktree`
in `tests/briefing.rs`, the in-flight tests in
`crates/langspan-core/tests/briefing.rs`, and for a partial index
`a_worktree_without_a_first_scan_makes_the_briefing_partial` (`src/daemon.rs`),
`a_partial_index_is_one_line_inside_the_cap` (`src/hook.rs`) and
`a_version_4_reply_leaves_out_a_partial_index` (`tests/protocol.rs`).

**Dependents in other worktrees (added later, issue #194).** A notice's
`dependents` are files of its own workspace, fixed when the notice is
recorded. A notice also keeps `elsewhere`: files of other worktrees of the
same repository, each `{workspace_id, path}`, that mention a changed name
(other than an addition), the notice's own path left out as in its own
worktree. After each scan of a worktree, the state owner reads that
worktree's index for the names of every held notice of the other worktrees
of its repository from this generation and within the briefing horizon, and
records the files not yet listed through `observe_interface_dependents`
(event `interface_dependents_found`), at most 32 per notice. The index
lags the scan by a pass, so a file written after the notice is listed a scan
or two later, and one that mentioned the name before the notice is listed at
the worktree's next scan. A replaced notice gains nothing. The paths are
relative to the other worktree's root. Nothing of this leaves the machine
([ADR 0035](0035-semantic-layer.md)). One command, one event, one outcome
(`interface_dependents_observed`) and the optional `elsewhere` field: a
store written by this build does not open in an earlier one
([ADR 0015](0015-version-tolerance.md)), earlier stores open in this one, and
the protocol stays at version 3. Tests:
`a_rename_notice_lists_a_caller_written_later_in_another_worktree` in
`tests/graph.rs`, and
`another_worktrees_files_are_added_to_a_held_notice_once_each_up_to_the_cap`
and `files_found_for_a_replaced_notice_change_nothing_when_replayed` in
`crates/langspan-core/tests/elsewhere.rs`. A file written after the change
was committed on its worktree's branch is listed the same way (issue #217,
`a_caller_written_after_a_committed_rename_is_listed_on_its_notice`); how
long such a change alerts at write time is in
[ADR 0029](0029-break-alerts-at-write-time.md).

**Export and import (added later, roadmap D5).** `langspan graph export
WORKSPACE --out FILE` writes what the index holds for the workspace's
current registration as JSON Lines; `langspan graph import WORKSPACE --in
FILE [--apply]` reads one back, as a dry run unless `--apply` is given. Both
are CLI-only, with no MCP tool, because they read or write a file the owner
chose. The format is `crate::index_file`: a header (`format`
`langspan-source-index`, `schema` 4, `workspace_root` as the root's last
component, `created_at` in Unix milliseconds) and one line per file indexed
at some version (`path`, `digest`, `partial`, `symbols`, `refs`, `imports`
and `calls`, rows in stored order). `calls` is in the line because detailed
lookups (D1) answer from it, so an imported index gives the same
`graph symbol --detail` answers. The state owner hands both
commands to the indexer thread, which stays the index's only writer and
replies itself; a connection waits up to 120 s for one, and the indexer
stops after 100 s. Export creates the file with `O_EXCL` and `O_NOFOLLOW` at
mode `0600`, and removes it on failure. Import refuses a header of another
format or schema and changes nothing. Each later line is parsed into typed
structures with the extractor's bounds (`MAX_SYMBOLS`, `MAX_REFERENCES`,
`MAX_IMPORTS`, `MAX_CALLS`, `NAME_BYTES`, `SIGNATURE_BYTES`); one over a bound
counts as `over_limits`, any other bad line, or a second line for one path,
as `malformed`. A file's rows load only when all of these hold: the SHA-256
of the file on disk equals the exported `digest`; the workspace's last
complete scan, which the indexer keeps per registration, lists the path; and
the index holds no version of the file yet. The first rule means an imported
row never describes content the checkout does not have. The second means the
next pass cannot report a loaded file's exports removed. The third means an
import never replaces what the local indexer parsed, so a change between two
local versions is still compared and noticed. A loaded file is written as a
first indexing would write it: its rows, its digest as both indexed and
scanned version, and for a clean parse the interface baseline, so the next
change to it is compared with the imported version. An import records no
interface notice. All loads commit in one transaction at the end, so an
error or the time limit leaves the index unchanged. An imported row is
trusted as far as its digest goes: a crafted export can claim other names
for content whose digest it knows, which misleads answers until the file
changes (see the threat model). Tests: `src/index_file.rs` (bounds, field
rules, header refusal, bounded line reading and a 20,000-case property test
of the parser), `an_import_never_replaces_a_version_this_index_parsed_and_sets_the_baseline`
and `a_path_missing_from_the_scan_is_not_imported` in `src/graph.rs`, and
`an_index_imported_into_a_fresh_state_directory_answers_the_same_without_a_parse`
and `only_files_with_the_exported_content_are_imported_and_the_rest_are_indexed_from_disk`
in `tests/graph.rs`. The second daemon there parses nothing
(`LANGSPAN_TEST_INDEX_FILES_PER_PASS=0`, a test-only variable), so its
answers can only come from the import.
Measured on 26 November 2026 with a release build, on a 4-CPU Intel Xeon at
2.10 GHz with 15 GiB of memory, Linux, Git 2.43.0: a repository of 4,050 Rust
files (three definitions each) reached full coverage 41.7, 41.9 and 41.8 s
after registration without an import (3 runs). Its export took 0.24 to
0.30 s and was 4,848,601 bytes. In a fresh state directory on the same
checkout, `graph import --apply`, retried until the first scan was in,
finished 0.9 s after registration in each run, with 4,050 of 4,050 files
current: 3,850 loaded, and 200 kept that the first pass had already parsed.

## Consequences and limits

- **Dependencies.** Tree-sitter and four grammars are compiled C, linked into
  the binary. Rows are in [the dependency notes](../dependencies.md).
- The index lags the working tree by one scan plus one indexing pass. With N
  workspaces a scan comes round about every N × 2 s. A workspace where a
  session acts (a registration, a prompt, a tool call) is scanned ahead of
  that order: at once, or 5 s after its last scan if that was sooner, so a
  busy workspace does not take every scan. That bound does not grow with N
  (issue #146).
- The scan versions at most 4,096 files per workspace. A larger repository is
  scanned partially (ADR 0003): changed files first, then clean ones in path
  order. The index holds the files the scan versioned; a file a partial scan
  left out keeps what was last indexed for it and is not reported removed.
- Impact is one level by default: files mentioning the names a file exports.
  With `--depth` it follows up to three levels of such name matches; each
  level compounds the false positives of the one before, and nothing is
  resolved. Import targets resolved by path narrow only `graph path` and
  `graph map` (roadmap D3); impact does not use them.
- A notice is recorded in one physical workspace, and its prompt briefing
  stays there. A session in another worktree of the repository is told of a
  renamed, removed or recounted function at the write that calls it
  ([ADR 0029](0029-break-alerts-at-write-time.md), issue #194), and of new or
  changed exports that are not committed (in flight), matched by name; once
  committed they are no longer in flight until they change again. The
  notice lists that worktree's files that mention the name in `elsewhere`.
  Other machines get none of these.
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
