// The Live view's story model (issue #491), run by tests/dashboard_live.rs.
// It loads src/dashboard-live.js as the page will, a plain script, and
// checks the story rule on /api/events version 2 rows.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";

const source = readFileSync(new URL("../src/dashboard-live.js", import.meta.url), "utf8");
// In this realm, so its arrays compare equal to the tests' own.
vm.runInThisContext(source);
const Live = globalThis.LangSpanLive;

const A = "claude-code.alice";
const B = "codex.bob";
const B_SUB = "codex.bob.helper";
const F = "src/billing/invoices.ts";

let seq = 0;
const row = (fields) => ({ seq: ++seq, paths: [], more_paths: 0, ...fields });
const registered = (id, parent) => row({ type: "session_registered", session_id: id, parent });
const edit = (id, at, path = F, tool = "Edit", result = "succeeded") =>
  row({ type: "operation_observed", session_id: id, at_ms: at, paths: [path], tool, result });
const read = (id, at, path = F) =>
  row({ type: "source_observed", session_id: id, at_ms: at, paths: [path], origin: "read" });
const held = (id, peer, at, refusal = "stale_base", path = F) =>
  row({ type: "write_refused", session_id: id, at_ms: at, paths: [path], peer_session_id: peer, refusal });

// Alice changes the file; Bob's helper is held, re-reads it and saves.
function incident() {
  seq = 0;
  return [
    row({ type: "clock_anchored", wall_ms: 1_800_000_000_000, clock_ms: 1000 }),
    registered(A),
    registered(B),
    registered(B_SUB, B),
    edit(A, 2000),
    held(B_SUB, A, 3000),
    read(B_SUB, 3500),
    edit(B_SUB, 4000),
  ];
}

test("a hold naming another agent, then that agent's save, is a story", () => {
  const events = incident();
  assert.deepEqual(
    Live.candidates(events).map((c) => [c.seq, c.save]),
    [[6, 8]],
  );
  const story = Live.story(events, 6);
  assert.deepEqual(
    story.agents.map((a) => [a.key, a.session, a.host]),
    [["A", A, "Claude Code"], ["B", B, "Codex"]],
  );
  assert.deepEqual(
    story.cards.map((c) => [c.agent, c.kind, c.file, c.tone]),
    [
      ["A", "changed", "invoices.ts", "neutral"],
      ["B", "held", "invoices.ts", "red"],
      ["B", "read", "invoices.ts", "neutral"],
      ["B", "saved", "invoices.ts", "green"],
    ],
  );
  assert.deepEqual(story.connector, { from: 0, to: 1, tone: "red" });
  assert.equal(
    story.caption,
    "Agent B tried to edit `invoices.ts` after Agent A changed it. LangSpan held it. Agent B re-read it and saved.",
  );
  assert.equal(story.at, 1_800_000_002_000, "the hold's wall time, from the one clock anchor");
});

test("no story without a peer, between one agent's sessions, or without a save", () => {
  seq = 0;
  const cases = [
    [registered(A), edit(A, 1), row({ type: "write_refused", session_id: B, at_ms: 2, paths: [F], refusal: "stale_base" }), edit(B, 3)],
    [registered(B), registered(B_SUB, B), edit(B, 1), held(B_SUB, B, 2), edit(B_SUB, 3)],
    [edit(A, 1), held(B, A, 2), edit(B, 3, F, "Edit", "failed"), edit(B, 4, F, "Bash"), edit(B, 5, "other.ts")],
    [edit(A, 1), edit(B, 2), held(B, A, 3)],
    [edit(A, 1), held(B, A, 2), edit(A, 3)],
  ];
  for (const events of cases) {
    assert.deepEqual(Live.candidates(events), [], JSON.stringify(events));
    assert.equal(Live.best(events), null);
  }
});

test("the newest story wins", () => {
  seq = 0;
  const events = [edit(A, 1), held(B, A, 2), edit(B, 3), held(A, B, 4, "unread_lines", "b.ts"), edit(A, 5, "b.ts")];
  assert.equal(Live.best(events).seq, 4);
  const story = Live.story(events, 4);
  assert.deepEqual(story.agents.map((a) => a.session), [B, A]);
  assert.equal(
    story.caption,
    "Agent B tried to edit lines of `b.ts` it hadn't read. LangSpan held it. Agent B saved on retry.",
  );
  assert.equal(story.at, null, "no clock anchor, no wall time");
});

test("each agent gets at most five cards; the rest fold into one", () => {
  const events = incident();
  for (let n = 0; n < 6; n++) events.push(read(B, 3100 + n, `docs/f${n}.md`));
  events.push(read(B, 3200, "docs/f0.md"), edit(B, 3300, "x.ts", "Bash"), edit(B, 3400, "y.ts"));
  events.push(read(A, 2100, "far.ts"), read(A, 900_000, "outside-the-window.ts"));
  const story = Live.story(events, 6);
  const mine = (agent) => story.cards.filter((c) => c.agent === agent);
  assert.equal(mine("B").length, 5);
  // Three key cards, the nearest other step (both reads of f0, merged), and
  // one card for the rest.
  assert.deepEqual(
    mine("B").map((c) => [c.kind, c.file, c.seqs.length]),
    [
      ["held", "invoices.ts", 1],
      ["read", "f0.md", 2],
      ["summary", null, 7],
      ["read", "invoices.ts", 1],
      ["saved", "invoices.ts", 1],
    ],
  );
  assert.equal(mine("B")[2].verb, "Read 5 files, edited 1 file and ran 1 command");
  const shown = mine("B").flatMap((c) => c.seqs);
  assert.equal(new Set(shown).size, shown.length, "no step twice");
  assert.deepEqual(mine("A").map((c) => c.kind), ["changed", "read"], "outside the window is left out");
  const times = story.cards.map((c) => c.at_ms);
  assert.deepEqual(times, [...times].sort((x, y) => x - y), "cards keep time order");
});

test("events without a time are inspector rows, never cards", () => {
  const events = incident();
  // A message recorded before it kept a time names no session in the feed.
  events.push(row({ type: "message_sent", workspace_id: "app", paths: [F] }));
  events.push(row({ type: "lease_granted", session_id: B_SUB, paths: [F] }));
  events.push(row({ type: "lease_granted", session_id: "cursor.carol", paths: ["unrelated.ts"] }));
  const story = Live.story(events, 6);
  assert.ok(story.cards.every((c) => c.at_ms != null));
  assert.deepEqual(
    story.untimed.map((r) => r.type),
    ["session_registered", "session_registered", "session_registered", "message_sent", "lease_granted"],
  );
});

test("the caption stays within twenty words, the file counted as one", () => {
  for (const refusal of ["stale_base", "unread_lines", "conflict"]) {
    for (const reread of [true, false]) {
      seq = 0;
      const name = "a very long name with spaces.ts";
      const events = [edit(A, 1, name), held(B, A, 2, refusal, name)];
      if (reread) events.push(read(B, 3, name));
      events.push(edit(B, 4, name));
      const caption = Live.story(events, 2).caption;
      const words = caption.replace("`" + name + "`", "F").split(/\s+/);
      assert.ok(words.length <= 20, caption);
    }
  }
});

test("path text is data: kept as given, never interpreted", () => {
  seq = 0;
  const hostile = "src/<img src=x onerror=alert(1)>.ts";
  const events = [edit(A, 1, hostile), held(B, A, 2, "stale_base", hostile), edit(B, 3, hostile)];
  const story = Live.story(events, 2);
  assert.equal(story.cards[1].path, hostile);
  assert.equal(story.cards[1].file, "<img src=x onerror=alert(1)>.ts");
  assert.equal(Live.story(events, 99), null, "an unknown seq is no story");
});

test("the feed is read until a page comes back empty, within a bound", async () => {
  const pages = { 0: [1, 2], 2: [3], 3: [4, 5], 5: [] };
  const asked = [];
  const fetchPage = async (after) => {
    asked.push(after);
    const events = pages[after].map((s) => ({ seq: s, type: "booted", paths: [], more_paths: 0 }));
    return { version: 2, next: events.length ? events.at(-1).seq : after, events };
  };
  const read = await Live.readFeed(fetchPage, 0, 10);
  assert.deepEqual(read.events.map((e) => e.seq), [1, 2, 3, 4, 5]);
  assert.deepEqual(asked, [0, 2, 3, 5]);
  assert.equal(read.next, 5);
  assert.equal(read.complete, true);
  const stuck = async (after) => ({ version: 2, next: after, events: [{ seq: after, type: "booted" }] });
  const bounded = await Live.readFeed(stuck, 7, 10);
  assert.equal(bounded.complete, false, "a page that doesn't advance stops the read");
});
