# ADR 0022: A file in conflict is refused at most once

**Status:** Implementation decision, recorded 25 November 2026. The behaviour
shipped earlier and was described only in a planning note; the capability
review of 25 November 2026 asked for this record (row B15).

## Context

During a merge, rebase, cherry-pick or stash pop that stops on a conflict,
Git writes conflict markers into the file. The observer marks such an entry
as `conflicted` when the index has it unmerged (`src/observer.rs`). A session
that read the file before the merge recorded a version that is no longer on
disk.

One possible rule is never to hold a file with an unresolved conflict. Under
LangSpan's source-version rule
([ADR 0005](0005-source-versions-and-stale-writes.md)), an edit whose file
differs from the session's recorded version is refused once with
`stale_base`.

## Decision

A file in conflict gets no special exemption. The ordinary rule applies, and
it already bounds the cost:

- A session with no earlier read of the file is admitted. Resolving the
  conflict is never held up by the version record.
- A session that read the file before the conflict is refused once, with
  `stale_base` naming the file. The refusal is what makes it read the markers
  it has to resolve, instead of writing over them from its old copy.
- Its next write is admitted, whatever it contains, and so is every write
  after it.

So a conflicted file is refused at most once per session that holds an older
read, and never for a session without one.

## Consequences

- An agent may lose one turn to the refusal. In exchange it does not resolve
  a conflict from a copy that has no markers.
- Tests: `a_file_in_conflict_is_refused_at_most_once_and_never_without_an_earlier_read`
  and `a_merge_conflict_is_scanned_and_an_edit_from_an_old_read_is_refused_once`
  (`tests/repository_shapes.rs`), with real Git merges.
- Not observed on a real host yet.
- This is a deliberate design choice; row B15 of the
  [roadmap](../roadmap.md) points here.
