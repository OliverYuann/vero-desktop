// Tray / menu-bar companion.
//
// Left-click (macOS, Windows) toggles a small frameless, always-on-top MINI
// WINDOW — a LOCAL page (dist/tray.html, strict CSP, no network) that shows
// the cached watchlist, today's alerts and events, and a quick Assistant box.
// Linux tray hosts (AppIndicator) never deliver left-clicks, only the menu,
// so the menu carries "Quick View" too.
//
// The mini window is created lazily on first use and then kept (hidden), so
// an app that never opens it never pays for a second webview.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use serde_json::json;
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, PhysicalPosition, Rect, Runtime, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};

use crate::app::{self, desk, EV_TRAY_ASSISTANT, EV_TRAY_SHOWN};

pub const TRAY_ID: &str = "vero-tray";
pub const TRAY_WINDOW: &str = "tray";
const SIZE: (f64, f64) = (360.0, 520.0);

pub fn build_tray<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open Vero", true, None::<&str>)?;
    let quick = MenuItem::with_id(app, "quick", "Quick View", true, None::<&str>)?;
    let assistant = MenuItem::with_id(app, "assistant", "Quick Assistant…", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Vero", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(
        app,
        &[&open, &quick, &assistant, &sep1, &settings, &sep2, &quit],
    )?;

    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or(tauri::Error::InvalidIcon(std::io::Error::other(
            "no default window icon configured",
        )))?;

    let visible = desk(app).settings.lock().unwrap().tray && app::feature_on("tray");
    let tray = TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .tooltip("Vero")
        .menu(&menu)
        // Left-click is the mini window; the menu stays on right-click.
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| handle_menu(app, event.id.as_ref()))
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                rect,
                ..
            } = event
            {
                toggle(tray.app_handle(), Some(rect));
            }
        })
        .build(app)?;
    let _ = tray.set_visible(visible);
    Ok(())
}

/// Menu ids shared by the tray menu and the app menu.
pub fn handle_menu<R: Runtime>(app: &AppHandle<R>, id: &str) {
    match id {
        "open" => {
            hide(app);
            app::reveal(app);
            app::consume_pending_notification(app);
        }
        "quick" => toggle(app, None),
        "assistant" => {
            show(app, None);
            let _ = app.emit_to(TRAY_WINDOW, EV_TRAY_ASSISTANT, json!({}));
        }
        "settings" => {
            hide(app);
            app::navigate_main(app, SETTINGS_PATH.to_string());
        }
        "quit" => app.exit(0),
        _ => {}
    }
}

/// Where "Settings…" lands in the web app. The desktop settings UI lives in
/// the web app (D2); this is the path it agreed to render it at.
pub const SETTINGS_PATH: &str = "/account?section=desktop";

fn window<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<WebviewWindow<R>> {
    if let Some(w) = app.get_webview_window(TRAY_WINDOW) {
        return Ok(w);
    }
    // Vibrancy where the OS has it: macOS HUD material, Windows 11 Mica
    // (Acrylic first so Windows 10 still gets a blur). The page switches to a
    // translucent background only when it sees `?vibrant` — elsewhere it is
    // opaque and never shows the desktop through a transparent window.
    let vibrant =
        cfg!(any(target_os = "macos", target_os = "windows")) && app::feature_on("vibrancy");
    let page = if vibrant {
        "tray.html?vibrant"
    } else {
        "tray.html"
    };

    #[allow(unused_mut)]
    let mut builder = WebviewWindowBuilder::new(app, TRAY_WINDOW, WebviewUrl::App(page.into()))
        .title("Vero")
        .inner_size(SIZE.0, SIZE.1)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        .shadow(true)
        // A local page that may never leave itself.
        .on_navigation(crate::nav::is_local_page)
        .on_new_window(|_, _| tauri::webview::NewWindowResponse::Deny);

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    if vibrant {
        use tauri::window::{Effect, EffectState, EffectsBuilder};
        #[cfg(target_os = "macos")]
        let effects = EffectsBuilder::new()
            .effect(Effect::HudWindow)
            .state(EffectState::Active)
            .radius(12.0)
            .build();
        #[cfg(target_os = "windows")]
        let effects = {
            let _ = EffectState::Active;
            EffectsBuilder::new()
                .effects([Effect::Acrylic, Effect::Mica])
                .build()
        };
        builder = builder.transparent(true).effects(effects);
    }
    builder.build()
}

/// Put the window next to the tray icon: centred under it when the icon is in
/// the top half of its monitor (macOS menu bar, top panels), above it when in
/// the bottom half (Windows taskbar), clamped to the monitor.
fn place<R: Runtime>(w: &WebviewWindow<R>, anchor: Option<Rect>) {
    let Some(rect) = anchor else {
        let _ = w.center();
        return;
    };
    let scale = w.scale_factor().unwrap_or(1.0);
    let pos = rect.position.to_physical::<f64>(scale);
    let size = rect.size.to_physical::<f64>(scale);
    let win = w
        .outer_size()
        .map(|s| (s.width as f64, s.height as f64))
        .unwrap_or((SIZE.0 * scale, SIZE.1 * scale));
    let monitor = w
        .monitor_from_point(pos.x, pos.y)
        .ok()
        .flatten()
        .or_else(|| w.primary_monitor().ok().flatten());
    let Some(m) = monitor else {
        let _ = w.center();
        return;
    };
    let (mx, my) = (m.position().x as f64, m.position().y as f64);
    let (mw, mh) = (m.size().width as f64, m.size().height as f64);
    let margin = 8.0 * scale;
    let mut x = pos.x + size.width / 2.0 - win.0 / 2.0;
    x = x.clamp(mx + margin, (mx + mw - win.0 - margin).max(mx + margin));
    let y = if pos.y < my + mh / 2.0 {
        pos.y + size.height + 6.0 * scale
    } else {
        pos.y - win.1 - 6.0 * scale
    };
    let y = y.clamp(my + margin, (my + mh - win.1 - margin).max(my + margin));
    let _ = w.set_position(PhysicalPosition::new(x, y));
}

pub fn show<R: Runtime>(app: &AppHandle<R>, anchor: Option<Rect>) {
    let Ok(w) = window(app) else { return };
    place(&w, anchor);
    let _ = w.show();
    let _ = w.set_focus();
    let _ = app.emit_to(TRAY_WINDOW, EV_TRAY_SHOWN, json!({}));
}

pub fn hide<R: Runtime>(app: &AppHandle<R>) {
    if let Some(w) = app.get_webview_window(TRAY_WINDOW) {
        let _ = w.hide();
    }
}

pub fn toggle<R: Runtime>(app: &AppHandle<R>, anchor: Option<Rect>) {
    let visible = app
        .get_webview_window(TRAY_WINDOW)
        .and_then(|w| w.is_visible().ok())
        .unwrap_or(false);
    if visible {
        hide(app);
        return;
    }
    // Clicking the tray icon while the mini window is open first blurs the
    // window (which hides it), then delivers the click. Without this guard
    // that click would immediately re-open it.
    let recently_hidden = desk(app)
        .tray_hidden_at
        .lock()
        .unwrap()
        .is_some_and(|t| t.elapsed() < Duration::from_millis(300));
    if !recently_hidden {
        show(app, anchor);
    }
}

/// Mini window lost focus → hide, like every menu-bar popover. Pinned mode
/// (`--show-tray-window`) keeps it up for screenshots and tests.
pub fn on_blur<R: Runtime>(app: &AppHandle<R>) {
    if desk(app).tray_pinned.load(Ordering::SeqCst) {
        return;
    }
    *desk(app).tray_hidden_at.lock().unwrap() = Some(Instant::now());
    hide(app);
}
