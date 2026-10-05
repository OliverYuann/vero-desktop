// Shared state, window construction, the navigation glue and every IPC
// command. Validation lives in `validate.rs` / `nav.rs`; this file only wires
// validated values to the runtime.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::json;
use tauri::{
    webview::NewWindowResponse, AppHandle, Emitter, Manager, Runtime, Url, WebviewUrl,
    WebviewWindow, WebviewWindowBuilder,
};
use tauri_plugin_opener::OpenerExt;

use crate::nav;
use crate::store::{self, CacheEntry, Settings};
use crate::validate::{self, CacheKind, WindowKind};

// ── Events (the contract D2 listens for) ────────────────────────────────────

pub const EV_NAVIGATE: &str = "vero://navigate";
pub const EV_COMMAND_PALETTE: &str = "vero://command-palette";
pub const EV_CSV_DROPPED: &str = "vero://csv-dropped";
pub const EV_ONLINE: &str = "vero://online";
pub const EV_NAVIGATE_HISTORY: &str = "vero://navigate-history";
/// Local pages only: the tray window was just shown (re-read the cache).
pub const EV_TRAY_SHOWN: &str = "vero://tray-shown";
/// Local pages only: focus the tray's Assistant box ("Quick Assistant").
pub const EV_TRAY_ASSISTANT: &str = "vero://tray-assistant";
/// Local pages only: a deep link arrived while the boot page was up; probe now.
pub const EV_BOOT_RETRY: &str = "vero://boot-retry";

/// Where the main window goes once the app origin is reachable.
pub const START_PATH: &str = "/auth";

/// Ceiling on simultaneously open pop-out windows.
const MAX_POPOUTS: usize = 12;

/// How long after a notification a focus of the main window counts as
/// "clicked the notification" on platforms that cannot report the click.
const NOTIFICATION_CLICK_WINDOW: Duration = Duration::from_secs(5 * 60);

/// Injected into every document a Vero WEB window loads, before page scripts.
/// `__VERO_DESKTOP__` / `data-vero-desktop` are the 0.1 contract the web app
/// already keys off (see README "Navigation policy"); `__VERO_DESKTOP_WINDOW__`
/// says which kind of window this is (`main`, or a pop-out kind) so the web
/// side can render a compact layout in a pop-out.
fn marker_script(kind: &str) -> String {
    format!(
        "window.__VERO_DESKTOP__ = true;window.__VERO_DESKTOP_WINDOW__ = {kind:?};\
(function(){{function mark(){{try{{var d=document.documentElement;d.setAttribute('data-vero-desktop','');\
d.setAttribute('data-vero-desktop-window',{kind:?});}}catch(e){{}}}}\
mark();document.addEventListener('readystatechange',mark);}})();"
    )
}

// ── Runtime feature switches ────────────────────────────────────────────────

/// Native kill-switches: `VERO_DISABLE=tray,hotkey,…` turns a feature off
/// without a rebuild (support escape hatch). Names match `desktop_info`'s
/// `features` keys. The web side has its own flags; this is the native half.
/// The app's dark `--bg-void` (styles.css), as the native window background.
pub const WINDOW_BG_DARK: (u8, u8, u8, u8) = (8, 9, 11, 255);

pub fn feature_on(name: &str) -> bool {
    static OFF: OnceLock<HashSet<String>> = OnceLock::new();
    let off = OFF.get_or_init(|| {
        std::env::var("VERO_DISABLE")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect()
    });
    !off.contains(name)
}

// ── State ───────────────────────────────────────────────────────────────────

pub struct Desk {
    pub t0: Instant,
    pub start_hidden: bool,
    pub tray_pinned: AtomicBool,
    pub painted: AtomicBool,
    /// Webview labels whose current document has called `desktop_info`
    /// (= the web bridge is listening for `vero://navigate`). Cleared on every
    /// page load.
    pub bridge_ready: Mutex<HashSet<String>>,
    /// In-app path to open once the main window leaves the boot page.
    pub pending_nav: Mutex<Option<String>>,
    pub pending_notification: Mutex<Option<(String, Instant)>>,
    pub online: Mutex<Option<bool>>,
    pub settings: Mutex<Settings>,
    pub hotkey: Mutex<Option<String>>,
    pub tray_hidden_at: Mutex<Option<Instant>>,
    /// Pop-out label → kind, for `desktop_info().window_kind`.
    pub popout_kinds: Mutex<std::collections::HashMap<String, &'static str>>,
    securestore: OnceLock<bool>,
}

impl Desk {
    pub fn new(t0: Instant, start_hidden: bool, settings: Settings) -> Self {
        Self {
            t0,
            start_hidden,
            tray_pinned: AtomicBool::new(false),
            painted: AtomicBool::new(false),
            bridge_ready: Mutex::new(HashSet::new()),
            pending_nav: Mutex::new(None),
            pending_notification: Mutex::new(None),
            online: Mutex::new(None),
            settings: Mutex::new(settings),
            hotkey: Mutex::new(None),
            tray_hidden_at: Mutex::new(None),
            popout_kinds: Mutex::new(std::collections::HashMap::new()),
            securestore: OnceLock::new(),
        }
    }

    pub fn securestore_available(&self) -> bool {
        *self.securestore.get_or_init(|| {
            feature_on("securestore") && nav::secure_store_allowed() && crate::secure::probe()
        })
    }

    pub fn ms(&self) -> u128 {
        self.t0.elapsed().as_millis()
    }
}

pub fn desk<R: Runtime>(app: &AppHandle<R>) -> tauri::State<'_, Desk> {
    app.state::<Desk>()
}

// ── Window helpers ──────────────────────────────────────────────────────────

/// Bring the main window back into view.
pub fn reveal<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Hand a URL to the user's default browser.
pub fn hand_off<R: Runtime>(app: &AppHandle<R>, url: &str) {
    if let Err(err) = app.opener().open_url(url, None::<&str>) {
        eprintln!("[vero] could not open {url} in the system browser: {err}");
    }
}

fn main_is_on_local_page<R: Runtime>(main: &WebviewWindow<R>) -> bool {
    main.url().map(|u| nav::is_local_page(&u)).unwrap_or(true)
}

/// Open an already-validated in-app path in the main window.
///
/// - Main window still on the boot page → remember it; it is where the boot
///   page goes once the origin answers (and nudge it to probe now).
/// - Web bridge listening → emit `vero://navigate {path}` so the SPA routes
///   client-side (no reload, state survives).
/// - Otherwise (bridge flag off, older web build) → a real navigation.
pub fn navigate_main<R: Runtime>(app: &AppHandle<R>, path: String) {
    let Some(main) = app.get_webview_window("main") else {
        return;
    };
    reveal(app);
    let d = desk(app);
    if main_is_on_local_page(&main) {
        let online = *d.online.lock().unwrap();
        *d.pending_nav.lock().unwrap() = Some(path.clone());
        if online == Some(true) {
            if let Some(url) = nav::app_url_for(&path) {
                let _ = main.navigate(url);
            }
        } else {
            let _ = main.emit_to("main", EV_BOOT_RETRY, json!({}));
        }
        return;
    }
    if d.bridge_ready.lock().unwrap().contains("main") {
        let _ = app.emit_to("main", EV_NAVIGATE, json!({ "path": path }));
    } else if let Some(url) = nav::app_url_for(&path) {
        let _ = main.navigate(url);
    }
}

/// Deep link → focus + destination. See `nav::resolve_deep_link`.
pub fn handle_deep_link<R: Runtime>(app: &AppHandle<R>, raw: &str) {
    if !feature_on("deeplinks") {
        reveal(app);
        return;
    }
    let Some(target) = nav::resolve_deep_link(raw) else {
        eprintln!("[vero] ignoring deep link with no usable destination");
        reveal(app);
        return;
    };
    if !nav::renders_in_app(&target) {
        // A real Vero URL that is not an app screen (e.g. /pricing): same
        // answer clicking it gets — the default browser.
        hand_off(app, target.as_str());
        return;
    }
    if target.path() == nav::AUTH_CALLBACK_PATH {
        // The PKCE return leg needs the real callback document, never a
        // client-side route change.
        if let Some(main) = app.get_webview_window("main") {
            let _ = main.navigate(target);
        }
        reveal(app);
        return;
    }
    eprintln!("[vero] deep link → {}", nav::path_of(&target));
    navigate_main(app, nav::path_of(&target));
}

/// The navigation policy shared by every WEB window (main + pop-outs).
fn on_navigation_policy<R: Runtime>(app: AppHandle<R>) -> impl Fn(&Url) -> bool + Send + 'static {
    move |url: &Url| {
        if nav::renders_in_app(url) {
            return true;
        }
        hand_off(&app, url.as_str());
        false
    }
}

/// `window.open` / `target=_blank` from a web window: app screens open in the
/// MAIN window (pop-outs are opened deliberately via `desktop_open_window`),
/// everything else goes to the browser. Never a stray webview.
fn on_new_window_policy<R: Runtime>(
    app: AppHandle<R>,
) -> impl Fn(Url, tauri::webview::NewWindowFeatures) -> NewWindowResponse<R> + Send + Sync + 'static
{
    move |url: Url, _features| {
        if nav::renders_in_app(&url) && nav::is_on_app_origin_with(&url, nav::app_origin()) {
            navigate_main(&app, nav::path_of(&url));
        } else if !nav::renders_in_app(&url) {
            hand_off(&app, url.as_str());
        }
        NewWindowResponse::Deny
    }
}

/// Build the main window from tauri.conf.json (`create: false` keeps its
/// geometry declarative; handlers can only be attached at build time).
///
/// It starts INVISIBLE on the local boot page and is shown by
/// `desktop_boot_painted` — i.e. the first thing the user sees is a painted
/// page, never a blank or white frame. A one-shot fallback shows it anyway
/// after 2 s in case the boot page cannot run.
pub fn build_main_window<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<WebviewWindow<R>> {
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == "main")
        .cloned()
        .expect("main window in tauri.conf.json");

    #[allow(unused_mut)]
    let mut builder = WebviewWindowBuilder::from_config(app, &config)?
        .visible(false)
        .initialization_script(marker_script("main"))
        .on_navigation(on_navigation_policy(app.clone()))
        .on_new_window(on_new_window_policy(app.clone()));

    // Every window starts DARK (the app's default theme): a dark native title
    // bar on Windows and macOS instead of the OS-light one over a dark page.
    // The web app switches it with `plugin:window|set_theme` when the user's
    // theme is light (src/lib/desktop/mount.tsx).
    builder = builder.theme(Some(tauri::Theme::Dark));

    // macOS: a TRANSPARENT titlebar — the window's own background colour shows
    // through it, so the traffic lights sit on the page colour, but the page
    // starts below it. That keeps native dragging and double-click-to-zoom on
    // every screen without the web app having to paint a drag strip or pad
    // itself (the old Overlay style needed both, and only V2 screens did it).
    // No vibrancy: a transparent webview let the desktop wallpaper show through
    // every not-quite-opaque frame (navigation, rubber-band scroll, the boot
    // page) — reported as "the background is messed up" (2026-10-05).
    #[cfg(target_os = "macos")]
    {
        builder = builder
            .title_bar_style(tauri::TitleBarStyle::Transparent)
            .hidden_title(true);
    }

    let window = builder.build()?;

    // The window background (seen in the macOS titlebar, during a resize and
    // between documents) is the app's dark --bg-void until the web app says
    // otherwise.
    let _ = window.set_background_color(Some(WINDOW_BG_DARK.into()));

    if !desk(app).start_hidden {
        let fallback = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(2000));
            if !desk(&fallback).painted.load(Ordering::SeqCst) {
                eprintln!(
                    "[vero] boot page did not report a paint in 2 s; showing the window anyway"
                );
                if let Some(w) = fallback.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
        });
    }
    Ok(window)
}

fn popout_label(kind: WindowKind, path: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    kind.as_str().hash(&mut h);
    path.hash(&mut h);
    format!("pop-{:016x}", h.finish())
}

pub fn open_popout<R: Runtime>(
    app: &AppHandle<R>,
    path: &str,
    kind: WindowKind,
) -> Result<String, String> {
    let label = popout_label(kind, path);
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
        return Ok(label);
    }
    let open = app
        .webview_windows()
        .keys()
        .filter(|l| l.starts_with("pop-"))
        .count();
    if open >= MAX_POPOUTS {
        return Err(format!("too many windows (max {MAX_POPOUTS})"));
    }
    let url = nav::app_url_for(path).ok_or("path: unparsable")?;
    let (w, h, min_w, min_h) = kind.geometry();
    let title = match kind {
        WindowKind::Company => "Vero — Company",
        WindowKind::Chart => "Vero — Chart",
        WindowKind::Panel => "Vero",
        WindowKind::Ticker => "Vero — Ticker",
    };
    #[allow(unused_mut)]
    let mut builder = WebviewWindowBuilder::new(app, &label, WebviewUrl::External(url))
        .title(title)
        .inner_size(w, h)
        .min_inner_size(min_w, min_h)
        .always_on_top(kind == WindowKind::Ticker)
        .initialization_script(marker_script(kind.as_str()))
        .on_navigation(on_navigation_policy(app.clone()))
        .on_new_window(on_new_window_policy(app.clone()));
    builder = builder
        .theme(Some(tauri::Theme::Dark))
        .background_color(WINDOW_BG_DARK.into());
    #[cfg(target_os = "macos")]
    {
        builder = builder
            .title_bar_style(tauri::TitleBarStyle::Transparent)
            .hidden_title(true);
    }
    builder
        .build()
        .map_err(|e| format!("could not open window: {e}"))?;
    desk(app)
        .popout_kinds
        .lock()
        .unwrap()
        .insert(label.clone(), kind.as_str());
    Ok(label)
}

// ── Notifications / badge ───────────────────────────────────────────────────

/// Called when the main window gains focus. If a notification was shown in
/// the last few minutes, treat this focus as its click-through.
pub fn consume_pending_notification<R: Runtime>(app: &AppHandle<R>) {
    let pending = desk(app).pending_notification.lock().unwrap().take();
    if let Some((path, at)) = pending {
        if at.elapsed() <= NOTIFICATION_CLICK_WINDOW {
            navigate_main(app, path);
        }
    }
}

#[cfg(target_os = "windows")]
fn badge_dot() -> tauri::image::Image<'static> {
    // 16×16 filled circle in --bearish-adjacent red for the taskbar overlay
    // (Windows has no numeric badge API for unpackaged apps).
    let mut rgba = vec![0u8; 16 * 16 * 4];
    for y in 0..16 {
        for x in 0..16 {
            let (dx, dy) = (x as f32 - 7.5, y as f32 - 7.5);
            if dx * dx + dy * dy <= 56.0 {
                let i = (y * 16 + x) * 4;
                rgba[i..i + 4].copy_from_slice(&[0xd9, 0x3f, 0x3f, 0xff]);
            }
        }
    }
    tauri::image::Image::new_owned(rgba, 16, 16)
}

// ── CSV drag-and-drop ───────────────────────────────────────────────────────

/// A file dropped on any Vero window: the first `.csv` ≤ 2 MB is read and
/// handed to the web app as `vero://csv-dropped {name, text}` — on the window
/// it was dropped on if that is a web window, otherwise on the main window.
pub fn handle_drop<R: Runtime>(app: &AppHandle<R>, label: &str, paths: &[std::path::PathBuf]) {
    if !feature_on("csvdrop") {
        return;
    }
    for p in paths.iter().take(8) {
        let Some(name) = p.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
            continue;
        };
        if !validate::is_csv_name(&name) {
            continue;
        }
        let Ok(meta) = std::fs::metadata(p) else {
            continue;
        };
        if !meta.is_file() || meta.len() > validate::CSV_MAX {
            eprintln!("[vero] dropped CSV ignored (not a file or larger than 2 MB): {name}");
            continue;
        }
        let Ok(bytes) = std::fs::read(p) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text).to_string();
        let target = if label == "main" || label.starts_with("pop-") {
            label
        } else {
            "main"
        };
        if target == "main" {
            reveal(app);
        }
        let _ = app.emit_to(
            target,
            EV_CSV_DROPPED,
            json!({ "name": name, "text": text }),
        );
        return;
    }
}

// ── Settings side effects ───────────────────────────────────────────────────

pub fn apply_hotkey<R: Runtime>(app: &AppHandle<R>, wanted: &str) -> Result<(), String> {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};
    let d = desk(app);
    let mut current = d.hotkey.lock().unwrap();
    if current.as_deref() == Some(wanted) {
        return Ok(());
    }
    // Parse BEFORE touching the registered one, so a typo in the settings UI
    // leaves the working hotkey in place.
    let next: Option<Shortcut> = if wanted.is_empty() || !feature_on("hotkey") {
        None
    } else {
        Some(wanted.parse().map_err(|e| format!("global_hotkey: {e}"))?)
    };
    let old: Option<Shortcut> = current.as_deref().and_then(|s| s.parse().ok());
    if let Some(old) = old {
        let _ = app.global_shortcut().unregister(old);
    }
    match next {
        None => {
            *current = None;
            Ok(())
        }
        Some(shortcut) => match app.global_shortcut().register(shortcut) {
            Ok(()) => {
                *current = Some(wanted.to_string());
                Ok(())
            }
            Err(e) => {
                // Taken by another app: put the previous one back.
                if let Some(old) = old {
                    let _ = app.global_shortcut().register(old);
                }
                Err(format!("global_hotkey: could not register {wanted}: {e}"))
            }
        },
    }
}

pub fn apply_autostart<R: Runtime>(app: &AppHandle<R>, on: bool) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    if !feature_on("autostart") {
        return Ok(());
    }
    let launcher = app.autolaunch();
    let enabled = launcher.is_enabled().unwrap_or(false);
    if on && !enabled {
        launcher
            .enable()
            .map_err(|e| format!("launch_at_login: {e}"))?;
    } else if !on && enabled {
        launcher
            .disable()
            .map_err(|e| format!("launch_at_login: {e}"))?;
    }
    Ok(())
}

pub fn apply_tray_visibility<R: Runtime>(app: &AppHandle<R>, on: bool) {
    if let Some(tray) = app.tray_by_id(crate::tray::TRAY_ID) {
        let _ = tray.set_visible(on && feature_on("tray"));
    }
}

// ── Commands: shared helpers ────────────────────────────────────────────────

fn label_of<R: Runtime>(w: &tauri::WebviewWindow<R>) -> String {
    w.label().to_string()
}

fn data_dir<R: Runtime>(app: &AppHandle<R>) -> Result<std::path::PathBuf, String> {
    app.path().app_data_dir().map_err(|e| e.to_string())
}

fn config_dir<R: Runtime>(app: &AppHandle<R>) -> Result<std::path::PathBuf, String> {
    app.path().app_config_dir().map_err(|e| e.to_string())
}

/// Belt to the capability's braces: commands meant for local pages also check
/// the caller's URL at call time.
fn require_local<R: Runtime>(w: &tauri::WebviewWindow<R>) -> Result<(), String> {
    match w.url() {
        Ok(u) if nav::is_local_page(&u) => Ok(()),
        _ => Err("not allowed from this page".into()),
    }
}

// ── Commands: the contract ──────────────────────────────────────────────────

#[derive(Serialize)]
pub struct Features {
    tray: bool,
    hotkey: bool,
    notifications: bool,
    multiwindow: bool,
    deeplinks: bool,
    autostart: bool,
    cache: bool,
    vibrancy: bool,
    securestore: bool,
    csvdrop: bool,
}

#[derive(Serialize)]
pub struct Info {
    version: String,
    platform: &'static str,
    features: Features,
    /// `main`, `tray`, or the pop-out label.
    window: String,
    /// `main` | `tray` | `company` | `chart` | `panel` | `ticker`.
    window_kind: String,
    app_origin: String,
}

/// 1. `desktop_info({ bridge? })`. Calling it with `bridge: true` tells the
/// shell that this document's web bridge is LISTENING for `vero://navigate`
/// and `vero://navigate-history`, so those are emitted (client-side routing,
/// no reload) instead of performing a real navigation. Without the flag the
/// shell keeps navigating for real — which always works, just with a reload.
/// The flag resets on every page load.
#[tauri::command(rename_all = "snake_case")]
pub fn desktop_info<R: Runtime>(
    app: AppHandle<R>,
    window: tauri::WebviewWindow<R>,
    bridge: Option<bool>,
) -> Info {
    let label = label_of(&window);
    let on_local = window
        .url()
        .map(|u| nav::is_local_page(&u))
        .unwrap_or(false);
    if !on_local && bridge.unwrap_or(false) {
        desk(&app)
            .bridge_ready
            .lock()
            .unwrap()
            .insert(label.clone());
    }
    let s = desk(&app).settings.lock().unwrap().clone();
    let window_kind = match label.as_str() {
        "main" | "tray" => label.clone(),
        _ => desk_popout_kind(&window),
    };
    let platform = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    };
    Info {
        version: app.package_info().version.to_string(),
        platform,
        features: Features {
            tray: feature_on("tray") && s.tray,
            hotkey: feature_on("hotkey") && desk(&app).hotkey.lock().unwrap().is_some(),
            notifications: feature_on("notifications") && s.notifications,
            multiwindow: feature_on("multiwindow"),
            deeplinks: feature_on("deeplinks"),
            autostart: feature_on("autostart"),
            cache: feature_on("cache"),
            // App windows are opaque since 1.0 (only the tray popover keeps
            // its frosted backing), so the web app never sees vibrancy.
            vibrancy: false,
            securestore: desk(&app).securestore_available(),
            csvdrop: feature_on("csvdrop"),
        },
        window: label,
        window_kind,
        app_origin: nav::app_origin().as_str().trim_end_matches('/').to_string(),
    }
}

fn desk_popout_kind<R: Runtime>(window: &tauri::WebviewWindow<R>) -> String {
    desk(window.app_handle())
        .popout_kinds
        .lock()
        .unwrap()
        .get(window.label())
        .copied()
        .unwrap_or("panel")
        .to_string()
}

/// 2a. `desktop_cache_put(kind, json, as_of)`
#[tauri::command(rename_all = "snake_case")]
pub fn desktop_cache_put<R: Runtime>(
    app: AppHandle<R>,
    kind: String,
    json: String,
    as_of: String,
) -> Result<(), String> {
    if !feature_on("cache") {
        return Ok(());
    }
    let kind = CacheKind::parse(&kind)?;
    validate::cache_json(&json)?;
    let as_of = validate::text("as_of", &as_of, validate::AS_OF_MAX, false)?;
    store::cache_put(&data_dir(&app)?, kind, json, as_of)
}

/// 2b. `desktop_cache_get(kind) -> {json, as_of, saved_at} | null`
#[tauri::command(rename_all = "snake_case")]
pub fn desktop_cache_get<R: Runtime>(
    app: AppHandle<R>,
    kind: String,
) -> Result<Option<CacheEntry>, String> {
    if !feature_on("cache") {
        return Ok(None);
    }
    let kind = CacheKind::parse(&kind)?;
    Ok(store::cache_get(&data_dir(&app)?, kind))
}

/// 2c. `desktop_cache_clear()` — call on sign-out so the next person at this
/// machine does not see the last user's watchlist on the boot page.
#[tauri::command(rename_all = "snake_case")]
pub fn desktop_cache_clear<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    store::cache_clear(&data_dir(&app)?)
}

/// 3. `desktop_notify(title, body, target_path) -> shown`
#[tauri::command(rename_all = "snake_case")]
pub fn desktop_notify<R: Runtime>(
    app: AppHandle<R>,
    title: String,
    body: String,
    target_path: String,
) -> Result<bool, String> {
    use tauri_plugin_notification::NotificationExt;
    let title = validate::text("title", &title, 120, true)?;
    let body = validate::text("body", &body, 400, false)?;
    let path = nav::validate_app_path(&target_path)?;
    if !feature_on("notifications") || !desk(&app).settings.lock().unwrap().notifications {
        return Ok(false);
    }
    app.notification()
        .builder()
        .title(&title)
        .body(&body)
        .show()
        .map_err(|e| format!("notification failed: {e}"))?;
    *desk(&app).pending_notification.lock().unwrap() = Some((path, Instant::now()));
    Ok(true)
}

/// 4. `desktop_set_badge(count)` — dock (macOS) / launcher (Linux, Unity API)
/// count; a red overlay dot on the Windows taskbar. Mirrors into the tray
/// tooltip everywhere.
#[tauri::command(rename_all = "snake_case")]
pub fn desktop_set_badge<R: Runtime>(app: AppHandle<R>, count: u32) -> Result<(), String> {
    let count = count.min(9999);
    if let Some(main) = app.get_webview_window("main") {
        #[cfg(target_os = "windows")]
        {
            let _ = main.set_overlay_icon(if count > 0 { Some(badge_dot()) } else { None });
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = main.set_badge_count(if count > 0 { Some(count as i64) } else { None });
        }
    }
    if let Some(tray) = app.tray_by_id(crate::tray::TRAY_ID) {
        let tip = if count > 0 {
            format!("Vero — {count} new")
        } else {
            "Vero".to_string()
        };
        let _ = tray.set_tooltip(Some(tip));
    }
    Ok(())
}

/// 5a. `desktop_open_window(path, kind) -> label`. Async: creating a window
/// from a synchronous command deadlocks on Windows.
#[tauri::command(rename_all = "snake_case")]
pub async fn desktop_open_window<R: Runtime>(
    app: AppHandle<R>,
    path: String,
    kind: String,
) -> Result<String, String> {
    if !feature_on("multiwindow") {
        return Err("multiwindow is disabled".into());
    }
    let kind = WindowKind::parse(&kind)?;
    let path = nav::validate_app_path(&path)?;
    open_popout(&app, &path, kind)
}

/// 5b. `desktop_set_always_on_top(on)` — the CALLING window.
#[tauri::command(rename_all = "snake_case")]
pub fn desktop_set_always_on_top<R: Runtime>(
    window: tauri::WebviewWindow<R>,
    on: bool,
) -> Result<(), String> {
    window.set_always_on_top(on).map_err(|e| e.to_string())
}

/// 6. Keychain. `sb-*` keys only, values ≤ 16 KB.
#[tauri::command(rename_all = "snake_case")]
pub async fn desktop_secure_get<R: Runtime>(
    app: AppHandle<R>,
    key: String,
) -> Result<Option<String>, String> {
    let key = validate::secure_key(&key)?;
    if !desk(&app).securestore_available() {
        return Err("securestore unavailable".into());
    }
    crate::secure::get(&key)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn desktop_secure_set<R: Runtime>(
    app: AppHandle<R>,
    key: String,
    value: String,
) -> Result<(), String> {
    let key = validate::secure_key(&key)?;
    validate::secure_value(&value)?;
    if !desk(&app).securestore_available() {
        return Err("securestore unavailable".into());
    }
    crate::secure::set(&key, &value)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn desktop_secure_remove<R: Runtime>(
    app: AppHandle<R>,
    key: String,
) -> Result<(), String> {
    let key = validate::secure_key(&key)?;
    if !desk(&app).securestore_available() {
        return Err("securestore unavailable".into());
    }
    crate::secure::remove(&key)
}

/// 7a. `desktop_get_settings()`
#[tauri::command(rename_all = "snake_case")]
pub fn desktop_get_settings<R: Runtime>(app: AppHandle<R>) -> Settings {
    desk(&app).settings.lock().unwrap().clone()
}

/// 7b. `desktop_set_settings({launch_at_login, global_hotkey, notifications,
/// tray})` — any subset of the four keys, flat (as in the contract); a nested
/// `{settings: {...}}` is accepted too. Returns the settings now in effect.
/// The hotkey is validated by actually registering it: on failure nothing is
/// saved and the previous hotkey stays registered.
#[tauri::command(rename_all = "snake_case")]
pub fn desktop_set_settings<R: Runtime>(
    app: AppHandle<R>,
    launch_at_login: Option<bool>,
    global_hotkey: Option<String>,
    notifications: Option<bool>,
    tray: Option<bool>,
    settings: Option<Settings>,
) -> Result<Settings, String> {
    let d = desk(&app);
    let mut next = d.settings.lock().unwrap().clone();
    if let Some(s) = settings {
        next = s;
    }
    if let Some(v) = launch_at_login {
        next.launch_at_login = v;
    }
    if let Some(v) = global_hotkey {
        next.global_hotkey = v;
    }
    if let Some(v) = notifications {
        next.notifications = v;
    }
    if let Some(v) = tray {
        next.tray = v;
    }
    next.global_hotkey = validate::hotkey(&next.global_hotkey)?;

    apply_hotkey(&app, &next.global_hotkey)?;
    apply_autostart(&app, next.launch_at_login)?;
    apply_tray_visibility(&app, next.tray);
    store::save_settings(&config_dir(&app)?, &next)?;
    *d.settings.lock().unwrap() = next.clone();
    Ok(next)
}

// ── Commands: local pages only ──────────────────────────────────────────────

/// Tray / boot page: open the main window at an in-app path.
#[tauri::command(rename_all = "snake_case")]
pub fn desktop_navigate<R: Runtime>(
    app: AppHandle<R>,
    window: tauri::WebviewWindow<R>,
    path: String,
) -> Result<(), String> {
    require_local(&window)?;
    let path = nav::validate_app_path(&path)?;
    if window.label() == "tray" {
        let _ = window.hide();
    }
    navigate_main(&app, path);
    Ok(())
}

#[derive(Serialize)]
pub struct Probe {
    online: bool,
    app_origin: String,
}

/// Boot page: is the app origin reachable? (HEAD, 2.5 s timeout, on a worker
/// thread.) If so, the main window navigates to it right away. Called by the
/// boot page on load and then every 30 s ONLY while the page is visible — the
/// shell itself runs no timers.
#[tauri::command(rename_all = "snake_case")]
pub async fn desktop_probe<R: Runtime>(
    app: AppHandle<R>,
    window: tauri::WebviewWindow<R>,
    go: Option<bool>,
    force: Option<bool>,
) -> Result<Probe, String> {
    require_local(&window)?;
    if window.label() != "main" {
        return Err("not allowed from this window".into());
    }
    let origin = nav::app_origin().clone();
    // "Open Vero anyway" on the offline screen: skip the probe and let the
    // webview try (a proxy the probe cannot use may still work for it).
    if force.unwrap_or(false) {
        let path = desk(&app)
            .pending_nav
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| START_PATH.to_string());
        if let Some(url) = nav::app_url_for(&path) {
            let _ = window.navigate(url);
        }
        return Ok(Probe {
            online: true,
            app_origin: origin.as_str().trim_end_matches('/').to_string(),
        });
    }
    let probe_url = origin.join(START_PATH).map_err(|e| e.to_string())?;
    let online = tauri::async_runtime::spawn_blocking(move || probe(&probe_url))
        .await
        .unwrap_or(false);

    let d = desk(&app);
    let changed = d.online.lock().unwrap().replace(online) != Some(online);
    if changed {
        let _ = app.emit(EV_ONLINE, json!({ "online": online }));
    }
    eprintln!(
        "[vero] probe {} → {} (+{} ms)",
        origin,
        if online { "online" } else { "offline" },
        d.ms()
    );
    if online && go.unwrap_or(true) {
        let path = d
            .pending_nav
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| START_PATH.to_string());
        if let Some(url) = nav::app_url_for(&path) {
            let _ = window.navigate(url);
        }
    }
    Ok(Probe {
        online,
        app_origin: origin.as_str().trim_end_matches('/').to_string(),
    })
}

fn probe(url: &Url) -> bool {
    let loopback = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    let mut cfg = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_millis(2500)))
        .http_status_as_error(false)
        .max_redirects(0);
    if loopback {
        cfg = cfg.proxy(None);
    }
    let agent: ureq::Agent = cfg.build().into();
    match agent.head(url.as_str()).call() {
        // Any HTTP answer below 500 means the app is there (a 3xx/4xx on HEAD
        // is still a live server). 5xx = broken deploy: stay on the snapshot.
        Ok(resp) => resp.status().as_u16() < 500,
        Err(_) => false,
    }
}

/// Boot page: first paint done. Shows the window (it was created invisible)
/// and logs cold-start time — the number VERIFY.md reports.
#[tauri::command(rename_all = "snake_case")]
pub fn desktop_boot_painted<R: Runtime>(
    app: AppHandle<R>,
    window: tauri::WebviewWindow<R>,
    cached: Option<bool>,
) -> Result<(), String> {
    require_local(&window)?;
    let d = desk(&app);
    if !d.painted.swap(true, Ordering::SeqCst) {
        eprintln!(
            "[vero] boot_painted +{} ms since process start (snapshot: {})",
            d.ms(),
            if cached.unwrap_or(false) {
                "cached"
            } else {
                "empty"
            }
        );
        if !d.start_hidden {
            let _ = window.show();
            let _ = window.set_focus();
        }
    }
    Ok(())
}

/// Tray: hide the mini window (Esc).
#[tauri::command(rename_all = "snake_case")]
pub fn desktop_tray_hide<R: Runtime>(window: tauri::WebviewWindow<R>) -> Result<(), String> {
    require_local(&window)?;
    if window.label() == "tray" {
        let _ = window.hide();
    }
    Ok(())
}

/// Debug builds only (`--selftest-ipc`): print what the remote page reported.
#[tauri::command(rename_all = "snake_case")]
pub fn desktop_debug_report(text: String) {
    if cfg!(debug_assertions) {
        let t: String = text.chars().take(4000).collect();
        eprintln!("[vero:selftest] {t}");
    }
}
