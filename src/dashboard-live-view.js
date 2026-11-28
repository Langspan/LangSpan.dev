// The Live view's renderer (issue #491, ADR 0013), one component for the
// dashboard and the homepage hero. It is the hero's renderer
// (Langspan/langspan.github.io#41, assets/product-first.js) with the
// closure's TIMING and layout passed in, in two parts:
// - pure, with no DOM: `prepare(timing)` gives the layout, the frame state
//   at a time, the camera, a held write's framing and a wire's route, which
//   tests/live-view-render.test.mjs checks;
// - `mount(root, timing)` draws it into the hero's markup and returns an
//   object whose `update(timing)` redraws with new data.
// A timing with `live: true` (LangSpanLive.pace) plays once and rests at the
// newest work; any other is a recorded loop, as on the hero. Every value
// from the coordinator is placed with textContent, never as markup.
"use strict";
var LangSpanLiveView = (function () {
  const bezier = ([x1, y1, x2, y2]) => (p) => {
    if (p <= 0) return 0;
    if (p >= 1) return 1;
    let lo = 0, hi = 1, u = p;
    for (let i = 0; i < 40; i++) {
      u = (lo + hi) / 2;
      const x = 3 * (1 - u) ** 2 * u * x1 + 3 * (1 - u) * u * u * x2 + u ** 3;
      if (x < p) lo = u; else hi = u;
    }
    return 3 * (1 - u) ** 2 * u * y1 + 3 * (1 - u) * u * u * y2 + u ** 3;
  };
  const clamp01 = (v) => Math.max(0, Math.min(1, v));
  // Three lane colors, as on the hero; a fourth lane takes the first again.
  const laneClass = (i) => "lane-" + "abc"[i % 3];
  // Lanes in view at once before the canvas scrolls (#491, 28 Nov).
  const VISIBLE = { wide: 6, narrow: 3 };
  const CORNER = 10;
  // Card style B's verb glyphs, the hero's shapes, drawn in currentColor.
  // Each sits beside its verb in words; none carries meaning alone.
  const GLYPHS = {
    edit: [{ fill: "none", stroke: "currentColor", "stroke-width": "1.6", "stroke-linejoin": "round" }, [["path", { d: "M3 13l1-3.5L11 2.5l2.5 2.5L6.5 12z" }]]],
    read: [{ fill: "none", stroke: "currentColor", "stroke-width": "1.6" }, [["path", { d: "M1.5 8s2.5-4.5 6.5-4.5S14.5 8 14.5 8s-2.5 4.5-6.5 4.5S1.5 8 1.5 8z" }], ["circle", { cx: "8", cy: "8", r: "2" }]]],
    command: [{ fill: "none", stroke: "currentColor", "stroke-width": "1.6", "stroke-linecap": "round", "stroke-linejoin": "round" }, [["path", { d: "M3 4.5l3.5 3.5L3 11.5M8.5 12h4.5" }]]],
    saved: [{ fill: "none", stroke: "currentColor", "stroke-width": "1.8", "stroke-linecap": "round", "stroke-linejoin": "round" }, [["path", { d: "M3 8.5l3.2 3L13 4.5" }]]],
    held: [{ fill: "currentColor" }, [["rect", { x: "4", y: "3", width: "2.6", height: "10", rx: "1" }], ["rect", { x: "9.4", y: "3", width: "2.6", height: "10", rx: "1" }]]],
    message: [{ fill: "none", stroke: "currentColor", "stroke-width": "1.5", "stroke-linejoin": "round" }, [["rect", { x: "2", y: "3.5", width: "12", height: "9", rx: "1.5" }], ["path", { d: "M2.5 4.5L8 9l5.5-4.5" }]]],
  };
  GLYPHS.sent = GLYPHS.got = GLYPHS.message;
  // `NS` is the SVG namespace, read from the markup's own <svg>.
  function glyph(NS, kind) {
    const [attrs, parts] = GLYPHS[kind] || GLYPHS.edit;
    const svg = document.createElementNS(NS, "svg");
    for (const [k, v] of Object.entries({ class: "glyph", viewBox: "0 0 16 16", "aria-hidden": "true", focusable: "false", ...attrs })) svg.setAttribute(k, v);
    for (const [tag, a] of parts) { const e = document.createElementNS(NS, tag); for (const [k, v] of Object.entries(a)) e.setAttribute(k, v); svg.append(e); }
    return svg;
  }

  function prepare(T) {
    const P = T.pacing, live = !!T.live, LOOP = T.duration_s;
    const easeIn = bezier(P.entrance_curve), easeFrame = bezier(P.framing.ease_curve), easeLift = bezier([0.37, 0, 0.63, 1]);
    const ramp = P.camera.ease_ms / 1000 / 2;
    // Flow time: the clock with each stop taken out. Around a stop the speed
    // ramps linearly to zero over `ramp` seconds and back, so the pan eases
    // into and out of each coordination moment.
    function stopped(t, a, b) {
      const r = ramp;
      // Integral of a trapezoid: 0 before a-r, rising to 1 at a, 1 until b, 0 after b+r.
      const up = t <= a - r ? 0 : t < a ? (t - (a - r)) ** 2 / (2 * r) : r / 2;
      const flat = Math.max(0, Math.min(t, b) - a);
      const down = t <= b ? 0 : t < b + r ? (t - b) - (t - b) ** 2 / (2 * r) : r / 2;
      return up + flat + down;
    }
    const STOPS = T.stops_s;
    const flowAt = (t) => t - STOPS.reduce((s, [a, b]) => s + stopped(t, a, b), 0);
    const FLOW_LOOP = flowAt(LOOP);
    const CARDS = T.cards, WIRES = T.wires, STATUS = T.status;
    const byId = Object.fromEntries(CARDS.map((c) => [c.id, c]));
    const order = Object.fromEntries(CARDS.map((c, i) => [c.id, i]));
    const FLOW_OF = Object.fromEntries(CARDS.map((c) => [c.id, flowAt(c.at)]));

    function frameState(seconds) {
      const t = live ? Math.max(0, Math.min(LOOP, seconds)) : ((seconds % LOOP) + LOOP) % LOOP;
      const cards = {};
      let newest = null;
      for (const c of CARDS) {
        const k = easeIn(clamp01((t - c.at) * 1000 / P.entrance_ms));
        cards[c.id] = [k, P.entrance_travel_px * (1 - k), 1, 1];
        if (t >= c.at) newest = c.id;
      }
      // Only the current moment's wire draws at full strength: after its stop
      // ends, a wire fades to fade_to over fade_ms.
      const fade = WIRES.map((w) => 1 - (1 - P.wire_fade.to) * easeIn(clamp01((t - w.stop[1]) * 1000 / P.wire_fade.ms)));
      const wires = WIRES.map((w) => {
        const p = easeIn(clamp01((t - w.at) * 1000 / w.ms));
        if (w.emphasis_at !== undefined) {
          const e = clamp01((t - w.emphasis_at) * 1000 / P.emphasis_ms);
          cards[w.to][2] = 1 + (P.emphasis_scale - 1) * easeLift(Math.sin(Math.PI * e));
        }
        return p;
      });
      for (const c of CARDS) if (c.kind === "saved") cards[c.id][3] = easeIn(clamp01((t - c.at - P.tint_delay_s) * 1000 / P.tint_ms));
      // Which stop, if any, the camera is holding for, with its 0..1 weight.
      // A held write's stop frames both of its cards: the camera eases out after
      // the held card enters, holds while the wire draws, and eases back before
      // the stop ends.
      let hold = null;
      const F = P.framing, fe = F.ease_ms / 1000;
      for (let i = 0; i < WIRES.length; i++) {
        if (WIRES[i].kind !== "held") continue;
        const [a, b] = WIRES[i].stop;
        const w = Math.min(easeFrame(clamp01((t - a - F.enter_after_s) / fe)), 1 - easeFrame(clamp01((t - (b - fe)) / fe)));
        if (t >= a && t <= b && w > 0) hold = [i, w];
      }
      let status = STATUS[0] || { agents: T.lanes.length, held: 0 };
      for (const s of STATUS) if (t >= s.at) status = s;
      return { t, frame: Math.floor(t * T.fps + 1e-6), flow: flowAt(t), cards, wires, fade, newest, hold, status: [status.agents, status.held] };
    }

    /* Layout from the canvas sizes `d` (the CSS custom properties). Card x
       comes from flow time, then each lane is packed so its cards never
       overlap. */
    function layout(d) {
      const l = { ...d };
      // Cards keep their order in time across all lanes: each card sits
      // K px per second of flow after the one before it, pushed right only as
      // far as needed to clear the last card in its own lane.
      const x = {}, lastRight = {};
      let prevX = null, prevF = 0;
      for (const c of CARDS) {
        const want = prevX === null ? l.K * FLOW_OF[c.id] : prevX + l.K * (FLOW_OF[c.id] - prevF);
        x[c.id] = lastRight[c.lane] === undefined ? want : Math.max(want, lastRight[c.lane] + l.gap);
        lastRight[c.lane] = x[c.id] + l.W; prevX = x[c.id]; prevF = FLOW_OF[c.id];
      }
      l.x = x;
      l.lanes = T.lanes.length;
      l.visible = live ? VISIBLE[l.narrow ? "narrow" : "wide"] : l.lanes;
      // A loop's length in px; live work has one copy, at offset 0.
      l.period = 0;
      if (!live && CARDS.length) {
        const first = CARDS[0], lastC = CARDS[CARDS.length - 1];
        let period = x[lastC.id] + l.K * (FLOW_LOOP - FLOW_OF[lastC.id] + FLOW_OF[first.id]) - x[first.id];
        for (const lane of T.lanes.keys()) {
          const mine = CARDS.filter((c) => c.lane === lane);
          if (mine.length) period = Math.max(period, x[mine[mine.length - 1].id] + l.W + l.gap - x[mine[0].id]);
        }
        l.period = period;
      }
      // Camera keys: when each card enters, it sits at newest_at across the
      // canvas. Between keys the camera moves at a steady speed in flow time.
      // Live work starts at the left edge until it reaches newest_at.
      const keys = [];
      const floor = live && CARDS.length ? x[CARDS[0].id] - (l.G + 24) : -Infinity;
      for (const c of CARDS) {
        const want = Math.max(floor, x[c.id] + l.W / 2 - P.camera.newest_at * l.Wc);
        keys.push([FLOW_OF[c.id], keys.length ? Math.max(keys[keys.length - 1][1], want) : want]);
      }
      l.keys = keys;
      return l;
    }
    const cardY = (L, c) => L.top + c.lane * L.row;
    const cardH = (L, c) => (c.kind === "held" ? L.HH : L.H);
    // Which loop copies can be on screen: copy 0 plays, earlier copies are
    // settled. Live work has one copy.
    function copies(L, viewL, viewR, margin) {
      if (live || !CARDS.length) return [0, 0];
      const nMin = Math.floor((viewL - margin - L.x[CARDS[CARDS.length - 1].id]) / L.period), nMax = Math.floor((viewR + margin) / L.period);
      return [Math.min(0, nMin), Math.min(0, nMax)];
    }

    /* Camera. In the flow the newest work lands at newest_at across the
       canvas; during a coordination moment it holds, and if the wire's far
       end would be off screen it eases back just enough to show both cards. */
    function flowCamera(L, st, fs) {
      const k = L.keys, n = k.length, f = st.flow;
      if (!n) return -(L.G + 24);
      let a, b;
      if (live && f <= k[0][0]) { a = b = k[0]; }
      else if (live && f >= k[n - 1][0]) { a = b = k[n - 1]; }
      else if (f < k[0][0]) { a = [k[n - 1][0] - FLOW_LOOP, k[n - 1][1] - L.period]; b = k[0]; }
      else if (f >= k[n - 1][0]) { a = k[n - 1]; b = [k[0][0] + FLOW_LOOP, k[0][1] + L.period]; }
      else { let i = 0; while (k[i + 1][0] <= f) i++; a = k[i]; b = k[i + 1]; }
      const base = a[1] + (b[1] - a[1]) * (b[0] > a[0] ? (f - a[0]) / (b[0] - a[0]) : 1);
      // At a flow scale below 1, the newest work stays at the same place across the canvas.
      return base + P.camera.newest_at * L.Wc * (1 - 1 / fs);
    }
    // The vertical offsets at scale s that show lanes lo to hi whole, as
    // [least, most]; least > most when they can't all show at once.
    const yRange = (L, lo, hi, s) => [-lo * L.row * s, L.Hc - L.top - (hi * L.row + L.HH) * s];
    // Live lanes scroll between the first lane and the last.
    function clampY(L, y, s) {
      if (!live) return y;
      return Math.max(Math.min(0, L.Hc - L.top - L.lanes * L.row * s), Math.min(0, y));
    }
    // The opening scroll: the window of lanes with the most cards in view
    // when the camera rests at the newest work, then the one holding the
    // newest card. Lanes busy only earlier never hide the current work.
    function openingY(L) {
      const k = L.keys, x0 = k.length ? k[k.length - 1][1] : 0, newest = CARDS.length ? CARDS[CARDS.length - 1].lane : 0;
      const seen = (c) => L.x[c.id] + L.W > x0 + L.G && L.x[c.id] < x0 + L.Wc;
      const counts = T.lanes.map((_, i) => CARDS.filter((c) => c.lane === i && seen(c)).length);
      let best = 0, most = -1;
      for (let j = 0; j + L.visible <= Math.max(L.lanes, L.visible); j++) {
        const n = 2 * counts.slice(j, j + L.visible).reduce((a, b) => a + b, 0) + (newest >= j && newest < j + L.visible ? 1 : 0);
        if (n > most) { most = n; best = j; }
      }
      return clampY(L, -best * L.row, 1);
    }
    /* How a held write is framed, from the layout alone:
       'both'     both cards fit at a scale that keeps card text at least
                  framing.min_text_px; the wire runs card to card.
       'neighbor' (wide) they don't fit; the wire starts from the source
                  lane's first card in view, marked "earlier".
       'chip'     (narrow, or lanes too far apart to show together) a small
                  source chip beside the held card.
       `y` is the range of vertical offsets that shows the lanes it needs. */
    function frameFor(L, i) {
      L.frames = L.frames || {};
      if (L.frames[i]) return L.frames[i];
      const w = WIRES[i], to = byId[w.to], from = w.from ? byId[w.from] : null, F = P.framing, m = F.margin_px;
      const sMin = Math.min(1, F.min_text_px[L.narrow ? "narrow" : "wide"] / L.minText);
      const src = from ? from.lane : w.from_lane, lo = Math.min(src, to.lane), hi = Math.max(src, to.lane);
      const fits = (s) => { const [a, b] = yRange(L, lo, hi, s); return a <= b; };
      const st = frameState(w.stop[0] + F.enter_after_s);
      const base = flowCamera(L, st, 1);
      let fr = null;
      if (from && L.x[from.id] < L.x[to.id]) {
        const span = L.x[to.id] + L.W - L.x[from.id], s = Math.min(1, (L.Wc - L.G - 2 * m) / span);
        if (s >= sMin && fits(s)) fr = { mode: "both", s, x: L.x[from.id] - (L.G + m) / s, from: from.id };
      }
      if (!fr) {
        const left = base + (L.G + 8);
        const near = fits(1) ? CARDS.filter((c) => c.lane === src && c.at <= to.at && L.x[c.id] >= left && L.x[c.id] + L.W <= L.x[to.id]) : [];
        fr = !L.narrow && near.length ? { mode: "neighbor", s: 1, x: base, from: near[0].id } : { mode: "chip", s: 1, x: base, from: null };
      }
      // A chip sits in the gap on the source's side of the held card.
      const side = src < to.lane ? Math.max(0, to.lane - 1) : Math.min(L.lanes - 1, to.lane + 1);
      fr.y = fr.mode === "chip" ? yRange(L, Math.min(side, to.lane), Math.max(side, to.lane), 1) : yRange(L, lo, hi, fr.s);
      return (L.frames[i] = fr);
    }
    // The camera while it follows the newest work: `fs` is the flow scale
    // (1 is the default view), `y` the lanes' scroll.
    function flowView(L, st, fs, y) {
      const cam = { s: fs, x: flowCamera(L, st, fs), y: clampY(L, y, fs) };
      if (st.hold && fs === 1) {
        const fr = frameFor(L, st.hold[0]), k = st.hold[1];
        cam.s = 1 + (fr.s - 1) * k; cam.x = cam.x + (fr.x - cam.x) * k;
        const want = Math.max(fr.y[0], Math.min(fr.y[1], cam.y));
        cam.y = cam.y + (want - cam.y) * k;
      }
      return cam;
    }

    /* A wire's route. Held and message wires run through the gap between
       two lanes and enter each card through its top or bottom edge, so they
       never cross a card's body. Between lanes further apart, the wire
       drops through the other lanes where none of their cards is. `chipX`
       is where a chip-framed wire starts. */
    const gapBelow = (L, lane) => L.top + lane * L.row + (L.HH + L.row) / 2;
    function wireRoute(L, i, off, fr, chipX) {
      const w = WIRES[i], to = byId[w.to];
      const from = fr.from ? byId[fr.from] : null, src = w.from ? byId[w.from].lane : w.from_lane;
      const down = to.lane > src;
      const gapA = gapBelow(L, down ? src : src - 1), gapB = gapBelow(L, down ? to.lane - 1 : to.lane);
      const tx = L.x[to.id] + off + L.W / 2, ty = down ? cardY(L, to) : cardY(L, to) + cardH(L, to);
      if (fr.mode === "chip") return { points: [[chipX, gapB], [tx, gapB], [tx, ty]], sx: chipX, sy: gapB, tx, ty, gapY: gapB, down, startsInGap: true, simple: true, label: [chipX, tx] };
      const sx = L.x[from.id] + off + L.W / 2, sy = down ? cardY(L, from) + cardH(L, from) : cardY(L, from);
      if (gapA === gapB) return { points: [[sx, sy], [sx, gapA], [tx, gapA], [tx, ty]], sx, sy, tx, ty, gapY: gapA, down, startsInGap: false, simple: true, label: [sx, tx] };
      const xc = column(L, sx, tx, Math.min(src, to.lane) + 1, Math.max(src, to.lane) - 1, off);
      const points = [[sx, sy], [sx, gapA], [xc, gapA], [xc, gapB], [tx, gapB], [tx, ty]].filter((p, j, all) => !j || p[0] !== all[j - 1][0] || p[1] !== all[j - 1][1]);
      return { points, sx, sy, tx, ty, gapY: gapB, down, startsInGap: false, simple: false, label: [xc, tx] };
    }
    // An x between sx and tx (or nearest their middle) where no card of
    // lanes lo to hi is, for a wire to pass those lanes.
    function column(L, sx, tx, lo, hi, off) {
      const mine = CARDS.filter((c) => c.lane >= lo && c.lane <= hi).map((c) => L.x[c.id] + off);
      const free = (x) => mine.every((cx) => x <= cx - 3 || x >= cx + L.W + 3);
      const mid = (sx + tx) / 2;
      const options = [mid, sx, tx, ...mine.flatMap((cx) => [cx - L.gap / 2, cx + L.W + L.gap / 2])].filter(free);
      options.sort((a, b) => Math.abs(a - mid) - Math.abs(b - mid));
      return options.length ? options[0] : mid;
    }
    // SVG path data for a route, with rounded corners.
    function pathD(rt) {
      const f = (v) => v.toFixed(1), p = rt.points, r = CORNER;
      let d = "M " + f(p[0][0]) + " " + f(p[0][1]);
      if (rt.simple && Math.abs(rt.tx - rt.sx) < 2 * r) return d + " C " + f(rt.sx) + " " + f(rt.gapY) + ", " + f(rt.tx) + " " + f(rt.gapY) + ", " + f(rt.tx) + " " + f(rt.ty);
      const len = (a, b) => Math.abs(b[0] - a[0]) + Math.abs(b[1] - a[1]);
      for (let j = 1; j < p.length - 1; j++) {
        const a = p[j - 1], b = p[j], c = p[j + 1];
        const din = [Math.sign(b[0] - a[0]), Math.sign(b[1] - a[1])], dout = [Math.sign(c[0] - b[0]), Math.sign(c[1] - b[1])];
        const rr = Math.min(r, j === 1 ? len(a, b) : len(a, b) / 2, j === p.length - 2 ? len(b, c) : len(b, c) / 2);
        d += " L " + f(b[0] - rr * din[0]) + " " + f(b[1] - rr * din[1]) + " Q " + f(b[0]) + " " + f(b[1]) + " " + f(b[0] + rr * dout[0]) + " " + f(b[1] + rr * dout[1]);
      }
      const z = p[p.length - 1];
      return d + " L " + f(z[0]) + " " + f(z[1]);
    }

    // Why a held wire's write waited. The hero's recording is a stale read;
    // live work says the refusal LangSpan recorded, as its card does.
    function heldWhy(i) {
      const w = WIRES[i], to = byId[w.to], from = w.from ? byId[w.from] : null;
      if (live) return to.sub.charAt(0).toUpperCase() + to.sub.slice(1) + ".";
      return T.lanes[w.from_lane].label + " changed " + to.code + (from && from.time ? " (" + from.time + ")" : "") + " after " + T.lanes[to.lane].label + " last read it.";
    }

    return { T, P, live, LOOP, CARDS, WIRES, byId, order, flowAt, frameState, layout, copies, cardY, cardH, flowCamera, clampY, openingY, frameFor, flowView, wireRoute, pathD, heldWhy };
  }

  /* ---------- Drawing: the hero's markup, pools and interactions ---------- */
  function mount(root, initial) {
    const $ = (s) => root.querySelector(s);
    const reduce = matchMedia("(prefers-reduced-motion: reduce)");
    const forcedStatic = new URLSearchParams(location.search).get("motion") === "static";
    const isStatic = () => reduce.matches || forcedStatic;
    const canvas = $("[data-canvas]"), world = $("[data-world]"), gutter = $("[data-gutter]");
    const layer = $("[data-cards]"), wireLayer = $("[data-wires]"), labelLayer = $("[data-wire-labels]");
    const countAgents = $("[data-status-agents]"), countHeld = $("[data-status-held]");
    const playBtn = $("[data-hero-play]");
    const pop = $("[data-pop]"), popBody = $("[data-pop-body]");
    const followBtn = $('[data-cam="follow"]');
    $("[data-static]")?.remove();
    root.classList.add("is-live");
    let V = prepare(initial), P = V.P, L = null, laneY = null;
    const NS = wireLayer.namespaceURI;

    function measure() {
      const cs = getComputedStyle(canvas), n = (v) => parseFloat(cs.getPropertyValue(v));
      const d = { W: n("--card-w"), H: n("--card-h"), HH: n("--held-h"), row: n("--row-h"), top: n("--row-top"), G: n("--gutter"), gap: n("--card-gap"),
        K: n("--flow-k"), Wc: canvas.clientWidth, Hc: canvas.clientHeight, minText: n("--card-min-text"), narrow: cs.getPropertyValue("--layout").trim() === "narrow" };
      if (V.live) {
        // The canvas is as tall as the lanes in view, so more lanes scroll.
        const shown = Math.max(1, Math.min(V.T.lanes.length, VISIBLE[d.narrow ? "narrow" : "wide"]));
        const h = d.top + shown * d.row;
        if (Math.abs(canvas.clientHeight - h) > 0.5) canvas.style.height = h + "px";
        d.Hc = h;
      }
      return d;
    }
    function layout() {
      L = V.layout(measure());
      if (laneY === null && V.CARDS.length) laneY = V.openingY(L);
      for (const el of gutter.children) el.style.setProperty("--y", (+el.dataset.lane * L.row) + "px");
    }
    function lanes() {
      gutter.replaceChildren(...V.T.lanes.map((lane, i) => {
        const b = document.createElement("button");
        b.type = "button"; b.className = "lane-name " + laneClass(i); b.dataset.lane = i; b.dataset.detailOpen = "lane:" + i;
        b.setAttribute("aria-haspopup", "dialog"); b.setAttribute("aria-expanded", "false");
        const full = document.createElement("span"), short = document.createElement("span");
        full.className = "full"; full.textContent = lane.label; short.className = "short"; short.textContent = lane.short;
        b.append(full, short);
        if (lane.meta) { const meta = document.createElement("span"); meta.className = "meta"; meta.textContent = lane.meta; b.append(meta); }
        return b;
      }));
    }
    if (V.live) lanes();

    const cam = { mode: "flow", s: 1, x: 0, y: 0, fs: 1 };
    let laneScrolled = false;

    // Pools: card buttons, wire paths and labels are reused as they scroll.
    const pool = new Map(), wirePool = new Map(), labelPool = new Map();
    const sig = (c) => [c.kind, c.lane, c.verb, c.code, c.sub, c.who].join("\n");
    function cardEl(key, c) {
      let el = pool.get(key);
      if (el) return el;
      el = document.createElement("li");
      el.className = "card " + c.kind + " " + laneClass(c.lane);
      el.dataset.card = c.id; el.dataset.seq = c.seq.join(" "); el.dataset.sig = sig(c);
      const b = document.createElement("button");
      b.type = "button"; b.className = "card-hit"; b.dataset.detailOpen = "card:" + c.id;
      b.setAttribute("aria-haspopup", "dialog"); b.setAttribute("aria-expanded", "false");
      const add = (tag, cls, text) => { if (!text) return; const e = document.createElement(tag); e.className = cls; e.textContent = text; b.append(e); };
      add("span", "who", c.who); add("span", "verb", c.verb); b.querySelector(".verb")?.prepend(glyph(NS, c.kind)); add("code", "path", c.code && c.code.split("/").slice(-2).join("/").replace(/([/_.])/g, "$1​")); add("span", "sub", c.sub);
      if (c.code) b.title = c.code;
      el.append(b);
      pool.set(key, el);
      return el;
    }
    function wireEl(key, w) {
      let g = wirePool.get(key);
      if (g) return g;
      g = document.createElementNS(NS, "g");
      g.setAttribute("class", "wire wire-" + w.kind);
      const p = document.createElementNS(NS, "path"); p.setAttribute("class", "wire-line"); p.setAttribute("pathLength", "1"); g.append(p);
      wirePool.set(key, g);
      return g;
    }
    function labelEl(key, w, i) {
      let b = labelPool.get(key);
      if (b) return b;
      b = document.createElement("button");
      b.type = "button"; b.className = "wire-label wire-label-" + w.kind; b.textContent = w.label;
      b.dataset.detailOpen = "wire:" + i; b.setAttribute("aria-haspopup", "dialog"); b.setAttribute("aria-expanded", "false");
      labelPool.set(key, b);
      return b;
    }
    const chipPool = new Map(), markPool = new Map(), chainPool = new Map();
    function chipEl(key, w, i) {
      let el = chipPool.get(key);
      if (el) return el;
      const to = V.byId[w.to], lane = V.T.lanes[w.from_lane];
      el = document.createElement("button");
      el.type = "button"; el.className = "source-chip"; el.dataset.detailOpen = "wire:" + i;
      el.setAttribute("aria-haspopup", "dialog"); el.setAttribute("aria-expanded", "false");
      const held = document.createElement("b");
      held.textContent = w.label;
      el.append(held, " · " + lane.short + " edited " + (to.code || "").split("/").pop() + " earlier");
      chipPool.set(key, el);
      return el;
    }
    function markEl(key) {
      let el = markPool.get(key);
      if (el) return el;
      el = document.createElement("span");
      el.className = "wire-earlier"; el.textContent = "earlier"; el.setAttribute("aria-hidden", "true");
      markPool.set(key, el);
      return el;
    }
    /* Each lane's chain of work: a line in the lane's color from each card to
       the agent's next card, with small nodes at the card edges. A long idle
       stretch in the log is drawn dashed and lighter. */
    const chainLayer = document.createElementNS(NS, "g");
    chainLayer.setAttribute("class", "chains");
    wireLayer.prepend(chainLayer);
    function drawChains(st, viewL, viewR, margin) {
      const want = new Set(), y = (c) => L.top + c.lane * L.row + L.H / 2;
      const nMin = V.live ? 0 : Math.floor((viewL - margin - L.period) / L.period);
      for (let n = nMin; n <= 0; n++) {
        for (const c of V.CARDS) {
          if (!c.next) continue;
          const nx = V.byId[c.next], nn = c.next_loop ? n + 1 : n;
          const a = L.x[c.id] + n * L.period + L.W, b = L.x[nx.id] + nn * L.period;
          if (b < viewL - margin || a > viewR + margin) continue;
          const oa = n < 0 ? 1 : st.cards[c.id][0], ob = nn < 0 ? 1 : nn > 0 ? 0 : st.cards[nx.id][0];
          if (oa < 0.001 || ob < 0.001) continue;
          const key = n + ":" + c.id;
          want.add(key);
          let g = chainPool.get(key);
          if (!g) {
            g = document.createElementNS(NS, "g");
            const line = document.createElementNS(NS, "line"), n1 = document.createElementNS(NS, "circle"), n2 = document.createElementNS(NS, "circle");
            n1.setAttribute("r", P.connector.node_px / 2); n2.setAttribute("r", P.connector.node_px / 2);
            g.append(line, n1, n2); chainPool.set(key, g);
          }
          g.setAttribute("class", "chain " + laneClass(c.lane) + (c.idle_after ? " idle" : ""));
          const [line, n1, n2] = g.children, yy = y(c).toFixed(1);
          line.setAttribute("x1", a.toFixed(1)); line.setAttribute("x2", b.toFixed(1)); line.setAttribute("y1", yy); line.setAttribute("y2", yy);
          n1.setAttribute("cx", a.toFixed(1)); n1.setAttribute("cy", yy); n2.setAttribute("cx", b.toFixed(1)); n2.setAttribute("cy", yy);
          g.style.opacity = Math.min(oa, ob).toFixed(3);
          if (g.parentNode !== chainLayer) chainLayer.append(g);
        }
      }
      for (const [key, g] of chainPool) if (!want.has(key)) { g.remove(); chainPool.delete(key); }
    }

    let last = null;
    function renderAt(seconds) {
      if (!L) layout();
      const st = V.frameState(seconds);
      if (cam.mode === "flow") {
        const v = V.flowView(L, st, cam.fs, V.live ? (laneY || 0) : 0);
        cam.s = v.s; cam.x = v.x; cam.y = v.y;
      } else if (V.live) cam.y = V.clampY(L, cam.y, cam.s);
      const s = cam.s, viewL = cam.x, viewR = cam.x + L.Wc / s, margin = L.W * 2;
      // Zooming keeps the first lane at its place under the status line.
      const ty = L.top * (1 - s) + cam.y;
      world.style.transform = "translate(" + (-cam.x * s).toFixed(1) + "px," + ty.toFixed(1) + "px) scale(" + s.toFixed(4) + ")";
      // The dot grid is part of the plane, so it moves and scales with the work.
      const dot = 18 * s;
      canvas.style.backgroundSize = dot.toFixed(2) + "px " + dot.toFixed(2) + "px";
      canvas.style.backgroundPosition = (((-cam.x * s) % dot) + dot).toFixed(2) + "px " + ((ty % dot) + dot).toFixed(2) + "px";
      canvas.classList.toggle("zoom-compact", s < P.zoom.compact_below && s >= P.zoom.blocks_below);
      canvas.classList.toggle("zoom-blocks", s < P.zoom.blocks_below);
      gutter.style.setProperty("--s", s); gutter.style.setProperty("--dy", cam.y + "px");
      const want = new Map(), wantW = new Map();
      const [n0, n1] = V.copies(L, viewL, viewR, margin);
      for (let n = n0; n <= n1; n++) {
        const off = n * L.period;
        for (const c of V.CARDS) {
          const x = L.x[c.id] + off;
          if (x + L.W < viewL - margin || x > viewR + margin) continue;
          const [o, y, k, ok] = n < 0 ? [1, 0, 1, 1] : st.cards[c.id];
          if (o < 0.001) continue;
          want.set(n + ":" + c.id, [c, x, o, y, k, ok]);
        }
        V.WIRES.forEach((w, i) => {
          const p = n < 0 ? 1 : st.wires[i];
          // A held wire's start (its card, chip or marker) shows from the moment the camera frames it.
          if (p < 0.001 && !(n === 0 && st.hold && st.hold[0] === i)) return;
          const to = V.byId[w.to], from = w.from ? V.byId[w.from] : null;
          const x2 = L.x[to.id] + off, x1 = from ? L.x[from.id] + off + L.W : x2 - 60;
          if (Math.max(x1, x2) < viewL - margin || Math.min(x1, x2) > viewR + margin) return;
          wantW.set(n + ":" + i, [w, i, p, off]);
        });
      }
      // Cards: place, then drop the ones that left the view.
      const order = [...want.keys()].sort((a, b) => { const [na, ia] = a.split(":"), [nb, ib] = b.split(":"); return na - nb || V.order[ia] - V.order[ib]; });
      let prev = null;
      for (const key of order) {
        const [c, x, o, y, k, ok] = want.get(key), el = cardEl(key, c);
        el.style.cssText = "left:" + x.toFixed(1) + "px;top:" + V.cardY(L, c) + "px;opacity:" + o.toFixed(4) + ";transform:translateY(" + y.toFixed(2) + "px)" + (k > 1.0001 ? " scale(" + k.toFixed(4) + ")" : "") + (c.kind === "saved" ? ";--ok:" + ok.toFixed(4) : "");
        if (el.parentNode !== layer || el.previousElementSibling !== prev) layer.insertBefore(el, prev ? prev.nextSibling : layer.firstChild);
        prev = el;
      }
      for (const [key, el] of pool) if (!want.has(key)) { if (el.contains(document.activeElement)) continue; el.remove(); pool.delete(key); }
      // Wires and their labels. Labels, markers and chips stay fully inside the canvas.
      const keepIn = (el, x, mode, clampIt) => {
        const w = el.offsetWidth, lo = L.G + 4, hi = L.Wc - 4;
        let sx = (x - cam.x) * s, left = mode === "center" ? sx - w / 2 : sx;
        if (clampIt) { left = Math.max(lo, Math.min(hi - w, left)); sx = mode === "center" ? left + w / 2 : left; el.style.left = (cam.x + sx / s).toFixed(1) + "px"; }
        el.style.visibility = left >= lo - 0.5 && left + w <= hi + 0.5 ? "visible" : "hidden";
      };
      for (const [key, [w, i, p, off]] of wantW) {
        const g = wireEl(key, w), to = V.byId[w.to];
        const fr = w.kind === "held" ? V.frameFor(L, i) : { mode: "both", from: w.from };
        const current = off === 0 && st.fade[i] > 0.999, alpha = off < 0 ? P.wire_fade.to : st.fade[i];
        let chipX = 0;
        if (fr.mode === "chip") {
          // A small chip in the gap names the earlier change and carries the label.
          const r0 = V.wireRoute(L, i, off, fr, 0);
          const chip = chipEl(key, w, i), cw = Math.min(240, (L.Wc - L.G) * 0.9);
          chip.style.maxWidth = cw.toFixed(0) + "px";
          chip.style.left = r0.tx.toFixed(1) + "px"; chip.style.top = r0.gapY.toFixed(1) + "px";
          chip.style.opacity = (Math.max(clamp01(p * 2), current && st.hold && st.hold[0] === i ? st.hold[1] : 0) * alpha).toFixed(3);
          chip.style.transform = "translate(-50%,-50%)" + (s < 1 ? " scale(" + (1 / s).toFixed(3) + ")" : "");
          if (chip.parentNode !== labelLayer) labelLayer.append(chip);
          keepIn(chip, r0.tx, "center", current);
          chipX = parseFloat(chip.style.left);
        }
        const rt = V.wireRoute(L, i, off, fr, chipX);
        for (const path of g.children) { path.setAttribute("d", V.pathD(rt)); path.style.strokeDashoffset = (1 - p).toFixed(4); }
        g.dataset.from = fr.from || ""; g.dataset.to = to.id; g.dataset.off = off;
        g.style.opacity = alpha.toFixed(3);
        if (g.parentNode !== wireLayer) wireLayer.append(g);
        if (fr.mode === "neighbor") {
          // The wire starts from a later card in the same lane: mark it "earlier".
          const mk = markEl(key);
          mk.style.left = (rt.sx + 6 / s).toFixed(1) + "px"; mk.style.top = (rt.sy + (rt.down ? 4 : -20) / s).toFixed(1) + "px";
          mk.style.opacity = (Math.max(clamp01(p * 2), current && st.hold && st.hold[0] === i ? st.hold[1] : 0) * alpha).toFixed(3);
          mk.style.transform = s < 1 ? "scale(" + (1 / s).toFixed(3) + ")" : "";
          if (mk.parentNode !== labelLayer) labelLayer.append(mk);
          keepIn(mk, rt.sx + 6 / s, "left", current);
        }
        if (fr.mode === "chip") continue;
        const [la, lb2] = rt.label, lb = labelEl(key, w, i), lx = Math.abs(lb2 - la) < 2 * CORNER ? lb2 + 40 / s : (la + lb2) / 2;
        lb.style.left = lx.toFixed(1) + "px"; lb.style.top = rt.gapY.toFixed(1) + "px";
        // Labels keep their size when the plane is zoomed out.
        lb.style.transform = "translate(-50%,-50%)" + (s < 1 ? " scale(" + (1 / s).toFixed(3) + ")" : "");
        lb.style.opacity = (clamp01(p * 2) * alpha).toFixed(3);
        if (lb.parentNode !== labelLayer) labelLayer.append(lb);
        keepIn(lb, lx, "center", current);
      }
      for (const [key, g] of wirePool) if (!wantW.has(key)) { g.remove(); wirePool.delete(key); }
      for (const [mine, mode] of [[chipPool, "chip"], [markPool, "neighbor"]]) {
        for (const [key, el] of mine) if (!wantW.has(key) || V.frameFor(L, +key.split(":")[1]).mode !== mode) { el.remove(); mine.delete(key); }
      }
      drawChains(st, viewL, viewR, margin);
      for (const [key, b] of labelPool) if (!wantW.has(key)) { if (b === document.activeElement) continue; b.remove(); labelPool.delete(key); }
      if (!last || last.status[0] !== st.status[0]) countAgents.textContent = st.status[0] + (st.status[0] === 1 ? " agent" : " agents");
      if (!last || last.status[1] !== st.status[1]) countHeld.textContent = st.status[1] + (st.status[1] === 1 ? " write" : " writes");
      last = st;
      return st;
    }

    const syncFollow = () => followBtn.setAttribute("aria-pressed", String(cam.mode === "flow"));
    function freeCamera() { if (cam.mode === "flow") { cam.mode = "free"; syncFollow(); } }
    function zoomAt(f, cx = L.Wc / 2, cy = L.Hc / 2) {
      freeCamera();
      const s = Math.max(P.zoom.min, Math.min(P.zoom.max, cam.s * f));
      const wx = cam.x + cx / cam.s;
      cam.y = cy - (cy - cam.y) * s / cam.s; cam.x = wx - cx / s; cam.s = s;
      renderAt(elapsed);
    }
    function pan(dx, dy) { freeCamera(); cam.x -= dx / cam.s; cam.y += dy; renderAt(elapsed); }
    function fit() {
      freeCamera();
      if (V.live) {
        // All the recorded work, from its first card to its newest.
        const xs = V.CARDS.map((c) => L.x[c.id]), x0 = xs.length ? Math.min(...xs) : 0, x1 = xs.length ? Math.max(...xs) + L.W : L.W;
        cam.s = Math.max(P.zoom.min, Math.min(1, (L.Wc - L.G - 24) / (x1 - x0)));
        cam.x = x0 - (L.G + 12) / cam.s; cam.y = V.clampY(L, cam.y, cam.s);
      } else {
        cam.s = Math.max(P.zoom.min, Math.min(1, (L.Wc - L.G - 24) / L.period));
        // The loop copy in the middle of the flow's view (the hero read an
        // undefined L.speed here, which made the camera NaN).
        const st = V.frameState(elapsed), n0 = Math.min(0, Math.floor((V.flowCamera(L, st, 1) + L.Wc / 2) / L.period));
        cam.x = n0 * L.period - (L.G + 12) / cam.s; cam.y = 0;
      }
      renderAt(elapsed);
    }
    // Follow newest scrolls back to the lanes with the newest work.
    function follow() { if (V.live && L) { laneScrolled = false; laneY = V.openingY(L); } cam.mode = "flow"; cam.fs = 1; syncFollow(); renderAt(elapsed); }

    /* The clock. Left alone, a recording loops and live work plays up to the
       newest card and rests there. Any interaction pauses it; it resumes from
       the current beat after IDLE_MS without interaction, or at once on Play
       or Follow newest. A visitor's Pause stays paused. */
    const IDLE_MS = 8000;
    // Live work that arrives while the clock is behind plays at most this
    // far back from the newest card.
    const CATCH_UP_S = 3;
    let elapsed = 0, playing = !isStatic(), held = false, visible = !("IntersectionObserver" in window), lastNow = 0, raf = 0, idle = 0;
    const sync = () => { playBtn.textContent = playing ? "Pause" : "Play"; playBtn.setAttribute("aria-label", playing ? "Pause the view" : "Play the view"); };
    function tick(now) {
      raf = 0;
      if (!playing || !visible || document.hidden) return;
      if (lastNow) elapsed += Math.min(0.1, (now - lastNow) / 1000);
      lastNow = now;
      if (V.live) elapsed = Math.min(elapsed, V.LOOP); else elapsed %= V.LOOP;
      renderAt(elapsed);
      if (V.live && elapsed >= V.LOOP) return;
      raf = requestAnimationFrame(tick);
    }
    function start() { if (!raf && playing && visible && !document.hidden) { lastNow = 0; raf = requestAnimationFrame(tick); } }
    function stop() { playing = false; if (raf) cancelAnimationFrame(raf); raf = 0; sync(); }
    function resume() { clearTimeout(idle); idle = 0; held = false; closePop(); follow(); playing = true; sync(); start(); }
    function touched() {
      if (isStatic() || held) return;
      if (playing) stop();
      clearTimeout(idle);
      idle = setTimeout(() => { if (!held && !isStatic()) resume(); }, IDLE_MS);
    }
    playBtn.addEventListener("click", () => { if (playing) { held = true; clearTimeout(idle); stop(); } else resume(); });

    $("[data-camera]").hidden = false;
    $("[data-camera]").addEventListener("click", (e) => {
      const b = e.target.closest("[data-cam]");
      if (!b) return;
      const kind = b.dataset.cam;
      if (kind === "follow") { if (cam.mode === "flow") { freeCamera(); touched(); } else { follow(); if (!held && !isStatic()) resume(); } return; }
      touched();
      if (kind === "in") zoomAt(P.zoom.step);
      else if (kind === "out") zoomAt(1 / P.zoom.step);
      else fit();
    });
    const scrolls = () => V.live && L && L.lanes > L.visible;
    canvas.addEventListener("wheel", (e) => {
      if (!e.ctrlKey && !e.metaKey) {
        // A plain wheel scrolls the page, or the lanes once the canvas has focus.
        if (!scrolls() || !canvas.contains(document.activeElement)) return;
        e.preventDefault();
        laneScrolled = true;
        if (cam.mode === "flow") laneY = V.clampY(L, (laneY || 0) - e.deltaY, 1); else cam.y -= e.deltaY;
        renderAt(elapsed);
        return;
      }
      e.preventDefault(); touched();
      const r = canvas.getBoundingClientRect();
      zoomAt(Math.exp(-e.deltaY / 300), e.clientX - r.left, e.clientY - r.top);
    }, { passive: false });
    const pointers = new Map();
    let drag = null, pinch = null;
    canvas.addEventListener("pointerdown", (e) => {
      if (e.button !== 0 || e.target.closest("[data-pop]")) return;
      pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
      if (pointers.size === 2) { const [a, b] = [...pointers.values()]; pinch = { d: Math.hypot(a.x - b.x, a.y - b.y) || 1 }; drag = null; }
      else if (!e.target.closest("button")) drag = { x: e.clientX, y: e.clientY, moved: false };
    });
    canvas.addEventListener("pointermove", (e) => {
      if (!pointers.has(e.pointerId)) return;
      pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
      if (pinch && pointers.size === 2) {
        const [a, b] = [...pointers.values()], d = Math.hypot(a.x - b.x, a.y - b.y) || 1, r = canvas.getBoundingClientRect();
        touched(); zoomAt(d / pinch.d, (a.x + b.x) / 2 - r.left, (a.y + b.y) / 2 - r.top); pinch.d = d; return;
      }
      if (!drag) return;
      const dx = e.clientX - drag.x, dy = e.clientY - drag.y;
      if (!drag.moved && Math.hypot(dx, dy) < 4) return;
      if (!drag.moved) { drag.moved = true; canvas.classList.add("panning"); canvas.setPointerCapture(e.pointerId); }
      touched(); pan(dx, dy); drag.x = e.clientX; drag.y = e.clientY;
    });
    const endPointer = (e) => { pointers.delete(e.pointerId); if (pointers.size < 2) pinch = null; if (!pointers.size) { drag = null; canvas.classList.remove("panning"); } };
    canvas.addEventListener("pointerup", endPointer); canvas.addEventListener("pointercancel", endPointer);
    canvas.addEventListener("keydown", (e) => {
      if (e.altKey || e.ctrlKey || e.metaKey) return;
      const step = 48, keys = { ArrowLeft: [step, 0], ArrowRight: [-step, 0], ArrowUp: [0, step], ArrowDown: [0, -step] };
      if (keys[e.key]) { e.preventDefault(); touched(); pan(...keys[e.key]); }
      else if (e.key === "+" || e.key === "=") { e.preventDefault(); touched(); zoomAt(P.zoom.step); }
      else if (e.key === "-" || e.key === "_") { e.preventDefault(); touched(); zoomAt(1 / P.zoom.step); }
      else if (e.key === "0") { e.preventDefault(); touched(); fit(); }
      else if (e.key === "l" || e.key === "L") { e.preventDefault(); follow(); if (!held && !isStatic()) resume(); }
    });
    canvas.addEventListener("focusin", (e) => { if (!pop.contains(e.target)) touched(); });
    const relayout = () => { L = null; layout(); renderAt(elapsed); };
    if ("ResizeObserver" in window) new ResizeObserver(relayout).observe(canvas); else addEventListener("resize", relayout);

    /* Details: a click on a card, a wire label or a lane name opens a small
       popover anchored to it, inside the canvas. It never opens on its own,
       and it shows recorded fields only. */
    let opener = null;
    function rows(list) {
      const dl = document.createElement("dl");
      for (const [k, v] of list) { const dt = document.createElement("dt"), dd = document.createElement("dd"); dt.textContent = k; dd.textContent = v; dl.append(dt, dd); }
      return dl;
    }
    const plain = (label) => label.replace("Agent · ", "");
    function details(key) {
      const [type, id] = key.split(":"), kind = document.createElement("p"), T = V.T;
      kind.className = "pop-kind";
      if (type === "card") {
        const c = V.byId[id], lane = T.lanes[c.lane];
        kind.textContent = c.title + " · " + lane.label;
        const extra = [];
        if (c.kind === "held") {
          const w = V.WIRES.find((x) => x.to === c.id);
          const after = V.CARDS.filter((x) => x.lane === c.lane && x.at > c.at && (x.kind === "read" || x.kind === "saved") && x.code === c.code);
          if (w) extra.push([V.live ? "Other agent" : "Changed by", T.lanes[w.from_lane].label]);
          // The hero's recording quotes LangSpan's reply; the live feed doesn't carry message text.
          if (!V.live) extra.push(["What LangSpan told the agent", "“… Read each file again, then make the edit against its current content.”"]);
          if (after.length) extra.push(["What happened next", after.map((x) => (x.time ? x.time + " · " : "") + (x.kind === "read" ? "read the file again" : "saved its edit")).join("; ")]);
        }
        return [kind, rows(c.rows.concat(extra))];
      }
      if (type === "wire") {
        const w = V.WIRES[+id], to = V.byId[w.to], from = w.from ? V.byId[w.from] : null;
        kind.textContent = w.label + " · from " + plain(T.lanes[from ? from.lane : w.from_lane].label) + " to " + plain(T.lanes[to.lane].label);
        const list = w.kind === "held"
          ? [["Why", V.heldWhy(+id)], ["Held at", to.time || "not recorded"]]
          : [["Sent", from.time], ["Got it", to.time + ", with its next prompt"]];
        return [kind, rows(list)];
      }
      const lane = T.lanes[+id];
      kind.textContent = lane.label;
      const list = [["Agent", lane.agent], ["Works in", lane.checkout]];
      if (lane.branch) list.push(["Branch", lane.branch]);
      // Recorded without a time, so never a card.
      const untimed = (T.untimed || []).filter((e) => e.lane === +id).map((e) => String(e.type).replace(/_/g, " "));
      if (untimed.length) list.push(["Also recorded, without a time", untimed.join(", ")]);
      return [kind, rows(list)];
    }
    function placePop(anchor) {
      const c = canvas.getBoundingClientRect(), a = anchor.getBoundingClientRect(), m = 8;
      pop.style.left = "0px"; pop.style.top = "0px"; pop.style.maxHeight = (c.height - 2 * m) + "px";
      const w = pop.offsetWidth, h = pop.offsetHeight;
      let x = a.left - c.left + a.width / 2 - w / 2, y = a.bottom - c.top + 8;
      if (y + h > c.height - m) y = a.top - c.top - 8 - h;
      if (y < m) y = Math.max(m, Math.min(c.height - h - m, a.top - c.top));
      x = Math.max(m, Math.min(c.width - w - m, x));
      pop.style.left = x.toFixed(0) + "px"; pop.style.top = y.toFixed(0) + "px";
    }
    function openPop(button) {
      if (opener) opener.setAttribute("aria-expanded", "false");
      popBody.replaceChildren(...details(button.dataset.detailOpen));
      pop.setAttribute("aria-label", "Details: " + button.textContent.replace(/\s+/g, " ").trim());
      pop.hidden = false; opener = button; button.setAttribute("aria-expanded", "true");
      placePop(button);
      $("[data-pop-close]").focus({ preventScroll: true });
    }
    function closePop(refocus) {
      if (pop.hidden) return;
      pop.hidden = true;
      if (opener) { opener.setAttribute("aria-expanded", "false"); if (refocus && opener.isConnected) opener.focus({ preventScroll: true }); }
      opener = null;
    }
    root.addEventListener("click", (e) => {
      const b = e.target.closest("[data-detail-open]");
      if (b) { touched(); if (opener === b && !pop.hidden) closePop(true); else openPop(b); }
    });
    $("[data-pop-close]").addEventListener("click", () => { touched(); closePop(true); });
    document.addEventListener("pointerdown", (e) => { if (!pop.hidden && !pop.contains(e.target) && !e.target.closest("[data-detail-open]")) closePop(false); });
    document.addEventListener("keydown", (e) => { if (e.key === "Escape" && !pop.hidden) { e.preventDefault(); closePop(true); } });

    if ("IntersectionObserver" in window) new IntersectionObserver(([e]) => { visible = e.isIntersecting; start(); }, { threshold: 0.2 }).observe(root);
    document.addEventListener("visibilitychange", start);
    reduce.addEventListener("change", () => { if (isStatic()) { stop(); elapsed = V.T.final_s; renderAt(elapsed); } });

    // New live data. Cards and wires are only ever appended, so each keeps
    // its element and place; a card whose text changed (a merged step)
    // redraws. The clock moves on to the new work unless the viewer paused.
    function update(T) {
      const first = !V.CARDS.length;
      const lanesChanged = JSON.stringify(T.lanes) !== JSON.stringify(V.T.lanes);
      const oldest = first ? null : V.CARDS[0].seq[0];
      V = prepare(T); P = V.P;
      // Trimmed history renumbers every card and moves every place, so a
      // free camera would rest where no work is: follow the newest again.
      if (!first && (!V.CARDS.length || V.CARDS[0].seq[0] !== oldest)) {
        for (const p of [pool, wirePool, labelPool, chipPool, markPool, chainPool]) { for (const el of p.values()) el.remove(); p.clear(); }
        laneScrolled = false; cam.mode = "flow"; cam.fs = 1; syncFollow();
      }
      for (const [key, el] of pool) {
        const c = V.byId[key.split(":")[1]];
        if (!c || el.dataset.sig !== sig(c)) { el.remove(); pool.delete(key); }
      }
      if (lanesChanged) lanes();
      if (opener && !opener.isConnected) closePop(false);
      L = null; layout();
      // Until the viewer scrolls the lanes, the view keeps to the newest work.
      if (V.live && cam.mode === "flow" && !laneScrolled) laneY = V.openingY(L);
      if (isStatic() || first) elapsed = V.LOOP;
      else if (playing) elapsed = Math.max(elapsed, V.LOOP - CATCH_UP_S);
      renderAt(elapsed);
      start();
    }

    layout();
    elapsed = isStatic() || V.live ? V.T.final_s : 0;
    renderAt(elapsed);
    sync(); syncFollow(); start();
    return {
      element: root, get timing() { return V.T; }, renderAt, frameState: (t) => V.frameState(t),
      layout: () => L, camera: () => ({ ...cam }), relayout, frameFor: (i) => V.frameFor(L, i), update,
      // For films: keep the flow at a given scale (1 is the default view).
      flowScale: (s) => { cam.mode = "flow"; cam.fs = s; syncFollow(); },
      clock: () => ({ elapsed, playing, held, visible, running: !!raf, resumeScheduled: !!idle }),
    };
  }

  return { prepare, mount, VISIBLE };
})();
