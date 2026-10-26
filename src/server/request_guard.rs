//! Checks on where a request came from, and the headers every response
//! carries (#514, #516).
//!
//! One middleware, wrapped around the whole site router, static assets
//! included, so no route can be added without it.
//!
//! - **Cross-site writes.** A method that changes something is refused when
//!   the browser says, through `Sec-Fetch-Site`, that another site sent it.
//!   `ridal gui` treats every request as its operator, so without this any
//!   page open in the same browser could write to the project; a site's
//!   `SameSite=Lax` cookie does not cover a sibling subdomain. The header is
//!   set by the browser and needs no knowledge of the public host name,
//!   which an `Origin`-against-`Host` comparison would: behind the
//!   documented nginx configuration Ridal sees `Host: 127.0.0.1:8000`. A
//!   request without it (curl, the Python client) is allowed, since only a
//!   browser can be made to send one on someone's behalf.
//! - **Hosts, under `ridal gui` only.** Anything but a loopback name is
//!   refused, which stops DNS rebinding: a page whose domain re-resolves to
//!   `127.0.0.1` still sends its own name as `Host`. `ridal server start`
//!   leaves that to its reverse proxy and certificate.
//! - **Security headers**, on every response that does not set its own:
//!   no framing by other origins, no `Referer` beyond this origin (an invite
//!   link carries its token in the path), and no content sniffing.
//! - **A Content Security Policy** (#517), so that if HTML were ever
//!   injected into a page, the browser would not run a script from it:
//!   scripts only from Ridal's own files, no inline script. Styles may be
//!   inline, since Leaflet sets them on every tile. A project's pages
//!   replace the default with one naming its own basemap and overlay hosts
//!   ([`project_policy`]).
//!
//! `--allow-cross-site-requests` turns off the first two, for someone who
//! drives the API from a page on another origin. The headers stay.

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method};
use axum::response::{IntoResponse, Response};

use super::app::AppState;
use super::routes::ApiError;

/// What [`guard`] checks.
#[derive(Debug, Clone, Copy)]
pub struct Guard {
    /// Refuse writes a browser says another site sent.
    pub cross_site: bool,
    /// Refuse a `Host` that is not a loopback name (`ridal gui`).
    pub loopback_hosts_only: bool,
}

/// The headers [`guard`] adds, each only when the handler did not set it,
/// besides the [`default_policy`].
const SECURITY_HEADERS: [(HeaderName, &str); 3] = [
    // For browsers that predate `frame-ancestors`.
    (header::X_FRAME_OPTIONS, "SAMEORIGIN"),
    // Map tiles, the one cross-origin request Ridal's pages make, set their
    // own policy: some tile servers (OpenStreetMap's) refuse a request
    // without a `Referer`. See `basemap` in `app.js`.
    (header::REFERRER_POLICY, "same-origin"),
    (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
];

/// The middleware: refuse what [`Guard`] says to, then add
/// [`SECURITY_HEADERS`] to whatever answers, refusals included.
pub async fn guard(
    State(guard): State<Guard>,
    request: Request,
    next: axum::middleware::Next,
) -> Response {
    let refusal = refusal(guard, request.method(), request.headers(), request.uri());
    let mut response = match refusal {
        Some(error) => error.into_response(),
        None => next.run(request).await,
    };
    let headers = response.headers_mut();
    for (name, value) in SECURITY_HEADERS {
        if !headers.contains_key(&name) {
            headers.insert(name, HeaderValue::from_static(value));
        }
    }
    if !headers.contains_key(header::CONTENT_SECURITY_POLICY) {
        headers.insert(header::CONTENT_SECURITY_POLICY, default_policy().clone());
    }
    response
}

/// The policy for a response that names no hosts of its own: the site's
/// pages, the API, assets and errors. It allows the built-in basemap, so
/// a map drawn without a project's list still has its tiles.
fn default_policy() -> &'static HeaderValue {
    static POLICY: std::sync::OnceLock<HeaderValue> = std::sync::OnceLock::new();
    POLICY.get_or_init(|| {
        let built_in = crate::project::basemaps::built_in();
        let tiles: Vec<String> = tile_source(&built_in.url).into_iter().collect();
        HeaderValue::from_str(&policy(&tiles, &[])).expect("the default policy is ASCII")
    })
}

/// A Content Security Policy that also allows images from `images` (map
/// tiles) and connections to `connections` (overlays the browser fetches).
///
/// `blob:` and `data:` images are the figure preview and Leaflet's own
/// icons; `form-action` and `base-uri` keep an injected `<form>` or
/// `<base>` from sending anything elsewhere.
fn policy(images: &[String], connections: &[String]) -> String {
    let with = |base: &str, extra: &[String]| {
        std::iter::once(base.to_string())
            .chain(extra.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ")
    };
    [
        "default-src 'self'".to_string(),
        "script-src 'self'".to_string(),
        "style-src 'self' 'unsafe-inline'".to_string(),
        with("img-src 'self' data: blob:", images),
        with("connect-src 'self'", connections),
        "object-src 'none'".to_string(),
        "base-uri 'self'".to_string(),
        "form-action 'self'".to_string(),
        "frame-ancestors 'self'".to_string(),
    ]
    .join("; ")
}

/// Middleware for a project's router: give its pages a policy that allows
/// the project's basemap tiles and overlays, read from its configuration
/// on every page so an edit in the settings applies at once. Other
/// responses are left to [`guard`]'s default.
pub async fn project_policy(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: axum::middleware::Next,
) -> Response {
    let mut response = next.run(request).await;
    let html = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/html"));
    if !html
        || response
            .headers()
            .contains_key(header::CONTENT_SECURITY_POLICY)
    {
        return response;
    }
    let tiles: BTreeSet<String> = super::routes::basemap_options(&state)
        .iter()
        .filter_map(|map| tile_source(&map.url))
        .collect();
    let overlays: BTreeSet<String> = super::routes::overlay_options(&state)
        .iter()
        .filter_map(|overlay| overlay_source(&overlay.url))
        .collect();
    let tiles: Vec<String> = tiles.into_iter().collect();
    let overlays: Vec<String> = overlays.into_iter().collect();
    if let Ok(value) = HeaderValue::from_str(&policy(&tiles, &overlays)) {
        response
            .headers_mut()
            .insert(header::CONTENT_SECURITY_POLICY, value);
    }
    response
}

/// The source expression for a basemap's tile URL template: its scheme,
/// host and port, with a `{s}` subdomain placeholder as `*`.
///
/// `None`, with a warning in the server log, for a host a policy cannot
/// name (a placeholder past the first label, an IPv6 address): the
/// basemap's tiles are then refused by the browser, and the rest of the
/// page works.
fn tile_source(url: &str) -> Option<String> {
    let source = origin_of(url);
    if source.is_none() {
        warn_once(url);
    }
    source
}

/// The source expression for an overlay's URL, or `None` for one on this
/// server (`/…`), which `'self'` already allows.
fn overlay_source(url: &str) -> Option<String> {
    if url.starts_with('/') && !url.starts_with("//") {
        return None;
    }
    tile_source(url)
}

/// Say once per URL that a policy cannot name its host, rather than on
/// every page.
fn warn_once(url: &str) {
    static WARNED: std::sync::Mutex<BTreeSet<String>> = std::sync::Mutex::new(BTreeSet::new());
    let mut warned = WARNED.lock().unwrap_or_else(|e| e.into_inner());
    if warned.insert(url.to_string()) {
        tracing::warn!(
            url,
            "the Content Security Policy cannot name this map URL's host, so \
             browsers will refuse what it serves"
        );
    }
}

/// `scheme://host[:port]` of a URL or URL template, as a CSP host source.
fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if !matches!(scheme, "http" | "https") {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.contains('@') {
        return None;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    };
    if let Some(port) = port {
        if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
    }
    let mut labels: Vec<String> = Vec::new();
    for (i, label) in host.split('.').enumerate() {
        let placeholder = label.contains('{');
        if placeholder && i == 0 {
            labels.push("*".to_string());
            continue;
        }
        let plain = !label.is_empty()
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-');
        if !plain {
            return None;
        }
        labels.push(label.to_ascii_lowercase());
    }
    // A bare `*` would allow every host.
    if labels.len() < 2 && labels.first().is_some_and(|l| l == "*") {
        return None;
    }
    let port = port.map(|p| format!(":{p}")).unwrap_or_default();
    Some(format!("{scheme}://{}{port}", labels.join(".")))
}

/// Why `guard` refuses a request, or `None` to let it through.
fn refusal(
    guard: Guard,
    method: &Method,
    headers: &HeaderMap,
    uri: &axum::http::Uri,
) -> Option<ApiError> {
    if guard.loopback_hosts_only {
        // HTTP/2 carries the host in the URI rather than in a header.
        let host = headers
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .or_else(|| uri.authority().map(|a| a.as_str()));
        // A request with no host at all cannot be a rebinding attack, whose
        // whole point is the browser sending the attacker's name.
        if let Some(host) = host {
            if !is_loopback_name(host) {
                return Some(ApiError::misdirected_request(
                    "unexpected_host",
                    format!(
                        "`ridal gui` answers only for localhost, 127.0.0.1 and [::1], \
                         not {host}. Open it through one of those, or start it with \
                         --allow-cross-site-requests."
                    ),
                ));
            }
        }
    }
    if guard.cross_site && !is_safe(method) {
        let site = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok());
        if let Some(site) = site {
            if !matches!(site, "same-origin" | "none") {
                return Some(ApiError::forbidden(
                    "cross_site_request",
                    format!(
                        "This server does not accept changes sent from another site \
                         (Sec-Fetch-Site: {site}). Start it with \
                         --allow-cross-site-requests to drive it from a page on \
                         another origin."
                    ),
                ));
            }
        }
    }
    None
}

/// Methods that change nothing, and so are never refused as cross-site.
fn is_safe(method: &Method) -> bool {
    matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// Whether a `Host` value names this machine's loopback interface: with or
/// without a port, in any case, with or without a trailing dot.
///
/// ```text
/// localhost  LOCALHOST:8000  localhost.:80  127.0.0.1:8000  [::1]  [::1]:8000
/// ```
fn is_loopback_name(host: &str) -> bool {
    let name = match host.strip_prefix('[') {
        // An IPv6 literal: everything up to the bracket, then a port or
        // nothing.
        Some(rest) => match rest.split_once(']') {
            Some((address, after)) if after.is_empty() || is_port(after) => {
                return address == "::1";
            }
            _ => return false,
        },
        None => match host.rsplit_once(':') {
            Some((name, port)) if is_port(&format!(":{port}")) => name,
            Some(_) => return false,
            None => host,
        },
    };
    let name = name.strip_suffix('.').unwrap_or(name);
    name.eq_ignore_ascii_case("localhost") || name == "127.0.0.1"
}

/// `:` followed by one or more digits.
fn is_port(text: &str) -> bool {
    text.strip_prefix(':')
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::{guard, is_loopback_name, Guard};
    use axum::http::header;
    use tower::ServiceExt;

    #[tokio::test]
    async fn a_header_the_handler_set_is_kept() {
        let app = axum::Router::new()
            .route(
                "/",
                axum::routing::get(|| async { ([(header::X_FRAME_OPTIONS, "DENY")], "framed?") }),
            )
            .layer(axum::middleware::from_fn_with_state(
                Guard {
                    cross_site: true,
                    loopback_hosts_only: false,
                },
                guard,
            ));
        let response = app
            .oneshot(
                axum::http::Request::get("/")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let headers = response.headers();
        assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY");
        assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    }

    #[test]
    fn a_map_url_becomes_the_source_of_its_host() {
        use super::origin_of;
        for (url, expected) in [
            (
                "https://tile.openstreetmap.org/{z}/{x}/{y}.png",
                Some("https://tile.openstreetmap.org"),
            ),
            (
                "https://{s}.tile.example.org/{z}/{x}/{y}.png",
                Some("https://*.tile.example.org"),
            ),
            (
                "http://Tiles.Example.org:8080/{z}/{x}/{y}",
                Some("http://tiles.example.org:8080"),
            ),
            ("https://10.0.0.5/{z}/{x}/{y}.png", Some("https://10.0.0.5")),
            (
                "https://static.example.org/stakes.geojson?v=2",
                Some("https://static.example.org"),
            ),
            // A policy cannot name these.
            (
                "https://tile{s}.example.org/{z}/{x}/{y}.png",
                Some("https://*.example.org"),
            ),
            ("https://a.{s}.example.org/{z}/{x}/{y}.png", None),
            ("https://[::1]:8080/{z}/{x}/{y}.png", None),
            ("https://{s}/{z}/{x}/{y}.png", None),
            ("https://user@example.org/{z}", None),
            ("https://example.org:/{z}", None),
            ("ftp://example.org/{z}", None),
            ("/shapes/stakes.geojson", None),
        ] {
            assert_eq!(origin_of(url).as_deref(), expected, "{url}");
        }
    }

    #[test]
    fn the_default_policy_allows_only_ridal_and_the_built_in_basemap() {
        let policy = super::default_policy().to_str().unwrap().to_string();
        assert!(policy.contains("script-src 'self';"), "{policy}");
        assert!(
            !policy.contains("script-src 'self' 'unsafe-inline'"),
            "{policy}"
        );
        assert!(policy.contains("frame-ancestors 'self'"), "{policy}");
        assert!(policy.contains("object-src 'none'"), "{policy}");
        assert!(
            policy.contains("img-src 'self' data: blob: https://server.arcgisonline.com;"),
            "{policy}"
        );
    }

    #[test]
    fn loopback_names_are_recognised_in_every_spelling() {
        for host in [
            "localhost",
            "LocalHost:8000",
            "localhost.",
            "localhost.:80",
            "127.0.0.1",
            "127.0.0.1:8000",
            "[::1]",
            "[::1]:8000",
        ] {
            assert!(is_loopback_name(host), "{host}");
        }
        for host in [
            "evil.example",
            "evil.example:8000",
            "localhost.evil.example",
            "127.0.0.2",
            "127.0.0.1.evil.example",
            "[::2]:8000",
            "[::1]evil",
            "localhost:",
            "localhost:80:80",
            "",
        ] {
            assert!(!is_loopback_name(host), "{host}");
        }
    }
}
