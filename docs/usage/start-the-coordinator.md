# Start the coordinator

`langspan setup` installs the coordinator as a login service. Without it, a
hook or `langspan mcp` that finds no coordinator for its state directory
starts `langspan daemon` in the background, so even a source build needs no
terminal kept open. It does so only when no login service of LangSpan's names
that state directory, never starts a second coordinator for it, spends at
most half of the hook's remaining budget, and logs the start to the hook's
stderr and to `daemon.log` in the state directory. Set
`LANGSPAN_AUTOSTART=off` to turn this off. The coordinator started this way
keeps only the environment it needs (`HOME`, `PATH`, the locale, `XDG_*`, the
host configuration directories `CLAUDE_CONFIG_DIR`, `CODEX_HOME` and
`GEMINI_CLI_HOME`, kept for removing an earlier Gemini CLI install, the SSH
agent socket and the desktop session variables),
not a host's API keys. A relative host configuration directory is made
absolute against the caller's working directory, since the coordinator runs
in `/`. The login service sets `PATH` to the one setup ran
with, so automatic checks find the same tools, and keeps any of
`CLAUDE_CONFIG_DIR`, `CODEX_HOME` and `GEMINI_CLI_HOME` set then, so its host
checks read the directories setup installed into.
[ADR 0040](../adr/0040-one-command-setup.md) records the details.

To run it in the foreground instead, from the LangSpan repository root:

```sh
./target/release/langspan daemon
```

The default state directory is `$XDG_STATE_HOME/langspan` or
`$HOME/.local/state/langspan`. Choose another with `--state-dir DIR` or
`LANGSPAN_STATE_DIR`. Use that same directory for every hook, CLI, and MCP client
coordinating local edits. Each machine runs its own daemon; remote sharing is a
separate setup below. The daemon requires owner-only permissions and does not
change workspace files.

The daemon checks its state directory every second and exits if the directory
was removed, was replaced by another folder at the same path, or is no longer
owner-only. It logs a `daemon_stopping` line naming the reason
(`state_directory_removed`, `state_directory_replaced` or
`state_directory_not_private`). The next hook or `langspan mcp` call starts a
coordinator for the folder now at that path, as it would after any stop; a
folder that others can open is refused at start.

SIGTERM, SIGINT and SIGHUP stop the daemon the same way, between requests, with
reason `signal` and exit status 0; `langspan uninstall --apply` sends SIGTERM.
A daemon that stops while an automatic check runs cancels the check and kills
its command's process group first, waiting up to two seconds for it; a check
still running then is logged as `auto_check_not_stopped`. SIGKILL can't be
caught, so a running check outlives a daemon killed that way.

A debug build of the daemon also exits after five minutes with no client and
no automatic check running (reason `idle`), so a coordinator left by a test
that was killed ends by itself. The test-only variable `LANGSPAN_TEST_IDLE_EXIT_MS` changes that limit
(`0` turns it off). A release build has no idle exit and ignores the variable.
