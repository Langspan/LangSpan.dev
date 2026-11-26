# ADR 0035: One command to set up, a login service and an on-demand coordinator

**Status:** Implementation decision, 26 November 2026. Issue
[#159](https://github.com/Langspan/LangSpan.dev/issues/159), part 1:
`langspan setup`, the login service and the on-demand start. The install
script and the Homebrew tap follow in a later change. Narrows part of I6
([ADR 0025](0025-distribution-lifecycle-and-store-beyond-the-first-release.md)).
Tested on Linux with a temporary home, real hook and daemon processes, and a
stand-in `systemctl` (`tests/setup.rs`); the unit is checked with
`systemd-analyze verify` where installed. The LaunchAgent is covered by a unit
test of its generated plist only: macOS was not run. No run on a real login
session, through a logout or a reboot, is retained yet.

## Context

Getting from a binary to coordinated sessions took three steps: `langspan
install --apply` for the hosts, `langspan daemon` in a terminal kept open, and
the skill as a separate flag. Nothing started the coordinator after a reboot,
and a hook that found none gave no decision and no briefing. Each step loses
people who wanted to try LangSpan.

## Decision

**`langspan setup`** prints one plan, asks once, then applies it:

1. The state directory, then the dry run of `langspan install --skill` for
   the hosts found (or `--hosts`), without its "run with --apply" hint.
2. The coordinator: the login service file in full, or why there is none.
3. `Set this up? [Y/n]` on stderr. `--yes` skips the question. With standard
   input not a terminal and no `--yes`, setup prints the plan, says nothing
   was changed, and exits 0.
4. It applies the install, creates the state directory owner-only, writes
   and starts the service (or, with none, starts a coordinator on demand),
   and runs `langspan doctor`.

When no file would change, the service file matches and the service runs (or,
with no service, a coordinator accepts connections), setup prints `Already
set up; nothing to change.` and changes nothing. `--apply` and `--json` are
refused; `--skill` is refused because it is always on.

**The login service** runs the installed binary as `langspan daemon
--state-dir DIR`, with DIR the absolute state directory setup resolved,
restarts it after a failure, and appends its output to `daemon.log` in DIR.
Setup creates that log owner-only before the manager opens it.

| Platform | File | Started by | Restart |
| --- | --- | --- | --- |
| macOS | `~/Library/LaunchAgents/dev.langspan.coordinator.plist` | `launchctl bootout` (ignored when nothing is loaded), then `launchctl bootstrap gui/UID FILE`; `RunAtLoad` starts it at each login | `KeepAlive` with `SuccessfulExit` false |
| Linux | `$XDG_CONFIG_HOME/systemd/user/langspan.service` (default `~/.config`) | `systemctl --user daemon-reload`, `enable`, `restart`; `WantedBy=default.target` starts it at each login | `Restart=on-failure`, `RestartSec=5` |

Each file carries a marker line. A file at that path without it stops setup
before anything is written, and `uninstall` leaves it in place. The unit
refuses paths with whitespace, quotes, a backslash, `%` or `$`, which systemd
would split or expand. Every manager command is bounded to 10 seconds. Linux
without a reachable user manager (`systemctl --user show` fails), another
platform, or `--home` (a home that is not the login user's) gets no service;
setup says so and starts a coordinator on demand instead.

`langspan uninstall --apply` stops the service (`launchctl bootout` or
`systemctl --user disable --now`) and removes the file; its dry run says it
would. With `--home` it removes the file only. With no host configuration
left, uninstall still removes the service.

**The on-demand start.** A hook or `langspan mcp` that finds no coordinator
accepting connections, where no service file of LangSpan's names its state
directory, starts `langspan daemon --state-dir DIR` itself:

- **One coordinator.** The daemon's existing lock on `daemon.lock` already
  makes a second one exit before it touches the socket. On top of that, a
  caller starts a process only while it holds `autostart.lock`, checks again
  under it, and records the start on the sleep-inclusive monotonic clock
  (`crate::clock`). Within 30 seconds of a recorded start, no caller starts
  another, so a coordinator that cannot start costs each hook a lock and a
  small read, not a process and a wait. A record from before a reboot is
  larger than the clock and is ignored.
- **Bounded.** A hook spends at most half of what is left of its budget on
  this, including waiting for the lock and for the new coordinator to accept,
  then handles its event as before. `langspan mcp` waits at most 2 seconds.
- **Detached.** Its own process group; standard input and output are
  `/dev/null`, so the host never waits on the hook's output pipe; standard
  error appends to `daemon.log`; the working directory is `/`. Its
  environment keeps only `HOME`, `PATH`, the locale, `TMPDIR`, `XDG_*`,
  `USER`, `LOGNAME`, `SHELL`, `HOSTNAME`, `SSH_AUTH_SOCK`, `GIT_SSH`,
  `GIT_SSH_COMMAND`, the desktop session variables notices need, and
  `LANGSPAN_STATE_DIR`: a host's API keys and tokens are not passed on.
- **Logged.** The caller writes one structured line to its stderr
  (`coordinator_started` with the process ID, or `coordinator_not_started`
  with the reason), and one line to `daemon.log`. A `daemon.log` over 8 MiB
  is moved to `daemon.log.1` before a start.
- **Off switch.** `LANGSPAN_AUTOSTART=off` turns it off.

A coordinator started on demand runs until it is stopped or the machine
restarts; it does not exit when idle.

## Consequences

- A source build works without a terminal kept open: the first hook starts
  the coordinator.
- An on-demand coordinator that is running when setup installs the service
  keeps the lock; the service fails and retries every five seconds (systemd)
  or ten (launchd) until it can take over. Setup's plan does not stop it.
- The hook's hot path gains one nonblocking connect when the coordinator is
  running, and a small bounded file read when it is not.
- `daemon.log` has no rotation under the login service; only the on-demand
  start moves a large one aside.
- Still to retain: a real macOS login with the LaunchAgent, a real Linux
  user manager, and a logout or reboot that brings the coordinator back.
