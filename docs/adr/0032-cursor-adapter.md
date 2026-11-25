# ADR 0032: Cursor hook adapter

**Status:** Implemented. **Not verified against a real Cursor run.** The adapter was built from Cursor's documentation and from the bundled source of the installed CLI. No event in its fixtures came from a Cursor run; the steps to capture one are below. Until a capture is taken and the fixtures are replaced, every statement in this ADR about what Cursor sends or does with a reply is a reading of documentation or code, not an observation.

## Context

Cursor runs hooks for its agent, both in the editor and in the `cursor-agent` CLI. LangSpan already has two adapters with the same structure: Claude Code ([ADR 0002](0002-claude-code-adapter.md)) and Codex ([ADR 0003](0003-native-coordination-observation.md)). A Cursor adapter lets a Cursor session hold and be refused files, have its reads and writes recorded, and receive peer briefings, on the same terms as the other two hosts.

### Sources

All were read on 24 November 2026.

- [Hooks](https://cursor.com/docs/agent/hooks): event list, common and per-event fields, `hooks.json` locations and schema, matchers, exit codes, fail-open behaviour. The page carries no date or version. Its only version string is the example `cursor_version` value `1.7.2`.
- [Third-party hooks](https://cursor.com/docs/reference/third-party-hooks): Cursor also loads the hooks in Claude Code's `.claude/settings.local.json`, `.claude/settings.json` and `~/.claude/settings.json`. This is on by default, under "Include Third-Party Plugins, Skills, and Other Configs".
- [MCP](https://cursor.com/docs/context/mcp) and [CLI MCP](https://cursor.com/docs/cli/mcp): `.cursor/mcp.json` and `~/.cursor/mcp.json`, the `mcpServers` stdio entry, approval before first use. The CLI uses the same files.
- The installed CLI: `cursor-agent` **2026.11.22-e9042b2**, Homebrew cask `cursor-cli`. Its bundled JavaScript (`dist-package/9622.index.js` and `index.js`) was read where the documentation is silent. Nothing from it is copied into LangSpan. What it shows:
  - **Tool inputs.** The documentation gives Shell's input only. Write's `tool_input` is `{file_path, content}`. Delete's and Read's are `{file_path}`. Shell's is `{command, cwd}`. Tool names are `Write`, `Delete`, `Read`, `Shell`, `Grep`, `List`, `ReadLints` and others. There is no separate edit tool: a change to part of a file arrives as a whole-file `Write`.
  - **Event envelope.** Every agent event gets `session_id` equal to `conversation_id`, plus `hook_event_name`, `cursor_version`, `workspace_roots`, `user_email` and `transcript_path`. `postToolUseFailure` has no `cwd`. A tool call inside a subagent carries `parent_tool_call_id`.
  - **Replies.** An empty stdout from a hook without `failClosed` is logged as a failed hook (`empty_stdout`) and the action proceeds. `permission` is optional in the reply schema. Invalid JSON, or a reply that fails the schema, *blocks* a permission hook.
  - **Context.** `additional_context` is accepted for `sessionStart`, `beforeSubmitPrompt`, `preToolUse`, `postToolUse` and `postToolUseFailure`, up to 10,000 characters. The documentation lists no context field for `beforeSubmitPrompt`.
  - **Transport.** By default the event is passed as a heredoc appended to the command string: the command, then `<<'CURSOR_HOOK_EOF'`, the JSON, and `CURSOR_HOOK_EOF`.
  - **Environment.** Hook processes get `CURSOR_PROJECT_DIR`, `CURSOR_VERSION` and `CLAUDE_PROJECT_DIR`.
- The session is `conversation_id`, and `workspace_roots[0]` stands in when `cwd` is empty. A decision is `{"permission":"allow"}` or `{"permission":"deny","user_message":…,"agent_message":…}`, and context is `{"additional_context":…}`.

## Decision

### Events

| Cursor event | Matcher | LangSpan |
|---|---|---|
| `preToolUse` | `^(Write|Delete|Shell)$` | Admission: a file write or delete, or a shell command's literal write set ([ADR 0011](0011-shell-writes.md)). |
| `postToolUse` | `^(Write|Delete|Shell|Read)$` | Observation of a completed write, delete or command. For `Read`, the version the session saw ([ADR 0005](0005-source-versions-and-stale-writes.md)). Peer news since the last briefing goes back as `additional_context` (documented). |
| `postToolUseFailure` | `^(Write|Delete|Shell)$` | A failed or interrupted call ends its admission. |
| `sessionStart` | none | Registration and the start briefing, as `additional_context` (documented). |
| `beforeSubmitPrompt` | none | The prompt's first line as the session's task (unless `--prompt-summary off`), source-index names, and a briefing as `additional_context`. The briefing field comes from the CLI source; the documentation does not list it. |
| `sessionEnd` | none | The session's work is withdrawn. |

These events are not used:

- `beforeShellExecution` and `afterShellExecution` duplicate `preToolUse` and `postToolUse` for `Shell`. The documentation gives them no `tool_use_id`, so a command's two halves could not be joined.
- `afterFileEdit` has the same problem: no tool-call ID in the documentation. The `postToolUse` of the `Write` covers it.
- `beforeReadFile` runs before a read. LangSpan records a completed read only.
- `stop` marks the end of a turn. LangSpan has no per-turn operation. Cursor would send a `followup_message` as a new user prompt, which is more than a briefing should do.
- `beforeMCPExecution`, `afterMCPExecution`, the Tab hooks, `subagentStart` and `subagentStop`, `preCompact`, `afterAgentResponse`, `afterAgentThought` and `workspaceOpen` have no LangSpan counterpart.

### Replies

The shared lifecycle in `hook.rs` produces Claude Code-shaped replies. `cursor::translate` rewrites them, and no other shape reaches Cursor:

- A denial becomes `{"permission":"deny","user_message":R,"agent_message":R}`.
- Context becomes `{"additional_context":C}`.
- Anything else is dropped. Cursor blocks on a reply it cannot read, so passing an unexpected shape on would turn a LangSpan failure into a denial.

Every other outcome prints nothing and exits 0: admitted, unsupported, unavailable, timeout and unknown alike. Cursor then logs a failed hook and the action proceeds under Cursor's own permission flow. The adapter never answers `allow` and never exits 2. In Cursor's merge an `allow` would count as an approval, which LangSpan has no standing to give. `{}` would also be valid under the CLI's schema but is not used: an empty reply cannot block in any version, while how an older Cursor or the editor validates `{}` is not known.

### Identity

- The session is `cursor.<conversation_id>`, falling back to `session_id`. The prompt request ID is `generation_id`, which changes with every user message.
- Cursor does not document the format of its IDs. A conversation or tool-call ID outside LangSpan's grammar (1 to 100 of `[A-Za-z0-9._-]`) is replaced by `h` plus 128 bits of its SHA-256, so distinct IDs stay distinct. The Claude Code and Codex adapters refuse such IDs instead.
- A call with `parent_tool_call_id` is a subagent's. It becomes its own session, `cursor.<conversation>.<parent call>`, with the conversation as its parent. This follows the CLI source and has not been observed.
- The working directory is `cwd`, then `workspace_roots[0]`. For Shell, the command's own `tool_input.cwd` comes first.
- A Write states a whole file, so the file is held whole: no region admission ([ADR 0012](0012-region-admission.md)). The CLI source builds the same Shell input, `{command, cwd}`, for a foreground and a background command, so a background command is treated as finished when `postToolUse` arrives. If one is in fact marked, or its `postToolUse` comes before it exits, its admission ends while it still runs. The capture must check this.

### Claude Code hooks run by Cursor

Cursor also runs LangSpan's Claude Code entries, with Cursor's payload. If those ran, one Cursor tool call would be coordinated twice under two identities, and the session would conflict with itself. Every Cursor payload carries `cursor_version` and no Claude Code payload does. So the Claude Code adapter answers `unsupported`, prints nothing, and names `langspan hooks install cursor`. This is tested.

### Installation

`langspan hooks install cursor --workspace W --root DIR [--apply]` writes:

- `ROOT/.cursor/hooks.json`: `version` 1 if missing, plus one flat entry per event: `{"type":"command","command":…,"timeout":5,"matcher":…}`. `failClosed` is not set, so a LangSpan failure never blocks Cursor.
- `ROOT/.cursor/mcp.json`: `mcpServers.langspan = {"type":"stdio","command":<langspan>,"args":["mcp","--state-dir",<dir>]}`. This is the same exact-shape entry Claude Code gets. An existing `langspan` entry that LangSpan did not write stops the install with nothing written.

`langspan install --hosts cursor` writes the same entries to `~/.cursor/hooks.json` and `~/.cursor/mcp.json`, in the user-level form ([ADR 0010](0010-user-level-install.md)). When `~/.cursor` exists, Cursor is among the default hosts.

Both reuse the existing guarded writer:

- dry run by default;
- every file planned before any is written;
- a change made meanwhile is planned against once more, and never overwritten;
- the file's mode is kept, new files are `0600`;
- idempotent;
- removal takes out only LangSpan's entries. `version` stays, because LangSpan cannot tell whether it added it. A project file that held only `version` round-trips exactly; at user level, `uninstall` removes a `hooks.json` left holding only `version`, since that changes nothing for Cursor and LangSpan may have created it (ADR 0010). A file that lacked it, which Cursor documents as required, keeps the added `version: 1`.
- the two project files are written one after the other. If the second write fails, the first stays written and the error names the file. Rerunning the command completes the install.

The ownership marker is a leading `LANGSPAN_MANAGED=cursor:v1 ` variable assignment, not the trailing `# langspan-managed:…` comment the other hosts use. Cursor appends the event to the command as a heredoc, and a trailing comment would swallow that redirection: the event's JSON, which includes a Write's file content, would then run as shell code. A test runs the installed command the way the CLI does, under `sh`, `bash` and `zsh`, with `$(touch …)` in the content, and checks that it is read as data.

`doctor --root` reports Cursor hooks and the project MCP entry when they are installed. It says nothing about Cursor when they are not. Every installer and doctor message says "built from documentation, not yet verified on a real Cursor".

### Taking a real capture

The capture needs a signed-in Cursor CLI and a workspace Cursor trusts. On 24 November 2026 the CLI was signed in, but the headless run stopped at "Workspace Trust Required". Passing `--trust` to answer for the owner was declined, so no capture exists. The owner takes it:

1. Sign in, if not already: `cursor-agent login`. The owner runs this, not LangSpan.
2. Start the coordinator: `langspan daemon`.
3. Run `bash scripts/capture-cursor.sh DIR`. It refuses to run unless `cursor-agent status` reports a signed-in account. It:
   - creates a throwaway Git repository under `DIR`;
   - installs LangSpan's project-level Cursor hooks there, with `LANGSPAN_CAPTURE_DIR` set in the hook command;
   - allows only `Shell(ls)` in that repository's `.cursor/cli.json`.

   It also prints the trust step. Either open `cursor-agent` in that repository once and choose to trust it, or rerun the script with `--trust`, which passes `--trust` for that one directory.
4. The script runs one minimal headless turn: `cursor-agent -p --output-format text "Read notes.txt, then change the line 'beta' to 'gamma' in it, then run the shell command ls. Reply with one word: done."`. This should produce `sessionStart`, `beforeSubmitPrompt`, `postToolUse` for `Read`, `preToolUse` and `postToolUse` for `Write`, `preToolUse` and `postToolUse` for `Shell`, and `sessionEnd`.
5. For a denial, it then holds `notes.txt` from a second session through the CLI and runs a second turn that asks for another edit. That turn should produce a `preToolUse` the adapter denies, and whatever Cursor reports next.

The raw events are left in `DIR/capture`, mode `0600`. Before any of them is committed, remove the home path, `user_email`, `transcript_path` and any model text beyond the test prompt. Commit them as `fixtures/cursor-cli-<version>/` with a README, and delete the constructed files they replace.

A capture must confirm:

- **Payloads.**
  - The fields and value types the adapter reads: `conversation_id`, `generation_id`, `tool_use_id`, `tool_name`, `tool_input.file_path`, `tool_input.command`, `tool_input.cwd`, `cwd`, `workspace_roots`, `is_interrupt`.
  - The format of real IDs, and whether any falls outside the grammar.
  - Whether `postToolUse` and `postToolUseFailure` carry the same `tool_use_id` as their `preToolUse`. Admission completion depends on it.
- **Events.**
  - That `sessionStart` and `sessionEnd` fire under the CLI at all.
  - What `cursor_version` reports.
- **Replies.**
  - That a deny reply stops the write, and that `agent_message` reaches the model.
  - That the start and prompt `additional_context` reach the model. The prompt one in particular, which the documentation does not list.
  - That an empty reply lets the call proceed.
- **Transport.** That the heredoc transport is the one actually used.
- **Background commands.** Whether a background Shell call is marked in its input, and when its `postToolUse` arrives.
- **Claude Code hooks.** That a repository with both Claude Code and Cursor installs coordinates each Cursor call once.
- **Editor.** The editor's agent shares the documentation but not this CLI source. It needs its own capture, through the editor's Hooks output channel or the same `LANGSPAN_CAPTURE_DIR` variable.

## Consequences

- Cursor joins Claude Code and Codex on the same daemon, ID scheme, briefing renderer, spool and installer. Adapter-specific code is the envelope parser and the reply translation.
- The supported tools give whole-file admission only. Cursor's Write states a whole file, so two sessions editing different functions of one file conflict where Claude Code's `Edit` would not.
- If Cursor changes its transport, ID format or reply schema, the adapter fails toward no decision. It never fails toward a denial or an approval, except where Cursor itself blocks a reply it cannot read, and the adapter sends only documented fields.
- Until a capture exists, the README, usage guide, installer and `doctor` all qualify Cursor as "built from documentation, not yet verified on a real Cursor".
