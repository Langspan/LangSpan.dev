# Antigravity hook events: constructed, not captured

**These files were written by hand. No Antigravity run produced them.** They
stand in until a real capture exists. Tests built on them show that LangSpan
handles this shape. They do not show that Antigravity sends it.

The shape follows Antigravity's hooks documentation,
<https://antigravity.google/docs/hooks/>, read on 25 November 2026. The page
carries no version or date. [ADR 0035](../../docs/adr/0035-antigravity-adapter.md)
lists what it leaves unclear.

- The common fields: `conversationId`, `workspacePaths`, `transcriptPath`,
  `artifactDirectoryPath`, `modelName`, `lastUserInput`.
- Tool events: `toolCall` (`name`, `args`) and `stepIdx`; `PostToolUse` adds
  `error`, empty when the call worked.
- `PreInvocation`: `invocationNum` and `initialNumSteps`.
- Tool argument keys as the page lists them: `write_to_file` `TargetFile`,
  `Overwrite`, `CodeContent`, `Description`; `replace_file_content`
  `TargetFile`, `Instruction`, `Description`, `AllowMultiple`,
  `TargetContent`, `ReplacementContent`, `StartLine`, `EndLine`;
  `multi_replace_file_content` `TargetFile`, `Instruction`, `Description`,
  `ReplacementChunks` (its chunk fields are not documented, so the list is
  empty here); `view_file` `AbsolutePath`; `run_command` `CommandLine`,
  `Cwd`, `WaitMsBeforeAsync`.

No event names itself: the installed command passes `--event NAME`, and each
file below is sent under the event in its row.

Values that are placeholders, not observed:

- The IDs are made up, in the UUID form the page describes.
- `stepIdx` values are made up; a call's `PreToolUse` and `PostToolUse` share
  one here, which is not verified.
- `modelName` is `placeholder-model`; `transcriptPath` and
  `artifactDirectoryPath` are placeholders, written with a leading `~` as
  the documentation's examples are.
- `/work/demo` is a placeholder root. The tests replace it with a temporary
  repository.

| File | Event | Used for |
|---|---|---|
| `session-start.json` | `SessionStart` | registration and the start briefing |
| `pre-invocation.json` | `PreInvocation` | the prompt summary, briefing and code briefing |
| `pre-write-to-file.json` | `PreToolUse` for `write_to_file` | admission of a whole file |
| `pre-replace.json`, `post-replace.json` | `PreToolUse`, `PostToolUse` for `replace_file_content` | region admission and observation |
| `pre-multi-replace.json` | `PreToolUse` for `multi_replace_file_content` | admission of a whole file |
| `post-view-file.json` | `PostToolUse` for `view_file` | the read version |
| `pre-run-command.json`, `post-run-command.json` | `PreToolUse`, `PostToolUse` for `run_command` | shell admission and observation |

To capture a real run, set `LANGSPAN_CAPTURE_DIR` to an existing directory in
the environment Antigravity runs its hooks in, install the hooks, and run one
session that reads, edits and runs a command. Each event the hook receives is
written there as one file. Put the captures in `fixtures/antigravity-<version>/`
with their own README, and delete each file here that a capture replaces.
