//! Markdown rendering for project descriptions (#285).
//!
//! The long description is administrator-authored prose rendered into every
//! member's catalog page. It is therefore trusted as *markup* and never as
//! HTML: Markdown's own syntax is rendered, but anything the author writes as
//! raw HTML comes out escaped, and a link or image pointing at a dangerous
//! URL scheme is neutered rather than emitted. That is the whole point of
//! choosing Markdown over raw HTML for the field.

use pulldown_cmark::{html, CowStr, Event, Options, Parser, Tag};

/// The Markdown extensions a description may use.
///
/// Deliberately a named list rather than `Options::all()`: a future extension
/// that emits HTML or interprets embedded content should be a decision, not
/// something that arrives by upgrading the crate.
fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_SMART_PUNCTUATION
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_HEADING_ATTRIBUTES
}

/// Whether `url` is safe to emit into `href`/`src`.
///
/// Relative URLs, fragments and the schemes a prose link has any business
/// using are allowed; anything that parses as a different scheme -- most
/// importantly `javascript:`, `data:` and `vbscript:` -- is not. Case- and
/// whitespace-insensitive, since `JavaScript:` and a leading control
/// character are how these are smuggled past a naive check.
fn is_safe_url(url: &str) -> bool {
    let trimmed = url.trim_start_matches(|c: char| c.is_whitespace() || c.is_control());
    match trimmed.split_once(':') {
        // No colon before a `/`, `?` or `#`: a relative path or fragment.
        None => true,
        Some((scheme, _)) => matches!(
            scheme.to_ascii_lowercase().as_str(),
            "http" | "https" | "mailto"
        ),
    }
}

/// A link destination, replaced with `#` when its scheme is not allowed.
fn safe_destination(dest: CowStr<'_>) -> CowStr<'_> {
    if is_safe_url(&dest) {
        dest
    } else {
        CowStr::Borrowed("#")
    }
}

/// Render `markdown` to HTML with raw HTML escaped and unsafe URLs stripped.
pub fn render(markdown: &str) -> String {
    let parser = Parser::new_ext(markdown, options()).map(|event| match event {
        // The two ways pulldown-cmark reports raw HTML. Turning each into a
        // text node is what makes the writer escape it.
        Event::Html(text) | Event::InlineHtml(text) => Event::Text(text),
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Link {
            link_type,
            dest_url: safe_destination(dest_url),
            title,
            id,
        }),
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Image {
            link_type,
            dest_url: safe_destination(dest_url),
            title,
            id,
        }),
        other => other,
    });

    let mut output = String::new();
    html::push_html(&mut output, parser);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_html_is_escaped_not_emitted() {
        let html = render("Before <script>alert(1)</script> after");
        assert!(!html.contains("<script>"), "{html}");
        assert!(html.contains("&lt;script&gt;"), "{html}");

        // A block-level tag is escaped too, rather than being a template for
        // an attribute-injection payload.
        let html = render("<img src=x onerror=alert(1)>");
        assert!(!html.contains("<img"), "{html}");
        assert!(html.contains("&lt;img"), "{html}");
    }

    #[test]
    fn markdown_structure_is_rendered() {
        let html =
            render("# Heading\n\n- one\n- two\n\n**bold** and [a link](https://example.org)");
        assert!(html.contains("<h1>Heading</h1>"), "{html}");
        assert!(html.contains("<li>one</li>"), "{html}");
        assert!(html.contains("<strong>bold</strong>"), "{html}");
        assert!(
            html.contains(r#"<a href="https://example.org">a link</a>"#),
            "{html}"
        );
    }

    #[test]
    fn dangerous_url_schemes_are_replaced() {
        for markdown in [
            "[x](javascript:alert(1))",
            "[x](JaVaScRiPt:alert(1))",
            "[x](data:text/html,<script>alert(1)</script>)",
            "![x](vbscript:msgbox(1))",
        ] {
            let html = render(markdown);
            assert!(!html.contains("javascript:"), "{html}");
            assert!(!html.contains("JaVaScRiPt:"), "{html}");
            assert!(!html.contains("data:"), "{html}");
            assert!(!html.contains("vbscript:"), "{html}");
        }
        // A relative link and a fragment are the ordinary cases and survive.
        assert!(render("[home](/catalog)").contains(r#"href="/catalog""#));
        assert!(render("[top](#top)").contains(r##"href="#top""##));
    }
}
