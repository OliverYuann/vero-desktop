// Navigation policy, the app origin, and the `vero://` deep-link mapping.
//
// Everything in this file is PURE — no runtime, no window, no network — which
// is why it carries most of the unit tests. It is the security boundary for
// three things at once:
//
//   1. which top-level navigations render inside a Vero window (everything else
//      goes to the default browser);
//   2. which `vero://` links turn into an in-app destination;
//   3. which in-app paths the IPC commands accept (`desktop_notify`,
//      `desktop_open_window`, `desktop_navigate` all route through
//      `validate_app_path`).
//
// See README.md "Navigation policy" and "Deep links".

use std::sync::OnceLock;
use tauri::Url;

/// The production app origin. Used unless `VERO_APP_URL` says otherwise.
pub const DEFAULT_APP_URL: &str = "https://www.verostocks.com";

/// Hosts that belong to the PRODUCTION app. The apex is listed alongside `www`
/// even though the entry URL is `www`: the apex 308-redirects, and a redirect
/// that we bounced to the system browser would be a confusing way to lose a
/// click. Only consulted while the configured origin is the production one.
const PROD_HOSTS: &[&str] = &["www.verostocks.com", "verostocks.com"];

/// Host serving the bundled `dist/` pages (boot page, tray mini window).
/// Windows uses `http://tauri.localhost/`; macOS and Linux use `tauri://`.
const LOCAL_HOST: &str = "tauri.localhost";

/// The only paths on the app origin that render **inside** a Vero window.
///
/// The app is the sign-in screen, the legal pages it links to, and the
/// authenticated product. Everything else is a website, and websites belong in
/// a browser — anything not on this list is handed to the default browser
/// rather than blocked. An ALLOW-list, not a block-list: a marketing page added
/// next month opens in the browser by default. Matching is segment-aware (see
/// `is_in_app_path`), so `/news` does not also admit `/newsletter`.
pub const IN_APP_PATHS: &[&str] = &[
    // Sign-in and everything reachable from it.
    "/auth",
    "/forgot-password",
    "/reset-password",
    // Legal — linked from sign-in and from each other.
    "/terms",
    "/privacy",
    "/cookies",
    "/disclaimer",
    // The authenticated product — one entry per top-level route under
    // `src/routes/_authenticated/` (checked 2026-10-02).
    "/account",
    "/admin",
    "/alerts",
    "/article",
    "/assistant",
    "/company",
    "/dashboard",
    "/earnings",
    "/forecast",
    "/fundamentals",
    "/live",
    "/news",
    "/notifications",
    "/onboarding",
    "/popout",
    "/portfolio",
    "/scorecard",
    "/settings",
    "/thesis",
    "/tools",
    "/upgrade",
    "/watchlist",
];

/// The custom URL scheme. The authoritative copy is
/// `plugins.deep-link.desktop.schemes` in tauri.conf.json, which is what the
/// installers read to register it. Changing one without the other does nothing.
const SCHEME: &str = "vero";

/// `vero://open?path=…` — navigate to an arbitrary in-app path.
const OPEN_ACTION: &str = "open";
/// `vero://auth-callback?code=…` — the PKCE return leg (see README "Auth").
const AUTH_ACTION: &str = "auth-callback";
/// `vero://stock/NVDA` → `/company/NVDA`.
const STOCK_ACTION: &str = "stock";
/// `vero://alert/<uuid>` → `/alerts?open=<uuid>`.
const ALERT_ACTION: &str = "alert";
/// `vero://assistant[?q=…]` → `/assistant[?q=…]`.
const ASSISTANT_ACTION: &str = "assistant";

/// Where an `auth-callback` link is sent. A fixed constant: nothing in the
/// incoming URL influences it.
pub const AUTH_CALLBACK_PATH: &str = "/auth/callback";

/// Ceiling on any single forwarded auth parameter.
const AUTH_PARAM_MAX_LEN: usize = 512;

/// Ceiling on an in-app path handed to a command or produced by a deep link.
pub const PATH_MAX_LEN: usize = 1024;

/// Ceiling on the assistant prompt carried by `vero://assistant?q=`.
pub const ASSISTANT_Q_MAX_LEN: usize = 500;

// ── The app origin ──────────────────────────────────────────────────────────

/// The origin the windows load and the only one granted IPC.
///
/// Resolution order:
///   1. `VERO_APP_URL` in the environment at RUN time,
///   2. `VERO_APP_URL` at BUILD time (`option_env!`),
///   3. `DEFAULT_APP_URL`.
///
/// Whatever is chosen is reduced to a bare origin (`scheme://host[:port]/`).
/// `https` is accepted for any host; plain `http` only for loopback, so a
/// typo can never put the app (and its IPC grant) on cleartext internet.
/// A runtime override that differs from the build-time origin also switches
/// the OS keychain commands off in release builds — see `secure_store_allowed`.
pub fn app_origin() -> &'static Url {
    static ORIGIN: OnceLock<Url> = OnceLock::new();
    ORIGIN.get_or_init(|| {
        let runtime = std::env::var("VERO_APP_URL")
            .ok()
            .filter(|v| !v.trim().is_empty());
        let chosen = runtime
            .as_deref()
            .and_then(parse_app_origin)
            .unwrap_or_else(build_time_origin);
        if runtime.is_some() && chosen != build_time_origin() {
            eprintln!("[vero] VERO_APP_URL override in effect: {chosen}");
        }
        chosen
    })
}

/// The origin compiled into this binary (build-time `VERO_APP_URL` or the
/// production default).
pub fn build_time_origin() -> Url {
    option_env!("VERO_APP_URL")
        .and_then(parse_app_origin)
        .unwrap_or_else(|| Url::parse(DEFAULT_APP_URL).expect("default origin parses"))
}

/// May the keychain commands run? Always in debug builds; in release builds
/// only when the window is on the origin compiled into the binary. Otherwise
/// a launcher that sets `VERO_APP_URL` could point the IPC grant at a server it
/// controls and read the Supabase refresh token out of the keychain.
pub fn secure_store_allowed() -> bool {
    cfg!(debug_assertions) || *app_origin() == build_time_origin()
}

/// Parse a candidate `VERO_APP_URL` into a bare origin, or reject it.
pub fn parse_app_origin(raw: &str) -> Option<Url> {
    let url = Url::parse(raw.trim()).ok()?;
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    match url.scheme() {
        "https" => {}
        "http" if is_loopback_host(&host) => {}
        _ => return None,
    }
    let origin = match url.port() {
        Some(port) => format!("{}://{}:{}/", url.scheme(), host, port),
        None => format!("{}://{}/", url.scheme(), host),
    };
    Url::parse(&origin).ok()
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1")
}

/// Is the configured origin the production one?
fn origin_is_prod(origin: &Url) -> bool {
    origin.host_str().is_some_and(|h| PROD_HOSTS.contains(&h))
}

/// `remote.urls` patterns for the runtime capability: the configured origin,
/// plus the apex when that origin is production.
pub fn remote_url_patterns(origin: &Url) -> Vec<String> {
    let mut out = vec![origin_pattern(origin)];
    if origin_is_prod(origin) {
        for host in PROD_HOSTS {
            let p = format!("https://{host}/*");
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

fn origin_pattern(origin: &Url) -> String {
    let host = origin.host_str().unwrap_or_default();
    match origin.port() {
        Some(port) => format!("{}://{}:{}/*", origin.scheme(), host, port),
        None => format!("{}://{}/*", origin.scheme(), host),
    }
}

/// Is this URL on the app origin (path not considered)?
pub fn is_on_app_origin_with(url: &Url, origin: &Url) -> bool {
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    if origin_is_prod(origin) {
        // Production keeps its historical behaviour: either prod host, http or
        // https (the apex redirect is the reason).
        return PROD_HOSTS.contains(&host.as_str());
    }
    url.scheme() == origin.scheme()
        && Some(host.as_str()) == origin.host_str()
        && url.port_or_known_default() == origin.port_or_known_default()
}

// ── Navigation gates ────────────────────────────────────────────────────────

/// Pure origin test: the app origin, the local bundle, or an inert scheme.
/// Also the last gate on `vero://` deep links.
pub fn is_app_url_with(url: &Url, origin: &Url) -> bool {
    match url.scheme() {
        "http" | "https" => {
            if url
                .host_str()
                .is_some_and(|h| h.eq_ignore_ascii_case(LOCAL_HOST))
            {
                return true;
            }
            is_on_app_origin_with(url, origin)
        }
        // Inert or internal: the custom protocol serving dist/, plus the
        // schemes a page uses on itself. Never hand these to the OS.
        "tauri" | "about" | "blob" | "data" | "javascript" => true,
        _ => false,
    }
}

/// Is this path one of the app's own screens? Segment-aware.
pub fn is_in_app_path(path: &str) -> bool {
    let path = path.trim_end_matches('/').to_ascii_lowercase();
    let path = if path.is_empty() { "/" } else { path.as_str() };
    IN_APP_PATHS.iter().any(|allowed| {
        path == *allowed
            || path
                .strip_prefix(allowed)
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// Should this URL render inside a Vero window, or be handed to the OS?
/// Two gates: the origin, then — on the app origin only — the path.
pub fn renders_in_app_with(url: &Url, origin: &Url) -> bool {
    if !is_app_url_with(url, origin) {
        return false;
    }
    if is_on_app_origin_with(url, origin) {
        return is_in_app_path(url.path());
    }
    // tauri.localhost, tauri://, about:blank, blob:, data: — the app's own
    // local surface, never the website.
    true
}

pub fn renders_in_app(url: &Url) -> bool {
    renders_in_app_with(url, app_origin())
}

/// Is this URL one of OUR local pages (boot page / tray), not the website?
pub fn is_local_page(url: &Url) -> bool {
    match url.scheme() {
        "tauri" => true,
        "http" | "https" => url
            .host_str()
            .is_some_and(|h| h.eq_ignore_ascii_case(LOCAL_HOST)),
        _ => false,
    }
}

// ── In-app path validation (commands) ───────────────────────────────────────

/// Validate an in-app path supplied over IPC (or produced by a deep link) and
/// return it normalised as `path[?query][#fragment]`.
///
/// Rules: printable ASCII only, ≤ `PATH_MAX_LEN`, starts with exactly one `/`
/// (so never `//host`), no backslashes; joined ONTO the app origin rather than
/// parsed on its own; the result must still be on that origin and on the
/// `IN_APP_PATHS` allow-list.
pub fn validate_app_path_with(path: &str, origin: &Url) -> Result<String, String> {
    if path.is_empty() || path.len() > PATH_MAX_LEN {
        return Err("path: length".into());
    }
    if !path.starts_with('/') || path.starts_with("//") {
        return Err("path: must be an absolute in-app path".into());
    }
    if path.contains('\\') || !path.chars().all(|c| c.is_ascii_graphic()) {
        return Err("path: invalid characters".into());
    }
    let joined = origin
        .join(path)
        .map_err(|_| "path: unparsable".to_string())?;
    if !is_on_app_origin_with(&joined, origin) || !is_in_app_path(joined.path()) {
        return Err("path: not an in-app screen".into());
    }
    let mut out = joined.path().to_string();
    if let Some(q) = joined.query() {
        out.push('?');
        out.push_str(q);
    }
    if let Some(f) = joined.fragment() {
        out.push('#');
        out.push_str(f);
    }
    Ok(out)
}

pub fn validate_app_path(path: &str) -> Result<String, String> {
    validate_app_path_with(path, app_origin())
}

/// The full app URL for an already-validated in-app path.
pub fn app_url_for(path: &str) -> Option<Url> {
    app_origin().join(path).ok()
}

// ── Deep links ──────────────────────────────────────────────────────────────

/// Turn an incoming `vero://` URL into the app URL it asks for.
///
///     vero://open?path=%2Fcompany%2FAAPL%3Ftab%3Dthesis
///     vero://auth-callback?code=<single-use PKCE code>
///     vero://stock/NVDA            → /company/NVDA
///     vero://alert/<uuid>          → /alerts?open=<uuid>
///     vero://assistant[?q=<text>]  → /assistant[?q=<text>]
///
/// `None` = focus the window, navigate nowhere. Never an error dialog.
///
/// SECURITY: everything after the scheme is untrusted (any web page can fire a
/// `vero://` link). Every branch builds its target by joining onto the fixed
/// app origin, and `is_app_url` is the last gate all of them pass.
pub fn resolve_deep_link_with(raw: &str, origin: &Url) -> Option<Url> {
    if raw.len() > 4096 {
        return None;
    }
    let url = Url::parse(raw).ok()?;
    if !url.scheme().eq_ignore_ascii_case(SCHEME) {
        return None;
    }
    // Custom schemes are not "special", so the authority lands in host_str().
    let action = url.host_str()?.to_ascii_lowercase();

    let target = match action.as_str() {
        OPEN_ACTION => resolve_open(&url, origin)?,
        AUTH_ACTION => resolve_auth_callback(&url, origin)?,
        STOCK_ACTION => {
            let ticker = single_segment(&url)?;
            let ticker = crate::validate::ticker(&ticker.to_ascii_uppercase()).ok()?;
            origin.join(&format!("/company/{ticker}")).ok()?
        }
        ALERT_ACTION => {
            let id = crate::validate::uuid(&single_segment(&url)?).ok()?;
            let mut t = origin.join("/alerts").ok()?;
            t.query_pairs_mut().append_pair("open", &id);
            t
        }
        ASSISTANT_ACTION => {
            let mut t = origin.join("/assistant").ok()?;
            if let Some((_, q)) = url.query_pairs().find(|(k, _)| k == "q") {
                let q = q.trim();
                if !q.is_empty()
                    && q.chars().count() <= ASSISTANT_Q_MAX_LEN
                    && !q.chars().any(char::is_control)
                {
                    t.query_pairs_mut().append_pair("q", q);
                }
            }
            t
        }
        _ => return None,
    };

    if !is_app_url_with(&target, origin) {
        return None;
    }
    Some(target)
}

pub fn resolve_deep_link(raw: &str) -> Option<Url> {
    resolve_deep_link_with(raw, app_origin())
}

/// The single path segment of `vero://stock/NVDA` (→ `NVDA`). Rejects empty
/// and multi-segment paths (`vero://stock/NVDA/../x`).
fn single_segment(url: &Url) -> Option<String> {
    let path = url.path().trim_start_matches('/').trim_end_matches('/');
    if path.is_empty() || path.contains('/') {
        return None;
    }
    Some(path.to_string())
}

/// `vero://open?path=…`. The caller's `is_app_url` re-check is what catches
/// `path=//evil.com`, which `join` resolves protocol-relatively.
fn resolve_open(url: &Url, origin: &Url) -> Option<Url> {
    let path = url
        .query_pairs()
        .find(|(k, _)| k == "path")
        .map(|(_, v)| v)?;
    if !path.starts_with('/') {
        return None;
    }
    origin.join(&path).ok()
}

/// `vero://auth-callback?code=…` → `<origin>/auth/callback?code=…`.
/// Only four known parameters cross over, each length-capped and
/// character-checked; the destination path is a constant.
fn resolve_auth_callback(url: &Url, origin: &Url) -> Option<Url> {
    let mut target = origin.join(AUTH_CALLBACK_PATH).ok()?;
    let mut carried = 0usize;
    {
        let mut query = target.query_pairs_mut();
        for (key, value) in url.query_pairs() {
            if !auth_param_is_acceptable(&key, &value) {
                continue;
            }
            query.append_pair(&key, &value);
            carried += 1;
        }
    }
    if carried == 0 {
        return None;
    }
    Some(target)
}

fn auth_param_is_acceptable(key: &str, value: &str) -> bool {
    if value.is_empty() || value.len() > AUTH_PARAM_MAX_LEN {
        return false;
    }
    match key {
        "code" | "error" | "error_code" => value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~')),
        "error_description" => value.chars().all(|c| c.is_ascii_graphic() || c == ' '),
        _ => false,
    }
}

/// The in-app part (`path?query`) of an app URL.
pub fn path_of(url: &Url) -> String {
    let mut out = url.path().to_string();
    if let Some(q) = url.query() {
        out.push('?');
        out.push_str(q);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prod() -> Url {
        Url::parse(DEFAULT_APP_URL).unwrap()
    }
    fn local() -> Url {
        parse_app_origin("http://127.0.0.1:8080").unwrap()
    }
    fn resolved(raw: &str) -> Option<String> {
        resolve_deep_link_with(raw, &prod()).map(|u| u.to_string())
    }
    fn in_app(raw: &str) -> bool {
        renders_in_app_with(&Url::parse(raw).expect("test URL should parse"), &prod())
    }

    // ── Existing deep-link behaviour (kept verbatim from the 0.1 shell) ─────

    #[test]
    fn open_action_resolves_onto_the_app_origin() {
        assert_eq!(
            resolved("vero://open?path=%2Fdashboard").as_deref(),
            Some("https://www.verostocks.com/dashboard")
        );
    }

    #[test]
    fn open_action_rejects_protocol_relative_paths() {
        assert_eq!(resolved("vero://open?path=//evil.example.com/pwned"), None);
    }

    #[test]
    fn unknown_actions_are_ignored() {
        assert_eq!(resolved("vero://whatever?path=%2Fdashboard"), None);
        assert_eq!(resolved("https://open?path=%2Fdashboard"), None);
    }

    #[test]
    fn auth_callback_carries_only_the_code_onto_a_fixed_path() {
        assert_eq!(
            resolved("vero://auth-callback?code=8bd9d4a2-1f3e-4c0a-9a0b-4a1d2f6c7e88").as_deref(),
            Some("https://www.verostocks.com/auth/callback?code=8bd9d4a2-1f3e-4c0a-9a0b-4a1d2f6c7e88")
        );
    }

    #[test]
    fn auth_callback_forwards_provider_errors() {
        let out = resolved(
            "vero://auth-callback?error=access_denied&error_code=403&error_description=The%20user%20refused",
        )
        .expect("errors should reach the callback screen");
        assert!(out.starts_with("https://www.verostocks.com/auth/callback?"));
        assert!(out.contains("error=access_denied"));
        assert!(out.contains("error_description=The+user+refused"));
    }

    #[test]
    fn auth_callback_drops_unknown_and_malformed_parameters() {
        assert_eq!(
            resolved("vero://auth-callback?next=%2F%2Fevil.example.com&code=a/b%3Fc"),
            None
        );
    }

    #[test]
    fn auth_callback_cannot_redirect_the_window_off_origin() {
        let out =
            resolved("vero://auth-callback?code=abc&redirect_to=https%3A%2F%2Fevil.example.com")
                .expect("the code alone is still usable");
        assert_eq!(out, "https://www.verostocks.com/auth/callback?code=abc");
    }

    #[test]
    fn auth_callback_without_usable_parameters_navigates_nowhere() {
        assert_eq!(resolved("vero://auth-callback"), None);
        assert_eq!(resolved("vero://auth-callback?code="), None);
    }

    #[test]
    fn auth_callback_rejects_absurdly_long_values() {
        let long = "a".repeat(AUTH_PARAM_MAX_LEN + 1);
        assert_eq!(resolved(&format!("vero://auth-callback?code={long}")), None);
    }

    // ── New deep links ──────────────────────────────────────────────────────

    #[test]
    fn stock_links_map_to_the_company_page() {
        assert_eq!(
            resolved("vero://stock/NVDA").as_deref(),
            Some("https://www.verostocks.com/company/NVDA")
        );
        assert_eq!(
            resolved("vero://stock/brk.b").as_deref(),
            Some("https://www.verostocks.com/company/BRK.B")
        );
        assert_eq!(
            resolved("vero://stock/NVDA/").as_deref(),
            Some("https://www.verostocks.com/company/NVDA")
        );
    }

    #[test]
    fn stock_links_reject_anything_that_is_not_a_ticker() {
        for bad in [
            "vero://stock",
            "vero://stock/",
            "vero://stock/..%2F..%2Fpricing",
            "vero://stock/%2F%2Fevil.example.com",
            "vero://stock/TOOLONGTICKER",
            "vero://stock/-NVDA",
            "vero://stock/NV%20DA",
            "vero://stock/NV%3CDA",
        ] {
            assert_eq!(resolved(bad), None, "{bad} should be rejected");
        }
        // Dot-segments are normalised by the URL parser BEFORE we see the path,
        // so they cannot climb out of /company/ — the worst case is a
        // nonsense ticker on the company page.
        assert_eq!(
            resolved("vero://stock/NVDA/../../pricing").as_deref(),
            Some("https://www.verostocks.com/company/PRICING")
        );
    }

    #[test]
    fn alert_links_carry_only_a_uuid() {
        assert_eq!(
            resolved("vero://alert/8BD9D4A2-1F3E-4C0A-9A0B-4A1D2F6C7E88").as_deref(),
            Some("https://www.verostocks.com/alerts?open=8bd9d4a2-1f3e-4c0a-9a0b-4a1d2f6c7e88")
        );
        assert_eq!(resolved("vero://alert/not-a-uuid"), None);
        assert_eq!(
            resolved("vero://alert/8bd9d4a2-1f3e-4c0a-9a0b-4a1d2f6c7e88&x=1"),
            None
        );
        assert_eq!(resolved("vero://alert"), None);
    }

    #[test]
    fn assistant_links_open_the_assistant_with_an_optional_prompt() {
        assert_eq!(
            resolved("vero://assistant").as_deref(),
            Some("https://www.verostocks.com/assistant")
        );
        assert_eq!(
            resolved("vero://assistant?q=why%20did%20NVDA%20move%3F").as_deref(),
            Some("https://www.verostocks.com/assistant?q=why+did+NVDA+move%3F")
        );
        let long = "a".repeat(ASSISTANT_Q_MAX_LEN + 1);
        assert_eq!(
            resolved(&format!("vero://assistant?q={long}")).as_deref(),
            Some("https://www.verostocks.com/assistant")
        );
    }

    #[test]
    fn deep_links_follow_a_configured_origin() {
        let o = local();
        assert_eq!(
            resolve_deep_link_with("vero://stock/NVDA", &o)
                .map(|u| u.to_string())
                .as_deref(),
            Some("http://127.0.0.1:8080/company/NVDA")
        );
        assert_eq!(
            resolve_deep_link_with("vero://open?path=//evil.example.com/x", &o),
            None
        );
    }

    // ── The in-app path allow-list ──────────────────────────────────────────

    #[test]
    fn app_screens_render_in_the_window() {
        for url in [
            "https://www.verostocks.com/auth",
            "https://www.verostocks.com/auth?mode=signup",
            "https://www.verostocks.com/auth/callback?code=abc",
            "https://www.verostocks.com/forgot-password",
            "https://www.verostocks.com/reset-password",
            "https://www.verostocks.com/dashboard",
            "https://www.verostocks.com/company/AAPL?tab=thesis",
            "https://www.verostocks.com/watchlist",
            "https://www.verostocks.com/upgrade/complete",
            "https://www.verostocks.com/portfolio",
            "https://www.verostocks.com/settings/memory",
            "https://verostocks.com/dashboard",
        ] {
            assert!(in_app(url), "{url} should render in-app");
        }
    }

    #[test]
    fn the_legal_pages_and_their_cross_links_render_in_the_window() {
        for url in ["/terms", "/privacy", "/cookies", "/disclaimer"] {
            assert!(
                in_app(&format!("https://www.verostocks.com{url}")),
                "{url} should render in-app"
            );
        }
    }

    #[test]
    fn the_marketing_site_goes_to_the_browser() {
        for url in [
            "https://www.verostocks.com/",
            "https://www.verostocks.com/pricing",
            "https://www.verostocks.com/blog",
            "https://www.verostocks.com/blog/some-post",
            "https://www.verostocks.com/about",
            "https://www.verostocks.com/faq",
            "https://www.verostocks.com/careers",
            "https://www.verostocks.com/download",
        ] {
            assert!(!in_app(url), "{url} should be handed to the browser");
        }
    }

    #[test]
    fn prefixes_match_whole_segments_only() {
        assert!(in_app("https://www.verostocks.com/news"));
        assert!(in_app("https://www.verostocks.com/news/"));
        assert!(in_app("https://www.verostocks.com/news/item-1"));
        assert!(!in_app("https://www.verostocks.com/newsletter"));
        assert!(!in_app("https://www.verostocks.com/authors/oliver"));
        assert!(!in_app("https://www.verostocks.com/toolsmith"));
    }

    #[test]
    fn the_local_pages_are_exempt_from_the_path_list() {
        assert!(in_app("http://tauri.localhost/"));
        assert!(in_app("http://tauri.localhost/index.html"));
        assert!(in_app("tauri://localhost/tray.html"));
        assert!(in_app("about:blank"));
    }

    #[test]
    fn off_origin_is_still_off_origin_under_the_path_gate() {
        assert!(!in_app("https://evil.example.com/pwned"));
        assert!(!in_app("https://evil.example.com/dashboard"));
        assert!(!in_app("https://evil.example.com/auth/callback?code=abc"));
        assert!(!in_app(
            "https://www.verostocks.com.evil.example.com/dashboard"
        ));
        assert!(!in_app("http://accounts.google.com/o/oauth2/v2/auth"));
    }

    #[test]
    fn deep_links_still_resolve_independently_of_the_path_gate() {
        let pricing =
            resolve_deep_link_with("vero://open?path=%2Fpricing", &prod()).expect("should resolve");
        assert_eq!(pricing.as_str(), "https://www.verostocks.com/pricing");
        assert!(!renders_in_app_with(&pricing, &prod()));
    }

    // ── Configurable origin ─────────────────────────────────────────────────

    #[test]
    fn app_origin_parsing_accepts_https_and_loopback_http_only() {
        assert_eq!(
            parse_app_origin("https://www.verostocks.com/auth")
                .unwrap()
                .as_str(),
            "https://www.verostocks.com/"
        );
        assert_eq!(
            parse_app_origin("http://127.0.0.1:8080").unwrap().as_str(),
            "http://127.0.0.1:8080/"
        );
        assert_eq!(
            parse_app_origin("http://localhost:3000/x?y")
                .unwrap()
                .as_str(),
            "http://localhost:3000/"
        );
        assert_eq!(
            parse_app_origin("https://vero-git-x.vercel.app")
                .unwrap()
                .as_str(),
            "https://vero-git-x.vercel.app/"
        );
        assert!(parse_app_origin("http://evil.example.com").is_none());
        assert!(parse_app_origin("https://user:pw@www.verostocks.com").is_none());
        assert!(parse_app_origin("file:///etc/passwd").is_none());
        assert!(parse_app_origin("javascript:alert(1)").is_none());
        assert!(parse_app_origin("").is_none());
    }

    #[test]
    fn a_configured_origin_is_matched_exactly() {
        let o = local();
        let yes = |u: &str| renders_in_app_with(&Url::parse(u).unwrap(), &o);
        assert!(yes("http://127.0.0.1:8080/dashboard"));
        assert!(yes("http://127.0.0.1:8080/auth?x=1"));
        assert!(!yes("http://127.0.0.1:8080/pricing"));
        assert!(!yes("http://127.0.0.1:9999/dashboard"));
        assert!(!yes("https://127.0.0.1:8080/dashboard"));
        assert!(!yes("http://localhost:8080/dashboard"));
        // Production is NOT implicitly trusted once the origin is overridden.
        assert!(!yes("https://www.verostocks.com/dashboard"));
        assert!(yes("http://tauri.localhost/index.html"));
    }

    #[test]
    fn remote_capability_patterns_cover_only_the_app_origin() {
        assert_eq!(
            remote_url_patterns(&local()),
            vec!["http://127.0.0.1:8080/*".to_string()]
        );
        assert_eq!(
            remote_url_patterns(&prod()),
            vec![
                "https://www.verostocks.com/*".to_string(),
                "https://verostocks.com/*".to_string()
            ]
        );
    }

    // ── Command path validation ─────────────────────────────────────────────

    #[test]
    fn command_paths_are_normalised_in_app_paths() {
        let v = |p: &str| validate_app_path_with(p, &prod());
        assert_eq!(v("/company/NVDA").unwrap(), "/company/NVDA");
        assert_eq!(v("/alerts?open=abc").unwrap(), "/alerts?open=abc");
        assert_eq!(
            v("/assistant?q=why%20NVDA").unwrap(),
            "/assistant?q=why%20NVDA"
        );
        assert_eq!(v("/company/NVDA/../../dashboard").unwrap(), "/dashboard");
    }

    #[test]
    fn command_paths_reject_everything_else() {
        let v = |p: &str| validate_app_path_with(p, &prod());
        for bad in [
            "",
            "dashboard",
            "//evil.example.com/dashboard",
            "/\\evil.example.com",
            "https://evil.example.com/dashboard",
            "/pricing",
            "/",
            "/company/../pricing",
            "/dash board",
            "/company/NVDA\n",
            "/companyé",
        ] {
            assert!(v(bad).is_err(), "{bad:?} should be rejected");
        }
        assert!(v(&format!("/company/{}", "a".repeat(PATH_MAX_LEN))).is_err());
    }

    #[test]
    fn popout_ticker_window_renders_in_app() {
        assert!(is_in_app_path("/popout/ticker/NVDA"));
        assert!(!is_in_app_path("/popoutx"));
    }
}
