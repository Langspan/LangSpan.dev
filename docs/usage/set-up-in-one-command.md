# Set up in one command

With `langspan` on your PATH (a [release](install-a-release.md#install-a-release) or a [source
build](build-from-source.md#build-from-source)), run:

```sh
langspan setup
```

Setup prints one plan, about a screen long, and asks once,
`Set this up? [Y/n]`:

- the state directory;
- the files `langspan install --skill` changes for each host whose
  configuration directory exists, one line each: user-level hooks, the MCP
  entry and the [LangSpan skill](connect-a-native-agent.md#the-langspan-skill);
- the coordinator's login service: on macOS a LaunchAgent,
  `~/Library/LaunchAgents/dev.langspan.coordinator.plist`; on Linux a
  `systemd --user` unit, `~/.config/systemd/user/langspan.service` (under
  `XDG_CONFIG_HOME` when set).

`--verbose` shows each file's full text as well; the skill's text is shown
once, however many hosts get a copy. If none of Claude Code, Codex or Cursor
has a configuration directory, setup says so in one sentence and changes
nothing: start one of them once and run setup again, or name a host with
`--hosts`.

It then applies the install, starts the service so the coordinator runs at
every login and restarts after a failure, and runs `langspan doctor`. The
service runs `langspan daemon --state-dir DIR` and appends its output to
`daemon.log` in the state directory. On Linux without a user manager, setup
installs no service, says so, and starts the coordinator in the background
instead.

`--yes` applies the plan without asking, for scripts. Without a terminal and
without `--yes`, setup prints the plan and changes nothing. A second run
changes nothing and prints `Already set up; nothing to change.` Close Claude
Code first, as for [`install --apply`](connect-a-native-agent.md#all-repositories-for-this-user).
`--hosts`, `--home` and `--state-dir` work as they do for `install`; with
`--home`, setup installs no login service. `langspan uninstall --apply` stops
and removes the service with everything else. Right after setup no workspace
is registered yet; that is normal, and `doctor` reports it as `ok`. The first
hook of an agent session in a Git repository registers one. Setup's last line
is the next step: `Try it: langspan demo`.

On a terminal, a command that fails prints `langspan: ` and what failed and
what to do. Otherwise, and with `--json`, the error is one JSON line on
stderr, `{"level":"error","message":...}`.

## See a refusal without an agent

To see LangSpan refuse an overlapping edit before you open an agent, run:

```sh
langspan demo
```

The demo needs no agent CLI or sign-in. It makes a temporary directory with
a home, a state directory and a small workspace of its own, and starts a
coordinator there. It then sends the
coordinator two example Claude Code sessions, `alice` and `bob`, through the
same hook code an installed hook runs:

1. `alice` edits `src/session.rs` and holds the file.
2. `bob` edits the same function. The hook refuses the edit and prints the
   exact reason that `bob`'s agent would receive.
3. `alice`'s edit is written, and the hold ends.
4. `bob` reads the file with `alice`'s change and edits it again. The hook
   admits that edit.

The demo then stops its coordinator and removes the temporary directory. It
doesn't connect to your coordinator, read your state directory or host
settings, or touch your repositories: its coordinator gets the temporary
home as `HOME`. It takes no options. If you stop it with Ctrl-C, remove the
directory it names on its second line.

An install script and a Homebrew formula that run `langspan setup` for you
are not published yet ([ADR 0035](../adr/0035-one-command-setup.md)).
