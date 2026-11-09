// Run with `node --test dashboard/logs.test.js`. Tests the dashboard's wording for log entries (logs.js).
const test = require("node:test");
const assert = require("node:assert/strict");
const logs = require("./logs.js");

const entry = (kind, data) => ({ id: 1, at: "2026-11-05T12:00:00.000Z", day: "2026-11-05", kind, data });

test("describes each kind in a line a person can read", () => {
  const d = (kind, data) => logs.describe(entry(kind, data));
  assert.equal(d("message", { content: "hello", attachments: [{}] }), "hello (+1 attachment)");
  assert.equal(d("message", { content: "", sticker_items: [{}, {}] }), "(+2 stickers)");
  assert.equal(d("message_edit", { content: "fixed" }), "Edited to: fixed");
  assert.equal(d("message_delete_bulk", { message_ids: ["1", "2"] }), "Bulk-deleted 2 messages");
  assert.equal(d("heard", { text: "hi there", audio_ms: 1500 }), "hi there (1.5s)");
  assert.equal(d("judged", { source: "voice", said: "you're bad", score: 0.87, hostile: true }),
    "87% sure it's hostile (voice): you're bad");
  assert.equal(d("hostile", { said: "ugh", score: 0.9, announced: true }), "Not nice, 90% sure: ugh");
  assert.equal(d("timed_out", { secs: 10, muted: true }), "Timed out for 10s, muted in VC");
  assert.equal(d("reaction_add", { emoji: { name: "😂" } }), "Reacted 😂");
  assert.equal(d("reaction_add", { emoji: { name: "pog", id: "1" } }), "Reacted :pog:");
  assert.equal(d("voice", { change: "join" }), "Joined voice");
  assert.equal(d("voice", { change: "update", old: { self_mute: false }, new: { self_mute: true, self_video: false } }),
    "Voice: muted");
  assert.equal(d("vc_session", { secs: 3725 }), "Spent 1h 2m in voice");
  assert.equal(d("presence", { status: "dnd", activities: [{ name: "Factorio" }] }), "dnd · Factorio");
  assert.equal(d("command", { name: "stats" }), "/stats");
  assert.equal(d("pardoned", { by: "42" }), "Pardoned");
  assert.equal(d("whatever", { a: 1 }), "");
  assert.equal(logs.describe({ kind: "message", data: "not json" }), "");
});

test("durations and percentages", () => {
  assert.equal(logs.duration(0), "0s");
  assert.equal(logs.duration(90), "1m 30s");
  assert.equal(logs.duration(3725), "1h 2m");
  assert.equal(logs.pct(0.874), "87%");
});

test("the groups match the ones the server sorts kinds into", () => {
  assert.deepEqual(logs.GROUPS.map(([g]) => g), ["said", "moderation", "voice", "presence", "other"]);
});
