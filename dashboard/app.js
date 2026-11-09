// The dashboard page: talks to the bot's /api with the key from the `/log dashboard` link, and
// draws everyone in the log and the chosen person's entries. Wording comes from logs.js.
(function () {
  "use strict";
  const { GROUPS, describe, pct } = window.NiceLogs;
  const $ = (id) => document.getElementById(id);
  const SVG = "http://www.w3.org/2000/svg";
  const KEY_STORE = "nicenovember-dashboard-key";

  /** An element with attributes and children (strings become text, never HTML). */
  function el(tag, attrs, ...children) {
    const node = tag.startsWith("svg:") ? document.createElementNS(SVG, tag.slice(4)) : document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v == null || v === false) continue;
      if (k === "class") node.setAttribute("class", v);
      else if (k.startsWith("on")) node.addEventListener(k.slice(2), v);
      else node.setAttribute(k, v === true ? "" : v);
    }
    for (const c of children.flat()) {
      if (c == null || c === false) continue;
      node.append(c instanceof Node ? c : document.createTextNode(String(c)));
    }
    return node;
  }

  // ---- the key and where we are ----

  /** The key from the link (then hidden from the address bar), or the one kept from last time. */
  function takeKey() {
    const fromLink = new URLSearchParams(location.hash.slice(1)).get("key");
    if (fromLink) {
      try { localStorage.setItem(KEY_STORE, fromLink); } catch { /* kept in memory only */ }
      history.replaceState(null, "", location.pathname);
      return fromLink;
    }
    try { return localStorage.getItem(KEY_STORE); } catch { return null; }
  }

  function forgetKey() {
    try { localStorage.removeItem(KEY_STORE); } catch { /* nothing kept */ }
  }

  const state = {
    key: takeKey(),
    people: [],
    who: new URLSearchParams(location.hash.slice(1)).get("who"),
    groups: new Set(),
    q: "",
    lastId: null,
    days: [],
    // Bumped on each new log query, so answers to older ones are dropped.
    asked: 0,
  };

  function notice(text) {
    $("notice").textContent = text;
    $("notice").hidden = false;
    $("app").hidden = true;
  }

  async function api(path) {
    const res = await fetch(path, { headers: { Authorization: `Bearer ${state.key}` } });
    const body = await res.json().catch(() => ({}));
    if (res.status === 401) {
      forgetKey();
      notice(body.error || "This link has expired. Run /log dashboard in Discord for a new one.");
      throw new Error("unauthorized");
    }
    if (!res.ok) throw new Error(body.error || `HTTP ${res.status}`);
    return body;
  }

  // ---- formatting ----

  const num = (n) => Number(n).toLocaleString();
  const dateTime = new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
  const dayFmt = new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric", timeZone: "UTC" });
  const dayLabel = (day) => dayFmt.format(new Date(`${day}T00:00:00Z`));
  const when = (at) => (at ? dateTime.format(new Date(at)) : "");
  const entries = (n) => `${num(n)} ${n === 1 ? "entry" : "entries"}`;
  const isBad = (e) => e.kind === "hostile" || e.kind === "timed_out" || (e.kind === "judged" && e.data && e.data.hostile);

  // ---- people ----

  function renderPeople() {
    const find = $("find").value.trim().toLowerCase();
    const list = $("people");
    list.replaceChildren(
      ...state.people
        .filter((p) => !find || p.name.toLowerCase().includes(find) || p.id.includes(find))
        .map((p) => el("li", {},
          el("button", { type: "button", "aria-current": p.id === state.who ? "true" : null, onclick: () => choose(p.id) },
            el("span", { class: "who" }, p.name),
            el("span", { class: "count" }, num(p.total)),
            el("span", { class: "sub" },
              p.hostile ? el("span", { class: "bad-count" }, `⚠ ${num(p.hostile)} not nice`) : null,
              p.hostile ? " · " : null,
              `last ${when(p.last)}`)))),
    );
    if (!list.children.length) list.append(el("li", { class: "muted" }, "Nobody matches."));
  }

  function choose(id) {
    state.who = id;
    history.replaceState(null, "", `${location.pathname}#who=${encodeURIComponent(id)}`);
    renderPeople();
    showPerson();
  }

  // ---- one person ----

  function person() {
    return state.people.find((p) => p.id === state.who);
  }

  function tile(label, value, bad) {
    return el("div", { class: `tile${bad ? " bad" : ""}` },
      el("div", { class: "label" }, label), el("div", { class: "value" }, value));
  }

  async function showPerson() {
    const p = person();
    if (!p) return;
    const role = p.participant == null ? null : p.participant ? "Participant" : "Not a participant";
    $("name").replaceChildren(p.name, role ? el("span", { class: "chip" }, role) : null);
    $("seen").textContent = p.id === "server"
      ? "Entries about the server, not a person: the bot joining and leaving voice, bulk deletes."
      : `First seen ${when(p.first)} · last seen ${when(p.last)} · id ${p.id}`;
    $("tiles").replaceChildren(
      tile("Entries", num(p.total)),
      tile("Said", num(p.said)),
      tile("Judged", num(p.judged)),
      tile("Avg. hostile", p.avg_score == null ? "—" : pct(p.avg_score)),
      tile("Not nice", num(p.hostile), p.hostile > 0),
      tile("Timeouts", num(p.timeouts), p.timeouts > 0),
    );
    loadLog();
    const who = state.who;
    try {
      const days = await api(`/api/people/${encodeURIComponent(who)}/days`);
      if (state.who !== who) return;
      state.days = fillDays(days);
      drawChart();
    } catch (e) {
      if (e.message !== "unauthorized") $("plot").textContent = `Couldn't load: ${e.message}`;
    }
  }

  // ---- activity per day ----

  /** Every day from the first to the last, with zeros for the days with nothing. */
  function fillDays(days) {
    if (!days.length) return [];
    const byDay = new Map(days.map((d) => [d.day, d]));
    const out = [];
    const end = new Date(`${days[days.length - 1].day}T00:00:00Z`);
    for (let t = new Date(`${days[0].day}T00:00:00Z`); t <= end; t.setUTCDate(t.getUTCDate() + 1)) {
      const day = t.toISOString().slice(0, 10);
      out.push(byDay.get(day) || { day, n: 0, hostile: 0 });
    }
    return out;
  }

  /** A round number at or above `n`, for the top of the axis. */
  function niceMax(n) {
    if (n <= 5) return 5;
    const step = 10 ** Math.floor(Math.log10(n));
    for (const m of [1, 2, 2.5, 5, 10]) if (m * step >= n) return m * step;
    return 10 * step;
  }

  /** A bar with its top corners rounded and its foot on the baseline. */
  function barPath(x, y, w, h) {
    const r = Math.min(4, w / 2, h);
    return `M${x},${y + h}V${y + r}Q${x},${y} ${x + r},${y}H${x + w - r}Q${x + w},${y} ${x + w},${y + r}V${y + h}Z`;
  }

  function drawChart() {
    const plot = $("plot");
    const days = state.days;
    const tbody = $("days-table").tBodies[0];
    tbody.replaceChildren(...days.filter((d) => d.n).map((d) =>
      el("tr", {}, el("td", {}, d.day), el("td", {}, num(d.n)), el("td", {}, num(d.hostile)))));
    if (!days.length) {
      plot.replaceChildren(el("p", { class: "muted" }, "Nothing logged."));
      return;
    }
    const W = Math.max(plot.clientWidth, 200);
    const H = plot.clientHeight;
    const pad = { l: 36, r: 4, t: 8, b: 20 };
    const iw = W - pad.l - pad.r;
    const ih = H - pad.t - pad.b;
    const top = niceMax(Math.max(...days.map((d) => d.n)));
    const slot = iw / days.length;
    const gap = slot > 6 ? 2 : 0;
    const y = (v) => pad.t + ih - (v / top) * ih;
    const svg = el("svg:svg", { viewBox: `0 0 ${W} ${H}`, role: "img", "aria-label": "Entries per day" });
    for (const v of [top / 2, top]) {
      svg.append(el("svg:line", { class: "gridline", x1: pad.l, x2: W - pad.r, y1: y(v), y2: y(v) }));
      svg.append(el("svg:text", { x: pad.l - 6, y: y(v) + 4, "text-anchor": "end" }, num(v)));
    }
    days.forEach((d, i) => {
      const x = pad.l + i * slot + gap / 2;
      const w = Math.max(slot - gap, 1);
      const h = (d.n / top) * ih;
      const hit = el("svg:rect", { class: "hit", x: pad.l + i * slot, y: pad.t, width: slot, height: ih + pad.b });
      const bar = h > 0 ? el("svg:path", { class: "bar", d: barPath(x, pad.t + ih - h, w, h) }) : null;
      const text = [el("b", {}, dayLabel(d.day)), ` · ${entries(d.n)}`,
        d.hostile ? ` · ${num(d.hostile)} not nice` : ""];
      hit.addEventListener("mousemove", (ev) => tip(ev, text));
      hit.addEventListener("mouseleave", hideTip);
      svg.append(hit);
      if (bar) svg.append(bar);
    });
    svg.append(el("svg:line", { class: "baseline", x1: pad.l, x2: W - pad.r, y1: pad.t + ih, y2: pad.t + ih }));
    const labelled = days.length > 1 ? [0, days.length - 1] : [0];
    for (const i of labelled) {
      const anchor = i === 0 ? "start" : "end";
      const x = i === 0 ? pad.l : W - pad.r;
      svg.append(el("svg:text", { x, y: H - 4, "text-anchor": anchor }, dayLabel(days[i].day)));
    }
    plot.replaceChildren(svg);
  }

  function tip(ev, content) {
    const t = $("tip");
    t.replaceChildren(...content);
    t.hidden = false;
    const x = Math.min(ev.clientX + 12, window.innerWidth - t.offsetWidth - 8);
    t.style.left = `${Math.max(8, x)}px`;
    t.style.top = `${ev.clientY - t.offsetHeight - 10}px`;
  }

  function hideTip() {
    $("tip").hidden = true;
  }

  // ---- the log ----

  function entryItem(e) {
    const text = describe(e);
    const bad = isBad(e);
    return el("li", { class: bad ? "bad" : null },
      el("time", { datetime: e.at }, when(e.at)),
      el("div", { class: "what" },
        bad ? el("span", { class: "flag" }, "⚠ ") : null,
        el("span", { class: "kind" }, e.kind),
        e.channel ? el("span", { class: "where" }, ` in #${e.channel}`) : e.channel_id ? el("span", { class: "where" }, ` in channel ${e.channel_id}`) : null,
        text ? el("div", { class: "text" }, text) : null),
      el("details", {},
        el("summary", {}, "Details"),
        el("pre", {}, JSON.stringify(e.data, null, 2))));
  }

  async function loadLog(more) {
    const asked = more ? state.asked : ++state.asked;
    if (!more) {
      state.lastId = null;
      $("log").replaceChildren();
    }
    $("more").hidden = true;
    $("log-status").textContent = "Loading…";
    const params = new URLSearchParams();
    if (state.groups.size) params.set("groups", [...state.groups].join(","));
    if (state.q) params.set("q", state.q);
    if (state.lastId != null) params.set("before", state.lastId);
    try {
      const page = await api(`/api/people/${encodeURIComponent(state.who)}/log?${params}`);
      if (asked !== state.asked) return;
      $("log").append(...page.entries.map(entryItem));
      if (page.entries.length) state.lastId = page.entries[page.entries.length - 1].id;
      $("more").hidden = !page.more;
      const shown = $("log").children.length;
      $("log-status").textContent = shown
        ? page.more ? "" : `${entries(shown)}, that's all.`
        : "Nothing matches.";
    } catch (e) {
      if (asked === state.asked && e.message !== "unauthorized") $("log-status").textContent = `Couldn't load: ${e.message}`;
    }
  }

  function renderGroups() {
    const all = el("button", { type: "button", "aria-pressed": String(!state.groups.size) }, "All");
    all.addEventListener("click", () => { state.groups.clear(); renderGroups(); loadLog(); });
    $("groups").replaceChildren(all, ...GROUPS.map(([g, label]) => {
      const b = el("button", { type: "button", "aria-pressed": String(state.groups.has(g)) }, label);
      b.addEventListener("click", () => {
        if (state.groups.has(g)) state.groups.delete(g);
        else state.groups.add(g);
        renderGroups();
        loadLog();
      });
      return b;
    }));
  }

  // ---- start ----

  async function loadPeople() {
    const body = await api("/api/people");
    $("guild").textContent = body.guild;
    document.title = `${body.guild} · NiceNovember Logs`;
    state.people = body.people;
    if (!person()) state.who = state.people.length ? state.people[0].id : null;
    $("notice").hidden = true;
    $("app").hidden = false;
    $("refresh").hidden = false;
    renderPeople();
    if (state.who) showPerson();
    else notice("Nothing has been logged yet.");
  }

  async function start() {
    if (!state.key) {
      notice("Run /log dashboard in Discord (admins only) and open the link it gives you.");
      return;
    }
    renderGroups();
    $("find").addEventListener("input", renderPeople);
    let typing;
    $("search").addEventListener("input", () => {
      clearTimeout(typing);
      typing = setTimeout(() => { state.q = $("search").value.trim(); loadLog(); }, 250);
    });
    $("more").addEventListener("click", () => loadLog(true));
    $("refresh").addEventListener("click", () => loadPeople().catch(failed));
    let resizing;
    window.addEventListener("resize", () => {
      clearTimeout(resizing);
      resizing = setTimeout(drawChart, 100);
    });
    await loadPeople();
  }

  function failed(e) {
    if (e.message !== "unauthorized") notice(`Couldn't load the log: ${e.message}`);
  }

  start().catch(failed);
})();
