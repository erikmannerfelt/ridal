//! The one font figures are drawn with (#142).
//!
//! DejaVu Sans, bundled into the binary so a PNG figure looks the same on
//! every machine that renders it: resvg is given this font and no system
//! fonts at all. It is also matplotlib's default, so a Ridal figure sits
//! comfortably beside one made there, and it covers the characters glacier
//! names and units need (`ø`, `å`, `µ`, `°`). The licence is in
//! `assets/fonts/LICENSE-DejaVu.txt`.
//!
//! An SVG figure names the same family, with a generic fallback, so it
//! looks the same wherever DejaVu Sans is installed and close to it
//! elsewhere.

use skrifa::{instance::LocationRef, instance::Size, MetadataProvider};

/// The bundled font file.
pub const FONT_DATA: &[u8] = include_bytes!("../../../assets/fonts/DejaVuSans.ttf");

/// The family name the font registers as, and the one SVG text asks for.
pub const FONT_FAMILY: &str = "DejaVu Sans";

/// `font-family` for SVG `<text>`: the bundled font first, then the
/// generic family a viewer without it falls back to.
pub const SVG_FONT_FAMILY: &str = "'DejaVu Sans', 'Bitstream Vera Sans', Verdana, sans-serif";

/// The width of `text` set in the bundled font at `size_px` pixels per em.
///
/// From the glyphs' advance widths, without kerning or shaping, which is
/// exact for the digits, signs and Latin text axis labels hold and close
/// enough for anything else. Used only to lay a figure out (how much room
/// the tick labels need), never to place glyphs, which resvg does itself.
pub fn text_width(text: &str, size_px: f64) -> f64 {
    let Ok(font) = skrifa::FontRef::new(FONT_DATA) else {
        // The bundled font is compiled in; failing to parse it is a build
        // problem, caught by the test below. A rough per-character estimate
        // keeps layout working regardless.
        return text.chars().count() as f64 * size_px * 0.6;
    };
    let charmap = font.charmap();
    let metrics = font.glyph_metrics(Size::new(size_px as f32), LocationRef::default());
    text.chars()
        .map(|c| {
            charmap
                .map(c)
                .and_then(|glyph| metrics.advance_width(glyph))
                .map_or(size_px * 0.6, f64::from)
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundled_font_parses_and_measures() {
        assert!(skrifa::FontRef::new(FONT_DATA).is_ok());
        // DejaVu Sans digits are 0.636 em wide.
        let w = text_width("100", 10.0);
        assert!((w - 19.08).abs() < 0.1, "{w}");
        assert!(text_width("Drønbreen", 10.0) > text_width("Dronbree", 10.0));
    }
}
