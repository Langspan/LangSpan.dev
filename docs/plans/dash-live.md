# Plan: dash-live (issue #491), re-planned 27 Nov

## What changed

The owner's later decisions replace the first plan:

- The dashboard has no story mode. There is no best-story selection, scene
  tour, narration caption, scene-driven card folding or inspector that opens
  on its own.
- The Live view matches the approved homepage hero (site PR
  Langspan/langspan.github.io#41, final head), as listed in #491's latest
  comment: card style B, lane chains, a solid gutter, the lateral flow, a
  coral Held wire through the lane gaps, past wires fading to 25%, framing
  for off-screen sources, and the hero's interactions.
- There is one renderer, shared by the site's hero (which passes a fixture)
  and the dashboard (which passes `/api/state` plus `/api/events`).

The earlier slice 1 is on this branch as one WIP commit, uncommitted until
now. Its feed version 2 stays, without `parent`: lanes are per session, so
nothing folds a subagent into its parent any more. Its story model
(`candidates`, `best`, `story`, captions, folding to five cards) is
deleted. Its tests that still apply move to the new model: one wire per
held write, untimed rows to the inspector, the feed reader, and path text
as data.

## The shared contract

The hero's renderer (`assets/product-first.js`) draws a single object,
`TIMING`, made of:

- `lanes`: `label`, `short`, `agent`, `checkout`, `branch`;
- `cards`: `id`, `lane`, `kind` (`edit`, `read`, `command`, `held`,
  `saved`, `sent`, `got`), `seq`, `time`, `verb`, `code`, `sub`, `title`,
  `rows`, `next`, `idle_after`, plus the playback `at` and `flow`;
- `wires`: `kind`, `label`, `from`, `from_lane`, `to`, `seq`, plus the
  playback `at`, `ms`, `stop` and `emphasis_at`;
- `status`;
- `pacing`.

The site's `build-hero-timeline.py` produces it from a recorded slice. The
shared component takes this shape: the site passes its fixture as it does
today, and the dashboard passes what `LangSpanLive.model(state, events)`
returns, after pacing. The model owns everything except the playback
fields; `pace` (slice 2) adds them. The model emits nothing the hero
doesn't already read.

## Slices

The whole of #491 is well over 1,000 changed lines: the hero's renderer is
about 500 lines of JS and 300 CSS rules, plus the page, the view switch,
screenshots and the real-coordinator test. The limit is about 300, so this
PR is slice 1 and says "Refs #491".

1. **This PR: the feed and the model.** It is independent, because nothing
   on the page reads the model yet.
2. **The shared renderer.** Port the hero block of `product-first.js` and
   its CSS into one shared script and stylesheet (not a new `src/`
   directory unless `namespaces.tsv` lists it). Drive it from a
   model plus a clock: replay loops a fixture, while live follows the
   newest work and has no loop copies. Add `pace(model, pacing)`. Copy the
   hero's TIMING into `fixtures/live-view-hero.json` and check that it
   renders exactly as the site does. Port the hero's QA checks (no wire
   through a card, no clipped label, a chain between consecutive cards,
   both ends of a held wire on screen while it draws) as geometry tests
   on the layout.
3. **The dashboard.** Inline the renderer through markers in
   `src/dashboard.html`, keeping one script block and one style block, so
   the CSP stays two hashes. Add a Live and Files switch. Poll
   `/api/events` from the last `next`. Test against a real coordinator at
   1440 and 390 px, light and dark, and with reduced motion. Attach the
   screenshots. Update ADR 0013. "Fixes #491".
4. **The site repo.** The hero loads the shared file. That is a separate
   PR in Langspan/langspan.github.io.

## Slice 1 in detail

### Files

- `crates/langspan-core/src/dashboard.rs`: feed version 2 rows. Keep
  `peer_session_id`, `refusal`, `tool`, `result`, `origin`, `wall_ms` and
  `clock_ms`, and drop `parent`. All of them are `serde(default)` and
  skipped when absent.
- `src/dashboard.rs` `feed_page`: fill those fields. Drop `parent`.
- `fixtures/dashboard-events-v2.json`: the golden page, without `parent`.
- `crates/langspan-core/tests/dashboard.rs`: the golden page in both
  directions. A version 1 page still decodes.
- `tests/dashboard.rs`: the real-process feed test asserts version 2
  (one line).
- `src/dashboard-live.js`, rewritten: pure, with no DOM, clock or network.
  (The plan named `src/live-view/model.js`, but every directory under
  `src/` must be a listed Rust namespace, `tests/ratchets.rs`
  `the_top_level_modules_are_the_listed_namespaces`, and
  `scripts/ratchets/namespaces.tsv` is outside this reservation. Slice 2
  chooses where the renderer's files live.)
  - `model(state, events)` returns `{lanes, cards, wires, status,
    untimed}` in the hero's shape.
  - **Lanes.** There is one lane for each local session that has a timed
    event. The lane takes its label, harness and checkout (`workspace_id`
    and branch) from the matching `/api/state` `agents` row. Lane
    reference text uses no IDs: a short session label, then the harness
    name.
  - **Cards.** Each card comes from one recorded step:
    - `source_observed` with origin `read` is "Read";
    - a successful `operation_observed` with an Edit-family tool is
      "Edited". Consecutive edits of one file within 20 s merge into one
      card with "N edits", as the hero groups them;
    - another successful operation is "Ran a command";
    - `write_refused` is a `held` card: "Write held" over the file, with
      the reason in words from the `refusal` kind ("changed since this
      agent read it", and so on);
    - the next successful edit of the held path by the held session is
      `saved`: "Saved after re-reading" if a read of that path came
      between the hold and the edit, "Saved on retry" if not.
  - **Time.** `time` is the wall time from the newest `clock_anchored`
    row. `rows` holds recorded fields only: file, time, why. `next` links
    each card to its lane's next card. `idle_after` marks a gap of more
    than 30 s before the lane's next card.
  - **Wires.** There is exactly one wire for each held write that names a
    peer (`peer_session_id`) with a lane: kind `held`, label "Held",
    `from` set to that peer's newest card on the held path before the
    hold, otherwise `null` with `from_lane`. A hold without a recorded
    peer gets no wire. Nothing else gets a wire. Remote sessions are never
    in the local log, so a remote edit never becomes a held card or a
    wire.
  - **Inspector rows.** Rows without a time (`session_registered`,
    `message_sent` and lease events) are never cards. They are returned
    in `untimed` for the inspector, in seq order, without an invented
    time.
  - **Status.** `status` is the number of agents and the number of writes
    held.
  - **`readFeed`.** Carried over from the first slice: it reads until a
    page comes back empty, within a page bound, and stops when a page
    doesn't advance.
- `tests/live-view-model.test.mjs` (new; replaces
  `tests/dashboard-live.test.mjs`, which is deleted) and
  `tests/live_view.rs` (new; replaces `tests/dashboard_live.rs`, which
  is deleted). The runner starts `node --test` only because node is the
  one runner for the page's JS. It prints `skipped:` without node, and
  fails then under `LANGSPAN_REQUIRE_TOOLS=1`.
- `fixtures/live-view-model.json` (new): the golden model output for the
  golden events page plus a small state.
- `docs/adr/0013-dashboard.md`: replace the "story" paragraph with the
  shared renderer and the model contract, and record the slices.
- `docs/security/threat-model.md`: no new boundary. Fix the sentence that
  names the story model.
- `docs/usage/the-live-dashboard.md`: the version 2 fields, without
  `parent`.
- The ratchet baselines for the two `dashboard.rs` files and for
  `crates/langspan-core/api.txt`, recorded or tightened.

### Test-first order

1. Rust. The golden page no longer has `parent`, and `feed_page` keeps
   every version 2 field from a real `Event` while it still drops message
   text and summaries. Watch it fail on `parent`, then remove the field.
2. Node, with each test written failing first:
   - one lane per session with a timed event, labelled from the state row
     and showing no IDs;
   - reads, edits, commands, holds and saves map to the hero's card kinds;
   - same-file edits within 20 s merge, and ones further apart don't;
   - exactly one held wire per held write with a peer that has a lane,
     none for a hold without a peer, and none for reads, commands or
     saves;
   - a wire's `from` is the peer's last card on the held path, otherwise
     `null` with `from_lane`;
   - "Saved after re-reading" only when a read came between the hold and
     the edit;
   - untimed rows are never cards and appear in `untimed` in seq order;
   - `time` comes from the clock anchor;
   - path text is kept as given;
   - every card and wire field is one of the hero's keys;
   - the model equals `fixtures/live-view-model.json`;
   - `readFeed` reads until a page comes back empty, within the bound, and
     stops on a page that doesn't advance.
3. Break the wire rule once (draw a wire for a hold without a peer) and
   confirm the test catches it.

### Shared contracts touched

- The `/api/events` wire format: version 1 goes to 2. New fields are
  optional, a version 1 page still decodes, and
  `fixtures/dashboard-events-v2.json` is the golden vector.
- The model shape is a new contract with the site hero, pinned by
  `fixtures/live-view-model.json`. No CLI output, `Outcome`, protocol
  command or daemon code changes, and the page itself is unchanged in
  this slice.
