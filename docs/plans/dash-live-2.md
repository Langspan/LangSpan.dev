# Plan: dash-live slice 2 (issue #491), the visible canvas

Slice 1 (#551) shipped `/api/events` version 2 and `LangSpanLive.model`,
which builds the hero's lanes, cards and wires from `/api/state` and
`/api/events`. Nothing on the page draws it yet. This slice ports the
homepage hero's renderer (site PR Langspan/langspan.github.io#41 at its
final head, `assets/product-first.js` and `product-first.css`) into one
shared component. `langspan dashboard` draws it with live data in a Live
view, and the file matrix stays as the second view.

The owner's binding decisions apply: no story mode, no tour, no captions,
no inspector that opens on its own, and no AI-written text. The design is
the approved hero's (#491, comment of 27 Nov), plus vertical scrolling
across lanes for many agents (#491, comment of 28 Nov).

## Files

- `src/dashboard-live-view.js` (new): the shared renderer, named
  `LangSpanLiveView`, in two parts.
  - Pure, with no DOM: `layout(timing, dims)`, `frameState(timing, t)`,
    `frameFor`, the wire route (`route`) and `flowCamera`. These are moved
    from the hero's closure with as few changes as the parameters need:
    the hero reads `TIMING` and `L` as closure variables, and here they
    are arguments. The node QA tests run against this part.
  - DOM: `mount(root, source)`. The hero's pools, chains, labels, chips,
    "earlier" markers, popover, camera (+, −, Fit, Follow newest,
    Play/Pause), drag, pinch, Ctrl+wheel, the keys, pause on interaction
    with resume after 8 s, reduced motion, and `?motion=static`.
    - `source` is `{timing}` for a fixture: the site, and the
      equality test against the hero.
    - Or it is `{live: true, update(cb)}` for the dashboard. Live mode has
      no loop copies. The clock runs to the newest card and stops there.
      Follow newest brings new work in from the right at the hero's
      `newest_at` (72%) at `--flow-k` (about 92 px/s).
  - New for many agents (#491, 28 Nov):
    - the canvas shows at most 6 lanes and scrolls vertically, with the
      lane names pinned in the gutter;
    - it opens scrolled to the window of lanes with the most cards;
    - a plain wheel scrolls the page until the canvas has focus, and then
      scrolls the lanes. Ctrl or Cmd with the wheel zooms, as on the hero.
  - Text from the coordinator is placed with `textContent` only, as on
    the hero.
- `src/dashboard-live-view.css` (new): the hero's `.live` rules, carried
  over as they are. Card style B (a 14 px verb glyph that always comes
  with its verb word, so no meaning is glyph-only), the solid gutter,
  lane chains, the coral Held wire without glow, fading, the dark
  popover, and the narrow (390 px) cut. Selectors stay scoped under
  `.live`, so they don't collide with the dashboard's own styles. Light
  and dark both follow the page's `data-theme`.
- `src/dashboard-live.js`: add `pace(model)`. It adds the playback fields
  the renderer reads (`at`, a wire's `at`, `ms`, `stop` and
  `emphasis_at`, plus `stops_s`, `status[]` and `pacing`). It works from
  the events alone, so a card keeps its place when new events arrive:
  - `at` is the recorded time, with any gap over 30 s shortened to a
    fixed idle step;
  - a held write's stop is the hero's 2.6 s hold.

  The pacing constants are the hero's `pacing.json` values, copied once
  into `fixtures/live-view-hero.json` and read from there by the tests.
- `src/dashboard.html`:
  - a Live/Files view switch, with Live the default when the feed has a
    timed event. The existing panels become the Files view, unchanged;
  - the hero's markup (title bar, status line, canvas, gutter, tools,
    popover);
  - polling of `/api/events` from the last `next`, through `readFeed`,
    every 2 s with the state poll;
  - three new markers inside the existing single `<script>` and `<style>`
    blocks, so the CSP stays exactly two hashes.
- `src/dashboard.rs` `page`: fill the markers from `include_str!` of the
  three files. Each marker must appear exactly once, or the page refuses
  to build, as with the existing markers. The unit tests cover two hashes
  and one block of each, and that the live page contains the renderer
  while the demo page keeps its scenes. Then the ratchet baseline
  `scripts/ratchets/baseline/src/dashboard.rs.tsv`.
- `fixtures/live-view-hero.json` (new): an equal copy of the hero's
  TIMING (`marketing/motion/hero-loop/timing.json` at the PR's head). It
  checks that the renderer draws the fixture as the site does.
- `tests/live-view-render.test.mjs` (new): the hero's QA checks, as
  geometry on the pure layout at 1440 and 390 widths, for both the hero
  fixture and `pace(model(...))` of the golden live model.
  - No wire segment crosses a card's rectangle (other than its own two
    end edges).
  - Every two consecutive cards of a lane are joined by a chain, and a
    gap over 30 s is the idle (dashed) kind.
  - Both ends of a held wire are inside the view while it draws, or the
    frame falls back to "neighbor" or "chip", never a bare lane edge.
  - Card text stays at least 10 px at 1440 and 12 px at 390 under the
    framing scale.
  - Labels aren't clipped. The canvas bounds use the hero's `keepIn`
    rule, with label width bounded from its text length at the CSS font
    size. The real-browser check below measures the actual width.
  - Live mode: `layout` of a growing event list keeps every earlier
    card's x, and no card overlaps another in its lane.
  - With 12 lanes, the opening scroll shows the busiest window, and the
    scroll is clamped to the lanes.
- `tests/live-view-model.test.mjs`: tests for `pace`: deterministic,
  stable under appended events, gaps shortened, one stop per held wire.
- `tests/live_view.rs`: run the new `.mjs` file too, with the same node
  skip and `LANGSPAN_REQUIRE_TOOLS` rule.
- `tests/dashboard.rs`: the real-coordinator test (a process and socket
  test, because the served page and its CSP are what's verified). The
  served live page carries the renderer and the Live markup, its CSP
  still has two hashes, and `/api/events` serves a hold that the page's
  model would draw.
- `docs/adr/0013-dashboard.md`: the Live view as built; the shared
  renderer and its two sources; live pacing; vertical scroll; what slice
  4 (the site loading the shared file) leaves open.
- `docs/usage/the-live-dashboard.md`: the Live and Files views, the
  controls and keys.
- `docs/IMPLEMENTATION_STATUS.md`: the Live view row, with the retained
  evidence.
- `docs/evidence/dashboard-live/` (new): screenshots at 1440 and 390,
  light and dark, from a real coordinator with two Claude Code sessions
  and a held write. Also reduced-motion stills, a short clip, and a
  README with the commands, versions and what they show (receipt, not
  benefit). The captures and the label-clipping check run in a real
  browser through Playwright from a scratch directory outside the
  repository (the site worktree's `review/` install). No dependency is
  added here.

No new dependency, model call, network call or trust boundary: the page
already reads `/api/events` on its own origin with the token, and every
value is placed as text. `docs/security/threat-model.md` is unchanged.

## Test-first order

1. `pace` tests fail (no `pace`), then add it.
2. The render geometry tests fail (no `dashboard-live-view.js`). Port the
   pure part, and make the hero fixture pass every QA check unchanged.
   That proves the port. Then the live-model checks and the 12-lane
   scroll checks. Break the route once (send a wire straight across) and
   confirm "no wire through a card" catches it.
3. The `src/dashboard.rs` page tests fail on the missing markers, then
   the markers and the includes are added.
4. The `tests/dashboard.rs` served-page assertions, then the page markup
   and polling.
5. Run a real coordinator plus `langspan dashboard`, drive two sessions
   into a held write, then capture the screenshots, the clip, the
   reduced-motion stills and the browser QA run.

## Shared contracts touched

- None on the wire. `/api/events` stays at version 2, and `/api/state` is
  unchanged.
- The renderer's input is the hero's TIMING shape. `pace` adds only fields
  the hero already reads, pinned by `fixtures/live-view-hero.json` and
  `fixtures/live-view-model.json`.
- `src/dashboard.html` and `src/dashboard.rs` are this task's alone. No
  other session in the briefing holds them.

## Size

The port is about 500 lines of JS and 250 of CSS carried over from the
hero. The new logic is `pace`, the live clock, vertical scroll, the mount
adapter and the page switch, about 300 lines. If the diff is still too
large to review, the cut is:

- (2a) the renderer, the fixture and the QA tests, with no page change;
- (2b) the dashboard page, the evidence and "Fixes #491".

The PR body will say which.

## Left after this slice

- The site's hero loading the shared file, a separate PR in
  Langspan/langspan.github.io.
- The fleet version of the homepage, langspan.github.io#42.
