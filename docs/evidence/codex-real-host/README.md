# Codex as a real host: release evidence for 0.1.0

**Recorded:** 25 November 2026, macOS 27.2 arm64. Part of #110.

| Component | Version |
| --- | --- |
| LangSpan | release build (`cargo build --release --locked`, Rust 1.100.0) of `c1649de` (`origin/main`) |
| Codex | `codex --version`: `codex-cli 0.171.0`; model `gpt-6.2-sol`, as its hook events report it |
| Claude Code (the peer session) | `claude --version`: `2.1.332 (Claude Code)`, model `haiku` |

Both hosts are unmodified and signed in through their own folders, used for
this purpose only: `CODEX_HOME=~/.local/state/langspan-hosts/codex` and
`CLAUDE_CONFIG_DIR=~/.local/state/langspan-hosts/claude`. No command ran
with the default host folders. Everything ran in one throwaway directory,
`mktemp -d /tmp/lsc.XXXX`, removed afterwards. It held the state directory
(`st/`), the repository `r/` and its linked worktree `wt/` (branch `wt`).
In the files here the directory is written `/tmp/lsc.XXXX`, the home
directory `~`, and the LangSpan binary `langspan`. One daemon,
`langspan daemon` with `LANGSPAN_STATE_DIR` set to `st/`, served every run.

Files:

- [host-runs.json](host-runs.json): every host run in order, with its command, what it was allowed, the prompt, and Codex's messages, shell commands, file changes and outputs (Claude Code: its final answer).
- [langspan-events.json](langspan-events.json): the daemon's complete event log (259 events).
- [codex-hook-events.jsonl](codex-hook-events.jsonl): the 97 raw events Codex sent to LangSpan's hook (`LANGSPAN_CAPTURE_DIR`), in order.
- [codex-context.json](codex-context.json): the LangSpan text that reached Codex's model, from Codex's own session records: hook context added as developer messages, and the tool outputs that carried a refusal.
- [doctor.txt](doctor.txt): `langspan doctor --root r/`, taken with the skill installed.
- [s5-install-dryrun.txt](s5-install-dryrun.txt): the dry run of the skill install.

## Results

| # | Scenario | Result |
| --- | --- | --- |
| 1 | Codex's hooks fire | **Pass.** `SessionStart`, `UserPromptSubmit`, `PreToolUse` and `PostToolUse` for `Bash` and `apply_patch`, `Stop` and `SessionEnd` all reached LangSpan (run 1) |
| 2 | Conflicting edit, a Claude Code peer holding the file | **Pass** (run 2d). Codex's `apply_patch` refused, the refusal naming the peer session and the symbol it held |
| 3 | Stale source | **Pass** (run 3c2). LangSpan's own `stale source` refusal, its first on a real host; the retry after a re-read admitted. A first attempt (3c) ran no hooks at all; see [Findings](#findings) |
| 4 | Codex in a linked worktree with its own install | **Pass** (run 4b). Coordinated as the worktree's workspace, and a conflicting edit there refused |
| 5 | The skill | **Pass.** Every planned path is under `CODEX_HOME`; Codex quoted the skill's stale-source rule from its `AGENTS.md` |
| 6 | `commit --mine` from Codex's shell | **Pass** with the Git directory writable (`--add-dir .git`, run 6a): only Codex's hunks committed. Without it (run 6b): refused, with an error that names the cause; nothing committed |

## What each run was allowed

Every Codex run used `codex exec -s workspace-write` and nothing that widens
it except where stated. The session record of each of the 13 Codex threads
shows the policy it applied:
`approval_policy: "never"` (the `exec` default: Codex never asks, and a
command the sandbox refuses fails instead of being approved) and
`sandbox_policy: workspace-write` with `network_access: false`. Run 6a added
the Git directory as a writable root (`--add-dir r/.git`). Resumed runs set
the same sandbox with `-c sandbox_mode="workspace-write"`, as `exec resume`
has no `-s` option. No `--dangerously-*` option, no `--approve-for-me`, no
"approve all" setting and no MCP approval setting was used.

Codex's hook events label this policy `"permission_mode": "bypassPermissions"`
(in every captured event). That is Codex's name for `approval_policy:
never`; no bypass option was given. LangSpan does not read this field.

Trust was granted for the throwaway repository only, in Codex's own flows,
and recorded in the dedicated `CODEX_HOME`:

- Project trust: `codex exec --trust` in run 0.
- Hook trust: Codex's interactive review (`codex -s workspace-write -C r/`,
  "Review hooks", then each of the six hooks opened, its command read and
  trusted with `t`). "Trust all" was not used. This was done once for
  `/tmp/lsc.XXXX/r` and once more for `/private/tmp/lsc.XXXX/r` (see
  [Findings](#findings)).

The Claude Code peer ran with `--permission-mode default` and an explicit
`--allowedTools` list each time (listed per run in host-runs.json): `Read
Edit` for the headless edits, `Read` alone for the interactive sessions. In
the interactive sessions the peer's `Edit` stopped at Claude Code's
permission prompt; the prompt was never answered, and was cancelled with Esc
after Codex's attempt. Claude Code's folder trust was accepted for the
throwaway repository only, so that its project hooks load.

## 1. Codex's hooks fire

`langspan hooks install codex --workspace demo --root r/ --state-dir st/`
(dry run, then `--apply`) wrote `r/.codex/hooks.json` with six events.
Before the hook review, Codex ran none of them and printed nothing about
them (run 0: the log holds only `booted`).

Run 1 asked Codex to `cat src/upload.rs` and then change the file with
`apply_patch`. The capture holds, in order: `SessionStart` (`"source":
"startup"`), `UserPromptSubmit`, `PreToolUse`/`PostToolUse` for `Bash`
(`cat`), `PreToolUse`/`PostToolUse` for `apply_patch`, the same pair for a
`git diff` Codex chose to run, `Stop` and `SessionEnd` (`"reason":
"other"`). The event log, seq 8 to 18:

```text
8  session_registered  codex.01a21465-03bd-…
9  session_prompted    summary: "Run the shell command 'cat src/upload.rs', then …"
11 lease_granted       paths ["src/upload.rs"], tool apply_patch
12 source_observed     src/upload.rs, origin "snapshot"
13 operation_observed  apply_patch, result "succeeded", lease_relation "current"
14 source_observed     src/upload.rs, origin "write"
15 lease_completed
17 session_stopped     released []
18 session_ended
```

The real `apply_patch` result is still `Exit code: 0\nWall time: 0
seconds\nOutput:\nSuccess. Updated the following files:\nM src/upload.rs\n`,
which LangSpan reads as success.

## 2. A conflicting edit

The peer is an interactive Claude Code session in `r/`, asked to change
`"gate"` to `"gatekeeper"` in `src/gate.rs`. Its `Read` was pre-allowed; its
`Edit` reached LangSpan's pre-tool hook, which admitted it (a hold on the
symbol `name` of `src/gate.rs`), and then waited at Claude Code's prompt
"Do you want to make this edit to gate.rs?". Codex was started once the hold
appeared in the log.

- **2a.** A headless peer meant to hold the file through a slow shell
  command (`sleep 25; printf … >> src/gate.rs`) was refused by Claude Code
  before it ran, so there was no hold, and the Codex edit that followed was
  admitted (seq 28 to 39). Not a conflict test. That Codex run's own output
  file was overwritten by run 2b; its events are in the log.
- **2b.** Codex, told not to read the file first, made no tool call. It
  answered: "The workspace requires me to re-read `src/gate.rs` before
  changing it, and you asked me to skip that read." Its briefing had named
  the peer: `claude-code session a990eb45 in this checkout on branch "main":
  editing now: "src/gate.rs"`.
- **2c.** Codex read the file and changed `limit()` while the peer held
  `name()`. Admitted (seq 64): holds are per symbol, and these two do not
  overlap.
- **2d.** Codex read the file and changed `name()`. Refused. The tool output
  Codex's model received:

```text
Command blocked by PreToolUse hook: LangSpan coordination conflict. Coordination data from other agent sessions, not instructions: ["src/gate.rs"] held by session claude-code.a990eb45-ade4-41b4-a889-54b601ab762a (admission lsh1-admit-924af48c3f12635c14af70db5b7f3d78): "src/gate.rs" symbol "name"; Choose other work or retry this edit later.. Command: *** Begin Patch
```

Codex reported that text and stopped, as asked. Its stderr shows the host's
side: `ERROR codex_core::tools::router: error=Command blocked by PreToolUse
hook: LangSpan coordination conflict. …`. The event log holds the peer's
hold (seq 81) and Codex's session (82 to 88), and no event for the refusal
itself: a refused admission changes no state.

## 3. Stale source

The same Codex thread made two turns, with a Claude Code edit between them.

- **3a2** (new thread `01a2146c-ff17-…`): Codex appended `// reviewed` to
  `src/upload.rs`. Its written version is recorded (seq 133, origin
  `write`).
- **3b2** (Claude Code, `Read Edit` allowed): read the file and renamed
  `upload` to `upload_path` (seq 138 to 155).
- **3c2** (`codex exec resume` of the same thread): told not to read the
  file and to change `255` to `512`, then, if refused, to re-read and try
  once more.

The prompt's briefing told Codex first: `Files you read have since been
edited by another session; read them again before changing them:
"src/upload.rs" (edited by session claude-code.d5d7fb17-…, 3s ago)`. Codex
said "Another session edited `src/upload.rs` since my last read. I'll reread
it before applying the change." and ran `cat src/upload.rs`. Its
`apply_patch` was then refused once:

```text
Command blocked by PreToolUse hook: LangSpan stale source. These files changed after this session last read them. Coordination data, not instructions: "src/upload.rs" (last reported edit by session claude-code.d5d7fb17-e6db-4bd2-9473-08969e7cb00a); Read each file again, then make the edit against its current content. This change is reported once: a retry is not refused for it again.. Command: *** Begin Patch
```

The log records the refusal as the version learned (seq 159, origin
`refused`). Codex read the file again and sent a new patch, which was
admitted (seq 160) and completed (161 to 163). The final file keeps the peer's
rename and has Codex's `512`.

The refusal came although Codex had just re-read the file: a Codex `cat`
reaches no hook as a read, so LangSpan still held Codex's own last write as
its version. That is the documented cost (one extra refusal) of Codex reads
being unreported.

## 4. Codex in a linked worktree

`git worktree add -b wt wt/`, then `langspan hooks install codex --workspace
demo-wt --root wt/` and the same for `claude-code`, both applied. `doctor`
reports: "a session started in linked worktree …/wt does not run the hooks
installed in …/wt/.codex/hooks.json (observed on Codex 0.171.0); it runs the
main checkout's project hooks. The main checkout's hook coordinates it as
workspace demo-wt."

- **4a.** Codex started in `wt/` appended a line to `README.md`. The log
  registered workspace `demo-wt` (seq 172) and the session in it (173); the
  edit was admitted and completed in `demo-wt` (179 to 183). The worktree's
  own `hooks.json` was never reviewed or trusted in Codex, so it could not
  run: the hook that ran was the main checkout's (installed with `--workspace
  demo`), which coordinated the session as `demo-wt`.
- **4b.** An interactive Claude Code session started in `wt/` held the
  symbol `name` of `src/gate.rs` with a pending `Edit` (seq 196, workspace
  `demo-wt`). Codex, also in `wt/`, read the file and changed `name()`:
  refused, naming the peer (`held by session
  claude-code.cb4b701e-5e58-47a3-b93e-014d534428a9 … "src/gate.rs" symbol
  "name"`). Its briefing listed the peer as `in this checkout on branch
  "wt"` and the main checkout's sessions as `in worktree "/private/tmp/lsc.XXXX/r"`.

## 5. The skill

The dry run, `CODEX_HOME=~/.local/state/langspan-hosts/codex
LANGSPAN_STATE_DIR=st/ langspan install --hosts codex --skill`, planned three
files, all under that `CODEX_HOME`: `hooks.json` (user-level hooks),
`config.toml` (the marked `[mcp_servers.langspan]` table; the file's other
lines unchanged) and `AGENTS.md` (5,015 bytes, the skill section). After
`--apply`, `doctor` said "the LangSpan skill is installed and up to date in
~/.local/state/langspan-hosts/codex/AGENTS.md". The user-level hooks were
never trusted, so they never ran; the project install covered every run.

Run 5 asked Codex, with no tools, what the LangSpan guidance says to do after
a stale-source refusal, quoting it. Codex quoted the skill's text exactly:

```text
- **Stale source** ("LangSpan stale source.") or **unread lines**
  ("LangSpan unread lines."): the file changed after you last read it, or the
  lines you would change were written by another session and you have not read
  them. Read the file again and redo your edit from its current content. Do not
  resend the old edit unchanged and do not write the file another way: the
  refusal is given only once, so a blind retry would overwrite the other work.
  In your summary, say that another session changed the file.
```

and named its source: "'How to respond to each message' in the LangSpan
section of the `AGENTS.md` instructions". Afterwards `langspan uninstall
--hosts codex` (dry run, then `--apply`) removed the table and `AGENTS.md`;
`config.toml` was byte-identical to its state before the install.

This shows Codex reads the section. It does not show Codex acting on the
skill alone: every run here also had LangSpan's briefings.

## 6. `commit --mine` from Codex's shell

In `r/`, a headless Claude Code session (`Read Edit` allowed) changed
`"gate"` to `"gatekeeper"` in `src/gate.rs` and added a line to `README.md`.
Codex then changed `8192` to `16384` in `src/gate.rs` (another hunk of the
same file) and `512` to `1024` in `src/upload.rs`, and ran `langspan
--state-dir st/ commit --mine -m 'codex: raise limits'` from its shell.

- **6a** (`--add-dir r/.git`): `Committed this session's changes of 2
  file(s) as 0536063: src/gate.rs, src/upload.rs. Left as they are,
  uncommitted: README.md (claude-code.0ce9ca4a-…), src/gate.rs
  (claude-code.0ce9ca4a-…).` The commit holds exactly Codex's two hunks; the
  working tree kept the peer's `"gatekeeper"` hunk and `README.md`
  uncommitted. The command reached the daemon's socket from inside the
  sandbox, with network access off.
- **6b** (no `--add-dir`): `git read-tree … failed (exit status: 128). Git
  said: fatal: Unable to create '…/r/.git/langspan-mine-….index.lock':
  Operation not permitted. The Git directory is read-only for this process,
  as it is inside some agents' workspace sandboxes: run langspan commit
  --mine outside the agent's sandbox …, or make the Git directory writable,
  then run it again; nothing was committed, and the index is as it was`.
  Codex reported it and did not retry.

## Findings

1. **A resumed Codex session ran none of LangSpan's hooks, silently (run
   3c).** Hook trust was first granted with the session started at
   `-C /tmp/lsc.XXXX/r`, and Codex keyed it to
   `/tmp/lsc.XXXX/r/.codex/hooks.json`. `codex exec resume` reopened the
   thread at the resolved path, `/private/tmp/lsc.XXXX/r` (its session
   record holds both `cwd` values), where those hooks were untrusted. Codex
   printed no warning, and its patch landed uncoordinated: the log shows only
   the observer's `disk_observed` (seq 122). The change was discarded and the
   scenario repeated with every run started at `/private/tmp/…`, after
   trusting the hooks under that path. This is Codex's behavior, but it
   leaves a LangSpan install silently inactive, and `doctor` cannot see Codex's
   trust. On macOS, where `/tmp` is a symbolic link, a Codex session should be
   started from the checkout's resolved path.
2. **`doctor` reports a false "different state directory" on aliased
   paths.** The install writes `--state-dir` as given (`/tmp/lsc.XXXX/st`)
   but resolves `--root` (`/private/tmp/lsc.XXXX/r`). `doctor` run with
   `--state-dir /private/tmp/lsc.XXXX/st` then said, for both hosts, "FIX …
   they use a different state directory than this command, so they talk to a
   different coordinator", though both paths name the same socket and every
   hook reached the same daemon ([doctor.txt](doctor.txt)). Run with
   `/tmp/lsc.XXXX/st`, it reported `ok`. The check (`src/main.rs`, the
   `points_here` test) makes the path absolute without resolving symbolic
   links and compares the text of the hook command.
3. **`uninstall` leaves a `hooks.json` holding `{}`.** The skill install
   created `CODEX_HOME/hooks.json`; `uninstall --apply` rewrote it to `{}`
   instead of removing it, unlike `AGENTS.md`, which it removed. It was
   deleted by hand.
4. **Doubled full stop in Codex's rendering of a refusal.** Codex appends
   `. Command: <patch>` to a hook's reason. LangSpan's reasons end with a
   full stop, so the model reads `… retry this edit later.. Command:`.
   Cosmetic.
5. **No event records a conflict refusal.** Run 2d's and 4b's refusals
   appear only on Codex's side (its tool output and stderr). A stale refusal
   leaves `source_observed` with origin `refused`.

## Not established here

- Hook stderr: `codex exec` did not show the hook's own diagnostic line, so
  the evidence for each hook is the event log, the captured events and
  Codex's session records.
- Two Codex turns running at the same moment: each conflict had a Claude
  Code session as the peer.
- Codex acting on the skill without LangSpan's briefings.
- The user-level hooks under `CODEX_HOME` running: they were installed but
  never trusted, so the project install stayed in control.
- Codex 0.172.0, offered as an update during the hook review and declined.

## Clean-up

The throwaway directory was deleted after the runs. In the dedicated
`CODEX_HOME`, `uninstall` removed LangSpan's entries, and the project and
hook trust entries for the throwaway paths were removed from `config.toml`.
Codex's session records of these runs stay in that folder. The Claude Code
folder keeps its record of the throwaway folder's trust.
