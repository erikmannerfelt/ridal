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
                [
                    (header::CONTENT_TYPE, $content_type),
                    (header::CACHE_CONTROL, "no-cache"),
                ],
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
                [
                    (header::CONTENT_TYPE, $content_type),
                    (header::CACHE_CONTROL, "no-cache"),
                ],
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

// First-party. `app.js` is shared; `index.js`/`viewer.js` are the
// per-page scripts, extracted from their templates so page logic is
// ordinary static JS rather than something only reachable through
// minijinja. The templates keep only what must be interpolated.
embedded_asset!(app_css, "app.css", "text/css");
embedded_asset!(app_js, "app.js", "text/javascript");
embedded_asset!(index_js, "index.js", "text/javascript");
embedded_asset!(viewer_js, "viewer.js", "text/javascript");
embedded_asset!(layers_js, "layers.js", "text/javascript");
embedded_asset!(settings_js, "settings.js", "text/javascript");
embedded_asset!(login_js, "login.js", "text/javascript");
embedded_asset!(picker_js, "picker.js", "text/javascript");
embedded_asset!(panel_js, "panel.js", "text/javascript");

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
        for key in [
            "trackColor",
            "siblingColor",
            "cursorColor",
            "basemap",
            "fetchJson",
            "reportError",
        ] {
            assert!(js.contains(key), "missing shared constant {key}");
        }
    }

    /// The built-in basemap is written twice -- in `basemaps.rs`, which
    /// every page is served from, and in `app.js`, as the fallback for a
    /// page carrying no basemap attribute at all.
    ///
    /// Only the second can go stale silently: a page would keep working,
    /// drawing different imagery than the project believes it offers, with
    /// nothing failing anywhere. Hence a test rather than a comment.
    #[test]
    fn the_built_in_matches_the_fallback_in_app_js() {
        let js = include_str!("assets/app.js");
        let built_in = crate::project::basemaps::built_in();
        for expected in [
            built_in.id.as_str(),
            built_in.name.as_str(),
            built_in.url.as_str(),
            built_in.attribution.as_deref().unwrap(),
        ] {
            assert!(
                js.contains(expected),
                "app.js's built-in fallback has drifted from basemaps.rs: it does \
                 not contain {expected:?}"
            );
        }
        assert!(
            js.contains(&format!("tile_size: {}", built_in.tile_size()))
                && js.contains(&format!("max_zoom: {}", built_in.max_zoom())),
            "app.js's built-in fallback must carry the same resolved tile size \
             and zoom as basemaps.rs"
        );
    }

    #[tokio::test]
    async fn page_scripts_are_served_and_contain_no_template_syntax() {
        // These were extracted out of their jinja templates; the whole
        // point is that they are now plain static assets. A stray `{{ }}`
        // would mean something template-dependent came along with them
        // and would reach the browser uninterpolated.
        for (name, response) in [
            ("index.js", index_js().await),
            ("viewer.js", viewer_js().await),
        ] {
            assert_eq!(
                response.headers().get(header::CONTENT_TYPE).unwrap(),
                "text/javascript",
                "{name}"
            );
            let js = body_of(response).await;
            assert!(!js.is_empty(), "{name} is empty");
            assert!(
                !js.contains("{{") && !js.contains("{%"),
                "{name} still contains template syntax"
            );
            // Both pages fetch through the shared wrapper rather than a
            // bare `.then(r => r.json())`, which discards the server's
            // error envelope.
            assert!(js.contains("RIDAL.fetchJson"), "{name} bypasses fetchJson");
            assert!(
                !js.contains("r.json()"),
                "{name} still has a bare fetch-and-parse"
            );
        }
    }

    /// The dark palette is written twice -- once for `prefers-color-scheme`
    /// and once for an explicit `data-theme="dark"` (#141) -- because CSS
    /// cannot share a declaration block between a media query and an
    /// attribute selector.
    ///
    /// Two copies drift. The failure would be quiet and half-visible: a
    /// token changed in one place leaves the people who chose dark looking
    /// at a page slightly different from the people whose device chose it
    /// for them.
    #[test]
    fn the_two_dark_theme_blocks_agree() {
        let css = strip_css_comments(include_str!("assets/app.css"));

        /// The `--token: value;` lines of the block starting at `marker`.
        fn tokens_after(css: &str, marker: &str) -> Vec<String> {
            let start = css
                .find(marker)
                .unwrap_or_else(|| panic!("app.css must contain a block starting with {marker:?}"))
                + marker.len();
            css[start..]
                .lines()
                .take_while(|line| !line.contains('}'))
                .map(str::trim)
                .filter(|line| line.starts_with("--"))
                .map(str::to_string)
                .collect()
        }

        let by_device = tokens_after(&css, ":root:not([data-theme=\"light\"]) {");
        let by_choice = tokens_after(&css, ":root[data-theme=\"dark\"] {");
        assert!(
            by_device.len() > 10,
            "the dark palette should be a full token set, found {by_device:?}"
        );
        assert_eq!(
            by_device, by_choice,
            "the dark palette must be identical whether the device asked for \
             it or the person did"
        );
    }

    /// The settings page derives an id from a display name, and shows the
    /// result while you type -- so the rule lives in `app.js` as a twin of
    /// `sanitize_to_slug` in `identity.rs`.
    ///
    /// A twin can drift, and this one would drift quietly: the page would
    /// keep working and simply store a different id than Ridal would derive
    /// for the same name. Rust cannot run the JavaScript, so this pins both
    /// halves of the contract it can reach -- the Rust rule by example, and
    /// the table and charset the JavaScript uses by inspection.
    #[test]
    fn the_browsers_slug_rule_mirrors_the_servers() {
        // The Rust side, by example. Nordic letters are transliterated
        // rather than collapsed, which is the part most likely to be
        // dropped by a reimplementation.
        assert_eq!(
            crate::identity::RadargramId::from_fallback("Drønbreen ortofoto 2024")
                .unwrap()
                .as_str(),
            "dronbreen-ortofoto-2024"
        );
        assert_eq!(
            crate::identity::RadargramId::from_fallback("Ålesund / Ærø")
                .unwrap()
                .as_str(),
            "aalesund-aero"
        );

        // And the JavaScript side, by inspection.
        let js = include_str!("assets/app.js");
        assert!(js.contains("slugify"), "app.js must derive ids");
        for entry in ["\"ø\": \"o\"", "\"æ\": \"ae\"", "\"å\": \"aa\""] {
            assert!(
                js.contains(entry),
                "app.js's slug rule must transliterate as identity.rs does, \
                 missing {entry}"
            );
        }
        assert!(
            js.contains("/[a-z0-9_-]/"),
            "app.js's slug rule must keep the same charset as identity.rs"
        );
    }

    fn strip_css_comments(css: &str) -> String {
        let mut out = String::with_capacity(css.len());
        let mut rest = css;
        while let Some(start) = rest.find("/*") {
            out.push_str(&rest[..start]);
            match rest[start..].find("*/") {
                Some(end) => rest = &rest[start + end + 2..],
                None => return out,
            }
        }
        out.push_str(rest);
        out
    }

    /// `hidden` must win over any author `display` rule.
    ///
    /// The UA stylesheet's `[hidden] { display: none }` loses to any author
    /// rule that sets `display`, so an element carrying both `hidden` and a
    /// class like `.controls` (`display: flex`) stays visible *and
    /// clickable*. The picker's "Selected line" panel shipped that way: it
    /// was permanently on screen, and its Delete button -- with no
    /// selection -- ran `features.splice(null, 1)`, which coerces to index
    /// 0 and deletes the first line on every press.
    ///
    /// Nothing about the markup or the JavaScript looks wrong when this
    /// happens, which is why it is pinned here.
    #[test]
    fn hidden_beats_author_display_rules() {
        // Comments first: the explanation above the rule also mentions
        // `[hidden]`, and would otherwise be found instead of the rule.
        let css = strip_css_comments(include_str!("assets/app.css"));
        let rule = css
            .split('}')
            .find(|block| block.contains("[hidden]"))
            .expect("app.css must define a [hidden] rule");
        assert!(
            rule.contains("display") && rule.contains("none") && rule.contains("!important"),
            "the [hidden] rule must be `display: none !important`, or an author \
             `display` rule will keep a hidden element visible and clickable. Found: {rule}"
        );
    }

    /// Names declared at the top level of a classic script.
    ///
    /// A crude scan -- a declaration keyword in column zero -- which is
    /// exactly right for these files, since every nested declaration in
    /// them is indented.
    fn top_level_declarations(source: &str) -> Vec<String> {
        source
            .lines()
            .filter_map(|line| {
                let rest = ["const ", "let ", "var ", "function ", "class "]
                    .iter()
                    .find_map(|keyword| line.strip_prefix(keyword))?;
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
                    .collect();
                (!name.is_empty()).then_some(name)
            })
            .collect()
    }

    /// The scripts each page loads, in load order. Must match
    /// `base.html.jinja` plus each template's `extrascript` block.
    fn page_bundles() -> Vec<(&'static str, Vec<(&'static str, &'static str)>)> {
        let app = ("app.js", include_str!("assets/app.js"));
        vec![
            (
                "index",
                vec![app, ("index.js", include_str!("assets/index.js"))],
            ),
            (
                "viewer",
                vec![
                    app,
                    ("viewer.js", include_str!("assets/viewer.js")),
                    ("picker.js", include_str!("assets/picker.js")),
                    ("panel.js", include_str!("assets/panel.js")),
                ],
            ),
            (
                "layers",
                vec![app, ("layers.js", include_str!("assets/layers.js"))],
            ),
            (
                "settings",
                vec![app, ("settings.js", include_str!("assets/settings.js"))],
            ),
        ]
    }

    /// Two scripts on one page must not declare the same top-level name.
    ///
    /// Classic scripts share a single global scope, so a duplicated `const`
    /// is a `SyntaxError` that kills the *whole* later file -- silently,
    /// with no server-side symptom at all. This shipped once: `picker.js`
    /// declared `const CFG`, which `viewer.js` already did, and every
    /// picking control stopped working while the page still rendered
    /// perfectly. Hence a test rather than a convention.
    #[test]
    fn no_two_scripts_on_a_page_declare_the_same_global() {
        for (page, scripts) in page_bundles() {
            let mut seen: Vec<(String, &str)> = Vec::new();
            for (name, source) in scripts {
                for declaration in top_level_declarations(source) {
                    if let Some((_, first)) = seen.iter().find(|(d, _)| *d == declaration) {
                        panic!(
                            "the {page} page loads {first} and {name}, which both declare a \
                             top-level `{declaration}`. Classic scripts share one global \
                             scope, so {name} would fail to parse entirely. Wrap it in an \
                             IIFE, or rename the declaration."
                        );
                    }
                    seen.push((declaration, name));
                }
            }
        }
    }

    /// The scanner has to actually find declarations, or the test above
    /// passes by seeing nothing.
    #[test]
    fn the_declaration_scanner_finds_what_it_should() {
        let source = "const A = 1;\nlet b = 2;\nfunction c() {\n  const nested = 3;\n}\n";
        assert_eq!(top_level_declarations(source), vec!["A", "b", "c"]);
        assert!(
            top_level_declarations(include_str!("assets/viewer.js")).contains(&"CFG".to_string()),
            "viewer.js should declare a top-level CFG"
        );
    }

    /// picker.js runs beside viewer.js, which already occupies most of the
    /// obvious names, so it is wrapped in an IIFE and must stay that way.
    #[test]
    fn picker_declares_nothing_globally() {
        let declarations = top_level_declarations(include_str!("assets/picker.js"));
        assert!(
            declarations.is_empty(),
            "picker.js must stay inside its IIFE; found {declarations:?}"
        );
    }
}
