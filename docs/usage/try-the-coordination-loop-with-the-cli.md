# Try the coordination loop with the CLI

For a one-command version through the hook, run
[`langspan demo`](set-up-in-one-command.md#see-a-refusal-without-an-agent). This standalone example
uses a fresh state directory, so its demo workspace cannot collide with an existing hook setup. In a new terminal at the LangSpan
repository root, create the private directory, note its printed path, and run a
separate coordinator:

```sh
LANGSPAN_DEMO_STATE="$(mktemp -d)"
printf '%s\n' "$LANGSPAN_DEMO_STATE"
./target/release/langspan daemon --state-dir "$LANGSPAN_DEMO_STATE"
```

In another terminal at the same repository root, set `LANGSPAN_STATE_DIR` to the
path printed above, then register two logical sessions:

```sh
export LANGSPAN_STATE_DIR=/path/printed/by/the/first/terminal
./target/release/langspan workspace add demo "$PWD" --request-id workspace-1
./target/release/langspan session start alice demo --request-id alice-session-1
./target/release/langspan session start bob demo --request-id bob-session-1
./target/release/langspan intent set alice src/lib.rs --summary "Review shared operations" --request-id alice-intent-1
./target/release/langspan status
./target/release/langspan brief bob --request-id bob-brief-demo-1
```

Request an overlapping file, then unrelated work:

```sh
./target/release/langspan admit alice src/lib.rs --ttl-ms 60000 --request-id alice-edit-1
./target/release/langspan admit bob src/lib.rs --ttl-ms 60000 --request-id bob-edit-1
# The second request reports a conflict and exits with code 2.

./target/release/langspan admit bob src/main.rs --ttl-ms 60000 --request-id bob-other-1
# The unrelated file can be admitted.

./target/release/langspan complete alice alice-edit-1 --request-id alice-complete-1
./target/release/langspan complete bob bob-other-1 --request-id bob-other-complete-1
./target/release/langspan admit bob src/lib.rs --ttl-ms 60000 --request-id bob-edit-2
./target/release/langspan complete bob bob-edit-2 --request-id bob-complete-2
./target/release/langspan events --after 0 --limit 20 --json
./target/release/langspan changes demo --after 0 --limit 20 --json
```

Run admission and completion within their requested lifetime. Stop the demo
coordinator with Ctrl+C when finished. These commands coordinate logical
participants; they do not edit the files or launch agents.
Use a fresh request ID for a new attempt after a conflict. If repeating the
example against an existing state directory, use fresh demo identities rather
than expecting historical receipts to become new work.

`status` shows what matters now, in one answer however long the coordinator has
run: workspaces, sessions that hold an open admission, were active in the last
hour, are seen alive or have just registered, and the admissions still open.
Completed, expired and invalidated admissions are counted per session in the
`summary` (`closed_admissions`), not listed. If the current state is still too
large for one 64 KiB answer, the summary names what it left out (`omitted`,
`omitted_sessions`); workspaces, links, turns and open admissions stay, and an
automatic check is left out only when its command alone is near a frame.
LangSpan's own commands that decide from one of these parts (`land` recognising
a retried landing, `doctor` judging a workspace's scan, `auto-check`) read it
complete when the view left it out. To read every record, history included,
page through it:

```sh
./target/release/langspan status --all --json
# Each page's outcome holds `records` and `next`; pass `next` back for the following page.
./target/release/langspan status --all --after 'lease:0:alice-edit-1' --json
```

Each record is `{"kind": ..., "key": ..., ...}` with the record's state-map key.
One record larger than a page (an entry at its limits, such as a session's 128
known file versions with long paths) comes as `{"kind": "part", "text": ...,
"offset": ..., "last": ...}` records instead: the `text` of its parts, joined in
order, is that record's JSON. A part at offset 0 starts the record again,
because it changed while it was being read. The cursor is opaque: pass back only
a `next` value. A record changed between pages is read as it is when its page
is answered. MCP's `langspan_status` takes the same `all` and `after` arguments.

`changes` reads persisted file and local-branch observations, with scan freshness
information. A disk change is unattributed; related operation IDs are a
correlation, not proof of authorship. Initial baselines and incomplete scans are
kept distinct from newly observed changes. Process registration alone is not
proof a process is alive; Linux process observation is available through
`session attach-pid SESSION PID --request-id ID`.
