// The Live view renderer's QA checks (issue #491), run by tests/live_view.rs.
// They are the homepage hero's checks, as geometry on the renderer's pure
// part (src/dashboard-live-view.js): no wire through a card, a chain between
// a lane's consecutive cards, both ends of a held wire in view while it
// draws, readable text under the framing scale, and labels that fit. Each
// runs on the hero's own recording (fixtures/live-view-hero.json) and on
// live work paced from /api/events rows, at 1440 and 390 px. A real browser
// measures the drawn page as well (docs/evidence/dashboard-live).
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";

for (const file of ["../src/dashboard-live.js", "../src/dashboard-live-view.js"]) {
  vm.runInThisContext(readFileSync(new URL(file, import.meta.url), "utf8"));
}
const Live = globalThis.LangSpanLive;
const View = globalThis.LangSpanLiveView;
const HERO = JSON.parse(readFileSync(new URL("../fixtures/live-view-hero.json", import.meta.url), "utf8"));

// The canvas sizes the CSS sets (src/dashboard-live-view.css, equal to the
// hero's), and the canvas the hero has at a 1440 px and a 390 px window.
const WIDE = { W: 136, H: 80, HH: 108, row: 154, top: 56, G: 200, gap: 12, K: 72, Wc: 1358, Hc: 520, minText: 12, narrow: false };
const NARROW = { W: 96, H: 100, HH: 150, row: 200, top: 48, G: 84, gap: 8, K: 46, Wc: 336, Hc: 640, minText: 12, narrow: true };
const MIN_TEXT = { wide: 10, narrow: 12 };
// A live canvas is as tall as the lanes in view (mount's measure).
function dims(base, timing) {
  if (!timing.live) return base;
  const shown = Math.max(1, Math.min(timing.lanes.length, View.VISIBLE[base.narrow ? "narrow" : "wide"]));
  return { ...base, Hc: base.top + shown * base.row };
}

// Live work: sessions edit, hold and save, with a hold across lanes that
// aren't neighbours so the wire must pass the lanes between.
let seq = 0;
const row = (fields) => ({ seq: ++seq, paths: [], more_paths: 0, ...fields });
const anchor = () => row({ type: "clock_anchored", wall_ms: 1_800_000_000_000, clock_ms: 0 });
const edit = (s, at, path) => row({ type: "operation_observed", session_id: s, workspace_id: "app", at_ms: at, paths: [path], tool: "Edit", result: "succeeded" });
const read = (s, at, path) => row({ type: "source_observed", session_id: s, at_ms: at, paths: [path], origin: "read" });
const held = (s, peer, at, path) => row({ type: "write_refused", session_id: s, workspace_id: "app", at_ms: at, paths: [path], peer_session_id: peer, refusal: "stale_base" });
const S = (i) => "claude-code.s" + i;
function liveEvents(lanes) {
  seq = 0;
  const out = [anchor()];
  let t = 1000;
  for (let round = 0; round < 4; round++) {
    for (let i = 0; i < lanes; i++) out.push(edit(S(i), (t += 1500), "src/lane" + i + "/file" + round + ".rs"));
  }
  // Lane 0 edits a file; the last lane is held on it, re-reads and saves.
  out.push(edit(S(0), (t += 2000), "src/shared.rs"));
  for (let i = 1; i < lanes - 1; i++) out.push(edit(S(i), (t += 300), "src/lane" + i + "/busy.rs"));
  out.push(held(S(lanes - 1), S(0), (t += 1000), "src/shared.rs"));
  out.push(read(S(lanes - 1), (t += 500), "src/shared.rs"));
  out.push(edit(S(lanes - 1), (t += 500), "src/shared.rs"));
  // And a neighbour's hold the other way, after an idle minute.
  out.push(edit(S(lanes - 1), (t += 60_000), "src/next.rs"));
  out.push(held(S(lanes - 2), S(lanes - 1), (t += 800), "src/next.rs"));
  return out;
}
const live = (lanes) => { const ev = liveEvents(lanes); return Live.pace(Live.model({ agents: [] }, ev), ev); };

// The dashboard's own lane spacing (src/dashboard.html), wide then narrow.
const PAGE = readFileSync(new URL("../src/dashboard.html", import.meta.url), "utf8");
const [DASH_WIDE_ROW, DASH_NARROW_ROW] = [...PAGE.matchAll(/#live-view \.live-canvas \{ --row-h: (\d+)px; \}/g)].map((m) => +m[1]);
const DASH = { "1440": { ...WIDE, row: DASH_WIDE_ROW }, "390": { ...NARROW, row: DASH_NARROW_ROW } };

const SCENES = [["the hero's recording", HERO], ["live work in three lanes", live(3)], ["live work in four lanes", live(4)]];
// The hero draws at its own sizes; live work draws at the dashboard's.
const sizes = (timing) => (timing.live ? [["1440", DASH["1440"]], ["390", DASH["390"]]] : [["1440", WIDE], ["390", NARROW]]);

test("the dashboard's lanes are 76 px apart, card edge to card edge, at 1440 and 390 px", () => {
  for (const base of Object.values(DASH)) assert.equal(base.row - base.H, 76, JSON.stringify(base));
});
const samples = (timing) => { const out = []; for (let t = 0; t <= timing.duration_s; t += 1 / 15) out.push(t); return out; };

// Axis-aligned segments of a route; the curve of a short route lies
// inside its control polygon, which is the same points.
const segments = (points) => points.slice(1).map((p, i) => [points[i], p]);
function crosses([[x1, y1], [x2, y2]], r) {
  const [ax, bx] = [Math.min(x1, x2), Math.max(x1, x2)], [ay, by] = [Math.min(y1, y2), Math.max(y1, y2)];
  // The open rectangle, 1 px in, so a wire may end on a card's edge.
  return ax < r.x + r.w - 1 && bx > r.x + 1 && ay < r.y + r.h - 1 && by > r.y + 1;
}
function rects(V, L) {
  const out = [];
  for (const n of V.live ? [0] : [-1, 0]) {
    for (const c of V.CARDS) out.push({ id: n + ":" + c.id, x: L.x[c.id] + n * L.period, y: V.cardY(L, c), w: L.W, h: V.cardH(L, c) });
  }
  // A NaN would make every comparison false and the check pass unseen.
  for (const r of out) assert.ok([r.x, r.y, r.w, r.h].every(Number.isFinite), JSON.stringify(r));
  return out;
}
function route(V, L, i) {
  const w = V.WIRES[i], fr = w.kind === "held" ? V.frameFor(L, i) : { mode: "both", from: w.from };
  return V.wireRoute(L, i, 0, fr, L.x[V.byId[w.to].id] + L.W / 2);
}

for (const [name, timing] of SCENES) {
  for (const [size, base] of sizes(timing)) {
    const V = View.prepare(timing);
    const L = V.layout(dims(base, timing));

    test(`${name} at ${size}: no wire crosses a card`, () => {
      assert.ok(V.WIRES.length > 0);
      for (let i = 0; i < V.WIRES.length; i++) {
        const rt = route(V, L, i);
        assert.ok(rt.points.flat().every(Number.isFinite), JSON.stringify(rt.points));
        for (const seg of segments(rt.points)) {
          for (const r of rects(V, L)) assert.ok(!crosses(seg, r), `wire ${i} crosses card ${r.id}: ${JSON.stringify(seg)}`);
        }
      }
    });

    test(`${name} at ${size}: each lane's consecutive cards are chained, and cards never overlap`, () => {
      for (let lane = 0; lane < timing.lanes.length; lane++) {
        const mine = V.CARDS.filter((c) => c.lane === lane);
        mine.forEach((c, j) => {
          const next = mine[j + 1];
          if (next) {
            assert.equal(c.next, next.id, `${c.id} should chain to ${next.id}`);
            assert.ok(L.x[next.id] >= L.x[c.id] + L.W + L.gap - 1e-6, `${c.id} and ${next.id} overlap`);
          } else if (timing.live) assert.equal(c.next, null, c.id);
          else assert.equal(V.byId[c.next].lane, lane, `${c.id} loops to its own lane`);
        });
      }
    });

    test(`${name} at ${size}: a held wire draws with both ends in view, or from an "earlier" card or a chip`, () => {
      V.WIRES.forEach((w, i) => {
        if (w.kind !== "held") return;
        const fr = V.frameFor(L, i);
        assert.ok(["both", "neighbor", "chip"].includes(fr.mode), fr.mode);
        assert.ok(fr.mode === "chip" || fr.from, "a wire never starts from a bare lane edge");
        if (L.narrow) assert.notEqual(fr.mode, "neighbor");
        for (let t = w.at; t <= w.at + w.ms / 1000; t += 1 / 30) {
          const st = V.frameState(t), cam = V.flowView(L, st, 1, 0);
          const inView = (id) => {
            const c = V.byId[id], left = (L.x[id] - cam.x) * cam.s, top = L.top + c.lane * L.row * cam.s + cam.y;
            return left >= L.G - 1 && left + L.W * cam.s <= L.Wc + 1 && top >= L.top - 1 && top + V.cardH(L, c) * cam.s <= L.Hc + 1;
          };
          assert.ok(inView(w.to), `t=${t.toFixed(2)}: the held card is out of view`);
          if (fr.from) assert.ok(inView(fr.from), `t=${t.toFixed(2)}: wire ${i}'s start (${fr.mode}) is out of view`);
        }
      });
    });

    test(`${name} at ${size}: card text stays readable under the framing scale`, () => {
      const least = MIN_TEXT[L.narrow ? "narrow" : "wide"];
      for (const t of samples(timing)) {
        const cam = V.flowView(L, V.frameState(t), 1, 0);
        assert.ok(L.minText * cam.s >= least - 1e-9, `t=${t.toFixed(2)}: ${(L.minText * cam.s).toFixed(2)} px`);
      }
    });

    test(`${name} at ${size}: labels, markers and chips fit inside the canvas`, () => {
      // Width bounded from the text at the CSS sizes: 11 px bold (about
      // 0.62 em a character) plus padding and border.
      const width = (text, px) => text.length * px * 0.62 + 24;
      const room = L.Wc - L.G - 8;
      for (const w of V.WIRES) assert.ok(width(w.label, 11) <= room, w.label);
      assert.ok(width("earlier", 11) <= room);
      // A chip wraps within its max width, min(240, 90% of the lanes' width).
      assert.ok(Math.min(240, (L.Wc - L.G) * 0.9) <= room);
    });
  }
}

test("the hero's recording loops, and live work rests at its newest card", () => {
  const hero = View.prepare(HERO);
  assert.equal(hero.frameState(HERO.duration_s + 1).t.toFixed(3), "1.000");
  const t = live(3), V = View.prepare(t);
  assert.equal(V.frameState(t.duration_s + 5).t, t.duration_s);
  const st = V.frameState(t.duration_s);
  assert.equal(st.newest, V.CARDS.at(-1).id);
  // The newest wire is still at full strength at rest.
  assert.equal(st.fade.at(-1), 1);
});

test("live: appending events keeps every earlier card's place", () => {
  const all = liveEvents(4);
  let before = null;
  for (let n = 6; n <= all.length; n += 3) {
    const ev = all.slice(0, n), t = Live.pace(Live.model({ agents: [] }, ev), ev);
    const L = View.prepare(t).layout(dims(WIDE, t));
    if (before) for (const [id, x] of Object.entries(before)) assert.equal(L.x[id], x, `${id} moved after ${n} events`);
    before = L.x;
  }
});

test("live: with 12 lanes the canvas shows 6, opens on the busiest, and scrolls only across the lanes", () => {
  seq = 0;
  const ev = [anchor()];
  let t = 1000;
  for (let i = 0; i < 12; i++) ev.push(edit(S(i), (t += 1000), "a" + i + ".rs"));
  // Lanes 5 to 10 are the busiest window.
  for (let k = 0; k < 6; k++) for (let i = 5; i <= 10; i++) ev.push(edit(S(i), (t += 25_000), "b" + i + "-" + k + ".rs"));
  const timing = Live.pace(Live.model({ agents: [] }, ev), ev);
  assert.equal(timing.lanes.length, 12);
  const V = View.prepare(timing), L = V.layout(dims(WIDE, timing));
  assert.equal(L.visible, 6);
  assert.equal(L.Hc, WIDE.top + 6 * WIDE.row);
  assert.equal(V.openingY(L), -5 * WIDE.row);
  assert.equal(V.clampY(L, 500, 1), 0);
  assert.equal(V.clampY(L, -1e6, 1), L.Hc - L.top - 12 * L.row);
  const N = V.layout(dims(NARROW, timing));
  assert.equal(N.visible, 3);
  assert.ok(V.openingY(N) <= 0 && V.openingY(N) >= -9 * NARROW.row);
});

test("live: the view opens on the lanes with the newest work, not on lanes that were busier earlier", () => {
  // The dogfood log's shape: six sessions worked hard and stopped; three
  // others work now, in lanes past the first window. Leases carry no time.
  seq = 0;
  const ev = [anchor()];
  let t = 1000;
  for (let k = 0; k < 20; k++) for (let i = 0; i < 6; i++) ev.push(edit(S(i), (t += 2000), "old" + i + "-" + k + ".rs"));
  for (let i = 6; i < 9; i++) ev.push(row({ type: "lease_granted", session_id: S(i), workspace_id: "app", paths: ["x.rs"] }));
  t += 3_600_000;
  for (let k = 0; k < 3; k++) for (let i = 6; i < 9; i++) ev.push(edit(S(i), (t += 3000), "new" + i + "-" + k + ".rs"));
  const timing = Live.pace(Live.model({ agents: [] }, ev), ev, { now_ms: t });
  assert.equal(timing.status.at(-1).agents, 3);
  assert.deepEqual(timing.working, [6, 7, 8]);
  for (const base of [WIDE, NARROW]) {
    const V = View.prepare(timing), L = V.layout(dims(base, timing));
    const y = V.openingY(L), cam = V.flowView(L, V.frameState(timing.duration_s), 1, y);
    const inView = (c) => {
      const left = (L.x[c.id] - cam.x) * cam.s, top = L.top + c.lane * L.row * cam.s + cam.y;
      return left + L.W > L.G && left < L.Wc && top >= L.top - 1 && top + V.cardH(L, c) <= L.Hc + 1;
    };
    assert.ok(inView(V.CARDS.at(-1)), `${base.narrow ? 390 : 1440}: the newest card is out of view`);
    assert.ok(V.CARDS.filter(inView).length >= 3, `${base.narrow ? 390 : 1440}: ${V.CARDS.filter(inView).length} cards in view`);
  }
});

test("live: a hold between lanes too far apart to show together is framed with a chip beside the held card", () => {
  seq = 0;
  const ev = [anchor()];
  let t = 1000;
  for (let i = 0; i < 10; i++) ev.push(edit(S(i), (t += 1000), "a" + i + ".rs"));
  ev.push(edit(S(0), (t += 1000), "shared.rs"), held(S(9), S(0), (t += 1000), "shared.rs"));
  const timing = Live.pace(Live.model({ agents: [] }, ev), ev);
  const V = View.prepare(timing), L = V.layout(dims(WIDE, timing));
  const fr = V.frameFor(L, 0);
  assert.equal(fr.mode, "chip");
  // The camera scrolls to the held card's lane while the wire draws.
  const w = V.WIRES[0], st = V.frameState(w.at + 0.3), cam = V.flowView(L, st, 1, 0);
  const top = L.top + 9 * L.row + cam.y;
  assert.ok(top >= L.top - 1 && top + L.HH <= L.Hc + 1, `held lane at ${top}`);
});

test("a held wire says the recorded refusal, not a stale read, for every kind", () => {
  const said = {
    stale_base: "Changed since this agent read it.",
    conflict: "Another agent is editing it.",
    unread_lines: "Lines another agent wrote, not read yet.",
    commit_held: "The commit would take another agent's changes.",
  };
  for (const [refusal, words] of Object.entries(said)) {
    seq = 0;
    const ev = [anchor(), read(S(1), 1000, "a.rs"), edit(S(0), 2000, "a.rs"), Object.assign(held(S(1), S(0), 3000, "a.rs"), { refusal })];
    const V = View.prepare(Live.pace(Live.model({ agents: [] }, ev), ev));
    assert.equal(V.WIRES.length, 1, refusal);
    assert.equal(V.heldWhy(0), words, refusal);
    // The source chip too, never "edited … earlier" for a hold that isn't one.
    assert.equal(V.chipText(0), " · " + V.T.lanes[V.WIRES[0].from_lane].short + ": " + words.charAt(0).toLowerCase() + words.slice(1, -1), refusal);
  }
  // The hero's recording keeps its own sentence.
  const hero = View.prepare(HERO), i = hero.WIRES.findIndex((w) => w.kind === "held");
  assert.match(hero.heldWhy(i), / last read it\.$/);
  assert.match(hero.chipText(i), / edited \S+ earlier$/);
});

test("the route of a hero wire is the hero's own path", () => {
  // The hero's c1 is held from lane 1 (chip), c31 from c3 in lane 0.
  const V = View.prepare(HERO), L = V.layout(WIDE);
  const i = HERO.wires.findIndex((w) => w.to === "c31"), fr = V.frameFor(L, i), rt = V.wireRoute(L, i, 0, fr, 0);
  const from = V.byId[fr.from], to = V.byId["c31"];
  const sx = L.x[from.id] + L.W / 2, sy = V.cardY(L, from) + V.cardH(L, from), gapY = L.top + (L.HH + L.row) / 2;
  const tx = L.x[to.id] + L.W / 2, ty = V.cardY(L, to), r = 10, h = Math.sign(tx - sx);
  const f = (v) => v.toFixed(1);
  const hero = "M " + f(sx) + " " + f(sy) + " L " + f(sx) + " " + f(gapY - r) + " Q " + f(sx) + " " + f(gapY) + " " + f(sx + r * h) + " " + f(gapY)
    + " L " + f(tx - r * h) + " " + f(gapY) + " Q " + f(tx) + " " + f(gapY) + " " + f(tx) + " " + f(gapY + r) + " L " + f(tx) + " " + f(ty);
  assert.equal(V.pathD(rt), hero);
});
