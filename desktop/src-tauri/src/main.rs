// Vero desktop app.
//
// Still a native window onto the deployed web app — no Supabase client, no
// Stripe/OpenAI key, no server functions, nothing secret compiled in. What the
// 0.2 shell adds is the native layer around that window:
//
//   - a tray / menu-bar companion with a local mini window   (tray.rs)
//   - a global hotkey that summons the command palette
//   - native notifications + dock/taskbar badge
//   - pop-out windows that remember their size and position
//   - `vero://` deep links (stock / alert / assistant / open / auth-callback)
//   - launch at login (off by default), close-to-tray
//   - an app menu, system light/dark, vibrancy where the OS has it
//   - an instant LOCAL boot page with an offline snapshot      (dist/)
//   - an OS-keychain store for the Supabase session
//
// THE SECURITY MODEL IN ONE PARAGRAPH. The app origin (production, or
// `VERO_APP_URL`) gets exactly the app's own `desktop_*` commands through two
// RUNTIME capabilities (`main-remote`, `popouts-remote`, below) — no fs, shell,
// http or notification plugin permissions; plugins are only ever driven from
// Rust inside validated commands. Every other origin gets nothing. The local
// pages (boot, tray) get a handful of read-only commands via
// capabilities/*.json. Every command validates its input (validate.rs, nav.rs).
// See README.md "Security model".

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod menu;
mod nav;
mod secure;
mod store;
mod tray;
mod validate;

use std::sync::atomic::Ordering;
use std::time::Instant;

#[cfg(target_os = "macos")]
use tauri::RunEvent;
use tauri::{ipc::CapabilityBuilder, webview::PageLoadEvent, DragDropEvent, Manager, WindowEvent};
use tauri_plugin_deep_link::DeepLinkExt;

use app::{desk, Desk};

/// Commands the app origin's MAIN window may call.
const MAIN_REMOTE_PERMISSIONS: &[&str] = &[
    "allow-desktop-info",
    "allow-desktop-cache-put",
    "allow-desktop-cache-get",
    "allow-desktop-cache-clear",
    "allow-desktop-notify",
    "allow-desktop-set-badge",
    "allow-desktop-open-window",
    "allow-desktop-set-always-on-top",
    "allow-desktop-secure-get",
    "allow-desktop-secure-set",
    "allow-desktop-secure-remove",
    "allow-desktop-get-settings",
    "allow-desktop-set-settings",
    // The bridge listens for vero://navigate & co.
    "core:event:allow-listen",
    "core:event:allow-unlisten",
    // `data-tauri-drag-region` (drag + double-click to zoom), and the title
    // bar following the app's light/dark theme. Nothing else from the window
    // plugin.
    "core:window:allow-start-dragging",
    "core:window:allow-internal-toggle-maximize",
    "core:window:allow-set-theme",
    "core:window:allow-set-background-color",
    "core:webview:allow-set-webview-background-color",
];

/// Commands the app origin may call from a POP-OUT window: the same web app
/// (so it needs the session store and the cache), but no notifications,
/// badge or settings — those belong to the main window.
const POPOUT_REMOTE_PERMISSIONS: &[&str] = &[
    "allow-desktop-info",
    "allow-desktop-cache-put",
    "allow-desktop-cache-get",
    "allow-desktop-open-window",
    "allow-desktop-set-always-on-top",
    "allow-desktop-secure-get",
    "allow-desktop-secure-set",
    "allow-desktop-secure-remove",
    "core:event:allow-listen",
    "core:event:allow-unlisten",
    "core:window:allow-start-dragging",
    "core:window:allow-internal-toggle-maximize",
    "core:window:allow-set-theme",
    "core:window:allow-set-background-color",
    "core:webview:allow-set-webview-background-color",
];

struct Flags {
    hidden: bool,
    show_tray_window: bool,
    selftest_ipc: bool,
}

fn parse_flags<I: IntoIterator<Item = String>>(args: I) -> Flags {
    let mut f = Flags {
        hidden: false,
        show_tray_window: false,
        selftest_ipc: false,
    };
    for a in args {
        match a.as_str() {
            // Passed by the autostart entry: start in the tray, no window.
            "--hidden" => f.hidden = true,
            // Opens the tray mini window centred and pinned (no hide on blur).
            // For screenshots/tests on hosts where the tray cannot be clicked.
            "--show-tray-window" => f.show_tray_window = true,
            // Debug builds only: exercise the IPC grant from the remote page.
            "--selftest-ipc" => f.selftest_ipc = cfg!(debug_assertions),
            _ => {}
        }
    }
    f
}

/// Debug-only: run from the REMOTE page once it has loaded, to prove what the
/// app origin can and cannot reach. Results are printed by
/// `desktop_debug_report` (a debug-only command).
const SELFTEST_JS: &str = r#"(async () => {
  const I = window.__TAURI_INTERNALS__; const r = { href: location.href };
  const t = async (name, cmd, args) => { try { r[name] = { ok: await I.invoke(cmd, args || {}) }; } catch (e) { r[name] = { denied: String(e).slice(0, 160) }; } };
  await t('info', 'desktop_info');
  await t('cache_put', 'desktop_cache_put', { kind: 'portfolio', json: '{"selftest":true}', as_of: new Date().toISOString() });
  await t('cache_get', 'desktop_cache_get', { kind: 'portfolio' });
  await t('cache_put_bad_kind', 'desktop_cache_put', { kind: '../settings', json: '{}', as_of: '' });
  await t('notify_bad_path', 'desktop_notify', { title: 'x', body: '', target_path: '//evil.example.com' });
  await t('settings', 'desktop_get_settings');
  await t('secure_bad_key', 'desktop_secure_get', { key: 'not-supabase' });
  await t('secure_roundtrip_set', 'desktop_secure_set', { key: 'sb-selftest-auth-token', value: 'x'.repeat(3000) });
  await t('secure_roundtrip_get_len', 'desktop_secure_get', { key: 'sb-selftest-auth-token' });
  if (r.secure_roundtrip_get_len && typeof r.secure_roundtrip_get_len.ok === 'string') r.secure_roundtrip_get_len.ok = r.secure_roundtrip_get_len.ok.length;
  await t('secure_remove', 'desktop_secure_remove', { key: 'sb-selftest-auth-token' });
  await t('LOCAL_ONLY_probe', 'desktop_probe');
  await t('LOCAL_ONLY_navigate', 'desktop_navigate', { path: '/dashboard' });
  await t('PLUGIN_notification', 'plugin:notification|notify', { options: { title: 'x' } });
  await t('PLUGIN_opener', 'plugin:opener|open_url', { url: 'https://example.com' });
  await t('PLUGIN_window_close', 'plugin:window|close', {});
  await t('notify', 'desktop_notify', { title: 'NVDA moved +4.1%', body: 'Selftest notification', target_path: '/alerts' });
  await t('badge', 'desktop_set_badge', { count: 3 });
  await t('hotkey_bad', 'desktop_set_settings', { global_hotkey: 'NotAKey+Space' });
  await t('hotkey_change', 'desktop_set_settings', { global_hotkey: 'Ctrl+Shift+Space' });
  await t('hotkey_restore', 'desktop_set_settings', { global_hotkey: 'Alt+Space' });
  await t('popout_bad', 'desktop_open_window', { path: '/pricing', kind: 'company' });
  await t('popout', 'desktop_open_window', { path: '/company/NVDA', kind: 'company' });
  await I.invoke('desktop_debug_report', { text: JSON.stringify(r) });
})();"#;

fn main() {
    let t0 = Instant::now();
    let flags = parse_flags(std::env::args().skip(1));

    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default();

    // Single-instance MUST be the first plugin (plugin docs). Its callback runs
    // in the ALREADY-RUNNING instance with the new process's argv — which on
    // Windows and Linux is where a `vero://` link arrives (the `deep-link`
    // feature forwards it to `on_open_url` below). macOS routes URLs to the
    // running process itself and neither needs nor builds this plugin.
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            // A link in argv is handled (and the window revealed) by the
            // deep-link listener; a plain second launch just surfaces Vero.
            let has_link = argv
                .iter()
                .any(|a| a.to_ascii_lowercase().starts_with("vero:"));
            let f = parse_flags(argv.into_iter().skip(1));
            if f.show_tray_window {
                desk(app).tray_pinned.store(true, Ordering::SeqCst);
                tray::show(app, None);
            } else if !has_link {
                app::reveal(app);
            }
        }));
    }

    builder = builder
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(
            tauri_plugin_window_state::Builder::default()
                // The tray window is positioned at the icon every time.
                .with_denylist(&[tray::TRAY_WINDOW])
                // Never let a restored state SHOW a window: the main window
                // stays hidden until the boot page paints, or for --hidden.
                .with_state_flags(
                    tauri_plugin_window_state::StateFlags::all()
                        & !tauri_plugin_window_state::StateFlags::VISIBLE,
                )
                .build(),
        )
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == tauri_plugin_global_shortcut::ShortcutState::Pressed {
                        app::reveal(app);
                        use tauri::Emitter;
                        let _ = app.emit_to("main", app::EV_COMMAND_PALETTE, serde_json::json!({}));
                    }
                })
                .build(),
        );

    // Auto-updater: compiled only with `--features updater`, and inert until
    // tauri.conf.json carries `plugins.updater` (pubkey + endpoints). Neither
    // exists today, deliberately. See README "Auto-update" / SIGNING.md.
    #[cfg(feature = "updater")]
    {
        builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    }

    let selftest = flags.selftest_ipc;
    let app = builder
        .invoke_handler(tauri::generate_handler![
            app::desktop_info,
            app::desktop_cache_put,
            app::desktop_cache_get,
            app::desktop_cache_clear,
            app::desktop_notify,
            app::desktop_set_badge,
            app::desktop_open_window,
            app::desktop_set_always_on_top,
            app::desktop_secure_get,
            app::desktop_secure_set,
            app::desktop_secure_remove,
            app::desktop_get_settings,
            app::desktop_set_settings,
            app::desktop_navigate,
            app::desktop_probe,
            app::desktop_boot_painted,
            app::desktop_tray_hide,
            app::desktop_debug_report,
        ])
        .on_page_load(move |webview, payload| {
            let label = webview.label().to_string();
            match payload.event() {
                // A new document: its bridge has not announced itself yet.
                PageLoadEvent::Started => {
                    desk(webview.app_handle())
                        .bridge_ready
                        .lock()
                        .unwrap()
                        .remove(&label);
                }
                PageLoadEvent::Finished => {
                    let remote = !nav::is_local_page(payload.url());
                    if label == "main" && remote {
                        eprintln!(
                            "[vero] main window on {} (+{} ms)",
                            nav::path_of(payload.url()),
                            desk(webview.app_handle()).ms()
                        );
                    }
                    if selftest && label == "main" && remote && payload.url().path() != "/" {
                        let _ = webview.eval(SELFTEST_JS);
                    }
                }
            }
        })
        .on_window_event(|window, event| {
            let app = window.app_handle();
            let label = window.label();
            match event {
                WindowEvent::CloseRequested { api, .. } if label == "main" => {
                    // Close = hide to the tray; the signed-in page keeps
                    // running (that is the background refresh). Quit lives in
                    // the tray and app menus. Without a tray, Windows/Linux
                    // quit instead — otherwise nothing could bring it back.
                    let tray_on =
                        desk(app).settings.lock().unwrap().tray && app::feature_on("tray");
                    if tray_on || cfg!(target_os = "macos") {
                        api.prevent_close();
                        let _ = window.hide();
                    } else {
                        app.exit(0);
                    }
                }
                WindowEvent::Focused(true) if label == "main" => {
                    // Returning from the browser (Stripe checkout, portal):
                    // nudge TanStack Query's focus manager, without a reload.
                    if let Some(webview) = app.get_webview_window("main") {
                        if webview
                            .url()
                            .map(|u| !nav::is_local_page(&u))
                            .unwrap_or(false)
                        {
                            let _ = webview.eval(
                                "try{window.dispatchEvent(new Event('focus'));\
                                 document.dispatchEvent(new Event('visibilitychange'));}catch(e){}",
                            );
                        }
                    }
                    app::consume_pending_notification(app);
                }
                WindowEvent::Focused(false) if label == tray::TRAY_WINDOW => tray::on_blur(app),
                WindowEvent::DragDrop(DragDropEvent::Drop { paths, .. }) => {
                    app::handle_drop(app, label, paths);
                }
                _ => {}
            }
        })
        .on_menu_event(|app, event| menu::handle(app, event.id.as_ref()))
        .setup(move |app| {
            let handle = app.handle().clone();
            let config_dir = app.path().app_config_dir()?;
            let settings = store::load_settings(&config_dir);
            app.manage(Desk::new(t0, flags.hidden, settings.clone()));

            // ── Remote capabilities: the app origin, and only it ────────────
            let patterns = nav::remote_url_patterns(nav::app_origin());
            let mut main_cap = CapabilityBuilder::new("main-remote")
                .local(false)
                .window("main");
            let mut pop_cap = CapabilityBuilder::new("popouts-remote")
                .local(false)
                .window("pop-*");
            for p in &patterns {
                main_cap = main_cap.remote(p.clone());
                pop_cap = pop_cap.remote(p.clone());
            }
            for perm in MAIN_REMOTE_PERMISSIONS {
                main_cap = main_cap.permission(*perm);
            }
            for perm in POPOUT_REMOTE_PERMISSIONS {
                pop_cap = pop_cap.permission(*perm);
            }
            #[cfg(debug_assertions)]
            {
                main_cap = main_cap.permission("allow-desktop-debug-report");
            }
            app.add_capability(main_cap)?;
            app.add_capability(pop_cap)?;
            eprintln!(
                "[vero] app origin {} (IPC granted to {:?})",
                nav::app_origin(),
                patterns
            );

            // ── Windows, menu, tray ─────────────────────────────────────────
            let main = app::build_main_window(&handle)?;
            // The menu lives in the macOS system menu bar. On Windows and Linux
            // a native menu is a light strip INSIDE the window above the dark
            // app, so there is none: everything in it is in the tray menu, the
            // web app's own shortcuts, or the webview's built-in keys.
            #[cfg(target_os = "macos")]
            {
                let _ = main;
                app.set_menu(menu::build(&handle)?)?;
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = main;
            }
            tray::build_tray(&handle)?;

            // ── Settings side effects ───────────────────────────────────────
            if let Err(e) = app::apply_hotkey(&handle, &settings.global_hotkey) {
                eprintln!("[vero] {e}");
            }
            if settings.launch_at_login {
                // Re-assert (the binary may have moved since it was enabled).
                if let Err(e) = app::apply_autostart(&handle, true) {
                    eprintln!("[vero] {e}");
                }
            }

            // ── Deep links ──────────────────────────────────────────────────
            // `register_all` only in debug: in a release build it would point
            // the scheme at wherever this binary happens to be (see README).
            #[cfg(debug_assertions)]
            {
                if let Err(err) = app.deep_link().register_all() {
                    eprintln!("[vero] dev-only deep link registration failed: {err}");
                }
            }
            // Cold start: launched BY a link.
            if let Ok(Some(urls)) = app.deep_link().get_current() {
                if let Some(url) = urls.first() {
                    app::handle_deep_link(&handle, url.as_str());
                }
            }
            // Warm: a link while running (single-instance forwards argv here).
            let link_app = handle.clone();
            app.deep_link().on_open_url(move |event| {
                for url in event.urls() {
                    app::handle_deep_link(&link_app, url.as_str());
                }
            });

            if flags.show_tray_window {
                desk(&handle).tray_pinned.store(true, Ordering::SeqCst);
                tray::show(&handle, None);
            }
            eprintln!("[vero] setup done +{} ms", desk(&handle).ms());
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build the Vero desktop app");

    #[allow(unused_variables)]
    app.run(|app, event| {
        // macOS: clicking the Dock icon with every window hidden.
        #[cfg(target_os = "macos")]
        if let RunEvent::Reopen { .. } = event {
            app::reveal(app);
            app::consume_pending_notification(app);
        }
    });
}
