// Small on-disk stores: the offline snapshot cache and the desktop settings.
//
// Plain JSON files, written atomically (temp file + rename) so a crash or a
// power cut mid-write leaves the previous version, never half a file. No
// SQLite: five documents and one settings object do not need a database.
//
//   <app data dir>/cache/<kind>.json   { json, as_of, saved_at }
//   <app config dir>/settings.json     Settings
//
// The cache holds what the signed-in user last saw (watchlist, alerts, …) so
// the boot page and the tray can render it instantly and offline. It is the
// user's own data on the user's own disk, unencrypted, same as a browser's
// IndexedDB would be; `desktop_cache_clear` wipes it (the web side calls that
// on sign-out).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::validate::CacheKind;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    /// The JSON document exactly as the web app supplied it (a string).
    pub json: String,
    /// The web app's own "data as of" label (ISO timestamp).
    pub as_of: String,
    /// When this shell wrote it, ISO-8601 UTC.
    pub saved_at: String,
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

pub fn cache_path(data_dir: &Path, kind: CacheKind) -> PathBuf {
    data_dir
        .join("cache")
        .join(format!("{}.json", kind.file_stem()))
}

pub fn cache_put(
    data_dir: &Path,
    kind: CacheKind,
    json: String,
    as_of: String,
) -> Result<(), String> {
    let entry = CacheEntry {
        json,
        as_of,
        saved_at: now_iso(),
    };
    let bytes = serde_json::to_vec(&entry).map_err(|e| e.to_string())?;
    write_atomic(&cache_path(data_dir, kind), &bytes)
        .map_err(|e| format!("cache write failed: {e}"))
}

pub fn cache_get(data_dir: &Path, kind: CacheKind) -> Option<CacheEntry> {
    let bytes = std::fs::read(cache_path(data_dir, kind)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn cache_clear(data_dir: &Path) -> Result<(), String> {
    for kind in CacheKind::ALL {
        let p = cache_path(data_dir, kind);
        if p.exists() {
            std::fs::remove_file(&p).map_err(|e| format!("cache clear failed: {e}"))?;
        }
    }
    Ok(())
}

/// Desktop-only preferences. Field names are the IPC contract
/// (`desktop_get_settings` / `desktop_set_settings`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Launch at login (tauri-plugin-autostart). OFF by default.
    pub launch_at_login: bool,
    /// Global shortcut that shows Vero and opens the command palette.
    /// Empty string = none.
    pub global_hotkey: String,
    /// Native notifications from `desktop_notify`.
    pub notifications: bool,
    /// Tray / menu-bar icon. When off, closing the main window quits
    /// (Windows/Linux) — otherwise the app would be running invisibly.
    pub tray: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            launch_at_login: false,
            global_hotkey: "Alt+Space".into(),
            notifications: true,
            tray: true,
        }
    }
}

pub fn settings_path(config_dir: &Path) -> PathBuf {
    config_dir.join("settings.json")
}

pub fn load_settings(config_dir: &Path) -> Settings {
    std::fs::read(settings_path(config_dir))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save_settings(config_dir: &Path, s: &Settings) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(s).map_err(|e| e.to_string())?;
    write_atomic(&settings_path(config_dir), &bytes)
        .map_err(|e| format!("settings write failed: {e}"))
}

/// ISO-8601 UTC timestamp without pulling in a date crate.
pub fn now_iso() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    iso_from_unix(secs)
}

fn iso_from_unix(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_dates() {
        assert_eq!(iso_from_unix(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_from_unix(1_790_000_000), "2026-09-21T14:13:20Z");
        assert_eq!(iso_from_unix(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn cache_round_trip_and_clear() {
        let dir = std::env::temp_dir().join(format!("vero-cache-test-{}", std::process::id()));
        cache_put(
            &dir,
            CacheKind::Watchlist,
            r#"{"items":[]}"#.into(),
            "2026-10-02T15:00:00Z".into(),
        )
        .unwrap();
        let got = cache_get(&dir, CacheKind::Watchlist).unwrap();
        assert_eq!(got.json, r#"{"items":[]}"#);
        assert_eq!(got.as_of, "2026-10-02T15:00:00Z");
        assert!(cache_get(&dir, CacheKind::Alerts).is_none());
        cache_clear(&dir).unwrap();
        assert!(cache_get(&dir, CacheKind::Watchlist).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_default_and_partial_files() {
        let s: Settings = serde_json::from_str(r#"{"tray":false}"#).unwrap();
        assert!(!s.tray);
        assert!(!s.launch_at_login, "launch at login must default OFF");
        assert_eq!(s.global_hotkey, "Alt+Space");
    }
}
