//! MiniJinja environment with embedded templates (#120).
//!
//! Templates are embedded via `include_str!` at compile time, not read
//! from disk at runtime -- production must not require a separate
//! frontend/template directory alongside the binary.

use minijinja::Environment;

pub fn environment() -> Environment<'static> {
    let mut env = Environment::new();
    env.add_template("base.html.jinja", include_str!("templates/base.html.jinja"))
        .expect("base template must parse");
    env.add_template(
        "index.html.jinja",
        include_str!("templates/index.html.jinja"),
    )
    .expect("index template must parse");
    env.add_template(
        "viewer.html.jinja",
        include_str!("templates/viewer.html.jinja"),
    )
    .expect("viewer template must parse");
    env.add_template(
        "layers.html.jinja",
        include_str!("templates/layers.html.jinja"),
    )
    .expect("layers template must parse");
    env.add_template(
        "settings.html.jinja",
        include_str!("templates/settings.html.jinja"),
    )
    .expect("settings template must parse");
    env.add_template(
        "login.html.jinja",
        include_str!("templates/login.html.jinja"),
    )
    .expect("login template must parse");
    env.add_template(
        "invite.html.jinja",
        include_str!("templates/invite.html.jinja"),
    )
    .expect("invite template must parse");
    env.add_template(
        "error.html.jinja",
        include_str!("templates/error.html.jinja"),
    )
    .expect("error template must parse");
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_templates_load_without_error() {
        // environment() itself panics on a template error, so simply
        // constructing it is the test; this also confirms each named
        // template is actually registered under the name routes.rs uses.
        let env = environment();
        for name in [
            "base.html.jinja",
            "index.html.jinja",
            "viewer.html.jinja",
            "layers.html.jinja",
            "settings.html.jinja",
            "login.html.jinja",
            "invite.html.jinja",
            "error.html.jinja",
        ] {
            assert!(env.get_template(name).is_ok(), "missing template {name}");
        }
    }

    #[test]
    fn interpolated_values_are_html_escaped() {
        // Load-bearing, and not obvious from the filenames: minijinja
        // decides auto-escaping from the extension, and treats a trailing
        // `.jinja` as ignorable -- so `invite.html.jinja` is escaped as
        // `.html` would be. Rename these templates to `.tmpl` and every
        // interpolation below silently becomes an injection point.
        //
        // The invite page is the sharpest case: its token comes straight
        // from the URL path and the page is deliberately reachable without
        // signing in.
        let env = environment();
        let hostile = "\" onmouseover=alert(1) x=\"";
        let out = env
            .get_template("invite.html.jinja")
            .unwrap()
            .render(minijinja::context! {
                token => hostile,
                min_password_len => 10,
            })
            .unwrap();
        // The property is that the quotes cannot close the attribute, not
        // that the payload's characters are absent: they stay, inert,
        // inside the value.
        assert!(
            out.contains(r#"data-token="&quot; onmouseover=alert(1) x=&quot;""#),
            "the quotes must be escaped so the attribute cannot be closed: {out}"
        );

        // And a free-text project name, which reaches the settings page
        // from `ridal.toml` rather than from a request.
        let out = env
            .get_template("settings.html.jinja")
            .unwrap()
            .render(minijinja::context! {
                project => true,
                project_name => "<script>alert(1)</script>",
                project_root => "/tmp",
            })
            .unwrap();
        assert!(!out.contains("<script>alert(1)</script>"), "{out}");
        assert!(out.contains("&lt;script&gt;"), "{out}");
    }

    /// The basemaps reach the browser as a body attribute, so the value is
    /// escaped as an attribute rather than trusted as script.
    ///
    /// This is what makes a free-text basemap name safe: it comes from
    /// `ridal.toml`, which an `operator` may write through the settings
    /// page, and the same JSON inside a `<script>` block would end the
    /// block at the first `</script>` in it.
    #[test]
    fn the_basemaps_are_delivered_as_an_escaped_attribute() {
        let env = environment();
        let hostile = serde_json::json!([{
            "id": "x",
            "name": "</script><img src=x onerror=alert(1)>",
            "url": "https://tile.example.org/{z}/{x}/{y}.png",
        }])
        .to_string();
        let out = env
            .get_template("layers.html.jinja")
            .unwrap()
            .render(minijinja::context! {
                project => true,
                basemaps_json => hostile,
                active_basemap => "x",
            })
            .unwrap();

        assert!(out.contains("data-basemaps=\""), "{out}");
        assert!(!out.contains("</script><img"), "{out}");
        // The payload's characters stay, inert: minijinja escapes `<`, `>`,
        // `"` and even `/`, so what reaches the attribute is
        // `&lt;&#x2f;script&gt;`.
        assert!(out.contains("&lt;") && out.contains("&gt;"), "{out}");
        // Specific: the page has a real <img> of its own, the wordmark.
        assert!(!out.contains("<img src=x"), "{out}");
        // And the quotes cannot close the attribute they sit in.
        assert!(!out.contains("data-basemaps=\"[{\"id\""), "{out}");
    }

    /// A page with no map carries no attribute at all, rather than an empty
    /// one that `JSON.parse` would choke on.
    #[test]
    fn a_page_without_basemaps_gets_no_attribute() {
        let env = environment();
        let out = env
            .get_template("layers.html.jinja")
            .unwrap()
            .render(minijinja::context! { project => true })
            .unwrap();
        assert!(out.contains("<body>"), "{out}");
        assert!(!out.contains("data-basemaps"), "{out}");
    }

    #[test]
    fn error_template_renders_with_expected_context() {
        let env = environment();
        let tmpl = env.get_template("error.html.jinja").unwrap();
        let out = tmpl
            .render(minijinja::context! {
                status => 404,
                code => "dataset_not_found",
                message => "no such dataset",
            })
            .unwrap();
        assert!(out.contains("404"));
        assert!(out.contains("dataset_not_found"));
        assert!(out.contains("no such dataset"));
    }
}
