# Plan: dash-live (issue #491, first slice)

## Why a slice

Issue #491 asks for a new page view: lanes, cards, one wire per incident, a
dark inspector, a camera with keys, reduced motion, phone width, a replay
fixture and screenshots. Built in full that is well over 1,000 changed lines,
and the limit is about 300. This pull request lands the first independent
slice. It says "Refs #491" and lists what is left under "Not done yet".

The feed from #500 can't yet tell the story the issue asks for. Today an
`/api/events` row keeps only `seq`, `type`, the first `session_id`,
`workspace_id`, `at_ms` and the paths. The selection rule needs more than
that. It needs to know:

- that a `write_refused` names another session (`peer_session_id`) and why
  (`kind`: stale_base, unread_lines and so on);
- that a later `operation_observed` was a successful `Edit` or `Write`
  (`tool`, `result`). Its time is `observed_at_ms`, which the feed drops
  today because it isn't named `at_ms`;
- whether a `source_observed` was a read or a write (`origin`);
- which session started a subagent (`session_registered` → `session.parent`),
  so a subagent's work counts as its parent's;
- how the daemon's clock maps to the wall clock (`clock_anchored` →
  `anchor.clock_ms`, `anchor.wall_ms`).

So slice 1 is the data and the model. Slice 2 is the view. Slice 3 is the
replay.

## Slice 1 (this pull request)

1. **Feed version 2** (`crates/langspan-core/src/dashboard.rs`,
   `src/dashboard.rs` `feed_page`). Each new field is optional and is
   added to the allowlist:
   - `peer_session_id`: an ID;
   - `refusal`: the `WriteRefused` kind, an enum;
   - `tool`: a host tool name, clipped;
   - `result`: an enum;
   - `origin`: an enum;
   - `parent`: an ID;
   - `wall_ms` and `clock_ms`: numbers, on `clock_anchored` only.

   `at_ms` also takes `observed_at_ms`. None of these fields is free text, so
   prompt summaries, message text, notes and payloads stay out.
   `DASHBOARD_EVENTS_VERSION` goes from 1 to 2. A version 1 reader still
   decodes every row, because the fields are `serde(default)` and skipped
   when absent.
2. **Golden vector**: `fixtures/dashboard-events-v2.json` holds one page
   with a refusal, an admitted edit, a read, a subagent registration and a
   clock anchor. It is checked in `crates/langspan-core/tests/dashboard.rs`
   in both directions: what the page encodes to, and that a version 1 page
   still decodes.
3. **Story model**, pure JS in a new file `src/dashboard-live.js` with no
   DOM and no clock:
   - `root(sessions, id)` folds subagents into their parent;
   - `wall(anchors, generation, at)`;
   - `candidates(events)` applies the rule: a `write_refused` with a peer
     whose root differs from the held session's root, followed by that
     agent's successful Edit or Write of the held path;
   - `best(events)` returns the newest candidate;
   - `story(events, seq)` returns lanes, cards and exactly one connector:
     - its tone is red for the hold and green for the save;
     - each agent gets at most 5 cards, with other steps merged by file and
       the rest folded into one card, "Read N files…";
     - the caption is at most 20 words.

   Events without a timestamp, such as `session_registered`, `message_sent`
   and lease events, never become a card; they are returned as inspector
   rows. The page doesn't include the file yet; slice 2 adds the marker that
   inlines it. That keeps this slice free of CSP-hash and golden-page churn.
4. **Tests, written first**:
   - `src/dashboard.rs` unit tests: `feed_page` carries each new field from
     a real `Event` value, and still drops message text and summaries (the
     existing test, extended).
   - `tests/dashboard_live.rs`: runs `node --test
     tests/dashboard-live.test.mjs`. It spawns a process only because node
     is the one runner for the page's JS. It prints `skipped:` when node is
     missing, and fails then under `LANGSPAN_REQUIRE_TOOLS=1`, like the go
     and pytest tests. The tests cover:
     - the selection rule: a same-root peer is rejected, a refusal without a
       peer is rejected, a refusal without a later save is rejected, and the
       newest candidate wins;
     - the card cap and the fold;
     - a caption of at most 20 words;
     - untimed events going to the inspector only;
     - hostile path text kept as data;
     - feed pages shorter than the limit, read until empty.

     I'll break the rule once to confirm the tests catch it.
5. **Docs**:
   - ADR 0013 gets the version 2 row fields and the plan for the Live view.
   - The threat model gets a review note: the allowlist grows by IDs, enums
     and numbers only, with no new boundary.
   - `docs/usage/the-live-dashboard.md` gets the field list.
   - Ratchet baselines go in for the new Rust test file, and the API
     baseline is updated for the new pub fields.

### What changed from this plan while building it

- The facts come from the typed `Event` (`DashboardEvent::detail`), not from
  key names, so a `kind` or `result` nested in another event's free text can
  never be picked up.
- An operation's `concurrent_holders` are other sessions' holdings. The
  breadth-first walk had added their paths to the operation's own; it now
  skips them, and the test checks that.
- In the feed, a `message_sent` row names no session (its sender is `from`),
  so the inspector also takes untimed rows that name the held file.
- No ratchet baseline was needed for the new test file; the core baseline
  grew by one pub item, `detail`, which the daemon crate calls.

## Later slices (not in this pull request)

- **Slice 2, the view**:
  - Live and Files toggle; the file matrix stays as the second view.
  - SVG tracks and wires, with cards as HTML buttons.
  - Camera with +, −, Fit and Follow newest; arrow keys, +/−, 0 and L.
  - The dark inspector overlay, about 40% wide.
  - Reduced motion and 390 px.
  - `dashboard-browser-check.mjs` cases.
  - Screenshots, light and dark, at 1440 and 390 px.
- **Slice 3, the replay**: the billing/invoices "Demo replay" fixture, with
  Pause and Replay, rendered by the same code; homepage embedding.
- Recorded +/− counts are a separate issue. The view shows line ranges only.

## Shared contracts touched

- **The `/api/events` wire format** goes to version 2. It is additive, and a
  golden vector covers it.
- **`src/dashboard.rs`**: the briefing shows session 6bfebfe8 (`lay-b-hosts`)
  editing this file now. My change is confined to `feed_page` and its tests,
  but the orchestrator should order the two.
- `crates/langspan-core/src/dashboard.rs`: only `DashboardEvent` and its
  constructor change.
