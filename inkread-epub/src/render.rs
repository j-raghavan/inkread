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
//! A readable book serif (**Spectral**, OFL) is embedded as the default family. Spectral, Noto Serif
//! and Noto Sans bundle real Bold/Italic/BoldItalic faces; the remaining families ship Regular only
//! and have their styles synthesized (a stem smear for weight, a shear for slant). Full shaping
//! (ligatures, complex scripts) is a later refinement — see the module's divergence note in
//! [`layout`](crate::layout).

use std::sync::{Arc, OnceLock, RwLock};

use ab_glyph::{point, Font, FontVec, GlyphId, PxScale, ScaleFont};
use hyphenation::{Hyphenator as _, Language, Load, Standard};

use crate::layout::{Hyphenator, LayoutOpts, Metrics, Page, SourceAnchor, Wrap};

/// The bundled default reading face — Spectral Regular (SIL OFL 1.1; see `fonts/OFL.txt`).
const DEFAULT_FONT: &[u8] = include_bytes!("../fonts/Spectral-Regular.ttf");

/// One selectable reading family. Only `regular` is required; a style the family does not bundle is
/// synthesized from the nearest face it does (see [`Synth`]), so a family may ship as little as one
/// file and still render bold and italic text distinguishably.
struct Family {
    name: &'static str,
    regular: &'static [u8],
    bold: Option<&'static [u8]>,
    italic: Option<&'static [u8]>,
    bold_italic: Option<&'static [u8]>,
}

/// The selectable reading families (the open-source set KOReader ships). `id` is the index; id 0 =
/// the default. Licenses are noted in LICENSES-3RDPARTY.md.
const READING_FONTS: &[Family] = &[
    // OFL 1.1 (serif, default)
    Family {
        name: "Spectral",
        regular: DEFAULT_FONT,
        bold: Some(include_bytes!("../fonts/Spectral-Bold.ttf")),
        italic: Some(include_bytes!("../fonts/Spectral-Italic.ttf")),
        bold_italic: Some(include_bytes!("../fonts/Spectral-BoldItalic.ttf")),
    },
    // OFL 1.1
    Family {
        name: "Noto Serif",
        regular: include_bytes!("../fonts/NotoSerif-Regular.ttf"),
        bold: Some(include_bytes!("../fonts/NotoSerif-Bold.ttf")),
        italic: Some(include_bytes!("../fonts/NotoSerif-Italic.ttf")),
        bold_italic: Some(include_bytes!("../fonts/NotoSerif-BoldItalic.ttf")),
    },
    // OFL 1.1
    Family {
        name: "Noto Sans",
        regular: include_bytes!("../fonts/NotoSans-Regular.ttf"),
        bold: Some(include_bytes!("../fonts/NotoSans-Bold.ttf")),
        italic: Some(include_bytes!("../fonts/NotoSans-Italic.ttf")),
        bold_italic: Some(include_bytes!("../fonts/NotoSans-BoldItalic.ttf")),
    },
    // GPL-3.0 + font exception
    Family {
        name: "Free Serif",
        regular: include_bytes!("../fonts/FreeSerif.ttf"),
        bold: None,
        italic: None,
        bold_italic: None,
    },
    // GPL-3.0 + font exception
    Family {
        name: "Free Sans",
        regular: include_bytes!("../fonts/FreeSans.ttf"),
        bold: None,
        italic: None,
        bold_italic: None,
    },
    // Apache-2.0
    Family {
        name: "Droid Mono",
        regular: include_bytes!("../fonts/DroidSansMono.ttf"),
        bold: None,
        italic: None,
        bold_italic: None,
    },
];

/// What a family cannot supply for a requested style, and the rasterizer must therefore fake. Both
/// `false` — a real face exists — is the good case and the one that looks right.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Synth {
    /// Thicken the stems: the family has no bold face.
    embolden: bool,
    /// Slant the glyph: the family has no italic face.
    oblique: bool,
}

/// Faux-italic slant as x per y — about 12 degrees, the conventional oblique. A sheared serif is not
/// a real italic (which redraws the letterforms), but it is unmistakably not upright.
const OBLIQUE_SLANT: f32 = 0.21;

/// Extra ink a synthesized bold adds per glyph, in pixels — and therefore extra *advance*, since the
/// smear widens the drawn glyph and the measured line has to pay for it. Scales with the font size:
/// a fixed 1px is invisible on a 68px title, while inline body bold wants to stay subtle.
fn embolden_px(size_px: f32) -> i32 {
    ((size_px / 26.0).round() as i32).clamp(1, 3)
}

/// Display names of the selectable reading faces, in `id` order (for the shell's font picker).
#[must_use]
pub fn reading_font_names() -> Vec<String> {
    READING_FONTS.iter().map(|f| f.name.to_string()).collect()
}

/// Fallback face for glyphs the reading face lacks — e.g. musical symbols (𝄞) in books like
/// *Project Hail Mary*, which Spectral has no glyphs for and would otherwise draw as `.notdef`
/// boxes. Noto Music (SIL OFL 1.1) covers the Musical Symbols block.
const FALLBACK_FONT: &[u8] = include_bytes!("../fonts/NotoMusic-Regular.ttf");

/// The bundled symbol fallback, parsed once and shared — every [`AbFont`] construction (each face
/// switch, each reflow view) reuses it instead of re-parsing the TTF.
fn bundled_fallback() -> Arc<FontVec> {
    static BUNDLED: OnceLock<Arc<FontVec>> = OnceLock::new();
    BUNDLED
        .get_or_init(|| {
            Arc::new(
                FontVec::try_from_vec(FALLBACK_FONT.to_vec())
                    .expect("bundled fallback face is valid"),
            )
        })
        .clone()
}

/// Fallback faces registered at runtime, in registration order. Process-wide by design: fonts are
/// immutable shared assets and `AbFont` values are rebuilt on every face switch/repagination, so a
/// registry lets each construction pick the chain up without threading state through every call
/// site (sessions, reflow views, the daily assembler).
fn extra_fallbacks() -> &'static RwLock<Vec<Arc<FontVec>>> {
    static EXTRA: OnceLock<RwLock<Vec<Arc<FontVec>>>> = OnceLock::new();
    EXTRA.get_or_init(|| RwLock::new(Vec::new()))
}

/// Register a runtime fallback face from raw TTF/OTF/TTC bytes for **all subsequently built**
/// [`AbFont`]s — the shell hands over faces the bundled set lacks (e.g. a device CJK font) so
/// scripts outside the bundled coverage stop rendering as `.notdef` boxes. `collection_index`
/// selects the face inside a TrueType collection (`0` for plain TTF/OTF). Returns `false` — never
/// panics — if the bytes don't parse or the index is out of range; already-built `AbFont`s are
/// unaffected (the shell registers at startup, before any document opens).
pub fn register_fallback_font(bytes: Vec<u8>, collection_index: u32) -> bool {
    let Ok(font) = FontVec::try_from_vec_and_index(bytes, collection_index) else {
        return false;
    };
    let mut extras = match extra_fallbacks().write() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(), // a font Vec can't be half-written; keep going
    };
    extras.push(Arc::new(font));
    true
}

/// The full fallback chain for a new [`AbFont`]: the bundled symbol face first (stable ordering —
/// runtime registrations can extend but never pre-empt bundled coverage), then runtime faces in
/// registration order.
fn fallback_chain() -> Vec<Arc<FontVec>> {
    let extras = match extra_fallbacks().read() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    std::iter::once(bundled_fallback())
        .chain(extras.iter().cloned())
        .collect()
}

/// Parse a bundled face; `None` if the bytes aren't a usable TTF/OTF.
fn parse_face(bytes: &'static [u8]) -> Option<FontVec> {
    FontVec::try_from_vec(bytes.to_vec()).ok()
}

/// A font for measuring + rasterizing reflow text. Owns its bytes (so it is `Send + Sync`, usable
/// from the `reader-core` document handle across the JNI thread). One reading *family* — a regular
/// face plus whichever of bold/italic/bold-italic it bundles — over the shared fallback chain,
/// consulted in order for any character the family doesn't cover.
pub struct AbFont {
    /// Always present, and the metric reference: line baselines come from this face so every run on
    /// a line sits on the same one, whatever style or fallback supplies the glyph.
    regular: FontVec,
    bold: Option<FontVec>,
    italic: Option<FontVec>,
    bold_italic: Option<FontVec>,
    fallbacks: Vec<Arc<FontVec>>,
}

impl AbFont {
    /// The embedded default reading family (Spectral), with the bundled symbol fallback.
    #[must_use]
    pub fn default_font() -> Self {
        Self::for_face(0)
    }

    /// The reading family for `id` (index into the bundled set; out-of-range → the default), each
    /// with the shared fallback chain (bundled Noto Music + any runtime-registered faces) so missing
    /// glyphs — musical symbols, scripts the bundled set lacks — still render.
    #[must_use]
    pub fn for_face(id: usize) -> Self {
        let family = READING_FONTS.get(id).unwrap_or(&READING_FONTS[0]);
        let regular = parse_face(family.regular)
            .or_else(|| parse_face(DEFAULT_FONT))
            .expect("a bundled reading face is valid");
        Self {
            regular,
            bold: family.bold.and_then(parse_face),
            italic: family.italic.and_then(parse_face),
            bold_italic: family.bold_italic.and_then(parse_face),
            fallbacks: fallback_chain(),
        }
    }

    /// Load a face from owned TTF/OTF bytes (e.g. a user-chosen font); `None` if unparseable. One
    /// file is one style, so bold and italic are synthesized from it. Gets the same shared fallback
    /// chain as the bundled families — a custom primary shouldn't lose symbol/script coverage.
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Option<Self> {
        FontVec::try_from_vec(bytes).ok().map(|regular| Self {
            regular,
            bold: None,
            italic: None,
            bold_italic: None,
            fallbacks: fallback_chain(),
        })
    }

    /// The family's face for a style, and whatever it cannot supply. Bold-italic degrades through
    /// the nearest real face — a bold one slanted, else an italic one thickened — before falling
    /// back to faking both on the regular.
    fn styled(&self, bold: bool, italic: bool) -> (&FontVec, Synth) {
        let fake = |embolden, oblique| Synth { embolden, oblique };
        match (bold, italic) {
            (false, false) => (&self.regular, Synth::default()),
            (true, false) => match &self.bold {
                Some(f) => (f, Synth::default()),
                None => (&self.regular, fake(true, false)),
            },
            (false, true) => match &self.italic {
                Some(f) => (f, Synth::default()),
                None => (&self.regular, fake(false, true)),
            },
            (true, true) => match (&self.bold_italic, &self.bold, &self.italic) {
                (Some(f), _, _) => (f, Synth::default()),
                (None, Some(f), _) => (f, fake(false, true)),
                (None, None, Some(f)) => (f, fake(true, false)),
                (None, None, None) => (&self.regular, fake(true, true)),
            },
        }
    }

    /// The face to render `ch` with for a style, plus what to synthesize onto it: the styled face if
    /// it has the glyph, else the first fallback that does, else the styled face (so an unknown
    /// glyph still renders its `.notdef`). Fallbacks are regular-weight only, so a styled run that
    /// lands on one has its style synthesized.
    fn face_for(&self, ch: char, bold: bool, italic: bool) -> (&FontVec, Synth) {
        let (face, synth) = self.styled(bold, italic);
        if face.glyph_id(ch).0 != 0 {
            return (face, synth);
        }
        match self.fallbacks.iter().find(|f| f.glyph_id(ch).0 != 0) {
            Some(f) => (
                f.as_ref(),
                Synth {
                    embolden: bold,
                    oblique: italic,
                },
            ),
            None => (face, synth),
        }
    }
}

/// English (US) Knuth-Liang hyphenation — the same pattern model KOReader uses — so justified lines
/// break long words like a book. Patterns are embedded (no filesystem); construction is fallible only
/// if the bundled data is corrupt, so [`Self::new`] is infallible in practice.
pub struct EnHyphenator {
    dict: Standard,
}

impl EnHyphenator {
    /// Load the embedded en-US patterns.
    #[must_use]
    pub fn new() -> Self {
        Self {
            dict: Standard::from_embedded(Language::EnglishUS)
                .expect("embedded en-US patterns valid"),
        }
    }
}

impl Default for EnHyphenator {
    fn default() -> Self {
        Self::new()
    }
}

impl Hyphenator for EnHyphenator {
    fn opportunities(&self, word: &str) -> Vec<usize> {
        // `breaks` are byte offsets into `word` where a soft hyphen may be inserted (ascending). The
        // dictionary enforces sensible left/right minimums, so short fragments don't occur.
        self.dict.hyphenate(word).breaks
    }
}

impl Metrics for AbFont {
    fn advance(&self, text: &str, size_px: f32, bold: bool, italic: bool) -> f32 {
        let scale = PxScale::from(size_px);
        let mut width = 0.0;
        // Track the previous glyph's face so kerning is only applied within the same face.
        let mut prev: Option<(&FontVec, GlyphId)> = None;
        for ch in text.chars() {
            let (face, synth) = self.face_for(ch, bold, italic);
            let sf = face.as_scaled(scale);
            let id = face.glyph_id(ch);
            if let Some((pf, pid)) = prev {
                if std::ptr::eq(pf, face) {
                    width += sf.kern(pid, id);
                }
            }
            width += sf.h_advance(id);
            if synth.embolden {
                width += embolden_px(size_px) as f32;
            }
            prev = Some((face, id));
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

    /// Write pixel `(x, y)` to `value` outright, clipping outside the canvas. Used for imagery,
    /// which replaces the page rather than inking over it.
    #[inline]
    fn put(&mut self, x: i32, y: i32, value: u8) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let idx = y as usize * self.width as usize + x as usize;
        self.pixels[idx] = value;
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

/// Draw one laid-out illustration into `canvas`, decoded and resampled to the box layout reserved.
///
/// A picture that will not resolve or will not decode leaves its box blank rather than failing the
/// page: a book with one broken image still reads (RR21-FR3).
fn draw_image(
    placed: &crate::layout::PlacedImage,
    margin: f32,
    top: f32,
    images: &dyn ImageSource,
    canvas: &mut GrayCanvas,
) {
    let Some(raw) = images.bytes(&placed.src) else {
        return;
    };
    let Ok(decoded) = crate::img::decode(&raw) else {
        return;
    };
    let gray = crate::img::scale_to_gray(&decoded, placed.width, placed.height);
    if gray.is_empty() {
        return;
    }
    let x0 = (margin.round() as i32) + placed.x;
    let y0 = (margin + top).round() as i32;
    for row in 0..placed.height {
        for col in 0..placed.width {
            let Some(&v) = gray.get(row as usize * placed.width as usize + col as usize) else {
                continue;
            };
            canvas.put(x0 + col as i32, y0 + row as i32, v);
        }
    }
}

/// Rasterize a laid-out [`Page`] into `canvas` at the page's pixel size, offsetting content by
/// `opts.margin`. The canvas should be `opts.page_w × opts.page_h`; out-of-bounds pixels are clipped.
pub fn render_page(page: &Page, opts: &LayoutOpts, font: &AbFont, canvas: &mut GrayCanvas) {
    render_page_with_images(page, opts, font, &NoImageBytes, canvas);
}

/// Supplies the encoded bytes of an image, so the renderer can draw it. Injected rather than owned:
/// the render stage holds no resources, exactly as layout holds none (#187).
pub trait ImageSource {
    /// The encoded (PNG/JPEG) bytes for `src`, or `None` when it cannot be resolved.
    fn bytes(&self, src: &str) -> Option<Vec<u8>>;
}

/// The no-images source: nothing resolves, so laid-out images are skipped.
pub struct NoImageBytes;

impl ImageSource for NoImageBytes {
    fn bytes(&self, _src: &str) -> Option<Vec<u8>> {
        None
    }
}

/// As [`render_page`], with `images` resolving illustrations to their bytes so they are drawn
/// rather than left blank (#187).
pub fn render_page_with_images(
    page: &Page,
    opts: &LayoutOpts,
    font: &AbFont,
    images: &dyn ImageSource,
    canvas: &mut GrayCanvas,
) {
    let margin = opts.margin;
    for line in &page.lines {
        if let Some(placed) = &line.image {
            draw_image(placed, margin, line.top, images, canvas);
            continue;
        }
        if line.rule {
            // A hairline rule across the content width, vertically centred in the line slot. On a
            // two-column page it spans only its own column (#194) — a rule running across the
            // gutter would read as a divider between the columns rather than within one.
            let y = (margin + line.top + line.height * 0.5).round() as i32;
            let x0 = (margin + line.column_x).round() as i32;
            let x1 = (margin + line.column_x + opts.column_width()).round() as i32;
            for x in x0..x1 {
                canvas.blend(x, y, 0.6);
            }
            continue;
        }
        for run in &line.runs {
            let scale = PxScale::from(run.size_px);
            // Baseline from the regular face so every run on the line — styled, or supplied by a
            // fallback — sits on the same one.
            let baseline = margin + line.top + font.regular.as_scaled(scale).ascent();
            let mut pen_x = margin + run.x;
            let mut prev: Option<(&FontVec, GlyphId)> = None;
            for ch in run.text.chars() {
                let (face, synth) = font.face_for(ch, run.bold, run.italic);
                let sf = face.as_scaled(scale);
                let id = face.glyph_id(ch);
                if let Some((pf, pid)) = prev {
                    if std::ptr::eq(pf, face) {
                        pen_x += sf.kern(pid, id);
                    }
                }
                // Kept in step with `page_glyphs` and `AbFont::advance`: all three walk a run the
                // same way, so a selection box lands on the ink it belongs to.
                let smear = if synth.embolden {
                    embolden_px(run.size_px)
                } else {
                    0
                };
                let glyph = id.with_scale_and_position(scale, point(pen_x, baseline));
                if let Some(outlined) = face.outline_glyph(glyph) {
                    let bb = outlined.px_bounds();
                    let (ox, oy) = (bb.min.x as i32, bb.min.y as i32);
                    outlined.draw(|gx, gy, c| {
                        let py = oy + gy as i32;
                        // Faux italic: shift each row sideways in proportion to its height above the
                        // baseline, which leans the glyph without touching its outline.
                        let slant = if synth.oblique {
                            ((baseline - py as f32) * OBLIQUE_SLANT).round() as i32
                        } else {
                            0
                        };
                        let px = ox + gx as i32 + slant;
                        canvas.blend(px, py, c);
                        // Faux bold: a horizontal smear thickens the stem.
                        for dx in 1..=smear {
                            canvas.blend(px + dx, py, c);
                        }
                    });
                }
                pen_x += sf.h_advance(id) + smear as f32;
                prev = Some((face, id));
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
    /// Set on the **last** glyph of a line the layout broke mid-word ([`Wrap`]) — the flag its run
    /// carries, moved onto the glyph selection actually reaches. `None` on every other glyph.
    pub wrap: Option<Wrap>,
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
            let scale = PxScale::from(run.size_px);
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
                        wrap: None,
                    });
                }
            }
            let mut pen_x = run_start;
            let mut prev: Option<(&FontVec, GlyphId)> = None;
            let run_first = out.len();
            // The glyph's chapter-relative offset = the run's first-char offset + its index in the run.
            for (i, ch) in run.text.chars().enumerate() {
                let (face, synth) = font.face_for(ch, run.bold, run.italic);
                let sf = face.as_scaled(scale);
                let id = face.glyph_id(ch);
                if let Some((pf, pid)) = prev {
                    if std::ptr::eq(pf, face) {
                        pen_x += sf.kern(pid, id);
                    }
                }
                // Same advance `render_page` uses, synthesized bold included, so the boxes track the
                // ink. A synthesized oblique leans the glyph within its box and doesn't move the pen.
                let adv = sf.h_advance(id)
                    + if synth.embolden {
                        embolden_px(run.size_px) as f32
                    } else {
                        0.0
                    };
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
                    wrap: None,
                });
                pen_x += adv;
                prev = Some((face, id));
            }
            // A run that ends its line mid-word hands the fact to its last glyph, which is the one
            // selection meets at the break.
            if run.wrap.is_some() && out.len() > run_first {
                if let Some(last) = out.last_mut() {
                    last.wrap = run.wrap;
                }
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
    use crate::css::BlockStyle;
    use crate::layout::paginate;

    fn paragraph(text: &str) -> Block {
        Block::Paragraph {
            content: vec![Inline::Run(TextRun {
                text: text.into(),
                bold: false,
                italic: false,
                href: None,
            })],
            style: BlockStyle::default(),
        }
    }

    fn ink_count(c: &GrayCanvas) -> usize {
        c.pixels.iter().filter(|&&p| p < 250).count()
    }

    #[test]
    fn fallback_face_covers_a_glyph_the_primary_lacks() {
        let f = AbFont::default_font();
        // G-clef (U+1D11E): Spectral has no glyph (→ .notdef box), Noto Music does. With the
        // fallback chain, face_for must resolve to a face that has it, and it must advance + render.
        let clef = '\u{1D11E}';
        assert_eq!(
            f.regular.glyph_id(clef).0,
            0,
            "Spectral has no clef glyph (would box)"
        );
        assert_ne!(
            f.face_for(clef, false, false).0.glyph_id(clef).0,
            0,
            "the fallback supplies a real clef glyph"
        );
        // It contributes positive width and inks pixels (not a blank .notdef).
        assert!(f.advance("\u{1D11E}", 40.0, false, false) > 0.0);
        let pages = paginate(
            &[paragraph("\u{1D11E}")],
            &LayoutOpts::new(300.0, 300.0, 40.0),
            &f,
        );
        let mut canvas = GrayCanvas::new(300, 300);
        render_page(
            &pages[0],
            &LayoutOpts::new(300.0, 300.0, 40.0),
            &f,
            &mut canvas,
        );
        assert!(ink_count(&canvas) > 0, "the clef renders actual ink");
    }

    /// Test-only Noto Sans SC subsets (SIL OFL 1.1; see LICENSES-3RDPARTY.md) — a plain TTF with a
    /// handful of Han glyphs, and a 2-face TTC whose face 0 is Latin-only and face 1 carries the
    /// same Han glyphs (so collection-index selection is observable).
    ///
    /// The registry these tests feed is process-global with no reset, and tests run in parallel:
    /// only assert *positively* (a registered glyph resolves). A test asserting a codepoint does
    /// NOT resolve through the chain would be order-dependent and flaky — probe the parsed face
    /// directly instead (see `ttc_collection_index_selects_the_face`).
    const CJK_SUBSET: &[u8] = include_bytes!("../fonts/test/NotoSansSC-subset.ttf");
    const CJK_TTC: &[u8] = include_bytes!("../fonts/test/NotoSansSC-test.ttc");

    #[test]
    fn registered_fallback_supplies_cjk_glyphs() {
        // Before the CJK face is reachable no bundled face covers Han — the primary would render
        // `.notdef` tofu (the reported bug). After registration every newly built AbFont resolves
        // Han to the registered face, and it advances + inks.
        let han = '\u{4F60}'; // 你
        assert!(register_fallback_font(CJK_SUBSET.to_vec(), 0));
        let f = AbFont::default_font();
        assert_eq!(f.regular.glyph_id(han).0, 0, "Spectral has no Han glyph");
        assert_ne!(
            f.face_for(han, false, false).0.glyph_id(han).0,
            0,
            "the registered fallback supplies 你"
        );
        assert!(f.advance("你好", 24.0, false, false) > 0.0);
        let opts = LayoutOpts::new(300.0, 300.0, 24.0);
        let pages = paginate(&[paragraph("你好")], &opts, &f);
        let mut canvas = GrayCanvas::new(300, 300);
        render_page(&pages[0], &opts, &f, &mut canvas);
        assert!(ink_count(&canvas) > 0, "Han glyphs render actual ink");
        // from_bytes primaries share the chain too (a custom face keeps script coverage).
        let custom = AbFont::from_bytes(DEFAULT_FONT.to_vec()).unwrap();
        assert_ne!(custom.face_for(han, false, false).0.glyph_id(han).0, 0);
    }

    #[test]
    fn ttc_collection_index_selects_the_face() {
        let han = '\u{4E16}'; // 世
                              // The index picks a distinct face — probe the parsed faces directly (registry-independent,
                              // so this stays order-safe against the other registering tests): face 0 is Latin-only,
                              // face 1 carries the Han glyphs.
        let face0 = FontVec::try_from_vec_and_index(CJK_TTC.to_vec(), 0).unwrap();
        let face1 = FontVec::try_from_vec_and_index(CJK_TTC.to_vec(), 1).unwrap();
        assert_eq!(face0.glyph_id(han).0, 0, "TTC face 0 has no 世");
        assert_ne!(face1.glyph_id(han).0, 0, "TTC face 1 supplies 世");
        // And the registry honors the index end-to-end: register Latin-only face 0 first, then
        // face 1 — resolution through the chain can therefore only come from the index-1 face.
        assert!(register_fallback_font(CJK_TTC.to_vec(), 0));
        assert!(register_fallback_font(CJK_TTC.to_vec(), 1));
        let f = AbFont::default_font();
        assert_ne!(
            f.face_for(han, false, false).0.glyph_id(han).0,
            0,
            "TTC face 1 supplies 世"
        );
        // An out-of-range collection index and garbage bytes are rejected, not panics (RR21-FR3).
        assert!(!register_fallback_font(CJK_TTC.to_vec(), 99));
        assert!(!register_fallback_font(vec![0u8; 64], 0));
        assert!(!register_fallback_font(Vec::new(), 0));
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

    /// One paragraph whose single run carries `bold`/`italic`, for the style tests below.
    fn styled_para(text: &str, bold: bool, italic: bool) -> Block {
        Block::Paragraph {
            content: vec![Inline::Run(TextRun {
                text: text.into(),
                bold,
                italic,
                href: None,
            })],
            style: BlockStyle::default(),
        }
    }

    /// Rasterize one styled paragraph and hand back the canvas.
    fn render_styled(text: &str, bold: bool, italic: bool) -> GrayCanvas {
        let font = AbFont::default_font();
        let opts = LayoutOpts::new(400.0, 200.0, 18.0);
        let pages = paginate(&[styled_para(text, bold, italic)], &opts, &font);
        let mut canvas = GrayCanvas::new(400, 200);
        render_page(&pages[0], &opts, &font, &mut canvas);
        canvas
    }

    /// A family that bundles Regular only, so every style is synthesized (Droid Mono, id 5).
    fn synth_only_family() -> AbFont {
        AbFont::for_face(5)
    }

    #[test]
    fn measured_width_accounts_for_a_synthesized_bold() {
        // The smear widens the drawn glyph, so the measured advance has to pay for it — otherwise
        // the line is laid out narrower than it is inked and justified text drifts.
        let font = synth_only_family();
        let plain = font.advance("Title", 18.0, false, false);
        let bold = font.advance("Title", 18.0, true, false);
        assert!(
            bold > plain,
            "bold must measure wider than regular: {bold} vs {plain}"
        );
        // Five glyphs, each smeared by `embolden_px`.
        let expected = plain + 5.0 * embolden_px(18.0) as f32;
        assert!((bold - expected).abs() < 0.01, "{bold} vs {expected}");
    }

    #[test]
    fn an_oblique_slant_does_not_change_the_advance() {
        // Faux italic leans the glyph inside its box; the pen does not move further. (A *real*
        // italic face has its own advances and is expected to differ — see the test below.)
        let font = synth_only_family();
        assert_eq!(
            font.advance("Title", 18.0, false, false),
            font.advance("Title", 18.0, false, true)
        );
    }

    #[test]
    fn italic_text_is_not_drawn_upright() {
        // Italic used to be parsed, carried through layout, and then ignored by the renderer, so it
        // rendered identically to regular. It must not.
        let plain = render_styled("handwriting", false, false);
        let italic = render_styled("handwriting", false, true);
        assert_ne!(
            plain.pixels, italic.pixels,
            "an italic run must not rasterize identically to a regular one"
        );
        assert!(ink_count(&italic) > 0);
    }

    #[test]
    fn page_glyph_boxes_follow_the_bold_advance() {
        // `page_glyphs` and `render_page` must walk a run identically, or a selection box lands
        // beside the ink it belongs to.
        let font = AbFont::default_font();
        let opts = LayoutOpts::new(400.0, 200.0, 18.0);
        let width = |bold| {
            let pages = paginate(&[styled_para("Title", bold, false)], &opts, &font);
            let g = page_glyphs(&pages[0], &opts, &font);
            let first = g.first().expect("a glyph");
            let last = g.last().expect("a glyph");
            last.x1 - first.x0
        };
        assert!(
            width(true) > width(false),
            "bold boxes must span the smeared ink"
        );
    }

    #[test]
    fn a_family_without_variants_synthesizes_both_styles() {
        // Droid Mono ships Regular only, so each style is faked — and the bold-italic case must
        // fake both rather than dropping one.
        let font = synth_only_family();
        assert_eq!(font.styled(false, false).1, Synth::default());
        assert!(font.styled(true, false).1.embolden);
        assert!(font.styled(false, true).1.oblique);
        let both = font.styled(true, true).1;
        assert!(both.embolden && both.oblique);
    }

    #[test]
    fn the_main_families_use_real_faces_and_fake_nothing() {
        // Spectral (the default), Noto Serif and Noto Sans bundle all four styles.
        for id in 0..3 {
            let font = AbFont::for_face(id);
            for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
                assert_eq!(
                    font.styled(bold, italic).1,
                    Synth::default(),
                    "face {id} bold={bold} italic={italic} should be a real face"
                );
            }
        }
    }

    #[test]
    fn a_real_italic_is_a_different_face_not_a_slanted_regular() {
        // The point of bundling the files: Spectral Italic redraws the letterforms, so its glyph
        // outlines and advances differ from the regular's — a shear could not produce this.
        let font = AbFont::default_font();
        let plain = font.advance("handwriting", 18.0, false, false);
        let italic = font.advance("handwriting", 18.0, false, true);
        assert!(
            (plain - italic).abs() > 0.01,
            "a real italic face has its own metrics: {plain} vs {italic}"
        );
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
            style: BlockStyle::default(),
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

    /// #187 — an illustration reaches the canvas as pixels.
    struct OneImage(Vec<u8>);

    impl ImageSource for OneImage {
        fn bytes(&self, src: &str) -> Option<Vec<u8>> {
            (src == "plate.png").then(|| self.0.clone())
        }
    }

    /// A solid mid-grey PNG of the given size.
    fn grey_png(w: u32, h: u32, level: u8) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().expect("header");
            writer
                .write_image_data(&vec![level; (w * h) as usize])
                .expect("data");
        }
        out
    }

    struct FixedSize(u32, u32);

    impl crate::layout::ImageSizer for FixedSize {
        fn size(&self, _src: &str) -> Option<(u32, u32)> {
            Some((self.0, self.1))
        }
    }

    fn image_page(opts: &LayoutOpts, w: u32, h: u32) -> Page {
        let blocks = [Block::Image {
            src: "plate.png".into(),
            alt: String::new(),
        }];
        crate::layout::paginate_with_images(
            &blocks,
            opts,
            &crate::measure::CachedMetrics::new(&AbFont::default_font()),
            &crate::layout::NoHyphen,
            &FixedSize(w, h),
        )
        .remove(0)
    }

    #[test]
    fn a_laid_out_image_is_drawn_onto_the_canvas() {
        let opts = LayoutOpts::new(200.0, 200.0, 16.0);
        let page = image_page(&opts, 40, 40);
        let mut canvas = GrayCanvas::new(200, 200);
        render_page_with_images(
            &page,
            &opts,
            &AbFont::default_font(),
            &OneImage(grey_png(40, 40, 100)),
            &mut canvas,
        );
        let drawn = canvas.pixels.iter().filter(|&&p| p == 100).count();
        assert!(drawn > 1_000, "expected a block of grey, got {drawn} px");
    }

    /// The same page rendered without a source must not fail — just leave the box empty.
    #[test]
    fn an_unresolvable_or_corrupt_image_leaves_the_page_blank_not_broken() {
        let opts = LayoutOpts::new(200.0, 200.0, 16.0);
        let page = image_page(&opts, 40, 40);
        for source in [
            Box::new(NoImageBytes) as Box<dyn ImageSource>,
            Box::new(OneImage(b"not an image".to_vec())),
            Box::new(OneImage(Vec::new())),
        ] {
            let mut canvas = GrayCanvas::new(200, 200);
            render_page_with_images(
                &page,
                &opts,
                &AbFont::default_font(),
                source.as_ref(),
                &mut canvas,
            );
            assert!(
                canvas.pixels.iter().all(|&p| p == 255),
                "nothing should have been drawn"
            );
        }
    }

    /// An image whose box runs past the canvas must clip, not panic or wrap.
    #[test]
    fn drawing_clips_at_the_canvas_edge() {
        let opts = LayoutOpts::new(200.0, 200.0, 16.0);
        let page = image_page(&opts, 400, 400);
        let mut canvas = GrayCanvas::new(60, 60); // deliberately smaller than the page
        render_page_with_images(
            &page,
            &opts,
            &AbFont::default_font(),
            &OneImage(grey_png(40, 40, 100)),
            &mut canvas,
        );
        assert_eq!(
            canvas.pixels.len(),
            60 * 60,
            "buffer must be untouched in size"
        );
    }
}
