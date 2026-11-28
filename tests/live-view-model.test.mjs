// The Live view's model (issue #491), run by tests/live_view.rs. It loads
// src/dashboard-live.js as the page will, a plain script, and checks the
// lanes, cards and wires it builds from /api/state and /api/events rows.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, writeFileSync } from "node:fs";
import vm from "node:vm";

const source = readFileSync(new URL("../src/dashboard-live.js", import.meta.url), "utf8");
// In this realm, so its arrays compare equal to the tests' own.
vm.runInThisContext(source);
const Live = globalThis.LangSpanLive;

// Times in UTC, so the tests don't depend on the machine's zone.
const utc = (ms) => new Date(ms).toISOString().slice(11, 19);
const model = (state, events) => Live.model(state, events, { time: utc });

const A = "claude-code.alice";
const B = "codex.bob";
const F = "src/billing/invoices.ts";

let seq = 0;
const row = (fields) => ({ seq: ++seq, paths: [], more_paths: 0, ...fields });
const anchor = () => row({ type: "clock_anchored", wall_ms: 1_800_000_000_000, clock_ms: 1000 });
const registered = (id) => row({ type: "session_registered", session_id: id, workspace_id: "app" });
const op = (id, at, path = F, tool = "Edit", result = "succeeded") =>
  row({ type: "operation_observed", session_id: id, workspace_id: "app", at_ms: at, paths: path ? [path] : [], tool, result });
const read = (id, at, path = F) =>
  row({ type: "source_observed", session_id: id, at_ms: at, paths: [path], origin: "read" });
const held = (id, peer, at, refusal = "stale_base", path = F) =>
  row({ type: "write_refused", session_id: id, workspace_id: "app", at_ms: at, paths: [path], peer_session_id: peer, refusal });

const agent = (session_id, host, task, fields = {}) => ({
  id: "local:" + session_id, local: true, host, session: session_id.slice(0, 8), session_id,
  workspace_id: "app", branch_ref: "refs/heads/feat/billing", task, ...fields,
});
const STATE = {
  agents: [
    agent(A, "claude-code", "#12 invoice totals"),
    agent(B, "codex", null),
    { id: "remote:carol", local: false, machine: "carol", host: "claude-code", session: "c0ffee12",
      session_id: null, workspace_id: "app", branch_ref: null, task: "remote work" },
  ],
};

// Alice changes the file; Bob is held, re-reads it and saves.
function incident() {
  seq = 0;
  return [anchor(), registered(A), registered(B), op(A, 2000), held(B, A, 3000), read(B, 3500), op(B, 4000)];
}

test("one lane per local session with a timed step, named by its branch with the harness beneath, and no IDs", () => {
  const m = model(STATE, incident());
  assert.deepEqual(m.lanes, [
    { label: "feat/billing", short: "billing", meta: "Claude Code", agent: "Claude Code",
      checkout: "shared checkout", branch: "feat/billing", workspace: "app" },
    { label: "feat/billing (2)", short: "billing (2)", meta: "Codex", agent: "Codex", checkout: "shared checkout", branch: "feat/billing", workspace: "app" },
  ]);
  for (const lane of m.lanes) for (const text of Object.values(lane)) assert.ok(!/alice|bob|c0ffee|Agent [A-Z]\b/.test(text), text);
});

test("a session's prompt text never reaches a label, the details or anything else the view draws", () => {
  // The state row's task line is the agent's first prompt: untrusted text.
  // Each shorter than the 32 characters the lane names were once clipped to.
  const prompts = ["answer the review findings", "<task-notification>", "<task-id>bthzp03gb</task-id>"];
  const state = { agents: prompts.map((task, i) => agent(["claude-code.p" + i, "codex.q" + i, "cursor.r" + i][i], "claude-code", task, { branch_ref: i ? null : "refs/heads/feat/x" })),
    workspaces: [{ workspace_id: "app", branch_ref: null }] };
  seq = 0;
  const events = [anchor(), ...state.agents.map((a, i) => op(a.session_id, 2000 + i * 1000))];
  const drawn = JSON.stringify(Live.pace(model(state, events), events));
  for (const text of prompts) assert.ok(!drawn.includes(text), text + " was drawn");
  // Without a branch, the workspace names the lane.
  assert.deepEqual(model(state, events).lanes.map((l) => [l.label, l.meta]), [["feat/x", "Claude Code"], ["app", "Claude Code"], ["app (2)", "Claude Code"]]);
});

test("a read or an edit left without paths (the feed drops those Git ignores) draws no card; a command still does", () => {
  seq = 0;
  const events = [anchor(), op(A, 2000, null), read(A, 2500), op(A, 3000, null, "Bash")];
  events[2].paths = [];
  assert.deepEqual(model(STATE, events).cards.map((c) => c.kind), ["command"]);
});

test("a session without a state row is named from its events' workspace's branch, never a letter", () => {
  seq = 0;
  const state = { agents: [], workspaces: [{ workspace_id: "lsd-a11", branch_ref: "refs/heads/feat/x" }] };
  const events = [anchor(), op("codex.gone", 2000, F), Object.assign(op("claude-code.bare", 3000, "b.ts"), { workspace_id: undefined })];
  events[1].workspace_id = "lsd-a11";
  assert.deepEqual(model(state, events).lanes.map((l) => [l.label, l.meta, l.workspace]), [
    ["feat/x", "Codex", "lsd-a11"], ["Claude Code session", "Claude Code", ""],
  ]);
});

test("agents working counts sessions with a tool step in the last 10 minutes; a lease counts at the row before it; a registration never", () => {
  seq = 0;
  const lease = (id) => row({ type: "lease_granted", session_id: id, workspace_id: "app", paths: [F] });
  const events = [anchor(), registered("codex.carol"), op(A, 2000), lease(B), op(A, 700_000, "b.ts"), lease(B)];
  const p = Live.pace(model(STATE, events), events, { now_ms: 700_000 });
  // At 700 s, A's edit and B's lease (placed at 700 s) are in the window; carol only registered.
  assert.deepEqual([p.status.at(-1).agents, p.working], [2, [0]]);
  assert.equal(model(STATE, events).status.agents, 2);
  const idle = Live.pace(model(STATE, events), events, { now_ms: 700_000 + 600_000 });
  assert.deepEqual([idle.status.at(-1).agents, idle.working], [0, []]);
});

test("trim keeps the newest rows and the newest clock anchor before them", () => {
  const rows = [{ seq: 1, type: "clock_anchored" }, { seq: 2 }, { seq: 3, type: "clock_anchored" }, { seq: 4 }, { seq: 5 }];
  assert.deepEqual(Live.trim(rows, 2).map((e) => e.seq), [3, 4, 5]);
  assert.deepEqual(Live.trim(rows, 3).map((e) => e.seq), [3, 4, 5]);
  assert.equal(Live.trim(rows, 9), rows);
});

test("a remote agent in the state is never a lane, and a session alone in its workspace has its own checkout", () => {
  seq = 0;
  const events = [anchor(), op(A, 2000)];
  events[1].workspace_id = "solo";
  const m = model(STATE, events);
  assert.equal(m.lanes.length, 1);
  assert.equal(m.lanes[0].checkout, "own checkout");
});

test("reads, edits, commands, holds and saves are the hero's card kinds, in words", () => {
  seq = 0;
  const events = [anchor(), op(A, 2000), op(A, 2500, null, "Bash"), held(B, A, 3000), read(B, 3500), op(B, 4000)];
  const m = model(STATE, events);
  assert.deepEqual(
    m.cards.map((c) => [c.lane, c.kind, c.verb, c.code, c.sub]),
    [
      [0, "edit", "Edited", F, undefined],
      [0, "command", "Ran a command", undefined, undefined],
      [1, "held", "Write held", F, "changed since this agent read it"],
      [1, "read", "Read", F, undefined],
      [1, "saved", "Saved after re-reading", F, undefined],
    ],
  );
  assert.equal(m.cards[2].who, "LangSpan");
  assert.equal(m.cards[3].title, "Read again");
  assert.deepEqual(m.status, { agents: 2, held: 1 });
});

test("a failed step is not a card, and a save without a read between is a retry", () => {
  seq = 0;
  const events = [anchor(), op(A, 2000), held(B, A, 3000), op(B, 3200, F, "Edit", "failed"), op(B, 4000)];
  const m = model(STATE, events);
  assert.deepEqual(m.cards.map((c) => c.verb), ["Edited", "Write held", "Saved on retry"]);
});

test("edits of one file within 20 s merge into one card; further apart, or another file, they don't", () => {
  seq = 0;
  const events = [anchor(), op(A, 2000), op(A, 12_000), op(A, 31_000), op(A, 60_000), op(A, 61_000, "src/other.ts")];
  const m = model(STATE, events);
  assert.deepEqual(m.cards.map((c) => [c.code, c.sub, c.seq]), [
    [F, "3 edits", [2, 3, 4]],
    [F, undefined, [5]],
    ["src/other.ts", undefined, [6]],
  ]);
  assert.deepEqual(m.cards[0].rows, [["File", F], ["Time", "08:00:01–08:00:30, 3 edits"]]);
});

test("reads in a row within 20 s are one card with a neutral count", () => {
  seq = 0;
  const events = [anchor(), read(A, 2000, "a.ts"), read(A, 3000, "b.ts"), read(A, 4000, "a.ts")];
  const m = model(STATE, events);
  assert.deepEqual(m.cards.map((c) => [c.verb, c.code, c.sub]), [["Read", "a.ts", "2 files"]]);
});

test("exactly one held wire per held write whose peer has a lane, and no other wire", () => {
  seq = 0;
  const events = [
    anchor(), op(A, 2000), held(B, A, 3000), read(B, 3500), op(B, 4000),
    held(B, null, 5000, "conflict", "src/x.ts"), // no recorded peer
    held(B, "claude-code.ghost", 6000, "stale_base", "src/y.ts"), // a peer without a lane
    op(A, 7000, null, "Bash"),
  ];
  const m = model(STATE, events);
  assert.equal(m.cards.filter((c) => c.kind === "held").length, 3);
  assert.deepEqual(m.wires, [{ kind: "held", label: "Held", from: "c0", from_lane: 0, to: "c1", seq: [3] }]);
  assert.deepEqual(m.status, { agents: 2, held: 3 });
});

test("a wire starts at the peer's newest card on the held path, or at its lane", () => {
  seq = 0;
  const events = [anchor(), op(A, 2000), op(A, 30_000), op(A, 31_000, "src/other.ts"), held(B, A, 32_000)];
  const m = model(STATE, events);
  assert.deepEqual(m.wires[0], { kind: "held", label: "Held", from: "c1", from_lane: 0, to: "c3", seq: [5] });
  seq = 0;
  const elsewhere = model(STATE, [anchor(), op(A, 2000, "src/other.ts"), held(B, A, 3000)]);
  assert.deepEqual(elsewhere.wires[0], { kind: "held", label: "Held", from: null, from_lane: 0, to: "c1", seq: [3] });
});

test("the held card says why in words, from the refusal kind", () => {
  seq = 0;
  const kinds = ["stale_base", "unread_lines", "conflict", "commit_held"];
  const events = [anchor(), ...kinds.map((k, i) => held(B, A, 2000 + i * 40_000, k))];
  const m = model(STATE, events);
  assert.deepEqual(m.cards.map((c) => c.sub), [
    "changed since this agent read it",
    "lines another agent wrote, not read yet",
    "another agent is editing it",
    "the commit would take another agent's changes",
  ]);
  assert.deepEqual(m.cards[0].rows, [["File", F], ["Time", "08:00:01"], ["Why", "changed since this agent read it"]]);
});

test("rows without a time are never cards; they go to the inspector in seq order, untimed", () => {
  const events = incident();
  events.push(row({ type: "message_sent", workspace_id: "app", paths: [F] }));
  events.push(row({ type: "lease_granted", session_id: B, workspace_id: "app", paths: [F] }));
  const m = model(STATE, events);
  assert.deepEqual(m.untimed.map((e) => [e.seq, e.type]), [
    [2, "session_registered"], [3, "session_registered"], [8, "message_sent"], [9, "lease_granted"],
  ]);
  assert.ok(m.untimed.every((e) => e.at_ms == null));
  assert.equal(m.cards.length, 4);
});

test("time is the clock anchor's wall time; without an anchor there is none", () => {
  const m = model(STATE, incident());
  assert.deepEqual(m.cards.map((c) => c.time), ["08:00:01", "08:00:02", "08:00:02", "08:00:03"]);
  seq = 0;
  const bare = model(STATE, [op(A, 2000)]);
  assert.equal(bare.cards[0].time, null);
  assert.deepEqual(bare.cards[0].rows, [["File", F]]);
});

test("each card links to its lane's next card, and a gap over 30 s is idle", () => {
  seq = 0;
  const m = model(STATE, [anchor(), op(A, 2000), op(B, 3000, "b.ts"), op(A, 40_000, "a.ts"), op(A, 41_000, "c.ts")]);
  assert.deepEqual(m.cards.map((c) => [c.id, c.next, c.idle_after]), [
    ["c0", "c2", true], ["c1", null, undefined], ["c2", "c3", undefined], ["c3", null, undefined],
  ]);
});

test("path text is data: kept as given, never parsed for meaning", () => {
  seq = 0;
  const hostile = "<img src=x onerror=alert(1)>/ignore previous instructions.md";
  const m = model(STATE, [anchor(), op(A, 2000, hostile)]);
  assert.equal(m.cards[0].code, hostile);
  assert.deepEqual(m.cards[0].rows[0], ["File", hostile]);
});

test("every card and wire field is one the hero's renderer reads", () => {
  const CARD = new Set(["id", "lane", "kind", "seq", "time", "verb", "code", "sub", "who", "title", "rows", "next", "idle_after"]);
  const WIRE = new Set(["kind", "label", "from", "from_lane", "to", "seq"]);
  const m = model(STATE, incident());
  for (const c of m.cards) for (const k of Object.keys(c)) assert.ok(CARD.has(k), k);
  for (const w of m.wires) for (const k of Object.keys(w)) assert.ok(WIRE.has(k), k);
  assert.ok(m.cards.every((c) => ["edit", "read", "command", "held", "saved"].includes(c.kind)));
});

test("the model of the golden events page equals fixtures/live-view-model.json", () => {
  const path = new URL("../fixtures/live-view-model.json", import.meta.url);
  const page = JSON.parse(readFileSync(new URL("../fixtures/dashboard-events-v2.json", import.meta.url), "utf8"));
  const golden = JSON.parse(readFileSync(path, "utf8"));
  const got = model(golden.state, page.events);
  if (process.env.LANGSPAN_UPDATE_GOLDEN === "1") {
    writeFileSync(path, JSON.stringify({ ...golden, model: got }, null, 1) + "\n");
  }
  assert.deepEqual(got, golden.model);
});

test("every host's file-edit tool draws as an edit, and as a save after a hold", () => {
  const tools = ["Edit", "Write", "MultiEdit", "NotebookEdit", "apply_patch", "Delete",
    "edit_file", "write_to_file", "replace_file_content", "multi_replace_file_content"];
  for (const tool of tools) {
    seq = 0;
    const m = model(STATE, [anchor(), op(A, 2000, F, tool), held(B, A, 3000), read(B, 3500), op(B, 4000, F, tool)]);
    assert.deepEqual(m.cards.map((c) => c.verb), ["Edited", "Write held", "Read", "Saved after re-reading"], tool);
  }
  for (const tool of ["Bash", "run_command", "Shell"]) {
    seq = 0;
    assert.equal(model(STATE, [anchor(), op(A, 2000, null, tool)]).cards[0].verb, "Ran a command", tool);
  }
});

const multi = (id, at, paths, tool = "apply_patch") => Object.assign(op(id, at, null, tool), { paths });

test("an edit of several files saves a hold on any of them, and its card names every file", () => {
  seq = 0;
  const m = model(STATE, [anchor(), held(B, A, 3000, "stale_base", "b.ts"), read(B, 3500, "b.ts"), multi(B, 4000, ["a.ts", "b.ts"])]);
  assert.deepEqual(m.cards.map((c) => [c.kind, c.verb, c.code]), [
    ["held", "Write held", "b.ts"], ["read", "Read", "b.ts"], ["saved", "Saved after re-reading", "a.ts"],
  ]);
  assert.deepEqual(m.cards[2].rows, [["File", "a.ts, b.ts"], ["Time", "08:00:03"], ["Started from", "the current file"]]);
  // The hold is saved: a later edit of b.ts is a plain edit.
  seq = 0;
  const later = model(STATE, [anchor(), held(B, A, 3000, "stale_base", "b.ts"), multi(B, 4000, ["a.ts", "b.ts"]), op(B, 60_000, "b.ts")]);
  assert.deepEqual(later.cards.map((c) => c.verb), ["Write held", "Saved on retry", "Edited"]);
});

test("one edit that saves two holds clears both, and is re-read only if both files were", () => {
  seq = 0;
  const events = [anchor(), held(B, A, 3000, "stale_base", "a.ts"), held(B, A, 3100, "stale_base", "b.ts"),
    read(B, 3500, "a.ts"), multi(B, 4000, ["a.ts", "b.ts"]), op(B, 60_000, "a.ts"), op(B, 90_000, "b.ts")];
  assert.deepEqual(model(STATE, events).cards.map((c) => c.verb),
    ["Write held", "Write held", "Read", "Saved on retry", "Edited", "Edited"]);
});

test("a hold on several files is re-read only when each file it saves was read", () => {
  seq = 0;
  const hold = () => Object.assign(held(B, A, 3000), { paths: ["a.ts", "b.ts"] });
  // a.ts was read again; the edit saves b.ts, which was not.
  const m = model(STATE, [anchor(), hold(), read(B, 3500, "a.ts"), op(B, 4000, "b.ts")]);
  assert.equal(m.cards[2].verb, "Saved on retry");
  seq = 0;
  const both = model(STATE, [anchor(), hold(), read(B, 3500, "b.ts"), op(B, 4000, "b.ts")]);
  assert.equal(both.cards[2].verb, "Saved after re-reading");
});

test("each row's time comes from its own generation's clock anchor", () => {
  seq = 0;
  // A restart re-anchors: at 1000 the clock read 08:00:00 in the first
  // generation; at 9000 it read 09:00:00 in the second.
  const events = [anchor(), op(A, 2000), row({ type: "clock_anchored", wall_ms: 1_800_003_600_000, clock_ms: 9000 }),
    op(A, 70_000, "b.ts")];
  assert.deepEqual(model(STATE, events).cards.map((c) => c.time), ["08:00:01", "09:01:01"]);
  // A row before the first anchor read has no known generation, so no time.
  seq = 0;
  const late = model(STATE, [op(A, 2000), anchor(), op(A, 70_000, "b.ts")]);
  assert.deepEqual(late.cards.map((c) => c.time), [null, "08:01:09"]);
});

test("an edit card of several files counts them, and merges only with an edit of the same files", () => {
  seq = 0;
  const events = [anchor(), multi(A, 2000, ["a.ts", "b.ts"]), multi(A, 3000, ["a.ts", "b.ts"]),
    multi(A, 4000, ["a.ts", "c.ts"]), op(A, 5000, "a.ts")];
  const m = model(STATE, events);
  assert.deepEqual(m.cards.map((c) => [c.code, c.sub, c.seq]), [
    ["a.ts", "2 edits, 2 files", [2, 3]], ["a.ts", "2 files", [4]], ["a.ts", undefined, [5]],
  ]);
  assert.deepEqual(m.cards[1].rows[0], ["File", "a.ts, c.ts"]);
});

test("a hold on several files lists them, and its wire starts at the peer's edit of any of them", () => {
  seq = 0;
  const events = [anchor(), op(A, 2000, "b.ts"), Object.assign(held(B, A, 3000), { paths: ["a.ts", "b.ts"] }),
    read(B, 3500, "b.ts"), op(B, 4000, "b.ts")];
  const m = model(STATE, events);
  assert.deepEqual(m.cards[1].rows[0], ["File", "a.ts, b.ts"]);
  assert.equal(m.cards[2].title, "Read again");
  assert.equal(m.cards[3].verb, "Saved after re-reading");
  assert.deepEqual(m.wires, [{ kind: "held", label: "Held", from: "c0", from_lane: 0, to: "c1", seq: [3] }]);
});

// pace: the playback fields the renderer reads, from the events alone.
const hero = JSON.parse(readFileSync(new URL("../fixtures/live-view-hero.json", import.meta.url), "utf8"));
const paced = (events, opts) => Live.pace(model(STATE, events), events, opts);

test("pace uses the hero's pacing numbers, unchanged", () => {
  assert.deepEqual(Live.PACING, hero.pacing);
});

test("pace is deterministic and spaces cards as the hero does, with a stop for each hold and save", () => {
  const a = paced(incident());
  assert.deepEqual(a, paced(incident()));
  const P = hero.pacing;
  // Edit at 2 s, held at 3 s, read at 3.5 s, saved at 4 s: each gap is
  // under 30 s, so it plays a little over gap_min_s.
  assert.deepEqual(a.cards.map((c) => [c.kind, c.at]), [["edit", 0.4], ["held", 0.753], ["read", 4.305], ["saved", 4.657]]);
  assert.deepEqual(a.stops_s, [[0.753, 3.953, "held"], [4.657, 5.657, "saved"]]);
  assert.deepEqual(a.wires, [{ kind: "held", label: "Held", from: "c0", from_lane: 0, to: "c1", seq: [5],
    at: 1.553, ms: P.wire_draw_ms, stop: [0.753, 3.953], emphasis_at: 2.203 }]);
  assert.equal(a.live, true);
  // The clock rests at the end of the last stop, so the newest wire is
  // still at full strength.
  assert.equal(a.duration_s, 5.657);
  assert.equal(a.final_s, a.duration_s);
  assert.deepEqual(a.lanes, model(STATE, incident()).lanes);
});

test("pace shortens any gap over 30 s to the hero's longest step", () => {
  seq = 0;
  const near = paced([anchor(), op(A, 2000, "a.ts"), op(A, 2000 + 30_000, "b.ts")]);
  seq = 0;
  const far = paced([anchor(), op(A, 2000, "a.ts"), op(A, 2000 + 3_600_000, "b.ts")]);
  assert.equal(near.cards[1].at, 0.85);
  assert.equal(far.cards[1].at, 0.85);
});

test("pace keeps every earlier card's time when events are appended", () => {
  const events = incident();
  const before = paced(events.slice(0, 5));
  const after = paced(events);
  for (const c of before.cards) assert.equal(after.cards.find((x) => x.id === c.id).at, c.at, c.id);
  for (const s of before.stops_s) assert.ok(after.stops_s.some((x) => x[0] === s[0] && x[1] === s[1]));
});

test("pace counts agents and the writes held in the 10 minutes before each card, or before now", () => {
  seq = 0;
  const events = [anchor(), op(A, 2000), held(B, A, 3000), op(A, 700_000, "b.ts"), held(B, A, 701_000, "stale_base", "b.ts")];
  const p = paced(events);
  // At 700 s only A has worked in the last 10 minutes.
  assert.deepEqual(p.status.map((s) => [s.agents, s.held]), [[1, 0], [2, 1], [1, 0], [2, 1]]);
  const later = paced(events, { now_ms: 701_000 + 600_000 });
  assert.deepEqual(later.status.at(-1), { at: later.cards.at(-1).at, agents: 0, held: 0 });
});

test("pace of no events is an empty view that rests at zero", () => {
  const p = paced([anchor()]);
  assert.deepEqual([p.cards, p.wires, p.stops_s, p.duration_s], [[], [], [], 0]);
});

test("rows without a time keep their lane, so a lane's details can list them", () => {
  const m = model(STATE, incident());
  assert.deepEqual(m.untimed.map((e) => [e.type, e.lane]), [["session_registered", 0], ["session_registered", 1]]);
});

test("readFeed reads until a page comes back empty, even after short pages", async () => {
  const pages = { 0: [1, 2], 2: [3], 3: [4, 5], 5: [] };
  const asked = [];
  const fetchPage = async (after) => {
    asked.push(after);
    const events = pages[after].map((s) => ({ seq: s }));
    return { events, next: events.length ? events.at(-1).seq : after };
  };
  const out = await Live.readFeed(fetchPage, 0, 10);
  assert.deepEqual(out, { events: [1, 2, 3, 4, 5].map((s) => ({ seq: s })), next: 5, complete: true });
  assert.deepEqual(asked, [0, 2, 3, 5]);
});

test("readFeed stops at its page bound, and on a page that doesn't advance", async () => {
  const endless = async (after) => ({ events: [{ seq: after + 1 }], next: after + 1 });
  assert.deepEqual(await Live.readFeed(endless, 0, 3), { events: [1, 2, 3].map((s) => ({ seq: s })), next: 3, complete: false });
  const stuck = async (after) => ({ events: [{ seq: after }], next: after });
  assert.deepEqual(await Live.readFeed(stuck, 7, 3), { events: [], next: 7, complete: false });
});

// A coordinator's /api/events over `log`: the rows after `after`, at most
// `limit` and at most `cap`, counting every row it serves. The store ends a
// page at 48 KiB, so a real page often holds far fewer rows than asked: 133
// of 1000 in a dogfood log's burst of branch observations.
function server(log, cap = Infinity) {
  const served = { rows: 0, pages: 0 };
  const fetchPage = async (after, limit) => {
    const start = log.findIndex((e) => e.seq > after);
    const events = start < 0 ? [] : log.slice(start, start + Math.min(limit, cap));
    served.rows += events.length;
    served.pages += 1;
    return { events, next: events.length ? events.at(-1).seq : after };
  };
  return { fetchPage, served };
}
const TAIL = { window_ms: 10 * 60 * 1000, keep: 4000, page: 1000 };
// A clock anchor at boot, then `n` edits `every` ms apart, seqs `step` apart.
function longLog(n, every, step = 1) {
  const log = [{ seq: 1, type: "clock_anchored", wall_ms: 1_800_000_000_000, clock_ms: 0, paths: [], more_paths: 0 }];
  for (let i = 0; i < n; i++) {
    log.push({ seq: 2 + i * step, type: "operation_observed", session_id: i % 2 ? A : B, workspace_id: "app",
      at_ms: 1000 + i * every, paths: ["src/f" + (i % 7) + ".rs"], more_paths: 0, tool: "Edit", result: "succeeded" });
  }
  return log;
}

test("the first read of a 50,000-event log takes only the last 10 minutes, without reading the log from its start", async () => {
  for (const step of [1, 3]) {
    const log = longLog(50_000, 1000, step);
    const { fetchPage, served } = server(log);
    const tail = await Live.readTail(fetchPage, TAIL);
    const newest = log.at(-1);
    const window = log.filter((e) => e.at_ms >= newest.at_ms - TAIL.window_ms);
    assert.equal(window.length, 601);
    assert.deepEqual(tail, { events: window, next: newest.seq }, "step " + step);
    // One page of the tail and two one-row probes per bit of the newest seq.
    assert.ok(served.rows <= TAIL.page + 2 * 18, served.rows + " rows read, step " + step);
    assert.ok(served.pages <= 1 + 2 * 18, served.pages + " pages read, step " + step);
    // And it draws: both lanes, the newest edit last.
    const m = Live.pace(Live.model(STATE, tail.events), tail.events);
    assert.equal(m.lanes.length, 2);
    assert.deepEqual(m.cards.at(-1).seq, [newest.seq]);
  }
});

test("a tail read through pages the store cut short still holds every newest row", async () => {
  const log = longLog(50_000, 1000, 3);
  const { fetchPage, served } = server(log, 133);
  const tail = await Live.readTail(fetchPage, TAIL);
  const newest = log.at(-1);
  assert.deepEqual(tail, { events: log.filter((e) => e.at_ms >= newest.at_ms - TAIL.window_ms), next: newest.seq });
  // Each 1000-seq span is about 333 rows here, three short pages and the
  // probes; the last page of a span can run past it into rows already read.
  assert.ok(served.pages <= 3 * 4 + 2 * 18, served.pages + " pages read");
});

test("the view's clock anchor times a tail that holds none, and replaces an older one", () => {
  const log = longLog(900, 1000);
  const tail = log.slice(-601);
  assert.ok(!tail.some((e) => e.type === "clock_anchored"));
  const given = { generation: 1, clock_ms: 0, wall_ms: 1_800_000_000_000 };
  const events = Live.withAnchor(tail, given);
  assert.deepEqual(events.slice(0, -1), tail);
  assert.deepEqual(events.at(-1), { seq: tail.at(-1).seq, type: "clock_anchored", paths: [], more_paths: 0, clock_ms: 0, wall_ms: 1_800_000_000_000 });
  // Wall time now, 2 s after the newest edit, through the anchor.
  const now = Live.daemonNow(events, given.wall_ms + log.at(-1).at_ms + 2000);
  assert.equal(now, log.at(-1).at_ms + 2000);
  const p = Live.pace(model(STATE, events), events, { now_ms: now });
  assert.equal(p.cards.at(-1).time, utc(given.wall_ms + log.at(-1).at_ms));
  assert.deepEqual(p.working, [0, 1]);
  // The tail's own anchor stands when it is the view's; an older one gives
  // way; without either the tail is untimed.
  const own = [{ seq: 5, type: "clock_anchored", clock_ms: 0, wall_ms: given.wall_ms, paths: [], more_paths: 0 }, ...tail];
  assert.equal(Live.withAnchor(own, given), own);
  const old = [{ ...own[0], clock_ms: -5 }, ...tail];
  assert.equal(Live.withAnchor(old, given).at(-1).clock_ms, 0);
  assert.equal(Live.withAnchor(tail, undefined), tail);
  assert.equal(Live.daemonNow(tail, 1), undefined);
});

test("a busy tail stops at its row bound, a short log is read whole with its anchor, and an empty one is empty", async () => {
  const busy = longLog(50_000, 10);
  const one = server(busy);
  const tail = await Live.readTail(one.fetchPage, TAIL);
  assert.deepEqual(tail.events, busy.slice(-TAIL.keep));
  assert.ok(one.served.rows <= TAIL.keep + 2 * 18, one.served.rows + " rows read");
  const short = longLog(20, 1000);
  assert.deepEqual(await Live.readTail(server(short).fetchPage, TAIL), { events: short, next: short.at(-1).seq });
  // Older than the window, but still the clock.
  const old = longLog(900, 1000);
  const kept = await Live.readTail(server(old).fetchPage, TAIL);
  assert.deepEqual(kept.events[0], old[0]);
  assert.equal(kept.events.length, 1 + 601);
  assert.deepEqual(await Live.readTail(server([]).fetchPage, TAIL), { events: [], next: 0 });
});

test("the Files view never draws a session's task line or intent: the agent's prompt, untrusted text", () => {
  const page = readFileSync(new URL("../src/dashboard.html", import.meta.url), "utf8");
  const script = page.slice(page.indexOf("<script"));
  // The page builds text only from fields it names, so naming neither of
  // these fields is what keeps them off the screen.
  assert.deepEqual([...script.matchAll(/\.(task|intent)\b|\[['"](task|intent)['"]\]/g)].map((m) => m[0]), []);
});
