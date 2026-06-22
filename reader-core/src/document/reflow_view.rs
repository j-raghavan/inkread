//! `ReflowView` — the reusable "reflowable units → layout cache → render / page-chars / repaginate"
//! engine that backs **PDF reflow** (ADR-INKREAD-0011, approach b).
//!
//! It takes a sequence of reflowable **units** (each a `Vec<Block>` — for PDF, one per *source*
//! page; ADR-0011 Decision 2 page-by-page) and reuses the existing `inkread-epub` layout/render
//! pipeline verbatim: `paginate` → `render_page` → `page_glyphs`. The result is that PDF reflow gets
//! font-size, line-spacing, alignment, selection, and search for free, with no new layout code.
//!
//! This mirrors the cache/repaginate shape of [`crate::document::reflow::EpubBackend`] but is a
//! standalone helper so the working EPUB backend is left untouched (approach b); a future refactor
//! could fold EPUB onto it. Each unit starts a new viewport page (book convention), which also gives
//! a stable unit → page mapping for reading-position preservation.

use std::cell::{Cell, RefCell};

use inkread_epub::layout::{paginate, Align, LayoutOpts, Page};
use inkread_epub::render::{page_glyphs, render_page as raster_page, AbFont, GrayCanvas};
use inkread_epub::Block;

use crate::document::text_select::{CharBox, NormRect};
use crate::error::{CoreError, CoreResult};
use crate::render::PixelBuffer;

/// Base body font size in device pixels at scale `1.0` — matches the EPUB backend so a PDF and an
/// EPUB read at the same nominal size (RR2-FR5 font-size control).
const BASE_FONT_PX: f32 = 38.0;

/// Clamp for the user text scale (font size). `1.0` = [`BASE_FONT_PX`].
const MIN_SCALE: f32 = 0.6;
const MAX_SCALE: f32 = 2.5;

/// A pagination of all units for one (viewport, typography) — recomputed on a metrics change.
struct Laid {
    opts: LayoutOpts,
    /// All viewport pages across all units, concatenated (a single global page index).
    pages: Vec<Page>,
    /// `unit_start[i]` = the global viewport page index where unit `i` begins.
    unit_start: Vec<usize>,
}

/// A reflowed view over reading-order [`Block`] units, with a cached pagination.
pub(crate) struct ReflowView {
    /// Reading-order units (PDF source pages) as content blocks.
    units: Vec<Vec<Block>>,
    /// The reading face (embedded default — shared with the EPUB path).
    font: AbFont,
    /// User text scale (font size); `1.0` = [`BASE_FONT_PX`].
    scale: Cell<f32>,
    /// Line-spacing multiple (RR4 — default 1.4).
    line_spacing: Cell<f32>,
    /// Text alignment (RR4 — default Left).
    align: Cell<Align>,
    /// The current pagination; recomputed when the viewport or typography changes.
    laid: RefCell<Laid>,
}

impl ReflowView {
    /// Build a view over `units`, paginated for the initial `(w, h)` viewport at the default
    /// typography. `w`/`h` may be a best-effort guess (the last render size); the first render
    /// repaginates to the true buffer via [`Self::render`].
    pub(crate) fn new(units: Vec<Vec<Block>>, w: u32, h: u32) -> Self {
        let font = AbFont::default_font();
        let laid = layout_all(
            &units,
            &font,
            w.max(1),
            h.max(1),
            BASE_FONT_PX,
            1.4,
            Align::Left,
        );
        Self {
            units,
            font,
            scale: Cell::new(1.0),
            line_spacing: Cell::new(1.4),
            align: Cell::new(Align::Left),
            laid: RefCell::new(laid),
        }
    }

    /// Total reflowed (viewport) page count for the current layout.
    pub(crate) fn page_count(&self) -> usize {
        self.laid.borrow().pages.len()
    }

    /// The global viewport page where reading-order `unit` begins (clamped to a valid page).
    pub(crate) fn unit_start_page(&self, unit: usize) -> usize {
        let laid = self.laid.borrow();
        laid.unit_start.get(unit).copied().unwrap_or(0)
    }

    /// The unit (PDF source page) that global viewport `page` falls in.
    pub(crate) fn unit_of(&self, page: usize) -> usize {
        let laid = self.laid.borrow();
        laid.unit_start
            .iter()
            .rposition(|&start| start <= page)
            .unwrap_or(0)
    }

    /// The effective body font size for the current user scale.
    fn font_px(&self) -> f32 {
        BASE_FONT_PX * self.scale.get()
    }

    /// Repaginate if the requested buffer dimensions or the effective font size differ from the
    /// cached layout (mirrors the EPUB backend's lazy repagination).
    fn ensure_laid(&self, w: u32, h: u32) {
        let font_px = self.font_px();
        let needs = {
            let laid = self.laid.borrow();
            laid.opts.page_w as u32 != w
                || laid.opts.page_h as u32 != h
                || (laid.opts.font_px - font_px).abs() > 0.01
        };
        if needs {
            let fresh = layout_all(
                &self.units,
                &self.font,
                w,
                h,
                font_px,
                self.line_spacing.get(),
                self.align.get(),
            );
            *self.laid.borrow_mut() = fresh;
        }
    }

    /// Repaginate at the current viewport/typography, anchoring to the unit `current_page` is in,
    /// and return that unit's new start page so the reader stays put across the reflow.
    fn repaginate_keeping_unit(&self, current_page: usize) -> usize {
        let unit = self.unit_of(current_page);
        let (w, h) = {
            let laid = self.laid.borrow();
            (laid.opts.page_w as u32, laid.opts.page_h as u32)
        };
        let fresh = layout_all(
            &self.units,
            &self.font,
            w,
            h,
            self.font_px(),
            self.line_spacing.get(),
            self.align.get(),
        );
        let target = fresh.unit_start.get(unit).copied().unwrap_or(0);
        *self.laid.borrow_mut() = fresh;
        target
    }

    /// Render viewport page `index` into `buf` (white background, 8-bit gray expanded to RGBA) —
    /// the same rasterization the EPUB backend uses. Repaginates lazily to the buffer first.
    pub(crate) fn render(&self, index: usize, buf: &mut PixelBuffer<'_>) -> CoreResult<()> {
        self.ensure_laid(buf.width(), buf.height());
        let laid = self.laid.borrow();
        let page = laid.pages.get(index).ok_or(CoreError::PageOutOfRange {
            requested: index,
            available: laid.pages.len(),
        })?;
        buf.fill_white();
        let mut canvas = GrayCanvas::new(buf.width(), buf.height());
        raster_page(page, &laid.opts, &self.font, &mut canvas);
        let dst = buf.bytes_mut();
        for (i, &g) in canvas.pixels.iter().enumerate() {
            let o = i * 4;
            dst[o] = g;
            dst[o + 1] = g;
            dst[o + 2] = g;
            dst[o + 3] = 0xFF;
        }
        Ok(())
    }

    /// The viewport page's glyphs as normalized [`CharBox`]es — the input to the shared selection +
    /// search logic (mirrors the EPUB backend's `page_chars`). Out-of-range → empty (RR21-FR3).
    pub(crate) fn page_chars(&self, index: usize) -> Vec<CharBox> {
        let laid = self.laid.borrow();
        let Some(page) = laid.pages.get(index) else {
            return Vec::new();
        };
        let pw = laid.opts.page_w.max(1.0);
        let ph = laid.opts.page_h.max(1.0);
        page_glyphs(page, &laid.opts, &self.font)
            .into_iter()
            .map(|g| CharBox {
                ch: g.ch,
                rect: NormRect {
                    x0: (g.x0 / pw).clamp(0.0, 1.0),
                    y0: (g.y0 / ph).clamp(0.0, 1.0),
                    x1: (g.x1 / pw).clamp(0.0, 1.0),
                    y1: (g.y1 / ph).clamp(0.0, 1.0),
                },
            })
            .collect()
    }

    /// Set the text scale (font size) and repaginate, returning the preserved-position page.
    pub(crate) fn set_scale(&self, scale: f32, current_page: usize) -> usize {
        let scale = if scale.is_finite() {
            scale.clamp(MIN_SCALE, MAX_SCALE)
        } else {
            1.0
        };
        self.scale.set(scale);
        self.repaginate_keeping_unit(current_page)
    }

    /// Set the line-spacing multiplier and repaginate, returning the preserved-position page.
    pub(crate) fn set_line_spacing(&self, mult: f32, current_page: usize) -> usize {
        let mult = if mult.is_finite() {
            mult.clamp(1.0, 2.5)
        } else {
            1.4
        };
        self.line_spacing.set(mult);
        self.repaginate_keeping_unit(current_page)
    }

    /// Set the alignment and repaginate, returning the preserved-position page.
    pub(crate) fn set_alignment(&self, align_code: i32, current_page: usize) -> usize {
        self.align.set(Align::from_code(align_code));
        self.repaginate_keeping_unit(current_page)
    }
}

/// Paginate every unit for `(w, h, font_px, line_spacing, align)`; each unit starts a page. Mirrors
/// the EPUB backend's `layout_all` (chapters → here, units).
fn layout_all(
    units: &[Vec<Block>],
    font: &AbFont,
    w: u32,
    h: u32,
    font_px: f32,
    line_spacing: f32,
    align: Align,
) -> Laid {
    let mut opts = LayoutOpts::new(w as f32, h as f32, font_px);
    opts.line_spacing = line_spacing;
    opts.align = align;
    let mut pages = Vec::new();
    let mut unit_start = Vec::with_capacity(units.len());
    for blocks in units {
        unit_start.push(pages.len());
        let mut ps = paginate(blocks, &opts, font);
        if ps.is_empty() {
            ps.push(Page::default()); // keep a 1:1 unit→start mapping even for an empty page
        }
        pages.append(&mut ps);
    }
    if pages.is_empty() {
        pages.push(Page::default());
    }
    Laid {
        opts,
        pages,
        unit_start,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use inkread_epub::{Inline, TextRun};

    // A heading flowed through the whole reflow pipeline (layout → render → page_chars) keeps its
    // words intact across font sizes — guards the device "regressio n" / "valu e" regression at the
    // integration level (the unit-level cause is covered in inkread-pdftext).
    #[test]
    fn heading_word_not_split_across_scales() {
        let unit = vec![Block::Heading {
            level: 2,
            content: vec![Inline::Run(TextRun {
                text: "Minimizing loss with logistic regression".to_string(),
                bold: true,
                italic: false,
                href: None,
            })],
        }];
        let view = ReflowView::new(vec![unit], 1404, 1872);
        for scale in [1.0_f32, 1.5, 2.0] {
            view.set_scale(scale, 0);
            let mut bytes = vec![0u8; 1404 * 1872 * 4];
            {
                let mut buf = PixelBuffer::from_rgba(&mut bytes, 1404, 1872).unwrap();
                view.render(0, &mut buf).unwrap();
            }
            let text: String = view.page_chars(0).iter().map(|c| c.ch).collect();
            // The word must never be broken mid-word (no "regressio n").
            assert!(
                !text.contains("regressio n"),
                "scale {scale} split the heading word: {text:?}"
            );
        }
    }
}
