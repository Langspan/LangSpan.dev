# ADR 0012: Region admission within one file

**Status:** Implementation decision for the owner-directed coordination work,
24 November 2026. Extends [ADR 0001](0001-local-coordination.md) (whole-set
file admission) and uses the parser of [ADR 0008](0008-derived-source-index.md).
Tested with the real daemon and hook executables; not yet observed on a host.

## Context

Admission held whole files. Two agents editing different functions of one file
were serialized: the second was denied for the whole file while the first edit
was in progress. Checking symbols instead has two pitfalls to avoid: keying
only top-level declarations, so two methods of one class always collide, and
letting an edit outside any declaration, or in a file that does not parse,
pass ungated.

## Decision

**Where an edit falls.** The hook works this out from the host's tool input and
the file as it is on disk, read once, so the regions and the admission's base
version describe the same bytes. The file must be at most 1 MiB, valid UTF-8,
and the work starts only while more than half the hook budget remains, so the
admission request keeps its time. Otherwise the file is held
whole.

| Tool | Located as |
| --- | --- |
| Claude Code `Edit` | The lines of the one occurrence of `old_string`. No occurrence, more than one, an empty `old_string` or `replace_all`: whole file. |
| Claude Code `MultiEdit` | The union over its edits, applied in order in memory. Each must match exactly once in the text the earlier ones left, and must not touch text an earlier one wrote (its place in the original file could not be stated). Anything else: whole file. |
| Claude Code `Write`, `NotebookEdit` | Whole file. |
| Codex `apply_patch`, `Update File` | Each hunk's context and removed lines must match exactly one place in the file under a comparison looser than any the host applies (ends trimmed, every character other than an ASCII letter or digit treated alike; trailing empty lines dropped, as the host retries without them). The range is the removed lines, or for an insertion the lines on either side. Context lines are not claimed: the host refuses a hunk whose context no longer matches. No match, more than one, or a hunk with only additions: whole file. |
| Codex `Add File`, `Delete File`, `Move to`, a file in two sections | Whole file. |

**What a region is.** For a language the source parser handles (Rust, Python,
TypeScript/JavaScript, Go), each touched line is assigned to the innermost
definition containing it. A definition's lines include the attributes,
decorators and comments directly above it. The region is that definition,
named by its **key**: its own name below the names of the definitions that
enclose it, outermost first (`Store::open`, `outer::inner`), at most 8 deep. It
also carries the definition's **digest**: SHA-256 of its lines with trailing
whitespace (and so carriage returns) removed. Any non-blank touched line inside
no definition (an import, a top-level statement, a Rust `impl` header), or a
file that parsed only partly, holds the whole file. Blank lines between
definitions are not assigned. For any other language the region is the line
range widened by 3 lines on each side.

**When two edits of one path conflict.** Both must carry regions, or it is a
conflict as before. Then:

- Symbol against symbol: a conflict if any key of one equals, or encloses, any
  key of the other. `Store::open` and `Store::close` are separate; `Store` (a
  class attribute, say) conflicts with both; `outer` conflicts with
  `outer::inner`. **The file version does not matter.** A teammate's edit to
  another function changes the file but not the identity of this one.
- Lines against lines: separate only if both were located in the same version
  of the file (its digest) and no ranges share a line.
- Symbols against lines, or anything else: a conflict.

**The base of a region.** This is the choice that most affects how often the
feature helps. Comparing regions only when both leases were located in the
same file version would make any edit elsewhere in the file a whole-file
conflict, which is exactly the case to tell apart. So symbol regions are
compared by key, which survives other edits, and each carries its own content
digest. Line ranges cannot be compared across versions and keep
the file digest. The symbol digest is recorded on the lease and is not yet
used in a decision; it is there so a later change can narrow the stale-source
rule per symbol.

**In the core.** `admit_tool` takes optional `regions`: for each path, the file
digest it was located in and 1 or more regions, all symbols or all line
ranges, at most 64 distinct regions in all, lines 1 to 2,000,000, keys of 1 to
8 names of 1 to 128 bytes without control characters. A malformed set is
`invalid_regions`. Regions are kept only for a path whose base in the same
request is that exact version; otherwise the path is held whole. They are
stored on the lease. A lease recorded before this change has no regions and
replays as held whole. A conflict reports the holder's regions in the
conflicting paths, and the hook's denial names the holder's symbol or lines
that overlap this edit, or says the edit could not be placed. A conflict
check after the fact (an observation's concurrent holders) uses the named
admission's regions. `admit` from the CLI or MCP carries no regions.

## After one region edit completes

Alice and Bob are admitted for `alpha` and `beta` of one file. Alice's edit
writes; the file has a new version. Bob's lease, located in the old version,
stays valid for `beta` until it completes or expires; a new request by anyone
for `alpha` or `gamma` is compared by key and is unaffected by the version
change. A new line-range request in a file of an unparsed language, located
in the new version, conflicts with Bob's older-version ranges until his lease
ends.

The stale-source rule ([ADR 0005](0005-source-versions-and-stale-writes.md))
is unchanged and independent. After Alice's write, Bob's own known version of
the file is older than the disk, so his next edit of that file is refused
once and he must read it again. With both rules active, two sessions editing
different functions of one file still each re-read after the other's write.
What this decision removes is the denial **while the other edit is in
progress**.

## Consequences and limits

- Names, not resolution. A Rust method is keyed by its name only, because an
  `impl` block is not a definition in the index: methods `new` of two types
  share a key and conflict. Two definitions with one name collide. This can
  only serialize more, never less.
- An edit of a definition's signature and an edit of its body are the same
  symbol. A change in one function that breaks another function's use of it is
  not a region overlap; the interface notices of ADR 0008 still apply.
- Placement is the hook's reading of the host's input; the host performs its
  own matching when it writes. If they disagree, a host match elsewhere would
  be an edit LangSpan did not admit for that place. The rules above refuse to
  place an edit whenever more than one place could match, which is the case
  where they could disagree.
- The parser now also runs in the short-lived hook process, on at most 1 MiB,
  started only in the first half of the hook budget. No timing is claimed: the
  hook's latency with a large file has not been measured.
- The protocol stays at version 3, unreleased: `admit_tool` and leases gain an
  optional `regions`, and a conflict an optional `regions`. Golden vector:
  `fixtures/admit-regions-request-v3.json`.
