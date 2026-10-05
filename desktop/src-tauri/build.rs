// Declares the app's own IPC commands so Tauri generates an `allow-<command>`
// permission for each. Declaring an app manifest at all is what switches the
// ACL ON for app commands: without it, any LOCAL page could call every
// command. With it, each window only reaches what its capability lists
// (capabilities/*.json for the local pages; the two remote capabilities are
// built at runtime in main.rs because the app origin is configurable).

const COMMANDS: &[&str] = &[
    // remote (the Vero web app) — see README "IPC contract"
    "desktop_info",
    "desktop_cache_put",
    "desktop_cache_get",
    "desktop_cache_clear",
    "desktop_notify",
    "desktop_set_badge",
    "desktop_open_window",
    "desktop_set_always_on_top",
    "desktop_secure_get",
    "desktop_secure_set",
    "desktop_secure_remove",
    "desktop_get_settings",
    "desktop_set_settings",
    // local pages only (boot page, tray mini window)
    "desktop_navigate",
    "desktop_probe",
    "desktop_boot_painted",
    "desktop_tray_hide",
    // debug builds only: lets `--selftest-ipc` print what the remote page saw
    "desktop_debug_report",
];

fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("failed to run tauri-build");
}
