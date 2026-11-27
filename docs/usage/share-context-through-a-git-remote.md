# Share context through a Git remote

The preview can exchange work summaries through an existing Git remote. Each
machine runs its own daemon with its clone registered as a workspace. Git uses
its existing authentication. The project owner's choice of transport and payload
remains open; [ADR 0006](../adr/0006-remote-awareness-through-git-refs.md) records the
implemented option and its limits.

To start sharing, replace `PROJECT` with a registered workspace ID and choose the
name teammates will see. `origin` is the Git remote name; choose another with
`--remote` if needed:

```sh
./target/release/langspan remote enable PROJECT \
  --name "Your name" --remote origin --request-id remote-enable-1
./target/release/langspan remote outbox PROJECT --json
./target/release/langspan remote sent PROJECT --limit 5
```

`remote sent` lists what has already left the machine, newest first: the time,
the sequence and the exact payload that was pushed.

Enabling starts background publication and receipt; it has no dry-run or
`--apply` step. The outbox command works only after sharing is enabled. It shows
the participant name and ID, session prefixes, host kinds, branches, and current
and recent file paths. Source contents stay out of this payload. The first line
of a session's prompt is included only with `--share-task on`; changing that
setting requires another enable command with a new request ID. One publishing
workspace covers its related worktrees.

Remote work appears in briefings as advice and does not hold files or deny local
edits. Its age is measured from receipt on this machine; work disappears from
briefings after ten minutes without a new publication. Remote failures leave
local coordination running. This exchange has been tested with two daemons and
two clones on one machine; hosted remotes and two-machine use remain unverified.

## Hosts that accept pushes only to branches

By default each machine publishes to `refs/langspan/peers/ID`, outside the
repository's branch list. Some hosts refuse every push outside `refs/heads/`; a
Claude Code cloud environment's Git proxy answered HTTP 403. The coordinator's
log then shows Git's own refusal text and suggests the option below. To publish
as a branch instead:

```sh
./target/release/langspan remote enable PROJECT \
  --name "Your name" --publish-as-branch --request-id remote-enable-2
```

The ref becomes the branch `langspan/peers/ID`. Everyone in the repository sees
it in the branch list, and a teammate's plain `git fetch` copies it into their
remote-tracking branches. Each `remote enable` states every choice again, so
leaving out `--publish-as-branch` (or `--share-task on`) turns it off. Moving
between the two removes the ref in the namespace no longer used. Every machine
reads both, so teammates who publish either way see each other.

Stop sharing with:

```sh
./target/release/langspan remote disable PROJECT --request-id remote-disable-1
```

The daemon stops publishing and retries removal of its remote ref until the
remote confirms it is gone. If the host refuses to delete it, the daemon says so
in its log with the exact ref to delete by hand, and replaces the ref once with
a final publication marked as withdrawn, so teammates stop showing your work.
`langspan doctor` keeps reporting the ref as not removed until it is gone; the
coordinator checks again about every ten minutes. Remote setup has no MCP tool. An agent with shell
access can run these CLI commands under its host's permission controls.
