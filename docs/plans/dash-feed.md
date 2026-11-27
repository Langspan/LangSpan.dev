# Plan: dash-feed (issue #500)

Add `GET /api/events?after=SEQ&limit=N` to `langspan dashboard`: a read-only,
cursor-paged feed of the coordinator's event log for faithful Live playback.

## Shape (version 1)

```json
{"available": true, "version": 1, "next": 42,
 "events": [{"seq": 41, "type": "lease_granted", "session_id": "…",
             "workspace_id": "…", "at_ms": 123, "paths": ["a.txt"], "more_paths": 0}]}
```

- One row per stored event, so seqs stay contiguous (no gaps a poll can't see).
- Each row is an **allowlist projection** of the event's JSON: `seq`, `type`,
  and only `session_id`, `workspace_id`, `at_ms` and paths (`path` strings and
  `paths` string arrays at any depth), at most `DASHBOARD_PATHS` paths and
  every string cut at `DASHBOARD_TEXT`, as the `/api/state` view does. Every
  other field is dropped, so prompt summaries (`session_prompted.summary`),
  publication payloads, check commands, task lines and any field added later
  are never sent. No message or note text is sent at all: the page already
  shows the texts it may show through `/api/state`, and the feed never widens
  that.
- `next` is the last returned seq, or `after` when the page is empty.
- `after` defaults to 0, `limit` to 100, range 1 to 1000 (the store's own
  bound); a non-numeric or out-of-range value is a 400 that says the range.
- Same token, `Host` check, method/body refusals, headers and `<>&` escaping as
  `/api/state`; `--demo` answers 404; coordinator unreachable is 503 with
  `available: false`.

## Where

Changed while building: the core crate may depend only on `serde`
(`crates/langspan-core/tests/architecture.rs`), so the JSON walk over an
event lives in the adapter, and the core owns only the row types and their
bounding.

- `crates/langspan-core/src/dashboard.rs`: `DashboardEvent::new` (clips every
  string and bounds the paths with the view's own `clip`/`paths`) and
  `DashboardEvents::new` (version and `next`).
- `crates/langspan-core/src/lib.rs`: re-export the two types.
- `src/dashboard.rs`: route `/api/events`, `feed_query`, `feed_page` (the
  allowlist walk), a handler that calls `Command::Events` through
  `client::call`; the existing JSON escaping becomes `json_reply`, used by
  both endpoints. Unit tests hold the contract: a golden page for a
  `session_prompted` with a summary, a `message_sent` with text, a
  `remote_published` payload, an `intent_set` task line and a
  `lease_granted` past the path bound, none of whose text may appear.
- `tests/dashboard.rs`: real-coordinator test (real daemon, hooks and TCP,
  because ordering across real appends and polls is what is verified): drive
  sessions, poll with small `limit` until empty, then add more events and poll
  again from `next`; assert seqs strictly increase by one across all polls,
  no prompt text appears, and token/Host refusals apply to the new path.
- Docs: `docs/adr/0013-dashboard.md` (records the endpoint, replaces the
  "remain possible later additions" line), `docs/usage/the-live-dashboard.md`,
  `docs/security/threat-model.md` (dashboard rows naming the served paths).
- Ratchet baselines for both `dashboard.rs` files (`--record`, pub surface
  grows by two items in core, justified in the PR).

## Test-first order

1. Core contract test (fails: no `dashboard_event`), then the projection.
2. Real-coordinator test (fails: 404), then the route and handler.
3. Docs, ratchets, fmt, clippy, full nextest.

## Shared contracts touched

The core public re-export list (lib.rs) and a new dashboard HTTP shape,
versioned `version: 1`. No socket protocol change: it uses the existing
`Command::Events`.
