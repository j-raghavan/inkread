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

use std::cell::{Cell, Ref, RefCell};

use inkread_epub::layout::{paginate_with, Align, Hyphenator, LayoutOpts, Page};
use inkread_epub::measure::{CachedHyphenator, CachedMetrics};
use inkread_epub::render::{render_page as raster_page, AbFont, EnHyphenator, GrayCanvas};
use inkread_epub::{parse_blocks, Block, EpubPackage, NavPoint};

use crate::document::text_select::{self, CharBox, NormRect, TextAnchor, TextSelection};
use crate::document::{Document, DocumentMetadata, SearchMatch, TocEntry};
use crate::error::{CoreError, CoreResult};
use crate::position::PinPosition;
use crate::render::PixelBuffer;

/// Base body font size in device pixels at scale `1.0` (Supernote-class panel). The user's text
/// scale multiplies this (RR2-FR5 font-size control).
const BASE_FONT_PX: f32 = 56.0;

/// Clamp for the user text scale (font size). `1.0` = [`BASE_FONT_PX`].
const MIN_SCALE: f32 = 0.6;
const MAX_SCALE: f32 = 3.0;

/// Default line-spacing multiple (RR4). Mirrors the shell's `DisplayPrefs.DEFAULT_LINE_SPACING`.
const DEFAULT_LINE_SPACING: f32 = 1.4;

/// A pagination of the whole book for one set of layout parameters — rebuilt only when those
/// parameters actually change (see [`EpubBackend::laid`]).
struct Laid {
    opts: LayoutOpts,
    /// The reading face this pagination was measured with. Not part of [`LayoutOpts`], but a
    /// different face means different metrics, so it belongs in the staleness key.
    font_id: usize,
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
    /// The reading face; `RefCell` so the `&self` font-family setter can swap it (RR4 / font select).
    font: RefCell<AbFont>,
    /// Soft-hyphenation for justified/narrow lines (book typography, like KOReader).
    hyph: EnHyphenator,
    /// The bundled-face index behind [`Self::font`], kept alongside it as the layout cache key.
    font_id: Cell<usize>,
    /// User text scale (font size); `1.0` = [`BASE_FONT_PX`]. Drives repagination.
    scale: Cell<f32>,
    /// Line-spacing multiple (RR4 — default [`DEFAULT_LINE_SPACING`]). Drives repagination.
    line_spacing: Cell<f32>,
    /// Text alignment (RR4 — default Left, matching every other reflow default). Drives repagination.
    align: Cell<Align>,
    /// The page size to lay out for; updated by the render path when the buffer changes.
    viewport: Cell<(u32, u32)>,
    /// The current pagination, or `None` before the first one is needed. Laying out lazily lets the
    /// open path apply the reader's saved typography *before* any pagination is built, so a cold
    /// open costs a single layout pass instead of one per setting (#161/#162).
    laid: RefCell<Option<Laid>>,
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
        Ok(Self {
            chapters,
            chapter_keys,
            nav: pkg.toc,
            meta,
            font: RefCell::new(AbFont::default_font()),
            hyph: EnHyphenator::new(),
            font_id: Cell::new(0),
            scale: Cell::new(1.0),
            line_spacing: Cell::new(DEFAULT_LINE_SPACING),
            align: Cell::new(Align::default()),
            viewport: Cell::new((viewport.width, viewport.height)),
            // Deferred: the first read paginates, so the saved typography applied right after open
            // is folded into that single pass rather than triggering one pass per setting.
            laid: RefCell::new(None),
        })
    }

    /// The effective body font size for the current user scale.
    fn font_px(&self) -> f32 {
        BASE_FONT_PX * self.scale.get()
    }

    /// The layout parameters the current settings + viewport ask for. Comparing this against the
    /// cached [`Laid::opts`] is what makes a redundant repagination free.
    fn requested_opts(&self) -> LayoutOpts {
        let (w, h) = self.viewport.get();
        let mut opts = LayoutOpts::new(w as f32, h as f32, self.font_px());
        opts.line_spacing = self.line_spacing.get();
        opts.align = self.align.get();
        opts
    }

    /// The current pagination, built on first use and rebuilt **only** when the requested layout
    /// parameters or the reading face actually differ from the cached ones.
    fn laid(&self) -> Ref<'_, Laid> {
        let stale = match self.laid.borrow().as_ref() {
            None => true,
            Some(laid) => laid.opts != self.requested_opts() || laid.font_id != self.font_id.get(),
        };
        if stale {
            let fresh = layout_all(
                &self.chapters,
                &self.font.borrow(),
                &self.hyph,
                self.requested_opts(),
                self.font_id.get(),
            );
            *self.laid.borrow_mut() = Some(fresh);
        }
        // Materialized directly above and never taken back out, so the borrow is always `Some`.
        Ref::map(self.laid.borrow(), |slot| {
            slot.as_ref().expect("pagination materialized above")
        })
    }

    /// Repaginate at the current viewport/scale, anchoring the reading position to the chapter
    /// `current_page` is in, and return that chapter's new start page (RR4 line-spacing/alignment).
    fn repaginate_keeping_chapter(&self, current_page: usize) -> Option<usize> {
        // Resolve the chapter against the pagination *as it stands* — the caller has already
        // applied the new setting, so going through `laid()` here would rebuild first and then
        // map `current_page` through the new pagination, landing in the wrong chapter.
        let chapter = match self.laid.borrow().as_ref() {
            // Nothing paginated yet (the open path applying saved typography): there is no reading
            // position to preserve, and returning without laying out keeps a cold open to one pass.
            None => return Some(0),
            Some(laid) => laid
                .chapter_start
                .iter()
                .rposition(|&start| start <= current_page)
                .unwrap_or(0),
        };
        // Rebuilds only if something really changed — re-picking the value already in effect in
        // the Adjust sheet costs nothing.
        Some(self.laid().chapter_start.get(chapter).copied().unwrap_or(0))
    }

    /// The page's glyphs as normalized [`CharBox`]es — the input to the pure selection + search
    /// logic (RR11 / RR2). Mirrors the PDF backend's `page_chars`: the layout's positioned glyphs
    /// (pixel space) normalized to `[0,1]`. An out-of-range page contributes nothing (RR21-FR3).
    fn page_chars(&self, index: usize) -> Vec<CharBox> {
        let laid = self.laid();
        let Some(page) = laid.pages.get(index) else {
            return Vec::new();
        };
        // Shared with the PDF-reflow backend so the glyph→CharBox + anchor mapping lives once.
        crate::document::reflow_view::page_charboxes(page, &laid.opts, &self.font.borrow())
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
        let laid = self.laid();
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

    /// Swap the reading face to the bundled `font_id`, recording the **normalized** index as the
    /// layout cache key. `AbFont::for_face` maps an out-of-range id onto the default face, so
    /// normalizing here keeps "id 99" and "id 0" from looking like a face change (RR21-FR3).
    fn apply_font(&self, font_id: i32) {
        let id = match usize::try_from(font_id) {
            Ok(id) if id < inkread_epub::reading_font_names().len() => id,
            _ => 0,
        };
        if id != self.font_id.get() {
            *self.font.borrow_mut() = AbFont::for_face(id);
            self.font_id.set(id);
        }
    }

    /// The chapter index that global `page` falls in (the last chapter whose start ≤ page).
    fn chapter_of(&self, page: usize) -> usize {
        let laid = self.laid();
        laid.chapter_start
            .iter()
            .rposition(|&start| start <= page)
            .unwrap_or(0)
    }
}

impl Document for EpubBackend {
    fn page_count(&self) -> usize {
        self.laid().pages.len()
    }

    fn metadata(&self) -> DocumentMetadata {
        self.meta.clone()
    }

    fn render_page(&self, index: usize, buf: &mut PixelBuffer<'_>) -> CoreResult<()> {
        self.viewport.set((buf.width(), buf.height()));
        let laid = self.laid();
        let page = laid.pages.get(index).ok_or(CoreError::PageOutOfRange {
            requested: index,
            available: laid.pages.len(),
        })?;
        buf.fill_white();
        let mut canvas = GrayCanvas::new(buf.width(), buf.height());
        raster_page(page, &laid.opts, &self.font.borrow(), &mut canvas);
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
        let laid = self.laid();
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

    fn text_line_span(&self, page: usize, start: (f32, f32), end: (f32, f32)) -> TextSelection {
        // EPUB needs the reading-order line span too (it was falling through to the empty trait
        // default — a multi-line drag selected nothing on a reflowed page) (#46 device finding).
        text_select::text_line_span(&self.page_chars(page), start, end)
    }

    fn search_page(&self, page: usize, query: &str) -> Vec<SearchMatch> {
        text_select::find_matches(&self.page_chars(page), query)
    }

    fn set_text_scale(&self, scale: f32, current_page: usize) -> Option<usize> {
        // Anchor the reading position to the current chapter, repaginate at the new size, then
        // return that chapter's new start page so the reader stays put across the reflow.
        self.scale.set(clamp_scale(scale));
        self.repaginate_keeping_chapter(current_page)
    }

    fn set_line_spacing(&self, mult: f32, current_page: usize) -> Option<usize> {
        self.line_spacing.set(clamp_line_spacing(mult));
        self.repaginate_keeping_chapter(current_page)
    }

    fn set_alignment(&self, align_code: i32, current_page: usize) -> Option<usize> {
        self.align.set(Align::from_code(align_code));
        self.repaginate_keeping_chapter(current_page)
    }

    fn set_font(&self, font_id: i32, current_page: usize) -> Option<usize> {
        // Swap the reading face, then repaginate (new metrics → new line breaks), keeping the chapter.
        self.apply_font(font_id);
        self.repaginate_keeping_chapter(current_page)
    }

    fn set_typography(
        &self,
        scale: f32,
        font_id: i32,
        line_spacing: f32,
        align_code: i32,
        current_page: usize,
    ) -> Option<usize> {
        self.scale.set(clamp_scale(scale));
        self.apply_font(font_id);
        self.line_spacing.set(clamp_line_spacing(line_spacing));
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

/// Clamp a user text scale into the supported range; a non-finite value falls back to `1.0`
/// (RR21-FR3 — the value crosses JNI, so it is validated here rather than trusted).
fn clamp_scale(scale: f32) -> f32 {
    if scale.is_finite() {
        scale.clamp(MIN_SCALE, MAX_SCALE)
    } else {
        1.0
    }
}

/// Clamp a line-spacing multiple; a non-finite value falls back to [`DEFAULT_LINE_SPACING`].
fn clamp_line_spacing(mult: f32) -> f32 {
    if mult.is_finite() {
        mult.clamp(1.0, 2.5)
    } else {
        DEFAULT_LINE_SPACING
    }
}

// Counts pagination passes on the current thread. Cost here is invisible to correctness tests —
// a redundant pass produces an identical layout — so the guard against one has to be asserted
// directly (#161/#162). Thread-local so parallel tests don't see each other's passes.
#[cfg(test)]
thread_local! {
    static LAYOUT_PASSES: Cell<usize> = const { Cell::new(0) };
}

/// Pagination passes performed on this thread since [`reset_layout_passes`].
#[cfg(test)]
fn layout_passes() -> usize {
    LAYOUT_PASSES.with(Cell::get)
}

#[cfg(test)]
fn reset_layout_passes() {
    LAYOUT_PASSES.with(|c| c.set(0));
}

/// Paginate every chapter for `opts` measured with `font`; each chapter starts a page.
fn layout_all(
    chapters: &[Vec<Block>],
    font: &AbFont,
    hyph: &dyn Hyphenator,
    opts: LayoutOpts,
    font_id: usize,
) -> Laid {
    #[cfg(test)]
    LAYOUT_PASSES.with(|c| c.set(c.get() + 1));
    // Memoize measurement across the *whole book*, not per chapter: prose repeats itself, and the
    // widths a later chapter needs have almost all been computed by an earlier one (#161/#162).
    let font = CachedMetrics::new(font);
    let hyph = CachedHyphenator::new(hyph);
    let mut pages = Vec::new();
    let mut chapter_start = Vec::with_capacity(chapters.len());
    for blocks in chapters {
        chapter_start.push(pages.len());
        let mut cps = paginate_with(blocks, &opts, &font, &hyph);
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
        font_id,
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

    /// Host preview (#66): render a compiled Daily issue EPUB to PNGs so the reading experience can
    /// be eyeballed on a host (the e-ink screencap is black). Run:
    ///   `INKREAD_ISSUE_EPUB=/path/issue.epub cargo test -p reader-core daily_render_dump -- --ignored --nocapture`
    /// → writes `target/daily-preview/page-NN.png`. Driven by `scripts/daily-preview.sh`.
    #[test]
    #[ignore = "host preview: needs INKREAD_ISSUE_EPUB=<path>; run with --ignored --nocapture"]
    fn daily_render_dump() {
        let Ok(path) = std::env::var("INKREAD_ISSUE_EPUB") else {
            eprintln!("SKIP daily_render_dump: set INKREAD_ISSUE_EPUB to a compiled issue .epub");
            return;
        };
        let bytes = std::fs::read(&path).expect("issue epub readable");
        let env_u32 = |k: &str, d: u32| {
            std::env::var(k)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d)
        };
        let (w, h) = (env_u32("INKREAD_W", 790), env_u32("INKREAD_H", 1024)); // device panel for real preview
        let b = EpubBackend::open(bytes, vp(w, h)).expect("open issue epub");
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/daily-preview");
        std::fs::create_dir_all(&out).unwrap();
        let pages = b.page_count().min(10);
        for i in 0..pages {
            let px = render(&b, i, w, h);
            let f = std::io::BufWriter::new(
                std::fs::File::create(out.join(format!("page-{i:02}.png"))).unwrap(),
            );
            let mut enc = png::Encoder::new(f, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header().unwrap().write_image_data(&px).unwrap();
        }
        eprintln!("wrote {pages} page PNGs to {}", out.display());
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

    #[test]
    fn set_font_repaginates_and_lists_faces() {
        let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
        let _ = render(&b, 0, 400, 600);
        let ch2_start = b.toc()[1].target_page.unwrap();
        // Switching the reading face repaginates (EPUB → Some), landing back at chapter 2's start.
        let new_page = b.set_font(2, ch2_start).unwrap();
        assert_eq!(new_page, b.toc()[1].target_page.unwrap());
        // An out-of-range id falls back to the default face, still repaginating.
        assert!(b.set_font(999, 0).is_some());
        // The bundled set is exposed for the picker (Spectral + the KOReader OSS faces).
        let names = inkread_epub::reading_font_names();
        assert!(
            names.len() >= 4 && names[0] == "Spectral",
            "faces: {names:?}"
        );
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
    fn epub_is_never_magnifiable() {
        // EPUB is always reflowed (font size, not zoom), so the shell must never magnify it (#61,
        // RR25-FR3) — the trait default for a reflowable backend.
        let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
        assert!(!b.is_magnifiable());
    }

    #[test]
    fn text_line_span_selects_reflowed_lines() {
        // Regression (#46 device finding): EpubBackend didn't override text_line_span, so a multi-line
        // drag on a reflowed page hit the Document trait's empty default and selected nothing — the
        // bbox path worked, the line-span path returned ''. The override must select real text.
        // A realistic reading viewport (the default body size needs a real column; a tiny one would
        // hyphenate/clip every word).
        let b = EpubBackend::open(SAMPLE.to_vec(), vp(800, 1200)).unwrap();
        let _ = render(&b, 0, 800, 1200);
        let sel = b.text_line_span(0, (0.05, 0.02), (0.60, 0.30)); // a drag down the top of page 0
        assert!(
            !sel.boxes.is_empty(),
            "reflowed line-span produces highlight boxes"
        );
        // Correctness, not just wiring: the span must be REAL page text. Pull a word the page
        // actually has and confirm the top-of-page span contains it (the first line is taken whole,
        // so the page's opening words are in range) — a wrong-glyph override would not contain it.
        let page_text: String = b.page_chars(0).iter().map(|c| c.ch).collect();
        let word = page_text
            .split_whitespace()
            .find(|w| w.chars().all(|c| c.is_alphabetic()) && w.len() >= 4)
            .expect("a real word on page 0");
        assert!(
            sel.text.contains(word),
            "line-span selects actual page text containing {word:?}, got '{}'",
            sel.text
        );
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
        let b = EpubBackend::open(SAMPLE.to_vec(), vp(800, 1200)).unwrap();
        let _ = render(&b, 0, 800, 1200);
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

    // ---- pagination cost (#161/#162) ----------------------------------------------------------
    // These assert *how often* the book is paginated, not what the pagination looks like. A
    // redundant pass is invisible to every other test here — it yields an identical layout — but on
    // a large book each one costs seconds, which is the whole substance of #161/#162.

    #[test]
    fn opening_a_book_does_not_paginate_until_something_reads_the_layout() {
        reset_layout_passes();
        let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
        assert_eq!(layout_passes(), 0, "open alone paginates nothing");
        // Metadata comes from the package, not the layout — the session reads it during open.
        let _ = b.metadata();
        assert_eq!(layout_passes(), 0, "metadata needs no pagination");
        let _ = b.page_count();
        assert_eq!(layout_passes(), 1, "the first read paginates once");
    }

    #[test]
    fn restoring_saved_typography_over_a_cold_open_costs_one_pagination() {
        reset_layout_passes();
        let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
        // Exactly what the shell does on open: restore four persisted settings, then read the count.
        let page = b.set_typography(1.25, 1, 1.7, 2, 0);
        let count = b.page_count();
        assert_eq!(
            layout_passes(),
            1,
            "a cold open restoring saved typography paginates once, not once per setting"
        );
        assert_eq!(page, Some(0), "a cold open resumes at the first page");
        assert!(count > 0);
    }

    #[test]
    fn reapplying_the_settings_already_in_effect_does_not_repaginate() {
        let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
        let before = b.page_count();
        reset_layout_passes();
        // Re-picking the value already selected in the Adjust sheet must cost nothing.
        assert_eq!(b.set_text_scale(1.0, 0), Some(0));
        assert_eq!(b.set_line_spacing(DEFAULT_LINE_SPACING, 0), Some(0));
        assert_eq!(b.set_alignment(0, 0), Some(0));
        assert_eq!(b.set_font(0, 0), Some(0));
        assert_eq!(layout_passes(), 0, "no setting changed → no repagination");
        assert_eq!(b.page_count(), before, "and the pagination is untouched");
        // A real change still repaginates.
        let _ = b.set_text_scale(2.0, 0);
        assert_eq!(layout_passes(), 1, "a changed setting repaginates once");
    }

    #[test]
    fn an_out_of_range_font_id_resolves_to_the_default_face_without_repaginating() {
        let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
        let before = b.page_count();
        reset_layout_passes();
        // `AbFont::for_face` maps both of these onto the default face, so neither is a face change
        // — the normalized id must reflect that rather than forcing a pass (RR21-FR3).
        assert_eq!(b.set_font(9999, 0), Some(0));
        assert_eq!(b.set_font(-3, 0), Some(0));
        assert_eq!(layout_passes(), 0, "out-of-range ids are the default face");
        assert_eq!(b.page_count(), before);
    }

    #[test]
    fn set_typography_lays_out_the_same_book_as_the_four_setters_do() {
        // The batched path is an optimization, so it must be indistinguishable from the individual
        // setters it replaces — same page count, same page content.
        let batched = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
        assert_eq!(batched.set_typography(1.5, 2, 1.2, 3, 0), Some(0));

        let stepwise = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
        let _ = stepwise.set_font(2, 0);
        let _ = stepwise.set_text_scale(1.5, 0);
        let _ = stepwise.set_line_spacing(1.2, 0);
        let _ = stepwise.set_alignment(3, 0);

        assert_eq!(batched.page_count(), stepwise.page_count());
        assert!(batched.page_count() > 0);
        for page in 0..batched.page_count() {
            assert_eq!(
                render(&batched, page, 400, 600),
                render(&stepwise, page, 400, 600),
                "page {page} renders identically either way"
            );
        }
    }

    #[test]
    fn the_default_alignment_matches_every_other_reflow_default() {
        // The shell's persisted `alignment` defaults to 0 and it only pushes the value to the core
        // when it is non-zero — so the core's untouched default has to *be* code 0 (Left), or a
        // fresh book renders in an alignment the Adjust sheet never claimed (#161 triage).
        let untouched = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
        let explicit_left = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
        let _ = explicit_left.set_alignment(0, 0);

        assert_eq!(untouched.page_count(), explicit_left.page_count());
        for page in 0..untouched.page_count() {
            assert_eq!(
                render(&untouched, page, 400, 600),
                render(&explicit_left, page, 400, 600),
                "page {page} of an untouched book is laid out Left, as code 0 promises"
            );
        }
        // ...and justifying really is a different layout, so the check above has teeth.
        let justified = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
        let _ = justified.set_alignment(1, 0);
        assert!(
            (0..untouched.page_count())
                .any(|p| render(&justified, p, 400, 600) != render(&untouched, p, 400, 600)),
            "Justify differs from the default, so the comparison above is meaningful"
        );
    }
}
