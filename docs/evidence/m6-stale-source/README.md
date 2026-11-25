# Source versions and stale writes: native-host evidence

**Recorded:** 24 November 2026, macOS 27.2 (arm64), Claude Code 2.1.330,
LangSpan release build of `feat/source-versions`, hooks installed with
`langspan hooks install claude-code --apply`. Paths are rewritten to
`/work/accept`. Final answers are in [host-runs.json](host-runs.json) and the
daemon's complete event log in [langspan-events.json](langspan-events.json).

## What was run

Three ordinary headless turns in one checkout:

1. **a1.** Session A (`4f1840dc…`) was asked to read `src/gate.rs`.
2. **b.** Session B (`06ae76fd…`) was asked to change `src/gate.rs`, and did.
3. **a2.** Session A was resumed and told to overwrite `src/gate.rs` with the
   Write tool without reading it again, and to report any refusal verbatim.

## What was observed

- Claude Code fires `PostToolUse` for `Read`. The log holds a
  `source_observed` record for A's read and for B's read, with the file's
  digest, and a second one for B after its edit with the new digest.
- A's overwrite did not happen. The file kept B's content.
- **The refusal came from Claude Code, not from LangSpan.** A reported the
  host's own text, "File has been modified since read, either by the user or
  by a linter." The log holds no admission request from A: the host rejected
  the call before running the PreToolUse hook. For a Claude Code Write or Edit
  after a Read, LangSpan's stale-source denial is therefore a second line
  behind a check the host already makes.
- **LangSpan supplied the attribution the host cannot.** On resume, A received
  a briefing (`briefing_delivered` in the log) and, unprompted, explained the
  refusal: another session in the checkout, `06ae76fd`, since ended, edited
  the file after A read it.

## What this does not establish

- LangSpan's own `denied_stale` reply has not been produced by a real host. It
  is exercised by the real-daemon tests in `tests/stale_source.rs`, where files
  really change on disk between hook calls.
- **Codex**, where no host read check was observed and LangSpan's comparison
  would be the only one, is unverified: its hooks need the owner's trust
  approval. Codex reads through shell commands, which no hook reports, so a
  Codex session's first edit of a file is never compared; later edits are
  compared with the version it last wrote.
