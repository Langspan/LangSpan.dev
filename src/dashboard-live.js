// The Live view's model (issue #491, ADR 0013): pure functions from
// `/api/state` and `/api/events` version 2 rows to the lanes, cards and wires
// the homepage hero's renderer draws. No DOM, clock or network. Every card is
// one recorded step (or a run of them, merged only for legibility); every
// wire is one held write. Paths and task lines are data, never interpreted.
"use strict";
var LangSpanLive = (function () {
  // Every adapter's file-changing tools; tests/live_view.rs checks this
  // against the adapters' own lists.
  const EDIT_TOOLS = new Set([
    "Edit", "Write", "MultiEdit", "NotebookEdit", // Claude Code
    "apply_patch", // Codex
    "Delete", // Cursor, with Write
    "edit_file", "write_to_file", "replace_file_content", "multi_replace_file_content", // Antigravity
  ]);
  // The hero's `group_edits_within_s` and `idle_after_s`.
  const MERGE_MS = 20 * 1000;
  const IDLE_MS = 30 * 1000;
  const HOSTS = {
    "claude-code": "Claude Code",
    codex: "Codex",
    cursor: "Cursor",
    antigravity: "Antigravity",
    "gemini-cli": "Gemini CLI",
  };
  const WHY = {
    stale_base: "changed since this agent read it",
    unread_lines: "lines another agent wrote, not read yet",
    conflict: "another agent is editing it",
    commit_held: "the commit would take another agent's changes",
  };

  const localTime = (ms) => new Date(ms).toTimeString().slice(0, 8);

  // A daemon time as wall-clock text, from the newest clock anchor; null
  // without one.
  function clock(events, format) {
    const anchor = events.filter((e) => e.type === "clock_anchored").pop();
    return (at) => (anchor && at != null ? format(anchor.wall_ms + (at - anchor.clock_ms)) : null);
  }

  // What a timed row of a local session draws as, or null. A read or edit
  // left without paths (the feed drops those Git ignores) draws nothing.
  function kindOf(e) {
    if (e.at_ms == null || !e.session_id) return null;
    if (e.type === "write_refused") return "held";
    if (e.type === "source_observed" && e.origin === "read") return e.paths.length ? "read" : null;
    if (e.type === "operation_observed" && e.result === "succeeded") {
      if (!EDIT_TOOLS.has(e.tool)) return "command";
      return e.paths.length ? "edit" : null;
    }
    return null;
  }

  // A lane is named by its branch, or else its workspace, with the harness
  // beneath: from the session's state row, or, for a session that has
  // ended, its events' workspace and that workspace's branch. Never by the
  // session's task line: that is the agent's prompt, untrusted text.
  function laneOf(row, session, workspace, branches, shared) {
    const host = (row && row.host) || String(session).split(".")[0];
    const agent = HOSTS[host] || host;
    const ref = (row && row.branch_ref) || branches.get(workspace);
    const branch = ref ? String(ref).replace(/^refs\/heads\//, "") : "";
    const name = branch || workspace || agent + " session";
    return {
      label: name,
      short: name.split("/").pop(),
      meta: agent,
      agent,
      checkout: shared ? "shared checkout" : "own checkout",
      branch,
      workspace: workspace || "",
    };
  }

  // Sessions working at `now`: with a recorded tool step in the WORKING_MS
  // up to it. A lease carries no time, so it counts at the time of the
  // newest timed row before it in the log.
  const WORKING_MS = 10 * 60 * 1000;
  function activity(events) {
    const out = [];
    let at = null;
    for (const e of events) {
      if (e.at_ms != null) at = e.at_ms;
      if (!e.session_id) continue;
      if (kindOf(e) || (e.type === "operation_observed" && e.at_ms != null)) out.push([e.at_ms, e.session_id]); // any result
      else if (e.type === "lease_granted" && at != null) out.push([at, e.session_id]);
    }
    return (now) => new Set(out.filter(([ms]) => ms > now - WORKING_MS && ms <= now).map(([, s]) => s));
  }

  // The hero's card for one merged run of steps.
  function draw(c, id, time) {
    const first = c.steps[0];
    const n = c.steps.length;
    const path = first.paths[0];
    const all = first.paths.join(", ");
    const [t0, t1] = [time(first.at_ms), time(c.steps[n - 1].at_ms)];
    const span = n > 1 && t0 !== null ? t0 + "–" + t1 : t0;
    const out = { id, lane: c.lane, kind: c.kind, seq: c.steps.map((e) => e.seq), time: t0 };
    const rows = [];
    const set = (key, value) => value !== undefined && (out[key] = value);
    if (c.kind === "edit") {
      Object.assign(out, { verb: "Edited", code: path });
      const sub = [n > 1 && n + " edits", first.paths.length > 1 && first.paths.length + " files"].filter(Boolean);
      set("sub", sub.length ? sub.join(", ") : undefined);
      rows.push(["File", all]);
      if (span !== null) rows.push(["Time", n > 1 ? span + ", " + n + " edits" : span]);
    } else if (c.kind === "read") {
      const files = [...new Set(c.steps.flatMap((e) => e.paths))];
      Object.assign(out, { verb: "Read", code: files[0] });
      set("sub", files.length > 1 ? files.length + " files" : undefined);
      rows.push(["File", files.join(", ")]);
      if (span !== null) rows.push(["Time", span]);
    } else if (c.kind === "command") {
      out.verb = "Ran a command";
      const files = first.paths.length;
      set("sub", files ? "changed " + files + (files === 1 ? " file" : " files") : undefined);
      if (files) rows.push(["Changed", first.paths.join(", ")]);
      if (t0 !== null) rows.push(["Time", t0]);
    } else if (c.kind === "held") {
      const why = WHY[first.refusal] || "held by LangSpan";
      Object.assign(out, { verb: "Write held", code: path, sub: why, who: "LangSpan" });
      rows.push(["File", all]);
      if (t0 !== null) rows.push(["Time", t0]);
      rows.push(["Why", why]);
    } else {
      Object.assign(out, { verb: c.reread ? "Saved after re-reading" : "Saved on retry", code: path });
      rows.push(["File", all]);
      if (t0 !== null) rows.push(["Time", t0]);
      if (c.reread) rows.push(["Started from", "the current file"]);
    }
    out.title = c.kind === "read" && c.again ? "Read again" : c.kind === "held" ? "Write held" : out.verb;
    out.rows = rows;
    out.next = null;
    return out;
  }

  // `{lanes, cards, wires, status, untimed}` in the hero's shape, without
  // its playback fields. `opts.time` formats a wall time (default: local).
  function model(state, events, opts = {}) {
    const time = clock(events, opts.time || localTime);
    const rows = new Map(
      ((state && state.agents) || []).filter((a) => a.local && a.session_id).map((a) => [a.session_id, a]),
    );
    const lane = new Map();
    const sessions = [];
    const work = [];
    const runs = [];
    const last = [];
    const open = []; // per lane: held path -> its run, until saved
    for (const e of events) {
      let kind = kindOf(e);
      if (!kind) continue;
      if (!lane.has(e.session_id)) {
        lane.set(e.session_id, sessions.length);
        sessions.push(e.session_id);
        work.push(e.workspace_id || (rows.get(e.session_id) || {}).workspace_id);
        open.push(new Map());
      }
      const l = lane.get(e.session_id);
      const files = e.paths.join("\n");
      const prev = last[l];
      let again = false;
      if (kind === "read") {
        for (const p of e.paths) {
          if (!open[l].has(p)) continue;
          open[l].get(p).reread = true;
          again = true;
        }
      }
      // An edit saves every hold on any of its files; it started from the
      // current files only if each of those holds was re-read.
      const holds = kind === "edit" ? e.paths.filter((p) => open[l].has(p)).map((p) => open[l].get(p)) : [];
      if (holds.length) {
        for (const p of e.paths) open[l].delete(p);
        kind = "saved";
      }
      const near = prev && prev.kind === kind && e.at_ms - prev.steps[prev.steps.length - 1].at_ms <= MERGE_MS;
      if (near && (kind === "read" || (kind === "edit" && prev.steps[0].paths.join("\n") === files))) {
        prev.steps.push(e);
        prev.again = prev.again || again;
        continue;
      }
      const run = { lane: l, kind, steps: [e], again, reread: holds.length > 0 && holds.every((h) => h.reread) };
      if (kind === "held") for (const p of e.paths) open[l].set(p, run);
      runs.push(run);
      last[l] = run;
    }
    const cards = runs.map((r, i) => draw(r, "c" + i, time));
    for (let i = 0; i < runs.length; i++) {
      const j = runs.findIndex((r, k) => k > i && r.lane === runs[i].lane);
      if (j < 0) continue;
      cards[i].next = cards[j].id;
      const gap = runs[j].steps[0].at_ms - runs[i].steps[runs[i].steps.length - 1].at_ms;
      if (gap > IDLE_MS) cards[i].idle_after = true;
    }
    const wires = [];
    runs.forEach((r, i) => {
      const e = r.steps[0];
      if (r.kind !== "held" || !lane.has(e.peer_session_id)) return;
      const peer = lane.get(e.peer_session_id);
      const changed = (x) => x.lane === peer && ["edit", "saved", "command"].includes(x.kind) && x.steps.some((s) => s.seq < e.seq && s.paths.some((p) => e.paths.includes(p)));
      const from = runs.findLastIndex(changed);
      wires.push({ kind: "held", label: "Held", from: from < 0 ? null : cards[from].id, from_lane: peer, to: cards[i].id, seq: [e.seq] });
    });
    const shared = (i) => work[i] !== undefined && work.filter((w) => w === work[i]).length > 1;
    const branches = new Map(((state && state.workspaces) || []).map((w) => [w.workspace_id, w.branch_ref]));
    const newest = runs.length ? Math.max(...runs.map((r) => r.steps[r.steps.length - 1].at_ms)) : 0;
    // Sessions on one branch are told apart by number, in lane order.
    const lanes = sessions.map((s, i) => laneOf(rows.get(s), s, work[i], branches, shared(i)));
    const seen = new Map();
    for (const l of lanes) {
      const n = (seen.get(l.label) || 0) + 1;
      seen.set(l.label, n);
      if (n > 1) Object.assign(l, { label: l.label + " (" + n + ")", short: l.short + " (" + n + ")" });
    }
    return {
      lanes,
      cards,
      wires,
      status: { agents: activity(events)(newest).size, held: runs.filter((r) => r.kind === "held").length },
      // With its lane, when its session has one, for that lane's details.
      untimed: events
        .filter((e) => e.at_ms == null && e.type !== "clock_anchored")
        .map((e) => (lane.has(e.session_id) ? { ...e, lane: lane.get(e.session_id) } : e)),
    };
  }

  // The hero's pacing, an equal copy of the site's
  // marketing/motion/hero-loop/pacing.json (tests/live-view-model.test.mjs
  // checks it against fixtures/live-view-hero.json).
  const PACING = {"about":"Numbers for the hero's motion. build-hero-timeline.py turns these and the recorded slice into timing.json.","lead_in_s":0.4,"gap_min_s":0.35,"gap_max_s":0.45,"gap_cap_s":4,"group_edits_within_s":20,"tail_s":0.6,"stop_held_s":3.2,"stop_saved_s":1.0,"stop_message_s":1.0,"wire_delay_s":0.8,"wire_draw_ms":600,"emphasis_gap_s":0.05,"emphasis_ms":150,"emphasis_scale":1.03,"message_delay_s":0.1,"message_draw_ms":400,"tint_delay_s":0.2,"tint_ms":300,"entrance_ms":200,"entrance_travel_px":8,"entrance_curve":[0.16,1,0.3,1],"camera":{"newest_at":0.72,"ease_ms":500,"ease_curve":[0.16,1,0.3,1],"px_per_flow_s":{"wide":72,"narrow":46,"film":58}},"zoom":{"min":0.25,"max":2,"step":1.25,"compact_below":0.7,"blocks_below":0.45},"tolerance_ms":50,"seek_samples_s":[1,8,15.5,20],"stills_s":[1,4,4.8,6,7.5,10],"banned":["springs with overshoot","typing effects","camera zoom or drift in the untouched loop beyond the lateral flow and the held-write framing","particles or confetti","glow","gradients","auto-opening panels","narration text"],"static_layout":{"about":"The wide layout's sizes, for the still shown without JavaScript; equal to the wide values in assets/product-first.css.","W":136,"H":80,"HH":108,"row":154,"top":56,"G":200,"gap":12,"K":72,"Wc":1358},"framing":{"about":"While a held write's wire draws, the camera frames both cards: it eases out over ease_ms, holds, and eases back over ease_ms before the stop ends. It never scales cards below min_text_px; if both cards can't fit, the wire starts from the nearest visible card in the source lane with an 'earlier' marker, or (narrow) from a source chip beside the held card.","enter_after_s":0.2,"ease_ms":600,"ease_curve":[0.16,1,0.3,1],"margin_px":48,"min_text_px":{"wide":10,"narrow":12},"hold_after_emphasis_s":1.0},"connector":{"about":"Each lane's chain of work: 2 px in the lane color from each card to that agent's next card, with 6 px nodes. A gap in the log longer than idle_after_s is drawn dashed and lighter.","width_px":2,"node_px":6,"idle_after_s":30},"wire_fade":{"about":"After its stop ends, a wire fades to this opacity, so only the current moment's wire is at full strength.","to":0.25,"ms":400}};
  // The status line counts holds this recent.
  const HELD_WINDOW_MS = 10 * 60 * 1000;
  const round = (s) => Math.round(s * 1000) / 1000;

  // The model with the playback fields the renderer reads, in the hero's
  // TIMING shape. Each card plays a step after the one before it, a little
  // longer for a longer recorded gap and never more than gap_max_s, and
  // the flow stops for each hold and save. Every value depends only on the
  // events up to its card, so appending events never moves a card.
  // `opts.now_ms`, the daemon clock now, counts the last holds up to now.
  function pace(m, events, opts = {}) {
    const P = PACING;
    const at = new Map(events.filter((e) => e.at_ms != null).map((e) => [e.seq, e.at_ms]));
    const working = activity(events);
    // Lanes are in the order their sessions' first steps appear, as in model.
    const laneOf = new Map();
    for (const e of events) if (kindOf(e) && !laneOf.has(e.session_id)) laneOf.set(e.session_id, laneOf.size);
    const stops = [];
    const status = [];
    const holds = [];
    const recent = (now) => holds.filter((ms) => ms > now - HELD_WINDOW_MS && ms <= now).length;
    let prev = null;
    let stopEnd = 0;
    const cards = m.cards.map((c) => {
      const first = at.get(c.seq[0]);
      let t = P.lead_in_s;
      if (prev) {
        const idle = Math.max(0, Math.min(1, (first - prev.end) / (P.connector.idle_after_s * 1000)));
        t = round(Math.max(prev.at, stopEnd) + P.gap_min_s + (P.gap_max_s - P.gap_min_s) * idle);
      }
      const stop = { held: P.stop_held_s, saved: P.stop_saved_s }[c.kind];
      if (stop) {
        stopEnd = round(t + stop);
        stops.push([t, stopEnd, c.kind]);
      }
      if (c.kind === "held") holds.push(first);
      const last = status[status.length - 1];
      const [agents, held] = [working(first).size, recent(first)];
      if (!last || last.agents !== agents || last.held !== held) status.push({ at: t, agents, held });
      prev = { at: t, end: at.get(c.seq[c.seq.length - 1]) };
      return { ...c, at: t };
    });
    const now = opts.now_ms != null ? opts.now_ms : prev ? prev.end : 0;
    const busy = working(now);
    if (prev && opts.now_ms != null) {
      const last = status[status.length - 1];
      if (last.agents !== busy.size || last.held !== recent(now)) status.push({ at: prev.at, agents: busy.size, held: recent(now) });
    }
    const byId = new Map(cards.map((c) => [c.id, c]));
    const wires = m.wires.map((w) => {
      const a = byId.get(w.to).at;
      const wat = round(a + P.wire_delay_s);
      return { ...w, at: wat, ms: P.wire_draw_ms, stop: [a, round(a + P.stop_held_s)], emphasis_at: round(wat + P.wire_draw_ms / 1000 + P.emphasis_gap_s) };
    });
    // The clock rests at the end of the last stop, so the newest wire is
    // still at full strength.
    const end = prev ? round(Math.max(prev.at + P.tail_s, stopEnd)) : 0;
    return {
      name: "live", live: true, fps: 30, duration_s: end, frames: Math.round(end * 30), final_s: end,
      pacing: P, lanes: m.lanes, stops_s: stops, status, wires, cards, untimed: m.untimed,
      // The lanes of the sessions working now, for the title bar.
      working: [...busy].filter((s) => laneOf.has(s)).map((s) => laneOf.get(s)).sort((a, b) => a - b),
    };
  }

  // The newest `keep` rows, and the newest clock anchor if it is older.
  function trim(events, keep) {
    if (events.length <= keep) return events;
    const anchor = events.filter((e) => e.type === "clock_anchored").pop();
    const out = events.slice(-keep);
    return anchor && !out.includes(anchor) ? [anchor, ...out] : out;
  }

  // Every row after `after`, page by page until one comes back empty. A page
  // can hold fewer rows than asked; at most `pages` pages are read, and a
  // page that doesn't advance ends the read (`complete: false`).
  async function readFeed(fetchPage, after, pages) {
    const events = [];
    for (let n = 0; n < pages; n++) {
      const page = await fetchPage(after);
      if (!page.events.length) return { events, next: after, complete: true };
      if (page.next <= after) break;
      events.push(...page.events);
      after = page.next;
    }
    return { events, next: after, complete: false };
  }

  return { model, pace, readFeed, trim, PACING };
})();
