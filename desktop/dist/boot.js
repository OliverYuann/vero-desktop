// Boot page logic. See index.html.
(async function () {
  "use strict";
  const V = window.Vero;
  const $ = (id) => document.getElementById(id);
  const RETRY_MS = 30000;
  let snap = null;
  let timer = null;
  let state = "connecting";

  function setState(next) {
    state = next;
    const island = $("island");
    island.dataset.state = next;
    const status = $("status");
    status.replaceChildren();
    const b = document.createElement("strong");
    if (next === "offline") {
      b.textContent = "Offline";
      status.append(b, snap && snap.asOf ? ` — showing data as of ${V.asOfLabel(snap.asOf)}` : " — no saved data yet");
    } else {
      b.textContent = "Vero";
      status.append(b, next === "opening" ? " · Opening…" : " · Connecting…");
    }
    $("actions").hidden = next !== "offline";
    if (!snap || !snap.any) {
      $("blank-text").textContent =
        next === "offline" ? "You're offline. Vero will open as soon as you're back." : "Opening Vero…";
    }
  }

  function render() {
    if (!snap || !snap.any) {
      $("snapshot").hidden = true;
      $("blank").hidden = false;
      return;
    }
    $("blank").hidden = true;
    $("snapshot").hidden = false;
    V.renderWatchlist($("watchlist"), snap.watchlist);
    const alerts = V.todaysAlerts(snap.alerts, 6);
    V.renderAlerts($("alerts"), alerts);
    $("alerts-empty").hidden = alerts.length > 0;
    const events = V.upcomingEvents(snap.events, 6);
    V.renderEvents($("events"), events);
    $("events-empty").hidden = events.length > 0;
    const label = snap.asOf ? `as of ${V.asOfLabel(snap.asOf)} · cached` : "cached";
    $("wl-asof").textContent = label;
    $("foot-text").textContent = `Snapshot from your last session, ${label}. Prices are not live until Vero connects.`;
  }

  function schedule() {
    clearTimeout(timer);
    timer = null;
    // Only while visible: a hidden boot page (window in the tray) costs nothing.
    if (state === "offline" && document.visibilityState === "visible") {
      timer = setTimeout(probe, RETRY_MS);
    }
  }

  async function probe() {
    clearTimeout(timer);
    timer = null;
    setState("connecting");
    let online = false;
    try {
      online = (await V.invoke("desktop_probe")).online;
    } catch (_) {
      online = false;
    }
    setState(online ? "opening" : "offline");
    schedule();
  }

  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible" && state === "offline" && !timer) probe();
    if (document.visibilityState === "hidden") {
      clearTimeout(timer);
      timer = null;
    }
  });
  $("retry").addEventListener("click", probe);
  $("force").addEventListener("click", () => {
    setState("opening");
    V.invoke("desktop_probe", { force: true }).catch(() => {});
  });
  V.listen("vero://boot-retry", probe).catch(() => {});

  try {
    snap = await V.loadSnapshot();
  } catch (_) {
    snap = null;
  }
  render();
  setState("connecting");
  // The window was created invisible; reporting the paint is what shows it.
  // Reported straight after the snapshot is in the DOM (layout forced) and
  // BEFORE probing: a hidden webview gets no frame callbacks and throttled
  // timers, so waiting for rAF here would only delay the first paint.
  void document.body.offsetHeight;
  try {
    await V.invoke("desktop_boot_painted", { cached: !!(snap && snap.any) });
  } catch (_) {
    /* the 2 s fallback in the shell shows the window anyway */
  }
  probe();
})();
