// Input validators for every IPC command. Pure functions, unit-tested below.
//
// Every command that the remote origin can reach runs its arguments through
// one of these before doing anything. The rule of thumb: enums are parsed into
// Rust enums (no free-form strings reach the filesystem or a window label),
// strings are length-capped in BYTES and CHARACTERS, and anything that names a
// place in the app goes through `nav::validate_app_path`.

use serde::Serialize;

/// `^[A-Z0-9][A-Z0-9.-]{0,9}$`
pub fn ticker(s: &str) -> Result<String, String> {
    let bytes = s.as_bytes();
    let ok = !bytes.is_empty()
        && bytes.len() <= 10
        && (bytes[0].is_ascii_uppercase() || bytes[0].is_ascii_digit())
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || *b == b'.' || *b == b'-');
    if ok {
        Ok(s.to_string())
    } else {
        Err("ticker: must match ^[A-Z0-9][A-Z0-9.-]{0,9}$".into())
    }
}

/// Canonical 8-4-4-4-12 hex UUID, returned lowercase.
pub fn uuid(s: &str) -> Result<String, String> {
    let ok = s.len() == 36
        && s.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        });
    if ok {
        Ok(s.to_ascii_lowercase())
    } else {
        Err("id: not a uuid".into())
    }
}

/// A human-readable string: non-empty after trimming (when `required`), at
/// most `max_chars` characters, no control characters except newline/tab.
pub fn text(field: &str, s: &str, max_chars: usize, required: bool) -> Result<String, String> {
    let t = s.trim();
    if required && t.is_empty() {
        return Err(format!("{field}: required"));
    }
    if t.chars().count() > max_chars {
        return Err(format!("{field}: longer than {max_chars} characters"));
    }
    if t.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
        return Err(format!("{field}: control characters"));
    }
    Ok(t.to_string())
}

/// The five cache slots. Parsed, never used as a raw string, so a `kind` can
/// never become `../../something` on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheKind {
    Workspace,
    Watchlist,
    Alerts,
    Events,
    Portfolio,
}

impl CacheKind {
    pub const ALL: [CacheKind; 5] = [
        CacheKind::Workspace,
        CacheKind::Watchlist,
        CacheKind::Alerts,
        CacheKind::Events,
        CacheKind::Portfolio,
    ];

    pub fn parse(s: &str) -> Result<Self, String> {
        Ok(match s {
            "workspace" => Self::Workspace,
            "watchlist" => Self::Watchlist,
            "alerts" => Self::Alerts,
            "events" => Self::Events,
            "portfolio" => Self::Portfolio,
            _ => return Err("kind: expected workspace|watchlist|alerts|events|portfolio".into()),
        })
    }

    pub fn file_stem(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Watchlist => "watchlist",
            Self::Alerts => "alerts",
            Self::Events => "events",
            Self::Portfolio => "portfolio",
        }
    }
}

/// Max size of one cached JSON document.
pub const CACHE_JSON_MAX: usize = 256 * 1024;
/// Max length of the `as_of` label (an ISO timestamp is 24–35 chars).
pub const AS_OF_MAX: usize = 64;

/// `json` must be ≤ 256 KB and actually parse as JSON.
pub fn cache_json(s: &str) -> Result<(), String> {
    if s.len() > CACHE_JSON_MAX {
        return Err("json: larger than 256 KB".into());
    }
    serde_json::from_str::<serde_json::Value>(s).map_err(|_| "json: not valid JSON".to_string())?;
    Ok(())
}

/// Pop-out window kinds; each maps to a default size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WindowKind {
    Company,
    Chart,
    Panel,
    Ticker,
}

impl WindowKind {
    pub fn parse(s: &str) -> Result<Self, String> {
        Ok(match s {
            "company" => Self::Company,
            "chart" => Self::Chart,
            "panel" => Self::Panel,
            "ticker" => Self::Ticker,
            // `page` is what src/lib/desktop-v2/tauri.ts sends for a generic
            // route; it gets the company-sized window.
            "page" => Self::Company,
            _ => return Err("kind: expected company|chart|panel|ticker|page".into()),
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Company => "company",
            Self::Chart => "chart",
            Self::Panel => "panel",
            Self::Ticker => "ticker",
        }
    }

    /// (width, height, min_width, min_height) in logical pixels.
    pub fn geometry(self) -> (f64, f64, f64, f64) {
        match self {
            Self::Company => (1180.0, 820.0, 720.0, 520.0),
            Self::Chart => (960.0, 620.0, 480.0, 320.0),
            Self::Panel => (460.0, 720.0, 320.0, 360.0),
            Self::Ticker => (360.0, 132.0, 240.0, 96.0),
        }
    }
}

/// Keychain keys: Supabase auth storage keys only (`sb-…`), ≤ 128 chars,
/// `[A-Za-z0-9._-]`. A narrow prefix means the remote page cannot use the
/// keychain as a general-purpose store.
pub fn secure_key(s: &str) -> Result<String, String> {
    let ok = s.starts_with("sb-")
        && s.len() > 3
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if ok {
        Ok(s.to_string())
    } else {
        Err("key: must be an sb-* Supabase storage key".into())
    }
}

/// Max size of one keychain value.
pub const SECURE_VALUE_MAX: usize = 16 * 1024;

pub fn secure_value(s: &str) -> Result<(), String> {
    if s.len() > SECURE_VALUE_MAX {
        return Err("value: larger than 16 KB".into());
    }
    Ok(())
}

/// A global shortcut string as the settings UI sends it ("Alt+Space",
/// "CmdOrCtrl+Shift+K"). Empty = no global shortcut. Parsed for real by the
/// global-shortcut plugin; this only bounds it.
pub fn hotkey(s: &str) -> Result<String, String> {
    let t = s.trim();
    if t.len() > 64 || !t.chars().all(|c| c.is_ascii_alphanumeric() || c == '+') {
        return Err("global_hotkey: expected e.g. Alt+Space".into());
    }
    Ok(t.to_string())
}

/// A dropped file is accepted as CSV only by extension and size.
pub const CSV_MAX: u64 = 2 * 1024 * 1024;

pub fn is_csv_name(name: &str) -> bool {
    name.len() <= 255
        && std::path::Path::new(name)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("csv"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tickers() {
        for ok in [
            "NVDA",
            "A",
            "BRK.B",
            "BF-B",
            "7203",
            "0700.HK",
            "ABCDEFGHIJ",
        ] {
            assert!(ticker(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "nvda",
            ".NVDA",
            "-X",
            "ABCDEFGHIJK",
            "NV DA",
            "NV/DA",
            "NVDA\n",
            "ÅAPL",
            "<b>",
        ] {
            assert!(ticker(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn uuids() {
        assert_eq!(
            uuid("8BD9D4A2-1F3E-4C0A-9A0B-4A1D2F6C7E88").unwrap(),
            "8bd9d4a2-1f3e-4c0a-9a0b-4a1d2f6c7e88"
        );
        for bad in [
            "",
            "8bd9d4a21f3e4c0a9a0b4a1d2f6c7e88",
            "8bd9d4a2-1f3e-4c0a-9a0b-4a1d2f6c7e8",
            "8bd9d4a2-1f3e-4c0a-9a0b-4a1d2f6c7e8g",
            "8bd9d4a2_1f3e_4c0a_9a0b_4a1d2f6c7e88",
        ] {
            assert!(uuid(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn text_fields() {
        assert_eq!(
            text("title", "  NVDA alert ", 120, true).unwrap(),
            "NVDA alert"
        );
        assert!(text("title", "   ", 120, true).is_err());
        assert!(text("body", "", 400, false).is_ok());
        assert!(text("title", &"x".repeat(121), 120, true).is_err());
        assert!(
            text("title", &"é".repeat(120), 120, true).is_ok(),
            "limit counts characters, not bytes"
        );
        assert!(text("title", "a\u{0007}b", 120, true).is_err());
        assert!(text("body", "line 1\nline 2", 400, false).is_ok());
    }

    #[test]
    fn cache_kinds_are_a_closed_set() {
        for k in CacheKind::ALL {
            assert_eq!(CacheKind::parse(k.file_stem()).unwrap(), k);
        }
        for bad in ["", "Workspace", "../settings", "workspace.json", "secrets"] {
            assert!(CacheKind::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn cache_json_is_bounded_and_parsed() {
        assert!(cache_json(r#"{"items":[]}"#).is_ok());
        assert!(cache_json("[1,2,3]").is_ok());
        assert!(cache_json("{not json").is_err());
        let big = format!("\"{}\"", "a".repeat(CACHE_JSON_MAX));
        assert!(cache_json(&big).is_err());
    }

    #[test]
    fn window_kinds() {
        for k in ["company", "chart", "panel", "ticker"] {
            assert_eq!(WindowKind::parse(k).unwrap().as_str(), k);
        }
        assert_eq!(WindowKind::parse("page").unwrap(), WindowKind::Company);
        assert!(WindowKind::parse("settings").is_err());
        assert!(WindowKind::parse("").is_err());
    }

    #[test]
    fn secure_keys_are_supabase_keys_only() {
        assert!(secure_key("sb-abcdefghijklmnop-auth-token").is_ok());
        assert!(secure_key("sb-127-auth-token-code-verifier").is_ok());
        for bad in [
            "",
            "sb-",
            "auth-token",
            "SB-x",
            "sb-a/b",
            "sb-a b",
            &format!("sb-{}", "a".repeat(126)),
        ] {
            assert!(secure_key(bad).is_err(), "{bad:?}");
        }
        assert!(secure_value(&"a".repeat(SECURE_VALUE_MAX)).is_ok());
        assert!(secure_value(&"a".repeat(SECURE_VALUE_MAX + 1)).is_err());
    }

    #[test]
    fn hotkeys() {
        assert_eq!(hotkey(" Alt+Space ").unwrap(), "Alt+Space");
        assert_eq!(hotkey("").unwrap(), "");
        assert!(hotkey("Alt+Space; rm -rf").is_err());
        assert!(hotkey(&"A+".repeat(40)).is_err());
    }

    #[test]
    fn csv_names() {
        assert!(is_csv_name("holdings.csv"));
        assert!(is_csv_name("Holdings.CSV"));
        assert!(!is_csv_name("holdings.csv.exe"));
        assert!(!is_csv_name("holdings.xlsx"));
        assert!(!is_csv_name("csv"));
    }
}
