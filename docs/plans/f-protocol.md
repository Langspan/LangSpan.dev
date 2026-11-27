# Plan: f-protocol (#331, #332 via #411, #413, #414)

## What is open

- #331: frozen event-type lists stop at 0.5.1. Releases 0.6.0, 0.7.0 and
  0.8.0 have shipped (`CHANGELOG.md`, tags `v0.6.0` to `v0.8.0`). No test
  fails when a published release has no list. The docs name 0.1.0 to 0.5.1 as
  the served window.
- #411: no per-release table of the event types each served release reads,
  and no list of response fields added without a version since
  `tolerant_responses`. No test ties that doc to `fixtures/event-types/`.
- #332, criterion 4: `docs/development-process.md` doesn't say what an added
  response field needs.
- #414: about 180 core types still carry `deny_unknown_fields` across 29
  modules. There's no walk of every type reachable from `Outcome`, and no
  test that injects an unknown field at every object depth.
- #413: an unknown event type is still withheld. The opaque record needs a
  page-event type in the core's `Outcome`, which `store.rs`, `daemon.rs`,
  `mcp.rs`, `dashboard.rs` and `cli/render.rs` all build or read.

## Slices

Together this is well over the ~300-line limit, and it spans many shared files
(about 25 core modules for #414, plus the daemon, store and MCP for #413). So
it lands in three pull requests, one after another:

1. **This run: release lists and docs.** Fixes #331 and #411. Refs #332, with
   criterion 4 done here.
2. **#414, next:** classify every type reachable from `Outcome`, make the
   response-only ones tolerant, and add the depth-injection golden test. That
   needs its own reservation of the core modules involved.
3. **#413, last:** decide in ADR 0015. The proposal is yes: an
   `opaque_events` capability (`None` since), and an untagged `PageEvent`
   (`Known(RecordedEvent)` or `Opaque { seq, type }`) in `Outcome::Events` and
   `Outcome::Changes`. Its wire form is unchanged for known types. Frozen
   releases keep being withheld, with golden vectors. That slice closes #332.

## Slice 1: what changes where (test first)

Facts checked at the tags:

- No `*-event-v10.json` vector changed between `v0.5.1` and `HEAD`, and
  `event_needs` has 78 arms, the same as `0.5.1.txt`. So 0.6.0, 0.7.0 and
  0.8.0 each read the 0.5.1 list. Check each list against the tag's event
  vectors (`git show v0.N.0:fixtures/<name>-event-v10.json`) before
  committing it.
- 0.6.0 and 0.7.0 name capabilities (`Request::new`): the 13 in their
  `CAPABILITIES`, without `rename_holds`. 0.8.0 names all 14. All three speak
  version 10.
- The only wire fixtures changed since `v0.6.0` are under
  `fixtures/capabilities/` (`rename_holds` in 0.8.0: `call_breaks`'s
  `callers`, `check_calls`'s `renamed`).

Steps:

1. **Test first (`tests/protocol.rs`):**
   - `every_published_release_has_a_frozen_list`: each `## [x.y.z]` heading in
     `CHANGELOG.md` has `fixtures/event-types/x.y.z.txt` and a `RELEASES` row,
     and every file has a row. It fails now (0.6.0 to 0.8.0 are missing).
   - Extend `RELEASES` with the capabilities each release names (`None` for
     0.1.0 to 0.4.0; 0.5.0 and 0.5.1 name their 10, which `Request::new`
     sends as `Capabilities::ALL.names()` at `v0.5.0`). For a release that
     names some, encode with
     `encode_with(negotiate(10, Some(names)), Some(names))`. Expect that
     release's list intersected with the current types, and `withheld`
     equal to the count left out (absent when it is 0).
   - Check: the new test fails for the right reason before the fixtures exist.
     Then break `event_needs` once (for example, give `ReadOutlined`
     `Version(3)`) and confirm the page test catches it on a named release.
2. **Fixtures:** `fixtures/event-types/0.6.0.txt`, `0.7.0.txt` and
   `0.8.0.txt`, each derived from its tag's event vectors and sorted like the
   existing lists.
3. **`src/protocol.rs`, unit test only:**
   `an_event_type_no_release_reads_needs_a_capability_of_its_own` reads the
   newest list (`0.8.0.txt`) instead of `0.5.1.txt`. No production code
   changes. Ratchet: line count unchanged; run `--tighten` if needed.
4. **Generated table (#411):**
   - `scripts/gen-docs.py` gains `release_table` from
     `fixtures/event-types/*.txt`, in version order. Columns: release, number
     of event types it reads, and the types added since the previous release.
     It is written into a marked region in `docs/usage/retries-and-recovery.md`.
   - `tests/tool_count_in_docs.rs`'s `--check` then fails when a fixture
     changes without the doc. That file needs no change.
   - Unit tests in `scripts/gen-docs/test_gen_docs.py`: rows are in version
     order with added types; a changed list makes the table stale; a list
     that isn't one name per line fails, naming the file. The test
     `Workspace` gains the new page and two lists.
   - Test first: write these, see them fail, then implement.
5. **Docs:**
   - `docs/usage/retries-and-recovery.md`: served window 0.1.0 to 0.8.0, the
     generated table, and "Response fields added without a version since
     `tolerant_responses`": `call_breaks`'s `callers` (0.8.0, `rename_holds`).
     Check the release of each entry against the tag before writing it.
   - `docs/development-process.md`: a row for the new generated region, and a
     short "Adding a response field" paragraph that summarises ADR 0015's
     rule (response-only object: no version and no capability; a stored or
     request object: a capability; removed or redefined: a version) and
     points at the frozen-list test (#332 criterion 4).
6. **Checks:** `cargo fmt`; both clippy runs; `cargo nextest run --workspace
   --locked`; `cargo test --doc`; `python3 scripts/gen-docs.py --check`;
   `python3 -I -B scripts/ratchets.py --check .`; then
   `git diff --color-moved`.

## Shared contracts touched

- None on the wire: no field, capability, version or `Outcome` change. Golden
  and contract vectors are unchanged.
- `scripts/gen-docs.py` and its tests: the shared generator. Another session
  (f-roadmap-gen) may touch it; the new table is a separate function and
  region, so a merge conflict would be limited to `tables()` and the test
  `Workspace`.
- `docs/development-process.md`'s generated-files table: one added row.
