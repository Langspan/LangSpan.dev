// The Live view's story model (issue #491, ADR 0013): pure functions over
// `/api/events` version 2 rows. No DOM, clock or network; the page draws
// what `story` returns. Path text is data and is never interpreted here.
"use strict";
var LangSpanLive = (function () {
  const EDIT_TOOLS = new Set(["Edit", "Write", "MultiEdit", "NotebookEdit"]);
  // Other steps this near the hold, either side, are the scene around it.
  const WINDOW_MS = 60 * 1000;
  // Cards per agent, the folded card included.
  const MAX_CARDS = 5;
  const HOSTS = {
    "claude-code": "Claude Code",
    codex: "Codex",
    cursor: "Cursor",
    antigravity: "Antigravity",
    "gemini-cli": "Gemini CLI",
  };
  const CAPTIONS = {
    stale_base: "{B} tried to edit {file} after {A} changed it.",
    unread_lines: "{B} tried to edit lines of {file} it hadn't read.",
    conflict: "{B} tried to edit {file} while {A} held it.",
  };
  const NEXT = { stale_base: "re-read it and saved.", unread_lines: "read them and saved.", conflict: "re-read it and saved." };

  const base = (path) => String(path).split("/").pop();

  // Each session's parent, from its registration.
  function parents(events) {
    const of = new Map();
    for (const e of events) if (e.type === "session_registered" && e.parent) of.set(e.session_id, e.parent);
    return of;
  }

  // The agent a session belongs to: a subagent's work counts as its parent's.
  function root(of, id) {
    const seen = new Set();
    while (of.has(id) && !seen.has(id)) {
      seen.add(id);
      id = of.get(id);
    }
    return id;
  }

  // Wall time of a daemon time, from the newest clock anchor; null without one.
  function wall(events) {
    const anchor = events.filter((e) => e.type === "clock_anchored").pop();
    return (at) => (anchor && at != null ? anchor.wall_ms + (at - anchor.clock_ms) : null);
  }

  const saves = (e, path, agent, of) =>
    e.type === "operation_observed" &&
    EDIT_TOOLS.has(e.tool) &&
    e.result === "succeeded" &&
    e.paths.includes(path) &&
    root(of, e.session_id) === agent;

  // Holds a story can tell, oldest first: a refusal naming a peer of another
  // agent, followed by the held agent's successful edit of the held path.
  function candidates(events) {
    const of = parents(events);
    const out = [];
    for (const e of events) {
      if (e.type !== "write_refused" || !e.peer_session_id || !e.paths.length) continue;
      const b = root(of, e.session_id);
      if (root(of, e.peer_session_id) === b) continue;
      const save = events.find((x) => x.seq > e.seq && saves(x, e.paths[0], b, of));
      if (save) out.push({ seq: e.seq, save: save.seq });
    }
    return out;
  }

  // The newest candidate, or null.
  function best(events) {
    return candidates(events).pop() || null;
  }

  function step(agent, e, kind, path) {
    return { agent, at_ms: e.at_ms, kind, path, seqs: [e.seq] };
  }

  // The other steps of one agent around the hold, merged by kind and file.
  function others(events, of, agent, keys, lo, hi) {
    const groups = new Map();
    for (const e of events) {
      if (e.at_ms == null || e.at_ms < lo || e.at_ms > hi || keys.has(e.seq)) continue;
      if (root(of, e.session_id) !== agent) continue;
      let kind = null;
      if (e.type === "source_observed" && e.origin === "read") kind = "read";
      else if (e.type === "operation_observed" && e.result === "succeeded") kind = EDIT_TOOLS.has(e.tool) ? "edit" : "command";
      if (!kind) continue;
      const paths = kind === "command" ? [null] : e.paths;
      for (const path of paths) {
        const key = kind === "command" ? "command " + e.seq : kind + " " + path;
        const group = groups.get(key) || { agent, kind, path, steps: [] };
        group.steps.push(e);
        groups.set(key, group);
      }
    }
    return [...groups.values()];
  }

  function fold(agent, rest) {
    const n = { read: 0, edit: 0, command: 0 };
    for (const g of rest) n[g.kind] += 1;
    const plural = (k, one, word) => `${one} ${n[k]} ${word}${n[k] === 1 ? "" : "s"}`;
    const parts = [];
    if (n.read) parts.push(plural("read", "read", "file"));
    if (n.edit) parts.push(plural("edit", "edited", "file"));
    if (n.command) parts.push(plural("command", "ran", "command"));
    const text = parts.length === 1 ? parts[0] : parts.slice(0, -1).join(", ") + " and " + parts.at(-1);
    const steps = rest.flatMap((g) => g.steps);
    return {
      agent,
      at_ms: Math.min(...steps.map((e) => e.at_ms)),
      kind: "summary",
      verb: text[0].toUpperCase() + text.slice(1),
      path: null,
      seqs: [...new Set(steps.map((e) => e.seq))],
    };
  }

  // The scene for the hold at `seq`: two agents, their cards in time order,
  // exactly one connector, a caption, and the untimed rows for the
  // inspector. Null when `seq` is not a story `candidates` finds.
  function story(events, seq) {
    const found = candidates(events).find((c) => c.seq === seq);
    if (!found) return null;
    const of = parents(events);
    const hold = events.find((e) => e.seq === seq);
    const save = events.find((e) => e.seq === found.save);
    const [a, b, path] = [root(of, hold.peer_session_id), root(of, hold.session_id), hold.paths[0]];
    const changed = events.filter((e) => e.seq < seq && saves(e, path, a, of)).pop();
    const reread = events.find(
      (e) => e.seq > seq && e.seq < save.seq && e.type === "source_observed" && e.origin === "read" &&
        e.paths.includes(path) && root(of, e.session_id) === b,
    );
    const key = [];
    if (changed) key.push(step("A", changed, "changed", path));
    key.push(step("B", hold, "held", path));
    if (reread) key.push(step("B", reread, "read", path));
    key.push(step("B", save, "saved", path));
    const keys = new Set(key.map((c) => c.seqs[0]));
    const lo = hold.at_ms - WINDOW_MS;
    const hi = Math.max(hold.at_ms + WINDOW_MS, save.at_ms);
    const cards = [];
    for (const [label, agent] of [["A", a], ["B", b]]) {
      const mine = key.filter((c) => c.agent === label);
      const groups = others(events, of, agent, keys, lo, hi);
      const distance = (g) => Math.min(...g.steps.map((e) => Math.abs(e.at_ms - hold.at_ms)));
      groups.sort((x, y) => distance(x) - distance(y));
      const free = MAX_CARDS - mine.length;
      const room = groups.length > free ? Math.max(0, free - 1) : free;
      cards.push(...mine);
      for (const g of groups.slice(0, room)) {
        cards.push({
          agent: label,
          at_ms: Math.min(...g.steps.map((e) => e.at_ms)),
          kind: g.kind,
          path: g.path,
          seqs: g.steps.map((e) => e.seq),
        });
      }
      if (groups.length > room) cards.push(fold(label, groups.slice(room)));
    }
    cards.sort((x, y) => x.at_ms - y.at_ms || x.seqs[0] - y.seqs[0]);
    for (const c of cards) {
      c.file = c.path == null ? null : base(c.path);
      c.tone = c.kind === "held" ? "red" : c.kind === "saved" ? "green" : "neutral";
    }
    const index = (kind) => cards.findIndex((c) => c.kind === kind);
    const ends = [a, b].map((id) => [id, a === id ? "A" : "B"]);
    const caption =
      (CAPTIONS[hold.refusal] || CAPTIONS.stale_base) +
      " LangSpan held it. {B} " +
      (reread ? NEXT[hold.refusal] || NEXT.stale_base : "saved on retry.");
    return {
      seq,
      at: wall(events)(hold.at_ms),
      agents: ends.map(([id, label]) => ({ key: label, session: id, host: HOSTS[id.split(".")[0]] || id.split(".")[0] })),
      cards,
      connector: { from: changed ? index("changed") : null, to: index("held"), tone: "red" },
      caption: caption
        .replaceAll("{A}", "Agent A")
        .replaceAll("{B}", "Agent B")
        .replace("{file}", "`" + base(path) + "`"),
      // Rows without a time that name either agent or the held file.
      untimed: events.filter(
        (e) =>
          e.at_ms == null &&
          ((e.session_id && [a, b].includes(root(of, e.session_id))) || e.paths.includes(path)),
      ),
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

  return { root, parents, wall, candidates, best, story, readFeed };
})();
