# Move, switch and remove a workspace

These are owner commands of the CLI only; no MCP tool moves or removes a
workspace ([ADR 0025](../adr/0025-distribution-lifecycle-and-store-beyond-the-first-release.md)).

```sh
langspan workspace list
mv /path/to/src/app /path/to/work/app
langspan workspace move app /path/to/work/app --request-id ws-move-1
```

```text
Workspace app now at /path/to/work/app. Kept: 6 notes, 2 pending messages, 14 check records, 3 landings.
Project hooks name the old root; rerun:
  langspan hooks install claude-code --workspace 'app' --root '/path/to/work/app' --state-dir '…' --apply
```

`workspace list [--after ID] [--json]` shows every workspace with its root and
whether it exists, its current sessions, notes, pending messages, check
records, and whether its automatic check and remote sharing are on. It answers
a page at a time, in workspace ID order: at most 100 workspaces, and fewer when
long roots would not fit one frame. When more remain, the page ends with
`More: langspan workspace list --after ID` (`next` in the JSON); run that for
the next page. A cursor whose workspace was removed since still continues after
it.

`workspace move ID NEW_ROOT` is for a checkout you moved: the old root must be
gone and NEW_ROOT must be the top of a Git checkout. With `--switch`, the old
root still exists and NEW_ROOT is another checkout of the same repository
(its object database holds the old root's `HEAD` commit), for example a fresh
clone that replaces a broken one; the old root then stops being coordinated
under the ID. `--switch` is refused (`root_derived_id`) when the ID is the one
user-level hooks derived from the old root, since new sessions there would
claim it again; move or delete the old checkout and run the move without
`--switch`, or register NEW_ROOT with `workspace add`. The ID, notes, pending
messages, check records, landings, the automatic check and the remote setting
are kept; an automatic check waiting or running in the old root is cancelled,
and the next writes in NEW_ROOT are checked there. File versions sessions read
under the old root are dropped, so a session's next edit of an existing file
has an unknown base until it reads it again; the next scan and the source
index start again from NEW_ROOT. A refused move changes nothing and says
which check failed: `not_checkout_top`, `old_root_exists`,
`old_root_missing`, `root_derived_id`, `different_repository` or
`workspace_conflict` (NEW_ROOT
is registered already or overlaps another workspace).

A checkout reached through a symbolic link is the same checkout. LangSpan
compares a registered root with the root Git reports by resolving links and
checking that both name the same directory (same device and inode). If you
move a checkout and leave a link at its old path, the workspace keeps
scanning, and sessions started through the link join it, under the
registered path. A link to a checkout of another repository (one without the
commit the last scan saw) stops the scan with `Git identity changed`.
`workspace move ID NEW_ROOT` to the link's target succeeds
without `--switch` and records NEW_ROOT. When the old root is a link to a
different directory, the `old_root_exists` refusal names the directory it
leads to; use `--switch` only if that is another checkout you mean to leave.
The checkout a registered link leads to cannot be registered, or receive a
moved workspace, under another ID. Its linked worktrees still can, at their
resolved paths: register a new one under the checkout's new place, or move
one registered under the old root to its resolved path. While the workspace
at the link holds an admission, such a registration is refused with
`workspace_in_use`; complete the admission or wait for it to expire.
If a registered root becomes a link to another registered workspace's
checkout, both workspaces refuse admission until you move or remove one of
them.

User-level hooks need nothing: an event from NEW_ROOT joins the moved ID.
Project-level hooks name their root in the host's configuration, which the
command does not edit; it prints the `hooks install` commands to rerun for the
hosts whose project hooks name this workspace.

`workspace remove ID` is a dry run that lists what would end: sessions,
intents, notes, pending messages and the automatic check. `workspace remove ID
--apply --request-id ID` takes the workspace out of `status`, `recap`,
`workspace list`, briefings and the dashboard; `langspan events` keeps its
history. A hook event from that root later registers it again, empty.

Moving and removing are refused while a session holds an admission there or a
landing holds or waits for its push turn (`workspace_busy`), and while remote
sharing is on (`remote_sharing_on`): run `langspan remote disable ID` first,
so the published ref is removed, and wait until that ref is gone (`langspan
doctor` shows its progress): the daemon removes it through the old checkout's
remote.
A refusal is recorded under its request ID; retry with a new one.
