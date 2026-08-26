//! Embedded frontend assets (#120): production must not require a
//! separate Node dev server or an assets directory alongside the binary.

use axum::http::header;
use axum::response::{IntoResponse, Response};

macro_rules! embedded_asset {
    ($fn_name:ident, $path:literal, $content_type:literal) => {
        pub async fn $fn_name() -> Response {
            (
                [(header::CONTENT_TYPE, $content_type)],
                include_bytes!(concat!("assets/vendor/", $path)).as_slice(),
            )
                .into_response()
        }
    };
}

embedded_asset!(leaflet_js, "leaflet.js", "text/javascript");
embedded_asset!(leaflet_css, "leaflet.css", "text/css");
embedded_asset!(marker_icon, "images/marker-icon.png", "image/png");
embedded_asset!(marker_icon_2x, "images/marker-icon-2x.png", "image/png");
embedded_asset!(marker_shadow, "images/marker-shadow.png", "image/png");
embedded_asset!(layers_png, "images/layers.png", "image/png");
embedded_asset!(layers_2x_png, "images/layers-2x.png", "image/png");

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
}
