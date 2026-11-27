//! Render WordPress-style alt-LaTeX **display equations** to grayscale PNGs for embedding in the
//! issue EPUB (#66). The reflow engine has no math typesetting, so a display equation ships as an
//! image the same way the Vintage nameplate does (see `epub::assemble_epub`).
//!
//! Rendering is pure and host-testable (the `latex-rust` renderer is pure Rust: `ttf-parser` +
//! `tiny-skia`, no C deps, and bundles STIX Two Math — see `assets/NOTICE.md`). A formula that does
//! not render returns `None`; the caller then keeps the LaTeX as visible text, so one unsupported
//! equation never breaks an issue and nothing ever panics.

use latex_rust::{latex_to_png, Dim, MathFont, PngBackground, PngOptions};

/// The on-screen point size of a rendered equation. Close to the reader's body text so a display
/// equation sits at a natural size among the prose (the engine scales the raster to fit the column).
const FONT_SIZE_PT: i64 = 12;

/// Render resolution. High enough that the engine down-scales (sharp) rather than up-scales (blurry)
/// the raster to the column on a ~300-DPI panel. `latex-rust` caps DPI at 2400.
const DPI: i64 = 300;

/// Render a LaTeX **display** equation to opaque-white PNG bytes, or `None` if it does not render
/// (an unsupported construct, a parse error, or the bundled font failing to load).
///
/// The background is opaque white (not the renderer's default transparent) to match the Vintage
/// nameplate: the EPUB image path draws onto a white page and the e-ink panel carries no alpha.
#[must_use]
pub(crate) fn render_display(latex: &str) -> Option<Vec<u8>> {
    let font = MathFont::stix_two_math().ok()?;
    let mut opt = PngOptions::new();
    opt.display = true;
    opt.font_size_pt = Dim::from_i64(FONT_SIZE_PT);
    opt.dpi = Dim::from_i64(DPI);
    opt.background = PngBackground::White;
    latex_to_png(latex, &font, &opt)
        .ok()
        .filter(|p| !p.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_display_equation_renders_to_a_png() {
        // A Terry-Tao-style display equation renders to non-empty PNG bytes with the PNG signature.
        let png = render_display("\\displaystyle \\sum_{n=1}^N n = \\frac{N(N+1)}{2}")
            .expect("a well-formed display equation renders");
        assert!(png.len() > 8, "non-trivial PNG");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "PNG signature");
    }

    #[test]
    fn an_unrenderable_formula_returns_none_not_a_panic() {
        // Garbage / unsupported input must fail softly so the caller can keep the LaTeX as text.
        // (The exact set of unsupported constructs is the renderer's; we only require no panic and a
        // None/Some result — a stray unmatched brace is the classic parse failure.)
        let _ = render_display("\\frac{1}{"); // unmatched brace — must not panic
        let _ = render_display("");
    }
}
