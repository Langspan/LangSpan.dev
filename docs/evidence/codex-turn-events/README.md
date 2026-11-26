# Codex end of reply and first patch: native-host evidence

**Recorded:** 26 November 2026, macOS, a real signed-in Codex (`codex exec`,
model `gpt-6.2-sol`; the Codex version was not recorded). The owner ran
`scripts/capture-codex.sh` and trusted the throwaway project and its hooks by
hand in Codex's `/hooks` screen; the script passed no trust, approval or
bypass option. [ADR 0027](../../adr/0027-turn-and-batch-events.md) records
the decisions this exercises; issue #11 holds the acceptance criteria.

Files:

- [langspan-events.txt](langspan-events.txt): every coordinator event the run
  recorded, `SEQ JSON` per line, as `langspan events` printed them.
- The raw Codex hook events:
  [fixtures/codex-2026-11-26](../../../fixtures/codex-2026-11-26/README.md).

Changes from the raw record, and nothing else: the repository path is
`/work/repo`, and the run's name in the workspace and session names is
`XXXXXX`. Codex session IDs are the fixtures' placeholders. Lease, operation
and observation IDs, which LangSpan derives from the real session IDs, are
numbered (`lsh1-admit-1`, ...). Process IDs are `1001` and `1002`, process
start tokens, the repository ID and the commit ID are zeros, and every time
is the coordinator's millisecond clock less 2,058,000,000. Content and input
digests, prompts and briefing signatures are as recorded.

## The run

| Turn | What happened | Coordinator events |
| --- | --- | --- |
| 1 | Session A ran `cat notes.txt`, then an `apply_patch` whose context does not apply. LangSpan admitted it with the base unknown and recorded the admitted version as a `snapshot`. Codex sent no `PostToolUse` for the failed patch, then `Stop` and `SessionEnd` | 17-24 |
| Peer | `capture-peer` wrote `notes.txt` through `langspan hook claude-code` | 25-30 |
| 2 | Session A, resumed, was briefed (33), ran `cat notes.txt` and patched `beta` to `gamma`. LangSpan refused it once as stale, naming the peer (37). Codex ran `cat notes.txt` again and sent the identical patch (input digest `14104956...` both times), which was admitted (39) and succeeded (40, 41). Then `Stop` and `SessionEnd` | 33-46 |
| 3 | A CLI session held `notes.txt` (47-49). New session B was briefed (53), ran `cat notes.txt` and patched `alpha`; LangSpan refused it for the conflict, naming the holder (58). As told, Codex did not retry. `Stop` released nothing, then `SessionEnd` | 47-62 |

`langspan status` after turn 1 showed no lease and session A's activity
`"ended": true`. After turn 3 the holder's lease was the only one.

`tests/turn_events.rs` replays the 27 events through the real hook
executable and daemon, with the peer and the holder where the script ran
them, and compares the admissions, versions, refusals, stops, ends and prompt
digests with this record
(`the_real_codex_capture_replays_to_the_real_runs_coordination`). They
match, content and input digests included.

## Issue #11's criteria

| Criterion | Shown by this run |
| --- | --- |
| A turn that ends with an admission still open records `session_stopped` naming it, and `status` shows the file free before the TTL would have ended it | **Yes.** The failed patch's admission (18, TTL to 623011) was completed by `Stop` (21) and named in `session_stopped` (22) at 594249, 28.8 seconds before its TTL. `status` after turn 1 shows no lease |
| `Stop`'s empty output does not start a continuation prompt | **Yes.** Each turn sent one `Stop`, with `stop_hook_active: false`, and then `SessionEnd`; no second `Stop` or further prompt followed |
| `SessionEnd` ends the session within its 3-second limit, and `status` shows it ended | **Yes.** `SessionEnd` arrived 11 to 22 ms after `Stop` (the capture files' times), and `session_ended` (24, 46) came before the next run's first event, less than a second after `session_stopped`; 62 is the run's last event. `status` after turn 1 shows the session ended. How Codex treats a hook that runs past 3 seconds is not observed: none did |
| A first patch of an unread file records a `snapshot` version, and the hook diagnostic says so | **Yes for the snapshot** (19, the digest of `alpha\nbeta\n`). The capture keeps hook input only, not the hook's diagnostic; replaying the same event through the hook prints "not compared for 1 file(s)" and "recorded as seen" (`a_codex_patch_that_failed_still_makes_its_next_attempt_compared`) |
| A patch after a peer's change to a snapshotted file is refused once with the peer named, and the identical retry is admitted | **Yes.** `write_refused` `stale_base` with `peer_session_id` the peer (37), then the same input admitted (39). The refusal text Codex's model saw is not in the capture; its next actions (reread, then the same patch) are what that text asks for |
| A Codex subagent's tool event is refused and does not end the parent's reply | **No.** The run started no subagent. Still tested with a constructed event only |
| The capture script exits non-zero with a message, and writes nothing, when Codex is not signed in | Not part of this run; tested with a stand-in `codex` (`tests/capture_codex.rs`) |
| The captured events extend `fixtures/codex-<version>/`, and the two named tests run against them | **Yes,** as `fixtures/codex-2026-11-26/`, since the version was not recorded. The script now saves `codex --version` for the next capture |

## Observed, not changed here

- A Codex shell read is not a read for LangSpan, as ADR 0027 states: in turn 2
  Codex ran `cat notes.txt` after the peer's write and before its patch, and
  the patch was still refused once.
- The briefings at the resumed and the new session's start were recorded as
  delivered (33, 53). Whether the model read them was not checked: the host's
  transcript was not read.
