# Desktop notices

When a session is refused, an automatic check fails or a handoff waits, the
person is often looking at something else. The coordinator can show a desktop
notice for these (roadmap H3). It is off until switched on:

```sh
langspan notify on                 # notices on, sound as it was (off by default)
langspan notify on --sound on      # with a sound
langspan notify status             # on or off, and which notifier was found
langspan notify off
```

A notice looks like this:

```text
LangSpan
demo: codex.7c2e9a1b… refused on src/api.rs (held by claude-code.4f1a0c3d…)
```

Three things raise one: a refusal (any `write_refused` event: a write held by
another session, a stale or unread edit, a held commit), an automatic check
that failed ([ADR 0030](../adr/0030-automatic-checks-after-write-batches.md)),
and a handoff no session has taken up for 10 minutes. The body names the
workspace's directory, the session (its host and the first eight characters of
its ID) and at most three paths with a count of the rest, and is at most 200
bytes. It never carries a prompt, a message's or handoff's text, a check's
output or file content.

The notice is shown by `osascript` on macOS and `notify-send` on other
systems, the first one found on the coordinator's `PATH`, as a child process
the coordinator kills after 2 seconds. With sound on, `notify-send` asks for
the `message-new-instant` sound and `osascript` for `Glass`. With sound off,
`notify-send` passes the `suppress-sound` hint, so a desktop set to play a
default sound for every notice stays quiet for these (a notification server
that ignores the hint may still play one), and `osascript` names no sound,
which macOS shows silently. It runs on a thread of its own, so no
hook or request waits for it, and nothing about it reaches the model, holds or
refuses anything, or uses the network. Where neither program is found,
nothing is shown, and `notify status` says so (it looks on its own `PATH`,
which is normally the coordinator's). A `notify-send` started by a
coordinator that has no desktop session in its environment (no
`DBUS_SESSION_BUS_ADDRESS`) fails; the coordinator logs that once and goes
on.

At most one notice of a kind is shown per 5 minutes and 10 per hour in total;
the rest are dropped. The coordinator's log (stderr) records each notice as
`notice_sent` or `notice_dropped` with its kind and reason (`kind_limit`,
`hourly_limit`, `queue_full`, `notifier_missing`, `notifier_failed`,
`notifier_timed_out`), and a notifier that is missing, fails or times out is
also logged once per coordinator run. No event is recorded.

`notify on|off` writes `notify` and, with `--sound`, `notify_sound` into
`config.json`, keeping every other setting; the coordinator reads them again
at each notice, so no restart is needed. The limits are set in the same file
(see [Configuration and limits](configuration-and-limits.md#configuration-and-limits)). A handoff that is
already waiting when the coordinator starts is timed from that start.
