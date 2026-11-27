# MCP

Configure an MCP client to launch the built executable with `mcp`:

```sh
./target/release/langspan mcp --state-dir /path/to/private/shared-state
```

Start the daemon separately with the same directory. MCP stdout is protocol
only. The official Rust SDK handles initialization and stdio transport.

| Tool | Operation |
| --- | --- |
| `langspan_workspace_register` | Register a canonical physical workspace |
| `langspan_session_register` | Register or reattach a logical participant |
| `langspan_intent_set` | Declare/update advisory work and file scope |
| `langspan_intent_end` | End advisory work independently of admission |
| `langspan_admit` | Request an entire file set or inspect conflicts |
| `langspan_complete` | Complete a currently valid held admission |
| `langspan_status` | Inspect open admissions, live or recent sessions, liveness observations and each session's last hook run (`summary.last_hook_age_ms`); `all` and `after` page through every record |
| `langspan_events` | Read an ordered page of persisted events |
| `langspan_changes` | Read file and local-branch observations with scan freshness |
| `langspan_brief` | Deliver bounded, relevant peer work to the asking session |
| `langspan_symbol` | Find indexed definitions by name, with source digests and coverage; with `detail`, also body digest, calls, callers, notes and last check; with `rank` and `session_id`, in the code briefing's order with each one's ranked dependents; with `renamed`, under the newest name a recorded rename leads to |
| `langspan_references` | Find name mentions, with coverage and truncation |
| `langspan_impact` | Find a file's exports and files mentioning those names; with `depth` 2 or 3, those files level by level |
| `langspan_path` | Find the shortest chains of calls from one name to another, each step found by import path or by name |
| `langspan_map` | Map the workspace's areas, with their files, exports and languages, and the imports and calls between them |
| `langspan_blind_spots` | List the files the index cannot answer for (partial, never parsed cleanly, waiting, too large, unscanned) and other languages by extension |
| `langspan_history` | A definition's recorded interface changes, newest first, with the last reported editor, its task line, nearby notes and messages, and changes back to an earlier signature; at most 50 |
| `langspan_check_write` | Before a write on a host without hooks: per path, its holder, other sessions' recent lines and whether it is stale; per symbol, a peer's recent signature change, removal or body lines, matched by name; records nothing |
| `langspan_integrate_check` | Combine branches onto the remote's target and run a command (an argument list, no shell, 1 to 3600 s) on exactly that tree; record the result. With `symbols`, also name definitions the branches changed in ways that disagree (advisory) |
| `langspan_land` | Dry run, or with `apply` take the push turn (waiting in line for up to `wait_s`, default 240) and push a combined result that has a passing check, retrying a moved target up to 3 attempts; conflicts come with their hunks |
| `langspan_land_queue` | Read who holds a target's push turn and who waits for it, in order |
| `langspan_flaky_tests` | Read which tests are known to flip on unchanged code, with runs, flips and the age of the last flip |
| `langspan_land_cancel` | Stop a landing waiting for the push turn, by its `request_id` |
| `langspan_recap` | Summarize one workspace's recorded activity, checks, notes and pending messages |
| `langspan_policy` | Read the coordination policy table in effect for a workspace: each case, advisory, wait or refuse, whether it can be set, and a wait's bound |
| `langspan_today` | The day so far from the event log: sessions, refusals, checks, landings and what is still open, within 2,400 bytes |
| `langspan_report` | The coordination report: per session and per host kind, edits admitted and not compared, shell writes seen after, reads, intents, refusals, unchanged retries and briefings, within 2,400 bytes |
| `langspan_remember` | Keep a local note about the workspace, named files or one definition (`symbol`), with up to 4 `tags` |
| `langspan_forget` | Remove a remembered note by ID |
| `langspan_recall` | Search a workspace's remembered notes: every query word in a note's text or paths, without case; best first, at most 50 |
| `langspan_send_message` | Send a local message to one session or every other session in the workspace |
| `langspan_show_message` | Read one message in full as it was sent, also after acknowledgement; for a briefing line marked `(cut; …)` |
| `langspan_message_status` | Ask which sessions were given a message, by ID or `"last"`: `delivered` (with `via` and age), `pending` or `ended`; read-only |
| `langspan_handoff` | Leave a message for the next other session to take up |
| `langspan_retract_message` | Withdraw a message this session sent, by ID or `"last"`; its readers are told to disregard it |
| `langspan_commit_mine` | Commit only this session's own uncommitted changes in a shared checkout, with the owner's identity and hooks |
| `langspan_my_changes` | List this session's own uncommitted files and hunks in a shared checkout, as `langspan_commit_mine` would split them; changes nothing |
| `langspan_adopt` | Take over an ended session's uncommitted lines in the same workspace; refused for a live session or another workspace's |

Every mutation tool requires a caller-owned `request_id` (for
`langspan_land_cancel`, it names the waiting landing). Conflicts are normal
results; unavailable coordination and domain errors are explicit tool errors.
The MCP briefing tool counts as delivery; the terminal `brief` command is a
peek. No tool launches agents, reads credentials, or changes workspace files.
`langspan_integrate_check` runs the given command as you, in a temporary
checkout, inside the `langspan mcp` process rather than the daemon; its output
goes to that process's standard error. `langspan_land` with `apply` pushes as
you, and `langspan_commit_mine` commits as you, running your Git hooks. Deny
any of them in your host's permission settings if agents should not do this.

## A shorter list on hooked hosts

`langspan mcp` lists every tool by default (`--tools all`). With
`--tools hooked --host claude-code` (or `--host codex`, the hosts whose hooks
report reliably), it leaves out `langspan_admit`, `langspan_complete`,
`langspan_session_register` and `langspan_workspace_register`, which the hooks
call for the agent, but only while that host's hooks report:

- The server asks the daemon's status when the client is initialized and then
  every 6 seconds, each with the client's bounded IPC deadline.
- It shortens the list once a session of that host (told by its ID prefix,
  `claude-code.` or `codex.`), in the registered workspace whose root is the
  deepest one holding the server's working directory, has hook activity in
  the daemon's current generation within the last 10 minutes.
- When that stops being true (10 minutes without a hook event, a new daemon
  generation, or the daemon not answering), every tool is listed again.
- Each change sends `notifications/tools/list_changed`; the server declares
  the `listChanged` capability only in this mode.
- Every tool answers when called by name, listed or not. The server
  instructions name the hidden tools and say the hooks call them.

Nothing new is stored: the decision is made from `status`. A server started
outside any registered workspace, or before any hook event, lists every tool.
