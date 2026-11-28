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
  const TASK_CHARS = 32;
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

  // What a timed row of a local session draws as, or null.
  function kindOf(e) {
    if (e.at_ms == null || !e.session_id) return null;
    if (e.type === "write_refused") return "held";
    if (e.type === "source_observed" && e.origin === "read") return "read";
    if (e.type === "operation_observed" && e.result === "succeeded") return EDIT_TOOLS.has(e.tool) ? "edit" : "command";
    return null;
  }

  function laneOf(row, session, index, shared) {
    const name = row && row.task ? String(row.task).slice(0, TASK_CHARS) : "Agent " + (index < 26 ? String.fromCharCode(65 + index) : index + 1);
    const host = (row && row.host) || String(session).split(".")[0];
    return {
      label: "Agent · " + name,
      short: name.split(" ").slice(0, 2).join(" "),
      agent: HOSTS[host] || host,
      checkout: shared ? "shared checkout" : "own checkout",
      branch: row && row.branch_ref ? String(row.branch_ref).replace(/^refs\/heads\//, "") : "",
    };
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
    return {
      lanes: sessions.map((s, i) => laneOf(rows.get(s), s, i, shared(i))),
      cards,
      wires,
      status: { agents: sessions.length, held: runs.filter((r) => r.kind === "held").length },
      untimed: events.filter((e) => e.at_ms == null && e.type !== "clock_anchored"),
    };
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

  return { model, readFeed };
})();
