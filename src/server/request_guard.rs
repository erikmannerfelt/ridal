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
//!
//! `--allow-cross-site-requests` turns off the first two, for someone who
//! drives the API from a page on another origin. The headers stay.

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method};
use axum::response::{IntoResponse, Response};

use super::routes::ApiError;

/// What [`guard`] checks.
#[derive(Debug, Clone, Copy)]
pub struct Guard {
    /// Refuse writes a browser says another site sent.
    pub cross_site: bool,
    /// Refuse a `Host` that is not a loopback name (`ridal gui`).
    pub loopback_hosts_only: bool,
}

/// The headers [`guard`] adds, each only when the handler did not set it.
const SECURITY_HEADERS: [(HeaderName, &str); 4] = [
    (header::CONTENT_SECURITY_POLICY, "frame-ancestors 'self'"),
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
    response
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
