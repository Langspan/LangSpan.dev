# The two-developer demonstration

`scripts/demo-two-developers.sh` runs the whole scenario and asserts every
outcome; it exits non-zero at the first one that differs. `sh scripts/check.sh`
runs it after the release build, so CI runs it on Linux.

```sh
cargo build --release --locked
bash scripts/demo-two-developers.sh
```

[transcript.txt](transcript.txt) is one run: 24 November 2026, macOS 27.2
arm64, Apple M6, release build of `feat/demonstration`.

## What it shows

| Step | Shown |
| --- | --- |
| 1 | A Codex session is told, at its start, about a Claude Code session in the same checkout: the file it edited and its task |
| 2 | A second edit of a file being edited is denied before it runs |
| 3 | A different file at the same moment is admitted: unrelated work is not serialized |
| 4 | An edit from an old read is refused once, naming the session that changed the file; after reading again it is admitted |
| 5 | An exported signature changes; the session whose file mentions it is told, with before and after |
| 6 | Across two coordinators and one shared remote, each developer's agent is told of the other's work; a task line leaves only where its owner chose; work in another clone is never held |
| 7 | Conflicting branches are reported before anything runs; landing without evidence is refused; two developers' branches pass together on one exact tree; evidence for both is not evidence for one; that exact result lands |
| 8 | With no coordinator a hook prints nothing; after a restart, earlier sessions are reported as uncoordinated until they act, and are coordinated again when they do |
| 9 | The remote disappears: local coordination continues; it returns: publication resumes |
| 10 | An ended session's edits stay reported, labelled; a developer who stops publishing disappears from the other's view |

## What it is not

- **No agent runs.** Every hook event is JSON written by the script in the
  shape the host sends. It shows what LangSpan does with those events. It
  does not show a model reading a briefing and choosing differently.
  Claude Code runs are in [m4-briefing](../m4-briefing/README.md) and
  [m6-stale-source](../m6-stale-source/README.md); Codex has none.
- **One machine.** The two developers are two directories, two state
  directories and two coordinator processes, with a local bare repository as
  the remote.
- The Codex events follow the documented contract, not captured Codex output.
