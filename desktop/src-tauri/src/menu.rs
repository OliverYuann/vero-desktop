// The application menu: Vero / Edit / View / Window / Help.
//
// macOS: the app-wide menu bar (and the Edit menu is what makes ⌘C/⌘V/⌘A work
// inside a WKWebView at all). Windows/Linux: attached to the MAIN window only,
// so the tray mini window and pop-outs stay chrome-free; the accelerators are
// what make Ctrl+[ / Ctrl+] / Ctrl+R work there.
//
// The command palette item has no accelerator on purpose: the web app already
// handles ⌘K/Ctrl+K itself, and a menu accelerator would swallow the key
// before the page sees it.

use serde_json::json;
use tauri::{
    menu::{AboutMetadata, Menu, MenuItem, PredefinedMenuItem, Submenu},
    AppHandle, Emitter, Manager, Runtime,
};

use crate::app::{self, desk, EV_COMMAND_PALETTE, EV_NAVIGATE_HISTORY};

// Installed on macOS only (main.rs): elsewhere a native menu is an in-window strip.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, Some("CmdOrCtrl+,"))?;
    let quit = MenuItem::with_id(app, "quit", "Quit Vero", true, Some("CmdOrCtrl+Q"))?;

    #[cfg(target_os = "macos")]
    let vero = Submenu::with_items(
        app,
        "Vero",
        true,
        &[
            &PredefinedMenuItem::about(app, Some("About Vero"), Some(AboutMetadata::default()))?,
            &PredefinedMenuItem::separator(app)?,
            &settings,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::services(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, None)?,
            &PredefinedMenuItem::hide_others(app, None)?,
            &PredefinedMenuItem::show_all(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;
    #[cfg(not(target_os = "macos"))]
    let vero = Submenu::with_items(
        app,
        "Vero",
        true,
        &[
            &settings,
            &PredefinedMenuItem::about(app, Some("About Vero"), Some(AboutMetadata::default()))?,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    let edit = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;

    let view = Submenu::with_items(
        app,
        "View",
        true,
        &[
            &MenuItem::with_id(app, "back", "Back", true, Some("CmdOrCtrl+["))?,
            &MenuItem::with_id(app, "forward", "Forward", true, Some("CmdOrCtrl+]"))?,
            &MenuItem::with_id(app, "reload", "Reload", true, Some("CmdOrCtrl+R"))?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "palette", "Command Palette", true, None::<&str>)?,
            &MenuItem::with_id(app, "quick", "Quick View", true, Some("CmdOrCtrl+Shift+V"))?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::fullscreen(app, None)?,
        ],
    )?;

    let window = Submenu::with_items(
        app,
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(app, None)?,
            &PredefinedMenuItem::maximize(app, Some("Zoom"))?,
            &MenuItem::with_id(app, "on-top", "Keep on Top", true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "open", "Vero", true, Some("CmdOrCtrl+0"))?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::close_window(app, None)?,
        ],
    )?;

    let help = Submenu::with_items(
        app,
        "Help",
        true,
        &[
            &MenuItem::with_id(app, "help", "Vero Help", true, None::<&str>)?,
            &MenuItem::with_id(app, "disclaimer", "Disclaimer", true, None::<&str>)?,
        ],
    )?;

    Menu::with_items(app, &[&vero, &edit, &view, &window, &help])
}

/// The window the user is looking at (focused), falling back to main.
fn focused_label<R: Runtime>(app: &AppHandle<R>) -> String {
    app.webview_windows()
        .into_iter()
        .find(|(label, w)| label != crate::tray::TRAY_WINDOW && w.is_focused().unwrap_or(false))
        .map(|(label, _)| label)
        .unwrap_or_else(|| "main".to_string())
}

pub fn handle<R: Runtime>(app: &AppHandle<R>, id: &str) {
    match id {
        "back" | "forward" => {
            let label = focused_label(app);
            if desk(app).bridge_ready.lock().unwrap().contains(&label) {
                let _ = app.emit_to(label.as_str(), EV_NAVIGATE_HISTORY, json!({ "dir": id }));
            } else if let Some(w) = app.get_webview_window(&label) {
                let _ = w.eval(if id == "back" {
                    "history.back()"
                } else {
                    "history.forward()"
                });
            }
        }
        "reload" => {
            if let Some(w) = app.get_webview_window(&focused_label(app)) {
                let _ = w.eval("location.reload()");
            }
        }
        "palette" => {
            app::reveal(app);
            let _ = app.emit_to("main", EV_COMMAND_PALETTE, json!({}));
        }
        "on-top" => {
            if let Some(w) = app.get_webview_window(&focused_label(app)) {
                let on = w.is_always_on_top().unwrap_or(false);
                let _ = w.set_always_on_top(!on);
            }
        }
        "help" => app::hand_off(app, &format!("{}faq", crate::nav::app_origin())),
        "disclaimer" => app::navigate_main(app, "/disclaimer".into()),
        other => crate::tray::handle_menu(app, other),
    }
}
