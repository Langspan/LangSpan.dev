# ADR 0039: What authority each command needs

**Status:** Implementation decision, 27 November 2026
([issue #334](https://github.com/Langspan/LangSpan.dev/issues/334); its
enforcement is [issue #335](https://github.com/Langspan/LangSpan.dev/issues/335)).
Adds to [ADR 0001](0001-local-coordination.md)'s local trust boundary.

## Context

The coordinator identified no caller. Access rested on the `0700` state
directory and the `0600` socket, and every client got the owner's full command
set, including the commands that make the coordinator run a program, read or
write a file at any path, publish to a remote or delete history.

The threat model accepted this because any process of the same user could
change those files directly. That reasoning doesn't cover a caller its host
confines: an agent whose sandbox stops it running programs outside it, writing
outside its workspace or reaching the network, but which can still connect to
the socket. The coordinator runs outside that sandbox. Asking it to act is a
confused deputy: the confined caller borrows the owner's authority. Which host
sandboxes allow the connection has not been tested. "Set through the CLI only"
meant there is no MCP tool, and read like an access control it wasn't.

## Decision

**Every command has one of two classes**, given by one exhaustive `match` in
the core (`langspan_core::authority`). A new `Command` variant doesn't compile
until it has a class.

- **Coordination**: records or reads coordination state. Any client of the
  socket may send it.
- **Owner grant**: the coordinator acts beyond that state with the owner's
  authority. Seven commands:

| Command | CLI | Why |
| --- | --- | --- |
| `auto_check_enable` | `langspan check auto WORKSPACE -- COMMAND` | Runs a program after a session's writes. |
| `remote_enable` | `langspan remote enable` | Sends work metadata off the machine. |
| `graph_export` | `langspan graph export` | Writes a file at any absolute path. |
| `graph_import` | `langspan graph import` | Reads a file at any absolute path. |
| `set_policy` | `langspan policy set` | Changes the safety control every session's admission meets. |
| `workspace_remove` | `langspan workspace remove` | Ends a workspace's sessions, notes and messages. |
| `compact` | `langspan compact` | Deletes events and receipts. |

The class is per command, not per argument: a dry run of `compact`,
`workspace_remove` or `graph_import` is an owner grant too, so one rule covers
each command. Turning a grant off (`auto_check_disable`, `remote_disable`) and
`workspace_move`, which rebinds a workspace only to a checkout of the same
repository, are coordination.

**The coordinator answers only its owner.** On accept it reads the peer's user
from the socket (`SO_PEERCRED` on Linux, `getpeereid` on macOS) and closes a
connection whose effective user isn't its own, or whose user it can't read,
logging `peer_refused`. The owner-only directory already keeps other users out;
this holds if that directory is ever opened.

**An owner grant needs the owner's setting** (proposed, not yet in force:
[issue #427](https://github.com/Langspan/LangSpan.dev/issues/427) waits on the
owner's decision; until then no command's behavior changes). The coordinator
carries one out only while `"owner_grants": true` is in `config.json` in the
state directory, read again for each owner-grant request, so the owner turns it
on and off without a restart. Otherwise it refuses with an error that names the
setting. The setting is not on the wire, in a hook or in an MCP tool: a
confined caller that can reach the socket but can't write the state directory
can't turn it on. Confirmation on a terminal was the other choice; the
coordinator can't tell whether its client has a person at a terminal, so it
would be the client's own claim.

## STRIDE at the socket

| Threat | Treatment |
| --- | --- |
| Spoofing | Another user's connection is refused by its kernel-reported user. A same-user client can claim any session ID, as before. |
| Tampering | Coordination commands can record false facts, as before; an owner grant can't change what runs, what leaves the machine or what is deleted without the owner's setting. |
| Repudiation | A refused peer is logged with its user ID. Owner grants carried out are recorded as before. |
| Information disclosure | Unchanged: any same-user client reads the coordination state. Index export, the one command that writes it to a path, is an owner grant. |
| Denial of service | A refused peer is closed at once and holds no client slot past the check. A same-user client can still fill the slots. |
| Elevation of privilege | A confined same-user caller is limited to coordination until the owner's setting allows owner grants. |

## Consequences

- No coordination command changes. Each owner-grant command refuses until the
  owner allows it, which is a breaking change for scripts that run them.
- While the setting is on, any same-user client can send owner grants. The
  owner can turn it on for one command and off again.
- A caller that can write the state directory, or run unconfined as the owner,
  is the owner as far as LangSpan can tell. The control holds against a caller
  confined away from the state directory, not against the owner's own
  unconfined processes.
- Which host sandboxes can reach the socket is open evidence in the
  [threat model](../security/threat-model.md).
