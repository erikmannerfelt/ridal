//! Embedded frontend assets (#120): production must not require a
//! separate Node dev server or an assets directory alongside the binary.

use axum::http::header;
use axum::response::{IntoResponse, Response};

/// Embed and serve one asset. `$path` is relative to `src/server/assets/`.
///
/// Note the `vendor/` prefix is written out at each call site rather than
/// baked into the macro: `scripts/vendor_leaflet.sh` does `rm -rf` on
/// `assets/vendor/`, so first-party files must live *outside* it or they
/// are silently deleted on the next Leaflet refresh.
macro_rules! embedded_asset {
    ($fn_name:ident, $path:literal, $content_type:literal) => {
        pub async fn $fn_name() -> Response {
            (
                [(header::CONTENT_TYPE, $content_type)],
                include_bytes!(concat!("assets/", $path)).as_slice(),
            )
                .into_response()
        }
    };
}

/// Like `embedded_asset!`, but for a path relative to the repo root
/// rather than `src/server/assets/` -- for `images/logo.{svg,png}`, which
/// live at the repo root (not under `assets/`) and must not move under
/// `assets/vendor/`, since `scripts/vendor_leaflet.sh` does `rm -rf` on
/// that directory.
macro_rules! embedded_repo_asset {
    ($fn_name:ident, $path:literal, $content_type:literal) => {
        pub async fn $fn_name() -> Response {
            (
                [(header::CONTENT_TYPE, $content_type)],
                include_bytes!(concat!("../../", $path)).as_slice(),
            )
                .into_response()
        }
    };
}

// Third-party, managed by scripts/vendor_leaflet.sh -- do not edit by hand.
embedded_asset!(leaflet_js, "vendor/leaflet.js", "text/javascript");
embedded_asset!(leaflet_css, "vendor/leaflet.css", "text/css");
embedded_asset!(marker_icon, "vendor/images/marker-icon.png", "image/png");
embedded_asset!(
    marker_icon_2x,
    "vendor/images/marker-icon-2x.png",
    "image/png"
);
embedded_asset!(
    marker_shadow,
    "vendor/images/marker-shadow.png",
    "image/png"
);
embedded_asset!(layers_png, "vendor/images/layers.png", "image/png");
embedded_asset!(layers_2x_png, "vendor/images/layers-2x.png", "image/png");

// First-party.
embedded_asset!(app_css, "app.css", "text/css");
embedded_asset!(app_js, "app.js", "text/javascript");

// Repo-root, first-party. Shown beside the "Ridal" wordmark in the shared
// header (base.html.jinja); logo.png doubles as the favicon.
embedded_repo_asset!(logo_svg, "images/logo.svg", "image/svg+xml");
embedded_repo_asset!(favicon, "images/logo.png", "image/png");

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn leaflet_js_is_served_with_correct_content_type_and_is_nonempty() {
        let response = leaflet_js().await;
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/javascript"
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(!body.is_empty());
        assert!(body.starts_with(b"/* @preserve") || body.len() > 1000);
    }

    async fn body_of(response: Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn app_css_defines_light_and_dark_token_sets() {
        let response = app_css().await;
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/css"
        );
        let css = body_of(response).await;

        // Pins the token contract cheaply: the whole theme is built on
        // these, and a dark override block is what makes
        // `color-scheme: light dark` an actual theme rather than a
        // declaration with light-only values behind it.
        for token in [
            "--color-bg",
            "--color-surface",
            "--color-text",
            "--color-border",
            "--color-accent",
            "--space-4",
            "--text-sm",
        ] {
            assert!(css.contains(token), "missing token {token}");
        }
        assert!(
            css.contains("prefers-color-scheme: dark"),
            "dark theme overrides must exist"
        );
    }

    #[tokio::test]
    async fn logo_svg_and_favicon_are_served_with_correct_content_types_and_are_nonempty() {
        let response = logo_svg().await;
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "image/svg+xml"
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(!body.is_empty());

        let response = favicon().await;
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "image/png"
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(!body.is_empty());
    }

    #[tokio::test]
    async fn app_js_defines_the_shared_constants_global() {
        let response = app_js().await;
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/javascript"
        );
        let js = body_of(response).await;
        assert!(js.contains("const RIDAL"));
        for key in ["trackColor", "siblingColor", "cursorColor", "basemap"] {
            assert!(js.contains(key), "missing shared constant {key}");
        }
    }
}
