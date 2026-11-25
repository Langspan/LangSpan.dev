# ADR 0006: Cross-machine awareness through the repository's Git remote

**Status:** Implementation decision for the owner-directed coordination work,
24 November 2026. **The choice of transport and payload is the owner's to
confirm**; this ADR records what was built so that
choice can be made against something that runs. Nothing is published unless
the owner enables it per workspace. Extends
[ADR 0004](0004-automatic-peer-briefing.md).

## Context

Two developers' agents, on two machines, need to learn of each other's work
without either developer relaying it. That needs a place both machines can
reach, an answer to who may take part, and rules for ordering, loss and
disconnection. The engineering rules add: no credential handled by LangSpan,
no listener without explicit configuration, no infrastructure the product
does not need, and no comparison of wall clocks across machines.

## Decision

**Transport: the repository's own Git remote.** Each participant owns one ref
on the remote, `refs/langspan/peers/<participant>`. It points at a single
commit with no parent, holding one file, `state.json`, of at most 16 KiB. Each
publication replaces that commit, so the ref carries the current state and no
history: a task line the owner later stops sharing is not left reachable from
it. Replacing it is a forced update of LangSpan's own single-writer ref; no
branch is ever force-pushed. The daemon publishes with `git push`
and receives with `git fetch` into `refs/langspan/remote/<remote>/*`.

This was chosen over a relay server because it needs none: every team that
shares a repository already has the reachable place, the transport security,
the authentication and the access list. Whoever may push to the repository may
take part, and nobody else can. LangSpan never sees a credential; Git and its
helpers hold them. The cost is latency (one pass every 5 s by default, plus
the push and fetch).

**Opt-in, per workspace, by the owner.** `langspan remote enable WORKSPACE
--name NAME [--remote origin] [--share-task on] [--publish-as-branch]`. There is deliberately no MCP
tool for it. An agent that can run shell commands can still run the CLI, as
the owner can; what stands between it and that command is the host's own
permission prompt, not LangSpan. One repository publishes through one
workspace, which covers its worktrees. `langspan remote outbox
WORKSPACE` prints exactly what is, or would be, published. Every publication
that left is an event (`remote_published`) in the local log, holding the exact
`state.json` pushed and the time of the push; `langspan remote sent WORKSPACE
[--limit N]` lists them, newest first. The payload is kept in the event rather
than a side table: it is at most 16 KiB, JSON escaping at most doubles it, so
one record fits both the 64 KiB frame and the 48 KiB event page, and the
listing stops before 48 KiB and says so. Records written before payloads were
kept are listed without one.

**Payload.** Participant ID (32 hex characters, created once per state
directory; an identifier, not a secret), the name the owner chose, a sequence
number, and for at most 8 local sessions in this repository: host kind, the
first 8 characters of the session ID, branch, up to 8 paths being edited and
up to 8 recently edited. The task line (the first line of the owner's prompt)
is included only with `--share-task on`. No file content, no digests, no
prompt beyond that line, nothing from other repositories.

**Authority.** None crosses machines. A teammate's publication never admits,
denies, holds or releases anything. It appears in briefings, labelled as
another machine's, with any files shared with the asking session's own work.
Two developers editing the same path in their own clones are never
serialized; the overlap is reported to both.

**Ordering.** A sender numbers its publications; the number only rises. A
receiver keeps one publication per sender and replaces it only with a higher
number, so a replay or a late arrival changes nothing. No clock is compared
across machines: a publication's age is the time since this daemon received
it.

**Identity.** The ref name is the identity the remote vouches for. A payload
that names a different participant than its ref is discarded.

**Disconnection and staleness.**

| Situation | Behavior |
| --- | --- |
| Remote unreachable | Logged once. Local admission, briefing and observation are unaffected. Publication and receipt resume on the next successful pass. |
| A teammate stops publishing (offline, daemon stopped) | Their last publication is shown with its age for 10 minutes after it was received, then dropped as disconnected. A sender with listed work republishes every 2 minutes so that quiet is distinguishable from gone. |
| A teammate disables publication | Their daemon deletes their ref and keeps trying, across restarts, until the remote confirms it is gone. Receivers see the ref gone and drop them at the next pass. The sequence is kept, so enabling again never reuses a number. If the host refuses the delete, the ref is replaced with a withdrawn publication and receivers drop them on reading it; see below. |
| Receiving daemon restarts | Everything received earlier is discarded and received again. |
| Publishing daemon restarts, or its state is reset | The sequence continues from the event log. With a reset state directory the participant ID changes, so it is a new participant; the old ref ages out. |
| A push is rejected | Nothing is recorded as published; the next pass tries again. |
| A push succeeds but recording it fails | The same sequence is pushed again next pass, replacing the ref with the same content. |
| More than 64 participants | The first 64 are read. Nobody is treated as withdrawn in a pass that did not list everyone. |

**The owner's repository.** Publication writes one blob, one tree and one
commit through a private index file, and two refs under `refs/langspan/` (with
`--publish-as-branch`, the remote ref is the branch
`refs/heads/langspan/peers/<participant>`; see below). It
does not touch the working tree, the index, `HEAD`, any branch, or Git
configuration. Pushes of this ref skip the owner's pre-push hooks
(`--no-verify`): those are written for branches, and the ref carries no
source. Refs under `refs/langspan/` are not branches or tags, so hosting
services do not run branch pipelines for them.

## Decisions the owner is asked to confirm

This repository's own rules for contributors forbid force pushes and
`--no-verify`. The daemon does the equivalent of both to LangSpan's own ref on
a remote, and only after the owner runs `langspan remote enable`. They are
product behaviour, not something a contributor runs, but they are the kind of
thing those rules exist to make someone stop and ask about.

| Behaviour | Why it was chosen | The alternative |
| --- | --- | --- |
| Each publication replaces the ref's single commit (a forced update of `refs/langspan/peers/<participant>` only); **if the remote refuses forced updates, it falls back to a fast-forward** | Nothing published earlier stays reachable, where the remote allows it | Always fast-forward: works everywhere, but every earlier publication, including task lines later switched off, stays readable in the ref's history |
| Pushes of that ref skip the owner's pre-push hooks | Those hooks are written for branches and can run a whole test suite; this ref holds one small JSON file and is pushed whenever work changes | Run the hooks: slower, and a failing hook stops publication |
| The repository's Git remote is the transport | No server, no credential handled, access is the repository's | A relay service the owner runs or trusts |
| Task lines are off unless `--share-task on` | A prompt's first line can hold something private | On by default, for fuller awareness |

**Found on a real second machine (24 November 2026).** A cloud machine's Git
proxy refused the forced update even though plain pushes to the same repository
succeeded; remotes with a "block force pushes" rule refuse it the same way.
LangSpan now tries the forced replacement first and, when it is refused, pushes
the same content as a fast-forward on top of what the remote holds. On such a
remote earlier publications, including task lines, stay reachable in the ref's
history. Test `a_remote_that_refuses_forced_updates_still_receives_every_publication`
reproduces the refusal with a server-side `update` hook.

**Found on a real cloud machine (25 November 2026): branches only.** A Claude
Code cloud environment's Git proxy, pushing to github.com over HTTPS, accepted
`git push origin C:refs/heads/langspan-probe/cloud` and refused with HTTP 403
(`RPC failed; HTTP 403 curl 22 The requested URL returned error: 403`) each of
`C:refs/langspan/probe/cloud`, `+C:refs/langspan/probe/cloud2` and
`C:refs/notes/langspan-probe`, and every delete push (`:refs/...`), even of the
branch it had just created. Publication to `refs/langspan/peers/<participant>`
can therefore never work there, forced or fast-forward.

Decision: an owner-chosen second namespace. `langspan remote enable ...
--publish-as-branch` publishes to `refs/heads/langspan/peers/<participant>`
with the same commit and fast-forward logic; the choice is part of the remote
link (`publish_as_branch`, absent in older records, which keep the default).
Every reader fetches both namespaces into LangSpan's own tracking refs
(`refs/langspan/remote/<remote>/` and `refs/langspan/remote-branch/<remote>/`)
with an empty `--refmap`, so a mixed team sees each other; a participant found
in both is taken at the higher sequence. Moving between the namespaces deletes
the ref left in the old one. A refused publication keeps the end of Git's own
refusal text (its last six lines, at most 1 KiB, control characters removed and
passed through the same secret redaction as every recorded text) in the error
and the daemon log, and outside `refs/heads/` suggests the option. A refusal
Git describes as HTTP 401/403, "refusing", "denied" or "protected" is the host
deciding, so it goes straight to the longest wait of the shared `Backoff`
(10 minutes) for that push alone: a different publication is tried at once,
and the pass itself is not backed off, so receiving teammates goes on. Any
other failure of the push or fetch counts toward the pass's `Backoff` (5 s,
30 s, 2 min, 10 min) as before. The fast-forward fallback stays for hosts that
refuse only forced updates.

It is opt-in, not a fallback the daemon picks, because the trade-off is the
owner's:

| Branch namespace | Cost |
| --- | --- |
| Works where only branches may be pushed | The ref is in the branch list of everyone with read access, and their plain `git fetch` copies it into `refs/remotes/` |
| Same payload and validation | Branch rules of the host apply to it (protection, naming, required checks); a branch named `langspan` in the repository would conflict with it |
| No pipeline config in its tree | A CI system configured outside the commit may still run for a pushed branch |
| | On a host that also refuses forced updates, earlier publications stay in its history (the existing fast-forward cost) |

**Withdrawal where deletes are refused.** The measured proxy refuses every
delete. `remote disable` then cannot remove the ref. The daemon says so in its
log with the exact ref to delete by hand, replaces the ref once with a final
publication marked `withdrawn` (no work listed, the next sequence, recorded as
a `remote_published` event with its exact payload, so `langspan remote sent`
lists it), and keeps the link marked not yet withdrawn. A permanent refusal of
the delete is asked again only after the longest wait, in case the owner
deleted the ref by hand meanwhile:
`langspan doctor` and `status` name the ref until a later pass sees it gone. A
receiver treats a withdrawn publication as the sender having left: no sessions
shown, no briefings. A receiver running an earlier LangSpan rejects the
unknown field and drops the sender only after the ten-minute staleness limit.
Tested against a local hub whose `update` hook imitates the proxy, and run
once against the cloud proxy itself in the two-machine run 3 (see below):
the delete was refused with HTTP 403, the withdrawn publication was pushed,
and the other cloud coordinator dropped the sender (`remote_forgotten`). Both
refs were left on the remote for deletion by hand.

## Consequences and limits

- **Run once on two machines, by one person.** The tests run two daemons
  with separate state and two clones against one shared remote on a single
  machine, through real `git push` and `git fetch`; that proves the protocol.
  [Two-machine run 3](../evidence/two-machine/README.md) (25 November 2026,
  LangSpan `592e284`, recorded in PR #45) added one run between a Mac and a
  Claude Code cloud machine through GitHub, with real `claude -p` and
  `codex exec` sessions. It showed:
  - the cloud publishing to `refs/heads/langspan/peers/<participant>`
    (`--publish-as-branch`) and the Mac to `refs/langspan/peers/<participant>`,
    each side receiving the other's;
  - briefings in both directions: Codex on the Mac quoted a running cloud
    session's task, and cloud sessions were briefed about the Mac's;
  - withdrawal where the host refuses deletes, as above.

  It did not show: two developers (one person ran fixed test prompts); any
  session changing its edit because of a briefing (one added a merge-conflict
  warning); two live sessions on different machines each told of the other's
  overlap (only one side was told); mid-turn delivery across machines (every
  briefing came at session start); landing across machines; network failure,
  a restart or the ten-minute staleness drop; or any host other than GitHub.
  It also found that the same code answers "is this work current?"
  differently by platform: on Linux an exited session leaves the publication
  at once, so a short headless session is invisible to sessions that start
  later elsewhere, while on macOS, where process liveness is unknown, an ended
  session's edits stay published for the 30-minute horizon.
- A hosting service may reject or hide refs outside `refs/heads` and
  `refs/tags`, or count them against quotas. GitHub, pushed to from the Mac in
  run 3, accepted and served `refs/langspan/peers/*`; the cloud's Git proxy
  refuses them (above). Other hosts are untested.
- Anyone with read access to the repository can read every publication,
  including task lines if shared. Anyone with push access can publish as a new
  participant, delete another participant's ref, or overwrite it with a forged
  one if the remote allows non-fast-forward pushes to it. The trust boundary
  is the repository's own.
- Latency is seconds, not milliseconds. It is awareness for the next prompt,
  not a lock.
- Replaced commits become unreachable on the remote; when the hosting service
  actually deletes them is its own garbage collection.
- Each pass runs `git fetch`, and `git push` when something changed. Nothing
  waits for a person: terminal prompts are off, `GIT_ASKPASS` is
  `/usr/bin/false`, and SSH runs with `-o BatchMode=yes`, so a key that needs a
  passphrase or a touch fails instead of prompting; such a setup needs an
  agent. An owner who set `GIT_SSH_COMMAND`, `GIT_SSH` or `core.sshCommand`
  keeps their own command, which must then not prompt. Credential helpers
  still run. Every Git child is its own process group, killed whole at the
  20 s deadline, so a hanging helper cannot outlive the pass.
- Repeated failures of the same network operation (fetch, publish, withdraw)
  back off: 5 s, 30 s, 2 min, then every 10 min, reset by a success and logged
  once per change. A teammate's rejected publication is not a network failure
  and does not delay the next pass. Local coordination never waits for any of
  this.
- Task lines, like every prompt summary, pass through the hook's redaction of
  likely secrets before they reach the daemon; a secret in a shape it does
  not recognise is still published if task lines are shared.
- A teammate's publication rejected as invalid is logged once per distinct
  reason, not per pass.
- Separate clones on one machine are grouped only if both publish.
- Source graph, semantic change notices and check evidence are not yet part
  of the payload.

### A move between namespaces while a push is under way

Found on macOS CI: a pass read the link, the owner switched to
`--publish-as-branch`, and the pass then pushed to the old ref and recorded
it. The record restored the signature the switch had cleared, so the branch
ref was never written and the old ref was never removed. A recorded
publication now says which namespace it went to (`as_branch`); one that went
to a namespace the owner has since left uses up its sequence but leaves the
signature unset, so the next pass publishes to the current one. Records from
earlier builds have no `as_branch` and are taken as current. Test:
`a_push_to_the_old_namespace_recorded_after_a_move_does_not_cancel_the_move`.
