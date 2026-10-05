// Tray mini window logic. See tray.html.
(function () {
  "use strict";
  const V = window.Vero;
  const $ = (id) => document.getElementById(id);

  async function refresh() {
    let snap = null;
    try {
      snap = await V.loadSnapshot();
    } catch (_) {
      snap = null;
    }
    const wl = snap ? snap.watchlist.slice(0, 6) : [];
    V.renderWatchlist($("watchlist"), wl);
    $("watchlist-empty").hidden = wl.length > 0;
    const alerts = snap ? V.todaysAlerts(snap.alerts, 4) : [];
    const events = snap ? V.upcomingEvents(snap.events, 4) : [];
    V.renderAlerts($("alerts"), alerts);
    V.renderEvents($("events"), events);
    $("today-empty").hidden = alerts.length + events.length > 0;
    $("asof").textContent =
      snap && snap.asOf ? `As of ${V.asOfLabel(snap.asOf)} · cached` : "No saved data yet";
  }

  $("ask").addEventListener("submit", (e) => {
    e.preventDefault();
    const q = $("q").value.trim();
    V.go(q ? `/assistant?q=${encodeURIComponent(q)}` : "/assistant");
    $("q").value = "";
  });
  $("open").addEventListener("click", () => V.go("/dashboard"));
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") V.invoke("desktop_tray_hide").catch(() => {});
  });
  V.listen("vero://tray-shown", refresh).catch(() => {});
  V.listen("vero://tray-assistant", () => $("q").focus()).catch(() => {});
  refresh();
})();
