# Tokens per merged change

`tokens` shows how many Claude Code tokens each merged change took, day by
local day. It's a way to watch the token overhead of the whole process over
time. It doesn't measure what LangSpan adds or saves: the
[coordination benchmark](../../benchmarks/coordination/README.md) is the
controlled measurement.

```text
$ ./target/release/langspan tokens --onto main --days 3
Claude Code tokens per merged change on main, by local day
2026-11-25: 4 merged, 25.4M tokens per change (3.1M uncached input, 0.2M output)
2026-11-26: 6 merged, 19.1M tokens per change (2.4M uncached input, 0.1M output)  [process change: AGENTS.md]
2026-11-27: no merged change, 4.0M tokens
Read the usage counts of 9120 messages in 212 transcripts under /Users/me/.claude/projects; no prompts, code or credentials.
Confounders:
- Tokens are every Claude Code session in this folder, including work on other repositories and work never merged.
- A merged change is one first-parent commit; changes differ in size and difficulty.
- The trend reflects the whole process (models, prompts, tasks, people), not LangSpan alone; the coordination benchmark is the controlled measurement.
- Other hosts' sessions and deleted transcripts are not counted.
```

The figures in this example are illustrative, not a measurement.

`--onto BRANCH` names the branch changes merge onto, in the repository at
`--root` (default: the current directory). `--days N` sets how many local
days to show, ending today: 14 by default, at most 90. It needs no
coordinator and changes nothing.

## The method

The same transcripts, history and time zone always give the same answer,
whatever order the files are read in.

- **Tokens** are the `usage` of every `assistant` line in the `.jsonl`
  transcripts under `projects` in the Claude Code folder: `CLAUDE_CONFIG_DIR`
  when it's set, else `~/.claude`. Claude Code writes one line per content
  block, each repeating its message's usage, and a resumed session copies
  earlier lines into its new file, so each message ID is counted once. Each
  count takes its largest value across those copies, and the message falls on
  the day of its earliest time.
- **Total** is input, cache writes, cache reads and output.
  **Uncached input** is input and cache writes: what wasn't served from the
  cache. **Output** is output tokens. No dollar amounts are shown.
- **A merged change** is a first-parent commit of `--onto`, on the local day
  of its committer time. Every first-parent commit is visited, so one whose
  date is older than its parent's doesn't hide the commits before it.
- **A process change** is a merged change that touches an `AGENTS.md`, a
  `CLAUDE.md` or a file under `.claude/`, including renaming one away. The
  day shows those paths.
- **Per change** is the day's tokens divided by its merged changes, rounded
  down. A day without a merged change shows its tokens and no per-change
  figure.
- Days are local, in the system's time zone (`TZ` is honored).

Only the message ID, time and the four usage counts are kept from each line.
Prompts, code, tool output and credentials are not stored or printed. Only
files changed during the window (and the day before it) are read. Links
aren't followed, the walk goes four directories deep, and a line over 4 MiB
is skipped. Lines with a usage that can't be read (one still being written,
say) are counted and shown, not guessed. More than 20,000 transcripts changed
in the window is an error: ask for fewer days.

`--json` prints `onto`, `transcripts` (the folder read), `files`, `messages`,
`unread_lines`, `days` and `confounders`. Each day has `day` (`YYYY-MM-DD`),
`merged`, `tokens` and `per_change` (each with `total`, `uncached_input` and
`output`; `per_change` is `null` without a merged change), and
`process_changes`.

## What it can't tell you

Each answer lists its confounders. Read the trend as the whole process
changing: models, prompts, the mix of tasks and who is working all move it.
The folder holds every Claude Code session on this machine, including work on
other repositories and work that never merged, and nothing from other hosts.
A change counts the same whatever its size.

When the Claude Code folder has no `projects`, `tokens` exits 1 and says to
set `CLAUDE_CONFIG_DIR`. When the branch can't be read it exits 1 and says to
check `--onto` and `--root`.
