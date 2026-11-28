# Using LangSpan

Build, connect native sessions, inspect coordination, and integrate through MCP.
For the product overview and support summary, see [the README](../README.md).
For a visual introduction, open the [interactive workflow](https://langspan.dev/#demo),
[launch film](https://langspan.dev/#film) or [recorded dashboard evidence](https://langspan.dev/#evidence).
Examples in these pages use the binary built from the LangSpan repository root.
If you installed a release binary on your PATH, replace
`./target/release/langspan` with `langspan`.

Each topic has its own page under [usage/](usage/). The headings below link
to those pages and keep the anchors this guide had as a single page.

## [Set up in one command](usage/set-up-in-one-command.md)

### [See a refusal without an agent](usage/set-up-in-one-command.md#see-a-refusal-without-an-agent)

## [Install a release](usage/install-a-release.md)

## [Build from source](usage/build-from-source.md)

## [Start the coordinator](usage/start-the-coordinator.md)

## [Connect a native agent](usage/connect-a-native-agent.md)

### [All repositories for this user](usage/connect-a-native-agent.md#all-repositories-for-this-user)

#### [An agent tool installed later](usage/connect-a-native-agent.md#an-agent-tool-installed-later)

#### [A status line in Claude Code](usage/connect-a-native-agent.md#a-status-line-in-claude-code)

#### [A shorter MCP tool list](usage/connect-a-native-agent.md#a-shorter-mcp-tool-list)

### [One project](usage/connect-a-native-agent.md#one-project)

#### [Codex: start and resume at the resolved path](usage/connect-a-native-agent.md#codex-start-and-resume-at-the-resolved-path)

### [Antigravity](usage/connect-a-native-agent.md#antigravity)

### [Removing an earlier Gemini CLI install](usage/connect-a-native-agent.md#removing-an-earlier-gemini-cli-install)

### [The LangSpan skill](usage/connect-a-native-agent.md#the-langspan-skill)

### [A LangSpan block in the repository's AGENTS.md](usage/connect-a-native-agent.md#a-langspan-block-in-the-repositorys-agentsmd)

## [Source versions](usage/source-versions.md)

### [Lines another session wrote](usage/source-versions.md#lines-another-session-wrote)

### [Outline first for whole reads of large files](usage/source-versions.md#outline-first-for-whole-reads-of-large-files)

## [Coordination policies](usage/coordination-policies.md)

## [Automatic peer briefings](usage/automatic-peer-briefings.md)

### [Interface notices between a session's steps](usage/automatic-peer-briefings.md#interface-notices-between-a-sessions-steps)

## [Share context through a Git remote](usage/share-context-through-a-git-remote.md)

### [Hosts that accept pushes only to branches](usage/share-context-through-a-git-remote.md#hosts-that-accept-pushes-only-to-branches)

## [Try the coordination loop with the CLI](usage/try-the-coordination-loop-with-the-cli.md)

## [Recap, notes and messages](usage/recap-notes-and-messages.md)

### [What happened since the last session](usage/recap-notes-and-messages.md#what-happened-since-the-last-session)

### [Notes about a definition](usage/recap-notes-and-messages.md#notes-about-a-definition)

### [The trail a session leaves when it ends](usage/recap-notes-and-messages.md#the-trail-a-session-leaves-when-it-ends)

## [The day so far](usage/the-day-so-far.md)

## [The live dashboard](usage/the-live-dashboard.md)

## [Refusals this month](usage/refusals-this-month.md)

## [The coordination report](usage/the-coordination-report.md)

### [What a session did after a refusal](usage/the-coordination-report.md#what-a-session-did-after-a-refusal)

## [Desktop notices](usage/desktop-notices.md)

## [Look up symbols and affected files](usage/look-up-symbols-and-affected-files.md)

### [Check before a write on a host without hooks](usage/look-up-symbols-and-affected-files.md#check-before-a-write-on-a-host-without-hooks)

## [Checks and landing](usage/checks-and-landing.md)

### [Automatic checks after a session's writes](usage/checks-and-landing.md#automatic-checks-after-a-sessions-writes)

#### [Which tests a write broke](usage/checks-and-landing.md#which-tests-a-write-broke)

#### [The wider check after an interface change](usage/checks-and-landing.md#the-wider-check-after-an-interface-change)

#### [Tests that flip on unchanged code](usage/checks-and-landing.md#tests-that-flip-on-unchanged-code)

## [Commits in a shared checkout](usage/commits-in-a-shared-checkout.md)

### [Take over an ended session's uncommitted work](usage/commits-in-a-shared-checkout.md#take-over-an-ended-sessions-uncommitted-work)

## [MCP](usage/mcp.md)

### [A shorter list on hooked hosts](usage/mcp.md#a-shorter-list-on-hooked-hosts)

## [Move, switch and remove a workspace](usage/move-switch-and-remove-a-workspace.md)

## [Retries and recovery](usage/retries-and-recovery.md)

### [Mixed versions after an upgrade](usage/retries-and-recovery.md#mixed-versions-after-an-upgrade)

## [Configuration and limits](usage/configuration-and-limits.md)

### [Retention](usage/configuration-and-limits.md#retention)

## Error codes

A refused request answers with an error outcome: a `code` from this list and
a `message` that says what failed and what to do. The code is a string on the
wire, so a client shows the message of a code it doesn't know. The list is
committed in `crates/langspan-core/tests/golden/error_codes.txt`; removing or
renaming a code changes it.

Retryable means the same request can succeed later with nothing changed: the
cause is temporary, such as a busy workspace or an index that hasn't caught up.
Retry with the same request ID. Any other code needs a changed request or a
step first, which the message names.

| Code | Retryable | Meaning |
| --- | --- | --- |
| `adoption_chain` | No | Too many earlier adoptions or subagents lead to that session; commit part of the work first. |
| `adoption_changed` | No | The adoption's record no longer matches the coordinator's state; adopt again with a new request ID. |
| `already_adopted` | No | Another session already adopted that session's work. |
| `already_published` | No | The repository already publishes through another workspace. |
| `already_retracted` | No | The message was already withdrawn. |
| `coordination_unavailable` | Yes | The coordinator did not answer in time, or its queue is unavailable; the outcome is unknown. |
| `different_repository` | No | The new root is a checkout of another repository. |
| `index_export_failed` | No | The index export file could not be created or written. |
| `index_import_refused` | No | The index file could not be imported, or the workspace has no complete scan yet. |
| `index_unavailable` | Yes | The source index is busy or not available. |
| `internal` | No | A request reached a part of the coordinator that does not answer it; report it as a bug. |
| `invalid_adoption` | No | A session cannot adopt its own work. |
| `invalid_alert` | No | An interface alert names 1 to 8 notices. |
| `invalid_away` | No | The away gap is outside 1 ms to 30 days. |
| `invalid_base` | No | A base version or changed file names a path the request does not cover. |
| `invalid_batch` | No | A tool batch names too few or too many operations. |
| `invalid_branch_oids` | No | A branch's head and tree are supplied together, or not at all. |
| `invalid_branch_ref` | No | A branch is not a valid `refs/heads/` name. |
| `invalid_calls` | No | A write check's calls or renamed names are out of bounds. |
| `invalid_check` | No | A check's name, command or definition findings are out of bounds. |
| `invalid_check_report` | No | A check report's sessions, classes, interface details or output are invalid. |
| `invalid_checkout` | No | A checkout change names nothing that changed. |
| `invalid_command` | No | A checkout run's command is too long or has control characters. |
| `invalid_content_digest` | No | A file content digest is not 64 hexadecimal characters. |
| `invalid_coverage` | No | A coverage record names too many paths. |
| `invalid_cursor` | No | The status cursor was not given by this coordinator. |
| `invalid_deadline` | No | A lease deadline overflows the clock. |
| `invalid_digest` | No | A digest is not a lowercase SHA-256. |
| `invalid_event` | No | A recorded event is malformed or out of bounds. |
| `invalid_failure` | No | A stop failure is not a host error type. |
| `invalid_frame` | No | The request frame could not be read as JSON. |
| `invalid_git_oid` | No | A Git object ID is not 40 or 64 hexadecimal characters. |
| `invalid_graph_context` | No | A code-context query's names or qualifiers are invalid. |
| `invalid_hosts` | No | A host name or host list is invalid. |
| `invalid_id` | No | An ID is not 1 to 128 ASCII letters, digits, dots, underscores or hyphens. |
| `invalid_input_digest` | No | A tool input digest is not 64 hexadecimal characters. |
| `invalid_landing` | No | A landing records too many commits. |
| `invalid_left` | No | A briefing names too many sessions that left work. |
| `invalid_limit` | No | A count, window or limit is out of its range. |
| `invalid_line_digests` | No | Prompt line digests are out of bounds. |
| `invalid_lines` | No | An edited file's line ranges are invalid. |
| `invalid_message` | No | A message is too long, names too many files, or is addressed to its sender. |
| `invalid_move` | No | The workspace is already registered at that root. |
| `invalid_name` | No | A definition or symbol name is invalid. |
| `invalid_note` | No | A note is too long or names too many files. |
| `invalid_notice` | No | An interface notice or change is malformed. |
| `invalid_overwrites` | No | A write's overwritten files are invalid. |
| `invalid_participant` | No | A participant ID or name is invalid. |
| `invalid_path` | No | A path is not a normalized relative path, is repeated, or is missing. |
| `invalid_paths` | No | A request names too few or too many paths. |
| `invalid_payload` | No | A publication record's payload is too large. |
| `invalid_policy` | No | A coordination policy value is not allowed. |
| `invalid_presence` | No | A read is recorded as presence instead of an observed version. |
| `invalid_process_identity` | No | A process identity lacks a PID or start token. |
| `invalid_publication` | No | A publication lists too many sessions or exceeds its bounds. |
| `invalid_query` | No | A recall query is empty, too long or has too many words. |
| `invalid_reason` | No | A landing leaves the queue for a reason the caller cannot give. |
| `invalid_refusal` | No | Only a held commit is reported as a refusal. |
| `invalid_regions` | No | An admission's line regions are invalid. |
| `invalid_remote` | No | A Git remote name is invalid. |
| `invalid_request` | No | The request does not parse as a typed request, or a field is out of bounds. |
| `invalid_request_id` | No | A request ID is invalid. |
| `invalid_retention` | No | The retention period is outside its range. |
| `invalid_root` | No | A root is not a nonempty canonical absolute path. |
| `invalid_sequence` | No | A publication record does not carry the next sequence. |
| `invalid_session` | No | A session names itself as its parent. |
| `invalid_summary` | No | A summary is too long or not one line. |
| `invalid_symbol` | No | A symbol is not one name of 1 to 200 bytes. |
| `invalid_symbols` | No | A write check names too many symbols. |
| `invalid_tag` | No | A tag or tag list is invalid. |
| `invalid_test_name` | No | A test name is invalid. |
| `invalid_test_results` | No | Test results do not match their run. |
| `invalid_told` | No | A delivery tells too few or too many notices. |
| `invalid_ttl` | No | A lease or push-turn duration is out of range. |
| `invalid_turn` | No | A landing both renews and waits for a turn. |
| `invalid_wait` | No | A wait is out of range or given where it does not apply. |
| `invalid_window` | No | A time window starts after now or is out of range. |
| `invalid_workspace_verification` | No | A verified overlap names an unknown workspace or the same root. |
| `lease_expired` | No | The lease has expired; request a new admission. |
| `lease_id_reused` | No | The lease ID already exists; use a new request ID. |
| `lease_not_active` | No | The lease is no longer active. |
| `lease_not_found` | No | The lease does not exist. |
| `message_not_kept` | No | The message is older than the oldest kept event. |
| `no_current_evidence` | No | A landing has no passing check for exactly the landed tree. |
| `not_checkout_top` | No | The root is not the top directory of its checkout. |
| `not_holder` | No | Only the session holding the lease can do that. |
| `not_queued` | No | The landing is no longer waiting for the push turn. |
| `not_sender` | No | Only the sender, or the owner, can withdraw a message. |
| `old_root_exists` | No | A move without `--switch` names an old root that still exists. |
| `old_root_missing` | No | A move with `--switch` names an old root that no longer exists. |
| `other_workspace` | No | That session worked in another workspace. |
| `own_publication` | No | A workspace does not observe its own publication. |
| `policy_fixed` | No | That coordination policy is fixed. |
| `presence_off` | No | This session's presence is off. |
| `process_identity_mismatch` | No | The session already names another process. |
| `read_timeout` | Yes | The request was not received before the I/O deadline. |
| `receipt_unreadable` | No | The request ID's earlier outcome cannot be read by this build; use a new request ID. |
| `recipient_not_found` | No | The recipient is not a session of this workspace. |
| `remote_not_enabled` | No | The workspace does not publish. |
| `remote_sharing_on` | No | The workspace publishes, or its ref is not yet removed; disable it first. |
| `request_id_conflict` | No | The request ID belongs to a different command; use a new ID. |
| `response_too_large` | No | The response exceeds 64 KiB; page it or ask for less. |
| `root_derived_id` | No | User-level hooks derive the workspace's ID from its old root, which still exists; move or delete that checkout first. |
| `secret_in_command` | No | The command holds what looks like a secret and was not recorded. |
| `secret_in_test_name` | No | A test name holds what looks like a secret. |
| `sender_not_in_workspace` | No | The sending or withdrawing session belongs to another workspace. |
| `session_conflict` | No | The session ID is bound to another workspace. |
| `session_ended` | No | The session has ended. |
| `session_live` | Yes | That session has not ended yet. |
| `session_not_found` | No | The session is not registered. |
| `snapshot_mismatch` | No | Compaction found a snapshot that does not match the daemon's state; nothing was deleted. |
| `stale_lease` | No | The lease belongs to a previous daemon generation. |
| `stale_liveness_observation` | No | A liveness observation is older than the evidence held. |
| `stale_session` | No | The session belongs to a previous daemon generation; register it again. |
| `status_part` | No | A status record part is applied only as its whole record. |
| `storage_error` | Yes | The state database could not be read or written. |
| `storage_operation` | No | The core refuses a command the daemon's stores answer. |
| `symbol_ambiguous` | No | The name is defined more than once; name its file. |
| `symbol_not_found` | No | No indexed definition has that name. |
| `too_many_attempts` | No | A push turn records at most 3 landing attempts. |
| `too_many_inputs` | No | A check or landing combines too many commits. |
| `too_many_paths` | No | A request names too many paths. |
| `too_many_related_operations` | No | A disk observation correlates too many operations. |
| `too_many_verified_overlaps` | No | A registration verifies too many overlapping workspaces. |
| `turn_lost` | No | The landing's push turn has ended. |
| `unknown_message` | No | No such message is held for this sender. |
| `unknown_notice` | No | The interface notice is no longer held. |
| `unknown_session` | No | A checkout run names an unregistered session. |
| `unknown_workspace` | No | The workspace is not registered. |
| `unsupported_version` | No | The request's protocol version is not served; restart the daemon after upgrading. |
| `workspace_busy` | Yes | Another session's admission or landing holds the workspace. |
| `workspace_changed` | Yes | The workspace moved, switched or was removed after the report was observed. |
| `workspace_conflict` | No | The ID or root is bound to another workspace, or overlaps one. |
| `workspace_in_use` | Yes | A current admission in an overlapping workspace covers this root. |
| `workspace_not_found` | No | The workspace is not registered. |
