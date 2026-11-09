// The dashboard's wording for log entries. Pure functions, no DOM, so `node --test dashboard/logs.test.js` can
// test them; the page loads this as a plain script, before app.js.
(function (root) {
  "use strict";

  /** The filter groups the server sorts kinds into, in the order they're shown. */
  const GROUPS = [
    ["said", "Said"], ["moderation", "Moderation"], ["voice", "Voice"],
    ["presence", "Presence"], ["other", "Other"],
  ];

  const pct = (s) => `${Math.round(s * 100)}%`;
  const plural = (n, w) => `${n} ${w}${n === 1 ? "" : "s"}`;

  function duration(secs) {
    const h = Math.floor(secs / 3600);
    const m = Math.floor((secs % 3600) / 60);
    const s = Math.round(secs % 60);
    if (h) return `${h}h ${m}m`;
    if (m) return `${m}m ${s}s`;
    return `${s}s`;
  }

  function emoji(em) {
    if (!em) return "?";
    return em.id ? `:${em.name}:` : em.name || "?";
  }

  function messageText(d) {
    const extras = [];
    const a = (d.attachments || []).length;
    const s = (d.sticker_items || d.stickers || []).length;
    if (a) extras.push(`+${plural(a, "attachment")}`);
    if (s) extras.push(`+${plural(s, "sticker")}`);
    const text = d.content || "";
    if (!extras.length) return text;
    return text ? `${text} (${extras.join(", ")})` : `(${extras.join(", ")})`;
  }

  const VOICE = { join: "Joined voice", leave: "Left voice", move: "Moved voice channel" };

  function voiceUpdate(d) {
    const o = d.old || {};
    const n = d.new || {};
    const flips = [
      ["self_mute", "muted", "unmuted"], ["self_deaf", "deafened", "undeafened"],
      ["self_stream", "started streaming", "stopped streaming"],
      ["self_video", "turned camera on", "turned camera off"],
      ["mute", "server-muted", "server-unmuted"], ["deaf", "server-deafened", "server-undeafened"],
    ]
      .filter(([k]) => Boolean(o[k]) !== Boolean(n[k]))
      .map(([k, on, off]) => (n[k] ? on : off));
    return flips.length ? `Voice: ${flips.join(", ")}` : "Voice state changed";
  }

  /** A one-line, human description of an entry ("" when there's nothing more than its kind). */
  function describe(e) {
    const d = e.data && typeof e.data === "object" ? e.data : {};
    switch (e.kind) {
      case "message": return messageText(d);
      case "message_edit": return `Edited to: ${d.content || ""}`;
      case "message_delete": return "Deleted a message";
      case "message_delete_bulk": return `Bulk-deleted ${plural((d.message_ids || []).length, "message")}`;
      case "heard": return `${d.text || ""}${d.audio_ms ? ` (${(d.audio_ms / 1000).toFixed(1)}s)` : ""}`;
      case "reaction_add": return `Reacted ${emoji(d.emoji)}`;
      case "reaction_remove": return `Removed reaction ${emoji(d.emoji)}`;
      case "thread_create": return `Started thread ${d.name || ""}`.trim();
      case "judged":
        return `${pct(d.score || 0)} sure it's hostile${d.source ? ` (${d.source})` : ""}: ${d.said || ""}`;
      case "judge_failed": return `Couldn't judge: ${d.said || ""}`;
      case "hostile": return `Not nice, ${pct(d.score || 0)} sure: ${d.said || ""}`;
      case "timed_out": return `Timed out for ${duration(d.secs || 0)}${d.muted ? ", muted in VC" : ""}`;
      case "timeout_released": return "Timeout ended";
      case "timeout_message": return `Message deleted (timed out): ${d.content || ""}`;
      case "timeout_reaction": return `Reaction removed (timed out): ${emoji(d.emoji)}`;
      case "timeout_vc": return "Muted on joining VC (timed out)";
      case "pardoned": return "Pardoned";
      case "voice": return VOICE[d.change] || voiceUpdate(d);
      case "vc_session": return `Spent ${duration(d.secs || 0)} in voice`;
      case "monitor_joined": return "Bot joined the monitored channel";
      case "monitor_left": return "Bot left voice";
      case "monitor_failed": return `Bot couldn't join voice: ${d.error || ""}`;
      case "presence":
        return [d.status, ...(d.activities || []).map((a) => a.name)].filter(Boolean).join(" · ");
      case "typing": return "Typing";
      case "command": return `/${d.name || ""}`;
      case "member_seen": return "Seen in the server";
      case "member_join": return "Joined the server";
      case "member_leave": return "Left the server";
      case "member_update": return "Profile or roles changed";
      default: return "";
    }
  }

  const api = { GROUPS, describe, duration, pct };
  if (typeof module === "object" && module.exports) module.exports = api;
  else root.NiceLogs = api;
})(this);
