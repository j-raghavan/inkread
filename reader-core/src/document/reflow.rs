//! Reflowable-format backend (EPUB) behind the [`Document`] trait (RR2-FR5, RR2-AC2).
//!
//! Adapts [`inkread_epub`] (parse → content model → layout → raster) to the core's [`Document`]
//! seam. Unlike the fixed PDF backend, a reflowable document's **page count depends on the
//! viewport + font size**: pages are (re)computed when the render buffer's dimensions change, held
//! behind a [`RefCell`] so the trait's `&self` render path can repaginate lazily. Each spine chapter
//! starts a new page (book convention), which also anchors TOC targets to page indices.
//!
//! Supports open → paginate → render → navigate → TOC → **font-size** ([`Document::set_text_scale`]
//! repaginates, preserving the chapter). `word_at`/`text_in_rect` (dictionary + selection on reflow
//! text) remain follow-ups.

use std::cell::{Cell, RefCell};

use inkread_epub::layout::{paginate, Align, LayoutOpts, Page};
use inkread_epub::render::{render_page as raster_page, AbFont, GrayCanvas};
use inkread_epub::{parse_blocks, Block, EpubPackage, NavPoint};

use crate::document::text_select::{self, CharBox, NormRect, TextAnchor, TextSelection};
use crate::document::{Document, DocumentMetadata, SearchMatch, TocEntry};
use crate::error::{CoreError, CoreResult};
use crate::position::PinPosition;
use crate::render::PixelBuffer;

/// Base body font size in device pixels at scale `1.0` (Supernote-class panel). The user's text
/// scale multiplies this (RR2-FR5 font-size control).
const BASE_FONT_PX: f32 = 38.0;

/// Clamp for the user text scale (font size). `1.0` = [`BASE_FONT_PX`].
const MIN_SCALE: f32 = 0.6;
const MAX_SCALE: f32 = 2.5;

/// A pagination of the whole book for one (viewport, font-size) — recomputed on a metrics change.
struct Laid {
    opts: LayoutOpts,
    /// All pages across all chapters, concatenated (a single global page index).
    pages: Vec<Page>,
    /// `chapter_start[i]` = the global page index where chapter `i` begins (TOC resolution).
    chapter_start: Vec<usize>,
}

/// The EPUB backend: parsed per-chapter content + the embedded reading face, with a cached layout.
pub struct EpubBackend {
    /// Reading-order chapters as content blocks.
    chapters: Vec<Vec<Block>>,
    /// Each chapter's resource basename (for matching TOC hrefs → chapter index).
    chapter_keys: Vec<String>,
    /// The table of contents from the package (resolved to page targets in [`Self::toc`]).
    nav: Vec<NavPoint>,
    /// Title/author.
    meta: DocumentMetadata,
    /// The reading face (embedded default).
    font: AbFont,
    /// User text scale (font size); `1.0` = [`BASE_FONT_PX`]. Drives repagination.
    scale: Cell<f32>,
    /// Line-spacing multiple (RR4 — default 1.4). Drives repagination.
    line_spacing: Cell<f32>,
    /// Text alignment (RR4 — default Left). Drives repagination.
    align: Cell<Align>,
    /// The current pagination; recomputed when the viewport or scale changes.
    laid: RefCell<Laid>,
}

impl EpubBackend {
    /// Parse `bytes` and paginate for the initial `viewport`. Maps parse failures to a typed error.
    pub fn open(bytes: Vec<u8>, viewport: crate::render::Viewport) -> CoreResult<Self> {
        let pkg = EpubPackage::open(bytes)
            .map_err(|e| CoreError::RenderBackend(format!("epub open: {e}")))?;
        let chapters: Vec<Vec<Block>> =
            pkg.chapters.iter().map(|c| parse_blocks(&c.html)).collect();
        let chapter_keys: Vec<String> = pkg.chapters.iter().map(|c| basename(&c.href)).collect();
        let meta = DocumentMetadata {
            title: pkg.title.clone(),
            author: pkg.author.clone(),
        };
        let font = AbFont::default_font();
        let laid = layout_all(
            &chapters,
            &font,
            viewport.width,
            viewport.height,
            BASE_FONT_PX,
            1.4,
            Align::Left,
        );
        Ok(Self {
            chapters,
            chapter_keys,
            nav: pkg.toc,
            meta,
            font,
            scale: Cell::new(1.0),
            line_spacing: Cell::new(1.4),
            align: Cell::new(Align::Left),
            laid: RefCell::new(laid),
        })
    }

    /// The effective body font size for the current user scale.
    fn font_px(&self) -> f32 {
        BASE_FONT_PX * self.scale.get()
    }

    /// Repaginate if the requested buffer dimensions or the effective font size differ from the
    /// cached layout.
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
                &self.chapters,
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

    /// Repaginate at the current viewport/scale, anchoring the reading position to the chapter
    /// `current_page` is in, and return that chapter's new start page (RR4 line-spacing/alignment).
    fn repaginate_keeping_chapter(&self, current_page: usize) -> Option<usize> {
        let chapter = self.chapter_of(current_page);
        let (w, h) = {
            let laid = self.laid.borrow();
            (laid.opts.page_w as u32, laid.opts.page_h as u32)
        };
        let fresh = layout_all(
            &self.chapters,
            &self.font,
            w,
            h,
            self.font_px(),
            self.line_spacing.get(),
            self.align.get(),
        );
        let target = fresh.chapter_start.get(chapter).copied().unwrap_or(0);
        *self.laid.borrow_mut() = fresh;
        Some(target)
    }

    /// The page's glyphs as normalized [`CharBox`]es — the input to the pure selection + search
    /// logic (RR11 / RR2). Mirrors the PDF backend's `page_chars`: the layout's positioned glyphs
    /// (pixel space) normalized to `[0,1]`. An out-of-range page contributes nothing (RR21-FR3).
    fn page_chars(&self, index: usize) -> Vec<CharBox> {
        let laid = self.laid.borrow();
        let Some(page) = laid.pages.get(index) else {
            return Vec::new();
        };
        // Shared with the PDF-reflow backend so the glyph→CharBox + anchor mapping lives once.
        crate::document::reflow_view::page_charboxes(page, &laid.opts, &self.font)
    }

    /// Frame a chapter-relative [`TextAnchor`] into a full [`PinPosition`] (RR6) for `chapter`. The
    /// backend owns the chapter identity; the offset is carried in `text_offset` so `position_int()`
    /// orders within the chapter, and `xpath = [block]` re-anchors the source block (ADR-0012 D2).
    //
    // `pin_at`/`page_pin`/`pin_to_page`/`selection_pins` are the PinPosition composition foundation
    // (ADR-0012 Phase 1, step 2), now exposed through the `Document` trait for the RR12
    // reading-position resume + Digest anchor wiring (#46).
    fn pin_at(&self, chapter: usize, anchor: TextAnchor) -> PinPosition {
        PinPosition {
            chapter_index: chapter as i32,
            chapter_id: self.chapter_keys.get(chapter).cloned().unwrap_or_default(),
            chapter_start: 0,
            chapter_end: i32::MAX,
            node_position: 0,
            text_offset: anchor.char_offset as i32,
            xpath: vec![anchor.block as i32],
        }
    }

    /// The [`PinPosition`] a global `page` starts at — its first anchored glyph (RR8/RR12 reading
    /// position). `None` for an empty page (no glyphs to anchor).
    pub(crate) fn page_pin(&self, page: usize) -> Option<PinPosition> {
        let chapter = self.chapter_of(page);
        let anchor = self.page_chars(page).into_iter().find_map(|c| c.anchor)?;
        Some(self.pin_at(chapter, anchor))
    }

    /// Resolve a [`PinPosition`] back to the global page that contains it after a re-layout — the
    /// re-anchoring that makes a highlight/Digest survive a font-size change (RR12-FR4). Picks, within
    /// the pin's chapter, the last page whose first anchored glyph is at or before the pin's offset.
    pub(crate) fn pin_to_page(&self, pin: &PinPosition) -> usize {
        let laid = self.laid.borrow();
        // Clamp a foreign/corrupt chapter index into range rather than scanning the whole book.
        let chapter =
            (pin.chapter_index.max(0) as usize).min(laid.chapter_start.len().saturating_sub(1));
        let start = laid.chapter_start.get(chapter).copied().unwrap_or(0);
        let end = laid
            .chapter_start
            .get(chapter + 1)
            .copied()
            .unwrap_or(laid.pages.len());
        drop(laid);
        let target = pin.text_offset.max(0);
        let mut best = start;
        for page in start..end {
            match self.page_chars(page).into_iter().find_map(|c| c.anchor) {
                Some(a) if (a.char_offset as i32) <= target => best = page,
                // A real anchor past the target → reading order says stop. An anchorless interior
                // page (a rule-only or empty page) must NOT stop the scan: keep looking.
                Some(_) => break,
                None => continue,
            }
        }
        best
    }

    /// The `[start, end]` [`PinPosition`] pair a selection rectangle covers on `page` — the anchor a
    /// highlight / note / Digest range stores (RR11-FR4 / RR12). `None` when nothing is selected.
    pub(crate) fn selection_pins(
        &self,
        page: usize,
        rect: NormRect,
    ) -> Option<(PinPosition, PinPosition)> {
        let chapter = self.chapter_of(page);
        let (start, end) = text_select::anchored_span(&self.page_chars(page), rect)?;
        Some((self.pin_at(chapter, start), self.pin_at(chapter, end)))
    }

    /// The chapter index that global `page` falls in (the last chapter whose start ≤ page).
    fn chapter_of(&self, page: usize) -> usize {
        let laid = self.laid.borrow();
        laid.chapter_start
            .iter()
            .rposition(|&start| start <= page)
            .unwrap_or(0)
    }
}

impl Document for EpubBackend {
    fn page_count(&self) -> usize {
        self.laid.borrow().pages.len()
    }

    fn metadata(&self) -> DocumentMetadata {
        self.meta.clone()
    }

    fn render_page(&self, index: usize, buf: &mut PixelBuffer<'_>) -> CoreResult<()> {
        self.ensure_laid(buf.width(), buf.height());
        let laid = self.laid.borrow();
        let page = laid.pages.get(index).ok_or(CoreError::PageOutOfRange {
            requested: index,
            available: laid.pages.len(),
        })?;
        buf.fill_white();
        let mut canvas = GrayCanvas::new(buf.width(), buf.height());
        raster_page(page, &laid.opts, &self.font, &mut canvas);
        // Expand 8-bit grayscale → opaque RGBA (CHANNEL_ORDER r,g,b,a). One byte → three equal.
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

    fn toc(&self) -> Vec<TocEntry> {
        let laid = self.laid.borrow();
        self.nav
            .iter()
            .map(|n| resolve_nav(n, &self.chapter_keys, &laid.chapter_start))
            .collect()
    }

    fn word_at(&self, page: usize, x: f32, y: f32) -> Option<TextSelection> {
        text_select::word_at(&self.page_chars(page), x, y)
    }

    fn text_in_rect(&self, page: usize, rect: NormRect) -> TextSelection {
        text_select::text_in_rect(&self.page_chars(page), rect)
    }

    fn search_page(&self, page: usize, query: &str) -> Vec<SearchMatch> {
        text_select::find_matches(&self.page_chars(page), query)
    }

    fn set_text_scale(&self, scale: f32, current_page: usize) -> Option<usize> {
        let scale = if scale.is_finite() {
            scale.clamp(MIN_SCALE, MAX_SCALE)
        } else {
            1.0
        };
        // Anchor the reading position to the current chapter, repaginate at the new size, then
        // return that chapter's new start page so the reader stays put across the reflow.
        self.scale.set(scale);
        self.repaginate_keeping_chapter(current_page)
    }

    fn set_line_spacing(&self, mult: f32, current_page: usize) -> Option<usize> {
        let mult = if mult.is_finite() {
            mult.clamp(1.0, 2.5)
        } else {
            1.4
        };
        self.line_spacing.set(mult);
        self.repaginate_keeping_chapter(current_page)
    }

    fn set_alignment(&self, align_code: i32, current_page: usize) -> Option<usize> {
        self.align.set(Align::from_code(align_code));
        self.repaginate_keeping_chapter(current_page)
    }

    // Reflow-stable anchors (RR8/RR12, ADR-0012): expose the inherent pin machinery through the
    // trait so the session can persist a resume/Digest locator that survives a re-layout. Fixed
    // layout keeps the trait defaults (`None`); fully-qualified calls hit the inherent impls above.
    fn page_pin(&self, page: usize) -> Option<PinPosition> {
        EpubBackend::page_pin(self, page)
    }

    fn pin_to_page(&self, pin: &PinPosition) -> Option<usize> {
        Some(EpubBackend::pin_to_page(self, pin))
    }

    fn selection_pins(&self, page: usize, rect: NormRect) -> Option<(PinPosition, PinPosition)> {
        EpubBackend::selection_pins(self, page, rect)
    }
}

/// Paginate every chapter for `(w, h, font_px, line_spacing, align)`; each chapter starts a page.
fn layout_all(
    chapters: &[Vec<Block>],
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
    let mut chapter_start = Vec::with_capacity(chapters.len());
    for blocks in chapters {
        chapter_start.push(pages.len());
        let mut cps = paginate(blocks, &opts, font);
        if cps.is_empty() {
            cps.push(Page::default()); // keep a 1:1 chapter→start mapping even for an empty chapter
        }
        pages.append(&mut cps);
    }
    if pages.is_empty() {
        pages.push(Page::default());
    }
    Laid {
        opts,
        pages,
        chapter_start,
    }
}

/// Resolve a [`NavPoint`] into a [`TocEntry`] with a page target (matched by resource basename).
fn resolve_nav(nav: &NavPoint, chapter_keys: &[String], chapter_start: &[usize]) -> TocEntry {
    let target_page = nav.href.as_ref().and_then(|h| {
        let key = basename(h);
        chapter_keys
            .iter()
            .position(|k| *k == key)
            .map(|ci| chapter_start[ci])
    });
    TocEntry {
        title: nav.label.clone(),
        target_page,
        children: nav
            .children
            .iter()
            .map(|c| resolve_nav(c, chapter_keys, chapter_start))
            .collect(),
    }
}

/// The filename portion of an href, sans any `#fragment` — the stable key for matching a TOC entry
/// to a spine chapter regardless of directory prefixes.
fn basename(href: &str) -> String {
    href.split('#')
        .next()
        .unwrap_or(href)
        .rsplit('/')
        .next()
        .unwrap_or(href)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::Viewport;

    const SAMPLE: &[u8] = include_bytes!("../../tests/fixtures/sample.epub");

    fn vp(w: u32, h: u32) -> Viewport {
        Viewport {
            width: w,
            height: h,
            dpi: 226,
        }
    }

    fn render(backend: &EpubBackend, index: usize, w: u32, h: u32) -> Vec<u8> {
        let mut bytes = vec![0u8; (w * h * 4) as usize];
        let mut buf = PixelBuffer::from_rgba(&mut bytes, w, h).unwrap();
        backend.render_page(index, &mut buf).unwrap();
        bytes
    }

    #[test]
    fn opens_paginates_and_exposes_metadata() {
        let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
        assert_eq!(b.metadata().title.as_deref(), Some("Reflow Sample"));
        // Two chapters, each ≥ 1 page.
        assert!(b.page_count() >= 2, "pages = {}", b.page_count());
    }

    #[test]
    fn renders_ink_and_respects_page_range() {
        let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
        let px = render(&b, 0, 400, 600);
        let inked = px.chunks_exact(4).filter(|p| p[0] < 250).count();
        assert!(inked > 50, "first page has rendered text: {inked}");

        let mut bytes = vec![0u8; 400 * 600 * 4];
        let mut buf = PixelBuffer::from_rgba(&mut bytes, 400, 600).unwrap();
        assert!(matches!(
            b.render_page(9999, &mut buf),
            Err(CoreError::PageOutOfRange { .. })
        ));
    }

    #[test]
    fn toc_resolves_to_page_targets() {
        let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
        let toc = b.toc();
        assert_eq!(toc.len(), 2, "two nav points");
        assert_eq!(toc[0].title, "Chapter One");
        assert_eq!(toc[0].target_page, Some(0), "ch1 starts at page 0");
        assert!(
            toc[1].target_page.unwrap() >= 1,
            "ch2 starts on a later page: {:?}",
            toc[1].target_page
        );
    }

    #[test]
    fn larger_text_scale_repaginates_and_keeps_the_chapter() {
        let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
        let _ = render(&b, 0, 400, 600); // settle the layout at this viewport
        let base_pages = b.page_count();
        // Reader is in chapter 2; bumping the size should land us back at chapter 2's new start.
        let ch2_start = b.toc()[1].target_page.unwrap();
        let new_page = b.set_text_scale(1.8, ch2_start).unwrap();
        assert!(
            b.page_count() >= base_pages,
            "bigger text ⇒ at least as many pages"
        );
        // The returned page is chapter 2's start under the new pagination.
        assert_eq!(new_page, b.toc()[1].target_page.unwrap());
        // PDF-style fixed layout would return None; EPUB returns Some.
        assert!(b.set_text_scale(1.0, 0).is_some());
    }

    /// The source character a pin currently resolves to: the glyph on `pin_to_page(pin)` whose anchor
    /// matches the pin's block + offset.
    fn char_at(b: &EpubBackend, pin: &PinPosition) -> Option<char> {
        let page = b.pin_to_page(pin);
        b.page_chars(page).into_iter().find_map(|c| {
            c.anchor.and_then(|a| {
                (a.block as i32 == pin.xpath[0] && a.char_offset as i32 == pin.text_offset)
                    .then_some(c.ch)
            })
        })
    }

    #[test]
    fn page_pin_anchors_to_the_first_glyph_and_first_page_is_chapter_start() {
        let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
        let _ = render(&b, 0, 400, 600);
        let pin = b.page_pin(0).expect("page 0 has text");
        assert_eq!(pin.chapter_index, 0, "page 0 is chapter 0");
        // The pin resolves to the first character actually painted on page 0.
        let first_ch = b.page_chars(0).into_iter().find(|c| c.anchor.is_some());
        assert_eq!(char_at(&b, &pin), first_ch.map(|c| c.ch));
    }

    #[test]
    fn pin_re_anchors_to_the_same_character_across_a_font_size_change() {
        // The headline guarantee (golden SPEC-INKREAD.md RR8-AC1 / RR12-FR4): a pin minted at one
        // size re-resolves to the *same source character* after the page reflows at a new size.
        let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
        let _ = render(&b, 0, 400, 600);
        // Mint a pin partway through chapter 2 so re-pagination genuinely moves it.
        let ch2 = b.toc()[1].target_page.unwrap();
        let pin = b.page_pin(ch2).or_else(|| b.page_pin(0)).expect("a pin");
        let before = char_at(&b, &pin).expect("char before reflow");

        let moved_page = b.set_text_scale(1.9, ch2).unwrap();
        let after = char_at(&b, &pin).expect("char after reflow");

        assert_eq!(
            before, after,
            "pin re-resolves to the same source character after reflow"
        );
        // Sanity: the pin still lands inside its own chapter under the new pagination.
        assert_eq!(
            b.chapter_of(b.pin_to_page(&pin)),
            pin.chapter_index.max(0) as usize
        );
        let _ = moved_page;
    }

    #[test]
    fn selection_pins_span_in_order_and_survive_a_font_size_change() {
        let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
        let _ = render(&b, 0, 400, 600);
        // A band over the top of page 0 selects the opening lines.
        let band = NormRect {
            x0: 0.0,
            y0: 0.0,
            x1: 1.0,
            y1: 0.5,
        };
        let (start, end) = b.selection_pins(0, band).expect("a selection on page 0");
        assert!(start <= end, "start pin precedes end pin");
        let (sc, ec) = (
            char_at(&b, &start).expect("start char"),
            char_at(&b, &end).expect("end char"),
        );

        // Reflow at a larger size: the same span endpoints re-resolve to the same characters.
        b.set_text_scale(1.7, 0).unwrap();
        assert_eq!(char_at(&b, &start), Some(sc), "start re-anchors");
        assert_eq!(char_at(&b, &end), Some(ec), "end re-anchors");
    }

    #[test]
    fn page_chars_recovers_words_for_selection() {
        let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
        let _ = render(&b, 0, 400, 600); // settle layout at this viewport
        let chars = b.page_chars(0);
        assert!(!chars.is_empty(), "first page exposes glyphs");
        let text: String = chars.iter().map(|c| c.ch).collect();
        assert!(
            text.contains(' '),
            "inter-word spaces are synthesized: {text:?}"
        );
        // Every box is on-page and non-degenerate horizontally for non-space glyphs.
        assert!(chars.iter().all(|c| c.rect.x0 >= 0.0 && c.rect.x1 <= 1.0));
    }

    #[test]
    fn search_finds_text_on_the_page_it_lives_on() {
        let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
        let _ = render(&b, 0, 400, 600);
        // Pull a real word off page 0 and search for it.
        let text: String = b.page_chars(0).iter().map(|c| c.ch).collect();
        let word = text
            .split_whitespace()
            .find(|w| w.chars().all(|c| c.is_alphabetic()) && w.len() >= 4)
            .expect("a searchable word on page 0")
            .to_string();
        let hits = b.search_page(0, &word);
        assert!(!hits.is_empty(), "found {word:?} on page 0");
        assert!(hits[0].boxes.iter().all(|bx| bx.x1 >= bx.x0));
        // The same query in a wildly different case still matches (case-insensitive).
        assert!(!b.search_page(0, &word.to_uppercase()).is_empty());
    }

    #[test]
    fn smaller_viewport_repaginates_to_more_pages() {
        let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
        let wide = b.page_count();
        // Render into a much shorter buffer → fewer lines/page → more pages (lazy repagination).
        let _ = render(&b, 0, 400, 200);
        let tall = b.page_count();
        assert!(
            tall > wide,
            "narrower/shorter viewport paginates longer: {wide} → {tall}"
        );
    }
}
