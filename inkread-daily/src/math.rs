//! Render WordPress-style alt-LaTeX **display equations** to grayscale PNGs for embedding in the
//! issue EPUB (#66). The reflow engine has no math typesetting, so a display equation ships as an
//! image the same way the Vintage nameplate does (see `epub::assemble_epub`).
//!
//! Rendering is pure and host-testable (the `latex-rust` renderer is pure Rust: `ttf-parser` +
//! `tiny-skia`, no C deps, and bundles STIX Two Math — see `assets/NOTICE.md`). A formula that does
//! not render returns `None`; the caller then keeps the LaTeX as visible text, so one unsupported
//! equation never breaks an issue. The renderer reports failures as `Err` (caught here), and the one
//! JNI caller runs assembly inside the bridge's `catch_unwind`, so a formula cannot crash the device.

use latex_rust::{latex_to_png, Dim, MathFont, PngBackground, PngOptions};

/// The on-screen point size of a rendered equation. Close to the reader's body text so a display
/// equation sits at a natural size among the prose (the engine scales the raster to fit the column).
const FONT_SIZE_PT: i64 = 12;

/// Render resolution. High enough that the engine down-scales (sharp) rather than up-scales (blurry)
/// the raster to the column on a ~300-DPI panel. `latex-rust` caps DPI at 2400.
const DPI: i64 = 300;

/// Load the bundled STIX Two Math face once, to render a batch of equations with (see
/// [`render_display`]). `None` if the embedded font fails to parse (a build-time invariant, so this
/// is not expected to fire), in which case the caller degrades every equation to its LaTeX text.
#[must_use]
pub(crate) fn load_font() -> Option<MathFont> {
    MathFont::stix_two_math().ok()
}

/// Render a LaTeX **display** equation to opaque-white PNG bytes with `font`, or `None` if it does
/// not render (an unsupported construct or a parse error — all reported as `Err`, never a panic).
///
/// The background is opaque white (not the renderer's default transparent) to match the Vintage
/// nameplate: the EPUB image path draws onto a white page and the e-ink panel carries no alpha.
#[must_use]
pub(crate) fn render_display(font: &MathFont, latex: &str) -> Option<Vec<u8>> {
    let mut opt = PngOptions::new();
    opt.display = true;
    opt.font_size_pt = Dim::from_i64(FONT_SIZE_PT);
    opt.dpi = Dim::from_i64(DPI);
    opt.background = PngBackground::White;
    latex_to_png(latex, font, &opt)
        .ok()
        .filter(|p| !p.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_display_equation_renders_to_a_png() {
        // A Terry-Tao-style display equation renders to non-empty PNG bytes with the PNG signature.
        let font = load_font().expect("the bundled STIX face loads");
        let png = render_display(&font, "\\displaystyle \\sum_{n=1}^N n = \\frac{N(N+1)}{2}")
            .expect("a well-formed display equation renders");
        assert!(png.len() > 8, "non-trivial PNG");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "PNG signature");
    }

    #[test]
    fn an_unrenderable_formula_returns_none_not_a_panic() {
        // An unparseable formula (unmatched brace) must fail softly — return None, not panic — so the
        // caller can keep the LaTeX as visible text.
        let font = load_font().expect("the bundled STIX face loads");
        assert!(
            render_display(&font, "\\frac{1}{").is_none(),
            "an unmatched brace does not render"
        );
    }
}
