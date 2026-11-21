//! Background auto-update (compiled only with `--features updater`).
//!
//! Inert unless the build also carried `plugins.updater` (a public key and
//! the GitHub Releases `latest.json` endpoint) — CI merges
//! `tauri.updater.conf.json` for that only when the updater signing secret
//! exists (SIGNING.md §3). Twenty seconds after start-up it asks the endpoint
//! once; an update is downloaded, its signature checked against the public key
//! by the plugin, and installed. macOS and Linux swap the app in place and run
//! the new version on the next launch; the Windows installer (passive mode)
//! restarts Vero itself. Every failure is logged and otherwise ignored: an
//! update is never worth interrupting someone. Kill switch:
//! `VERO_DISABLE=updater`.

use std::time::Duration;

use tauri::{AppHandle, Runtime};
use tauri_plugin_updater::UpdaterExt;

pub fn spawn_check<R: Runtime>(app: AppHandle<R>) {
    if !crate::app::feature_on("updater") {
        return;
    }
    tauri::async_runtime::spawn(async move {
        let _ =
            tauri::async_runtime::spawn_blocking(|| std::thread::sleep(Duration::from_secs(20)))
                .await;
        let updater = match app.updater() {
            Ok(u) => u,
            Err(e) => {
                eprintln!("[vero] updater unavailable: {e}");
                return;
            }
        };
        match updater.check().await {
            Ok(Some(update)) => {
                eprintln!(
                    "[vero] update {} available (running {})",
                    update.version, update.current_version
                );
                match update.download_and_install(|_, _| {}, || {}).await {
                    Ok(()) => eprintln!("[vero] update {} installed", update.version),
                    Err(e) => eprintln!("[vero] update failed: {e}"),
                }
            }
            Ok(None) => eprintln!("[vero] up to date"),
            Err(e) => eprintln!("[vero] update check failed: {e}"),
        }
    });
}
