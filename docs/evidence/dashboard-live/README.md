# The dashboard's Live view (issue #491)

`scenario.sh` starts a real coordinator in a temporary directory, drives
three Claude Code sessions in one checkout through the real `langspan hook`
executable, and starts `langspan dashboard` against it. The hook events are
constructed; no Claude Code process runs. The sessions read and edit files,
one write is held because the file changed since that session read it, and
the session reads the file again and saves. It prints the dashboard URL and
runs until interrupted.

```sh
cargo build --locked
bash docs/evidence/dashboard-live/scenario.sh target/debug/langspan
```

## Retained so far

Nothing captured in a browser. The PR that added the view could not run the
binary or a browser in its session, so the screenshots at 1440 and 390 px in
both themes, the reduced-motion stills, the short clip and the browser's
label-clipping check are still to be made from this scenario and added here.

What was checked without a browser:

- `tests/live-view-render.test.mjs`: the hero's QA checks as geometry on the
  renderer's layout, at 1440 and 390 px, on the hero's recording and on live
  work (no wire through a card, chains, both ends of a held wire in view,
  text size under framing, labels within the canvas, 12-lane scrolling).
- `tests/dashboard.rs`, `the_served_page_draws_the_live_view_from_a_real_hold`:
  a real coordinator and dashboard serve the Live markup and renderer under a
  policy of two hashes, and `/api/events` serves a real held write with its
  time, session, peer and path.

A screenshot or a delivered event shows what the page draws, not that anyone
changed an approach because of it ([mission](../../mission.md#what-success-means)).
