//! Phase 4 — glyph **metrics + rasterization** (ADR-INKREAD-0007 / RR2-FR5, RR2-AC2).
//!
//! Two pieces, both on `ab_glyph` (pure Rust, Apache-2.0/MIT; cross-compiles to aarch64-android
//! with no native font library):
//!
//! 1. [`AbFont`] implements [`Metrics`](crate::layout::Metrics) with **real glyph advances** (and
//!    kerning), so [`paginate`](crate::layout::paginate) lays text out to the actual font.
//! 2. [`render_page`] rasterizes a laid-out [`Page`] into a [`GrayCanvas`] (8-bit, `255` = paper,
//!    `0` = ink) — the grayscale surface the `reader-core` adapter (Phase 5) converts into the
//!    RGBA `PixelBuffer` the shell blits.
//!
//! A readable book serif (**Spectral**, OFL) is embedded as the default face. Bold is synthesized by
//! a 1px horizontal smear (so headings read heavier without bundling a second face); true bold/italic
//! faces and full shaping (ligatures, complex scripts) are later refinements — see the module's
//! divergence note in [`layout`](crate::layout).

use ab_glyph::{point, Font, FontVec, PxScale, ScaleFont};

use crate::layout::{LayoutOpts, Metrics, Page, SourceAnchor};

/// The bundled default reading face — Spectral Regular (SIL OFL 1.1; see `fonts/OFL.txt`).
const DEFAULT_FONT: &[u8] = include_bytes!("../fonts/Spectral-Regular.ttf");

/// A font for measuring + rasterizing reflow text. Owns its bytes (so it is `Send + Sync`, usable
/// from the `reader-core` document handle across the JNI thread).
pub struct AbFont {
    font: FontVec,
}

impl AbFont {
    /// The embedded default reading face.
    #[must_use]
    pub fn default_font() -> Self {
        Self::from_bytes(DEFAULT_FONT.to_vec()).expect("bundled Spectral font is valid")
    }

    /// Load a face from owned TTF/OTF bytes (e.g. a user-chosen font); `None` if unparseable.
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Option<Self> {
        FontVec::try_from_vec(bytes).ok().map(|font| Self { font })
    }
}

impl Metrics for AbFont {
    fn advance(&self, text: &str, size_px: f32, _bold: bool, _italic: bool) -> f32 {
        let sf = self.font.as_scaled(PxScale::from(size_px));
        let mut width = 0.0;
        let mut prev = None;
        for ch in text.chars() {
            let id = self.font.glyph_id(ch);
            if let Some(p) = prev {
                width += sf.kern(p, id);
            }
            width += sf.h_advance(id);
            prev = Some(id);
        }
        width
    }
}

/// An 8-bit grayscale canvas: `255` = white paper, `0` = black ink (e-ink native).
#[derive(Debug, Clone)]
pub struct GrayCanvas {
    pub width: u32,
    pub height: u32,
    /// Row-major, one byte per pixel.
    pub pixels: Vec<u8>,
}

impl GrayCanvas {
    /// A blank (all-white) canvas.
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![255u8; (width as usize) * (height as usize)],
        }
    }

    /// Darken pixel `(x, y)` by `coverage` ∈ [0,1] (alpha-over black onto the current value).
    #[inline]
    fn blend(&mut self, x: i32, y: i32, coverage: f32) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 || coverage <= 0.0 {
            return;
        }
        let idx = y as usize * self.width as usize + x as usize;
        let cur = self.pixels[idx] as f32;
        let v = cur * (1.0 - coverage.min(1.0));
        self.pixels[idx] = v.round().clamp(0.0, 255.0) as u8;
    }
}

/// Rasterize a laid-out [`Page`] into `canvas` at the page's pixel size, offsetting content by
/// `opts.margin`. The canvas should be `opts.page_w × opts.page_h`; out-of-bounds pixels are clipped.
pub fn render_page(page: &Page, opts: &LayoutOpts, font: &AbFont, canvas: &mut GrayCanvas) {
    let margin = opts.margin;
    for line in &page.lines {
        if line.rule {
            // A hairline rule across the content width, vertically centred in the line slot.
            let y = (margin + line.top + line.height * 0.5).round() as i32;
            let x0 = margin.round() as i32;
            let x1 = (opts.page_w - margin).round() as i32;
            for x in x0..x1 {
                canvas.blend(x, y, 0.6);
            }
            continue;
        }
        for run in &line.runs {
            let scale = PxScale::from(run.size_px);
            let sf = font.font.as_scaled(scale);
            let ascent = sf.ascent();
            let baseline = margin + line.top + ascent;
            let mut pen_x = margin + run.x;
            let mut prev = None;
            for ch in run.text.chars() {
                let id = font.font.glyph_id(ch);
                if let Some(p) = prev {
                    pen_x += sf.kern(p, id);
                }
                let glyph = id.with_scale_and_position(scale, point(pen_x, baseline));
                if let Some(outlined) = font.font.outline_glyph(glyph) {
                    let bb = outlined.px_bounds();
                    let (ox, oy) = (bb.min.x as i32, bb.min.y as i32);
                    outlined.draw(|gx, gy, c| {
                        let px = ox + gx as i32;
                        let py = oy + gy as i32;
                        canvas.blend(px, py, c);
                        if run.bold {
                            // Synthesized bold: a horizontal smear thickens the stem. Scale the smear
                            // with the font size so large headings read clearly bold (a fixed 1px is
                            // invisible on a 68px title); inline body bold stays a subtle 1px.
                            let weight = ((run.size_px / 26.0).round() as i32).clamp(1, 3);
                            for dx in 1..=weight {
                                canvas.blend(px + dx, py, c);
                            }
                        }
                    });
                }
                pen_x += sf.h_advance(id);
                prev = Some(id);
            }
        }
    }
}

/// A glyph with its pixel box on the page (top-left origin), mirroring [`render_page`]'s layout
/// transform. Feeds text selection + in-document search in `reader-core` (which normalizes the box
/// to `[0,1]`). The vertical extent is the **line box** (`top..top+height`) so boxes on a line
/// align, matching the selection logic's "same line" grouping.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacedGlyph {
    pub ch: char,
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    /// Reflow-stable source anchor of this glyph (ADR-INKREAD-0012): the run's `block` and the
    /// glyph's chapter-relative `char_offset`. Lets `reader-core` mint a `PinPosition` from a
    /// selection or a page's first glyph.
    pub anchor: SourceAnchor,
}

/// Extract a laid-out [`Page`]'s glyphs as positioned boxes (pixel space), walking runs **exactly**
/// like [`render_page`] so a selection/search highlight lands on the painted glyphs. A single space
/// glyph is synthesized between consecutive runs on a line (the layout drops inter-word spaces into
/// run `x` offsets) so multi-word selection/search reads with spaces. Rule lines contribute nothing.
#[must_use]
pub fn page_glyphs(page: &Page, opts: &LayoutOpts, font: &AbFont) -> Vec<PlacedGlyph> {
    let margin = opts.margin;
    let mut out = Vec::new();
    for line in &page.lines {
        if line.rule {
            continue;
        }
        let y0 = margin + line.top;
        let y1 = y0 + line.height;
        let mut prev_run_end: Option<(f32, SourceAnchor)> = None;
        for run in &line.runs {
            let sf = font.font.as_scaled(PxScale::from(run.size_px));
            let run_start = margin + run.x;
            // Bridge the gap to the previous run on this line with a space glyph, anchored just past
            // the previous run's last character (its char_offset + its length = the space position).
            if let Some((end, prev_anchor)) = prev_run_end {
                if run_start > end {
                    out.push(PlacedGlyph {
                        ch: ' ',
                        x0: end,
                        y0,
                        x1: run_start,
                        y1,
                        anchor: prev_anchor,
                    });
                }
            }
            let mut pen_x = run_start;
            let mut prev = None;
            // The glyph's chapter-relative offset = the run's first-char offset + its index in the run.
            for (i, ch) in run.text.chars().enumerate() {
                let id = font.font.glyph_id(ch);
                if let Some(p) = prev {
                    pen_x += sf.kern(p, id);
                }
                let adv = sf.h_advance(id);
                out.push(PlacedGlyph {
                    ch,
                    x0: pen_x,
                    y0,
                    x1: pen_x + adv,
                    y1,
                    anchor: SourceAnchor {
                        block: run.anchor.block,
                        char_offset: run.anchor.char_offset + i,
                    },
                });
                pen_x += adv;
                prev = Some(id);
            }
            let run_end_anchor = SourceAnchor {
                block: run.anchor.block,
                char_offset: run.anchor.char_offset + run.text.chars().count(),
            };
            prev_run_end = Some((pen_x, run_end_anchor));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::{Block, Inline, TextRun};
    use crate::layout::paginate;

    fn paragraph(text: &str) -> Block {
        Block::Paragraph {
            content: vec![Inline::Run(TextRun {
                text: text.into(),
                bold: false,
                italic: false,
                href: None,
            })],
        }
    }

    fn ink_count(c: &GrayCanvas) -> usize {
        c.pixels.iter().filter(|&&p| p < 250).count()
    }

    #[test]
    fn advance_is_positive_and_scales_linearly() {
        let f = AbFont::default_font();
        let a16 = f.advance("Reading", 16.0, false, false);
        let a32 = f.advance("Reading", 32.0, false, false);
        assert!(a16 > 0.0);
        assert!(
            (a32 - 2.0 * a16).abs() < 0.5,
            "advance scales with size: {a16} {a32}"
        );
        // Wider string ⇒ wider advance.
        assert!(f.advance("Reading more", 16.0, false, false) > a16);
    }

    #[test]
    fn renders_ink_within_margins() {
        let font = AbFont::default_font();
        let opts = LayoutOpts::new(400.0, 600.0, 18.0);
        let pages = paginate(&[paragraph("Hello, reflowed world.")], &opts, &font);
        let mut canvas = GrayCanvas::new(400, 600);
        render_page(&pages[0], &opts, &font, &mut canvas);

        assert!(ink_count(&canvas) > 50, "text produced ink");
        // The four corners (inside the margin) stay white.
        let m = opts.margin as i32;
        for &(x, y) in &[(2, 2), (398, 2), (2, 598), (398, 598)] {
            let idx = (y.min(599) as usize) * 400 + x.min(399) as usize;
            assert_eq!(canvas.pixels[idx], 255, "corner ({x},{y}) is paper");
        }
        // Ink lives below the top margin (no glyphs painted above it).
        let top_band: usize = (0..(m.max(1) as usize))
            .flat_map(|y| (0..400).map(move |x| (y, x)))
            .filter(|&(y, x)| canvas.pixels[y * 400 + x] < 250)
            .count();
        assert_eq!(top_band, 0, "no ink above the top margin");
    }

    #[test]
    fn page_glyphs_recovers_text_in_reading_order() {
        let font = AbFont::default_font();
        let opts = LayoutOpts::new(400.0, 600.0, 18.0);
        let pages = paginate(&[paragraph("Hello reflowed world")], &opts, &font);
        let glyphs = page_glyphs(&pages[0], &opts, &font);
        let text: String = glyphs.iter().map(|g| g.ch).collect();
        assert_eq!(text, "Hello reflowed world", "words rejoined with spaces");
        // Boxes are inside the page and ordered left-to-right on the (single) line.
        assert!(glyphs.iter().all(|g| g.x0 >= 0.0 && g.x1 <= 400.0));
        let first_word = glyphs
            .iter()
            .take_while(|g| g.ch != ' ')
            .collect::<Vec<_>>();
        assert!(first_word.windows(2).all(|w| w[0].x0 <= w[1].x0));
    }

    #[test]
    fn glyph_anchors_index_the_chapter_text() {
        // On one line each glyph's char_offset equals its position in the rejoined text (words +
        // single inter-word spaces), and all sit in block 0.
        let font = AbFont::default_font();
        let opts = LayoutOpts::new(4000.0, 600.0, 18.0);
        let pages = paginate(&[paragraph("Hello reflowed world")], &opts, &font);
        let glyphs = page_glyphs(&pages[0], &opts, &font);
        assert_eq!(glyphs.len(), "Hello reflowed world".chars().count());
        for (i, g) in glyphs.iter().enumerate() {
            assert_eq!(g.anchor.block, 0);
            assert_eq!(g.anchor.char_offset, i, "glyph {:?} at {i}", g.ch);
        }
    }

    #[test]
    fn glyph_anchors_are_stable_across_font_size() {
        // The headline property (RR8-AC1 / ADR-INKREAD-0012): a character keeps its
        // (block, char_offset) when the page reflows at a different size, so a highlight/Digest
        // re-resolves to the same text. Synthesized inter-word spaces are layout-dependent and
        // excluded; word characters must agree exactly.
        let font = AbFont::default_font();
        let blocks = [
            paragraph("The quick brown fox jumps over"),
            paragraph("the lazy dog sleeps soundly now"),
        ];
        let collect = |fp: f32| -> std::collections::BTreeMap<(usize, usize), char> {
            let opts = LayoutOpts::new(220.0, 400.0, fp);
            paginate(&blocks, &opts, &font)
                .iter()
                .flat_map(|p| page_glyphs(p, &opts, &font))
                .filter(|g| g.ch != ' ')
                .map(|g| ((g.anchor.block, g.anchor.char_offset), g.ch))
                .collect()
        };
        let small = collect(13.0);
        let large = collect(26.0);
        assert!(!small.is_empty());
        assert_eq!(
            small, large,
            "a glyph keeps its (block, char_offset) across a font-size change"
        );
    }

    #[test]
    fn empty_page_is_blank() {
        let font = AbFont::default_font();
        let opts = LayoutOpts::new(200.0, 200.0, 16.0);
        let mut canvas = GrayCanvas::new(200, 200);
        render_page(&Page::default(), &opts, &font, &mut canvas);
        assert_eq!(ink_count(&canvas), 0);
    }

    #[test]
    fn bold_heading_inks_more_than_regular() {
        let font = AbFont::default_font();
        let opts = LayoutOpts::new(400.0, 600.0, 18.0);

        let heading = Block::Heading {
            level: 3,
            content: vec![Inline::Run(TextRun {
                text: "Title".into(),
                bold: false,
                italic: false,
                href: None,
            })],
        };
        let pg_bold = paginate(&[heading], &opts, &font);
        let mut c_bold = GrayCanvas::new(400, 600);
        render_page(&pg_bold[0], &opts, &font, &mut c_bold);

        let pg_plain = paginate(&[paragraph("Title")], &opts, &font);
        let mut c_plain = GrayCanvas::new(400, 600);
        render_page(&pg_plain[0], &opts, &font, &mut c_plain);

        // The h3 is larger AND bold-smeared → strictly more ink than the body-size plain word.
        assert!(
            ink_count(&c_bold) > ink_count(&c_plain),
            "bold heading inks more: {} vs {}",
            ink_count(&c_bold),
            ink_count(&c_plain)
        );
    }
}
