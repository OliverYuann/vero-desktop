// Shared helpers for the local pages (boot + tray). No network, no innerHTML
// with data: everything cached is rendered with textContent.
//
// Snapshot shapes (written by the web app via desktop_cache_put; see
// README "Offline snapshot format"). Readers are deliberately tolerant —
// a field may be named a few ways, and a list may be bare or under `items`.
//
//   workspace: { watchlist: Quote[], alerts: Alert[], events: Event[] }
//   watchlist: { items: Quote[] } | Quote[]
//   alerts:    { items: Alert[] } | Alert[]
//   events:    { items: Event[] } | Event[]
//   Quote = { symbol, name?, price?, changePct? }   (changePct in percent: 1.25 = +1.25%)
//   Alert = { id?, title, symbol?, at?, severity? ("high"|"medium"|"low") }
//   Event = { title, symbol?, at?, kind? }

(function () {
  "use strict";

  const T = window.__TAURI__;
  const invoke = (cmd, args) => T.core.invoke(cmd, args || {});
  const listen = (ev, fn) => T.event.listen(ev, fn);
  const TICKER = /^[A-Z0-9][A-Z0-9.-]{0,9}$/;
  const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

  if (new URLSearchParams(location.search).has("vibrant")) {
    document.documentElement.classList.add("vibrant");
  }

  async function readCache(kind) {
    try {
      const e = await invoke("desktop_cache_get", { kind });
      if (!e) return null;
      return { data: JSON.parse(e.json), as_of: e.as_of, saved_at: e.saved_at };
    } catch (_) {
      return null;
    }
  }

  function list(x, keys) {
    if (Array.isArray(x)) return x;
    if (x && typeof x === "object") {
      for (const k of keys) if (Array.isArray(x[k])) return x[k];
    }
    return [];
  }

  function num(v) {
    const n = typeof v === "string" ? parseFloat(v) : v;
    return typeof n === "number" && isFinite(n) ? n : null;
  }

  function str(v, max) {
    return typeof v === "string" ? v.slice(0, max || 200) : "";
  }

  function quote(o) {
    if (!o || typeof o !== "object") return null;
    const symbol = str(o.symbol || o.ticker, 12).toUpperCase();
    if (!TICKER.test(symbol)) return null;
    return {
      symbol,
      name: str(o.name || o.company || o.companyName, 60),
      price: num(o.price ?? o.last ?? o.close ?? o.c),
      changePct: num(o.changePct ?? o.change_pct ?? o.changePercent ?? o.change_percent ?? o.pct ?? o.dp),
    };
  }

  function when(v) {
    if (!v) return null;
    const d = new Date(v);
    return isNaN(d.getTime()) ? null : d;
  }

  function alert(o) {
    if (!o || typeof o !== "object") return null;
    const title = str(o.title || o.headline || o.message || o.summary, 160);
    if (!title) return null;
    return {
      id: str(o.id, 40),
      title,
      symbol: str(o.symbol || o.ticker, 12).toUpperCase(),
      at: when(o.at || o.created_at || o.createdAt || o.time || o.triggered_at),
      high: /^(high|critical|urgent)$/i.test(str(o.severity || o.priority || o.importance, 12)),
    };
  }

  function event(o) {
    if (!o || typeof o !== "object") return null;
    const title = str(o.title || o.name || o.label || o.event, 160);
    if (!title) return null;
    return {
      title,
      symbol: str(o.symbol || o.ticker, 12).toUpperCase(),
      at: when(o.at || o.date || o.time || o.starts_at || o.startsAt),
      kind: str(o.kind || o.type, 24),
    };
  }

  /** The cached workspace: the combined `workspace` doc first, then the
   *  per-kind docs for anything it lacks. */
  async function loadSnapshot() {
    const [ws, wl, al, ev] = await Promise.all(["workspace", "watchlist", "alerts", "events"].map(readCache));
    const pick = (fromWs, own, keys) => {
      const a = ws ? list(ws.data && ws.data[fromWs], keys) : [];
      if (a.length) return { rows: a, as_of: ws.as_of };
      if (own) return { rows: list(own.data, keys), as_of: own.as_of };
      return { rows: [], as_of: null };
    };
    const w = pick("watchlist", wl, ["items", "quotes", "rows"]);
    const a = pick("alerts", al, ["items", "alerts", "rows"]);
    const e = pick("events", ev, ["items", "events", "rows"]);
    const stamps = [w.as_of, a.as_of, e.as_of].map(when).filter(Boolean);
    const asOf = stamps.length ? new Date(Math.max(...stamps.map((d) => d.getTime()))) : null;
    const snap = {
      watchlist: w.rows.map(quote).filter(Boolean).slice(0, 40),
      alerts: a.rows.map(alert).filter(Boolean),
      events: e.rows.map(event).filter(Boolean),
      asOf,
    };
    snap.any = snap.watchlist.length + snap.alerts.length + snap.events.length > 0;
    return snap;
  }

  const sameDay = (a, b) =>
    a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();

  function todaysAlerts(alerts, n) {
    const now = new Date();
    const today = alerts.filter((x) => !x.at || sameDay(x.at, now));
    return (today.length ? today : alerts).sort((x, y) => (y.at || 0) - (x.at || 0)).slice(0, n);
  }

  function upcomingEvents(events, n) {
    const start = new Date();
    start.setHours(0, 0, 0, 0);
    return events
      .filter((x) => !x.at || x.at >= start)
      .sort((x, y) => (x.at || 0) - (y.at || 0))
      .slice(0, n);
  }

  const fmtTime = new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit" });
  const fmtDay = new Intl.DateTimeFormat(undefined, { weekday: "short" });
  const fmtDate = new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" });
  const fmtPrice = new Intl.NumberFormat(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 2 });

  function asOfLabel(d) {
    if (!d) return "";
    const now = new Date();
    return sameDay(d, now) ? fmtTime.format(d) : `${fmtDate.format(d)}, ${fmtTime.format(d)}`;
  }

  function shortWhen(d) {
    if (!d) return "";
    const now = new Date();
    if (sameDay(d, now)) return fmtTime.format(d);
    const diff = (d - now) / 86400000;
    return diff > -1 && diff < 6 ? fmtDay.format(d) : fmtDate.format(d);
  }

  function el(tag, cls, text) {
    const n = document.createElement(tag);
    if (cls) n.className = cls;
    if (text != null) n.textContent = text;
    return n;
  }

  function go(path) {
    return invoke("desktop_navigate", { path }).catch(() => {});
  }

  function renderWatchlist(ul, rows) {
    ul.replaceChildren();
    for (const q of rows) {
      const li = el("li", "row click");
      li.tabIndex = 0;
      const lead = el("span", "lead");
      lead.append(el("span", "sym", q.symbol));
      if (q.name) lead.append(el("span", "name", q.name));
      li.append(lead);
      li.append(el("span", "price num muted", q.price == null ? "—" : fmtPrice.format(q.price)));
      const c = q.changePct;
      const cls = c == null || c === 0 ? "flat" : c > 0 ? "up" : "down";
      const sign = c > 0 ? "+" : c < 0 ? "−" : "";
      li.append(el("span", `chg num ${cls}`, c == null ? "—" : `${sign}${Math.abs(c).toFixed(2)}%`));
      const open = () => go(`/company/${encodeURIComponent(q.symbol)}`);
      li.addEventListener("click", open);
      li.addEventListener("keydown", (e) => e.key === "Enter" && open());
      ul.append(li);
    }
  }

  function renderAlerts(ul, rows) {
    ul.replaceChildren();
    for (const a of rows) {
      const li = el("li", `item click${a.high ? " high" : ""}`);
      li.tabIndex = 0;
      li.append(el("span", "when num", shortWhen(a.at)));
      li.append(el("span", "title", a.title));
      li.append(el("span", "sub", a.symbol || "Alert"));
      const open = () => go(UUID.test(a.id) ? `/alerts?open=${a.id.toLowerCase()}` : "/alerts");
      li.addEventListener("click", open);
      li.addEventListener("keydown", (e) => e.key === "Enter" && open());
      ul.append(li);
    }
  }

  function renderEvents(ul, rows) {
    ul.replaceChildren();
    for (const e of rows) {
      const li = el("li", "item");
      li.append(el("span", "when num", shortWhen(e.at)));
      li.append(el("span", "title", e.title));
      li.append(el("span", "sub", [e.symbol, e.kind].filter(Boolean).join(" · ") || "Event"));
      if (TICKER.test(e.symbol)) {
        li.classList.add("click");
        li.tabIndex = 0;
        li.addEventListener("click", () => go(`/company/${encodeURIComponent(e.symbol)}`));
      }
      ul.append(li);
    }
  }

  window.Vero = {
    invoke,
    listen,
    loadSnapshot,
    todaysAlerts,
    upcomingEvents,
    asOfLabel,
    renderWatchlist,
    renderAlerts,
    renderEvents,
    el,
    go,
  };
})();
