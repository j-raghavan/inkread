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
use std::collections::HashMap;

use inkread_epub::layout::{
    paginate_upto, paginate_with_images, Align, Hyphenator, ImageSizer, LayoutOpts, Page,
};
use inkread_epub::measure::{CachedHyphenator, CachedMetrics};
use inkread_epub::render::{
    render_page_with_images as raster_page, AbFont, EnHyphenator, GrayCanvas, ImageSource,
};
use inkread_epub::{parse_blocks_with, Block, EpubPackage, ImageStore, NavPoint, Stylesheet};

use crate::document::text_select::{self, CharBox, NormRect, TextAnchor, TextSelection};
use crate::document::{Document, DocumentMetadata, SearchMatch, TocEntry};
use crate::error::{CoreError, CoreResult};
use crate::persistence::{PaginationCache, PaginationProgress};
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

/// How many materialized chapters to keep. Reading walks forward through one chapter at a time, so
/// a couple of entries covers a page turn across a chapter boundary and a jump back, while keeping
/// the retained page structures bounded regardless of how long the book is.
const CHAPTER_CACHE: usize = 3;

/// The settings a pagination was built for, exactly as the reader set them.
///
/// Staleness is decided by comparing this — not the [`LayoutOpts`] derived from it — and a
/// cancelled re-layout restores it verbatim. Comparing derived values instead would mean
/// reconstructing the settings on the revert path (recovering `scale` by dividing `font_px` back
/// out, and so on); any float that failed to round-trip would leave the surviving pagination
/// looking permanently stale, so every later read would re-run the work the reader just cancelled.
/// Keeping the compared value and the restored value the same thing removes that class of bug
/// rather than relying on the arithmetic to be exact.
#[derive(Debug, Clone, Copy, PartialEq)]
struct LayoutRequest {
    /// Page size in device pixels.
    viewport: (u32, u32),
    /// User text scale; `1.0` = [`BASE_FONT_PX`]. Always finite (see [`clamp_scale`]), so the
    /// derived `PartialEq` can never be defeated by a NaN.
    scale: f32,
    line_spacing: f32,
    align: Align,
    /// Text columns per page (#194).
    columns: u8,
    /// The bundled reading face. Not part of [`LayoutOpts`], but a different face means different
    /// metrics, so it belongs in the staleness key.
    font_id: usize,
}

/// A pagination of the whole book for one set of layout parameters — rebuilt only when those
/// parameters actually change (see [`EpubBackend::laid`]).
///
/// This is the **index only**: where each chapter starts and how long the book is. The pages
/// themselves are materialized per chapter on demand ([`EpubBackend::with_page`]), because holding
/// every page of a long book costs tens of megabytes of positioned runs for content that is not on
/// screen — and because the index is the part worth persisting across launches (#162).
struct Laid {
    /// What was asked for.
    request: LayoutRequest,
    /// What that resolved to for the layout engine.
    opts: LayoutOpts,
    /// `chapter_start[i]` = the global page index where chapter `i` begins (TOC resolution).
    chapter_start: Vec<usize>,
    /// Total pages across every chapter.
    total_pages: usize,
}

impl Laid {
    /// Build the index from per-chapter page counts. Every chapter occupies at least one page, so
    /// the chapter→page mapping stays 1:1 even for an empty chapter.
    fn from_chapter_pages(
        request: LayoutRequest,
        opts: LayoutOpts,
        chapter_pages: &[usize],
    ) -> Self {
        let mut chapter_start = Vec::with_capacity(chapter_pages.len());
        let mut total_pages = 0;
        for &pages in chapter_pages {
            chapter_start.push(total_pages);
            total_pages += pages.max(1);
        }
        Self {
            request,
            opts,
            chapter_start,
            // A book with no chapters at all still presents one (blank) page.
            total_pages: total_pages.max(1),
        }
    }

    /// The chapter index that global `page` falls in (the last chapter whose start ≤ page).
    fn chapter_of(&self, page: usize) -> usize {
        self.chapter_start
            .iter()
            .rposition(|&start| start <= page)
            .unwrap_or(0)
    }
}

/// A chapter's content: its source XHTML until something actually needs it, then the parsed blocks.
///
/// Parsing every chapter at open was the one cost no cache removed — paid in full on every open of
/// every book, for chapters the reader never looks at. On a book whose pagination is already cached
/// (the common case after the first open) nothing but the chapter being read has to be parsed at
/// all (#186).
enum Chapter {
    /// Not yet needed; holds the XHTML the parse will consume.
    Source(String),
    /// Parsed. The source is dropped here, so a fully-read book costs what it always did.
    Parsed(Vec<Block>),
}

impl Chapter {
    /// The parsed blocks, or `&[]` if this chapter has not been through [`EpubBackend::parse_chapter`].
    fn blocks(&self) -> &[Block] {
        match self {
            Chapter::Parsed(blocks) => blocks,
            Chapter::Source(_) => &[],
        }
    }
}

/// The EPUB backend: parsed per-chapter content + the embedded reading face, with a cached layout.
pub struct EpubBackend {
    /// Reading-order chapters, each parsed on first use (see [`Chapter`]).
    chapters: RefCell<Vec<Chapter>>,
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
    /// Text columns per page (#194 — default 1). Drives repagination.
    columns: Cell<u8>,
    /// The page size to lay out for; updated by the render path when the buffer changes.
    viewport: Cell<(u32, u32)>,
    /// The current pagination index, or `None` before the first one is needed. Laying out lazily
    /// lets the open path apply the reader's saved typography *before* any pagination is built, so
    /// a cold open costs a single layout pass instead of one per setting (#161/#162).
    laid: RefCell<Option<Laid>>,
    /// Recently materialized chapters, most recent last. Bounded by [`CHAPTER_CACHE`] and cleared
    /// whenever [`Self::laid`] is rebuilt, since a re-layout invalidates every page in it.
    chapter_pages: RefCell<Vec<ChapterPages>>,
    /// Where paginations survive between launches, once a store is attached (#162).
    pagination_cache: RefCell<Option<Box<dyn PaginationCache>>>,
    /// Where a pagination in flight is reported, and where cancellation is asked about (#161).
    progress: RefCell<Option<Box<dyn PaginationProgress>>>,
    /// The book's declared block styling, applied as each chapter is parsed (#188). A property of
    /// the book, not a setting, so it never invalidates a pagination the way typography does.
    stylesheet: Stylesheet,
    /// The book's illustrations (#187), read from the container when one is laid out or drawn.
    images: ImageStore,
    /// Intrinsic sizes already looked up, since laying out a chapter asks for every image in it and
    /// each miss costs a container read.
    image_sizes: RefCell<HashMap<String, Option<(u32, u32)>>>,
}

impl ImageSizer for EpubBackend {
    fn size(&self, src: &str) -> Option<(u32, u32)> {
        if let Some(hit) = self.image_sizes.borrow().get(src) {
            return *hit;
        }
        let size = self.images.size(src);
        self.image_sizes.borrow_mut().insert(src.to_string(), size);
        size
    }
}

impl ImageSource for EpubBackend {
    fn bytes(&self, src: &str) -> Option<Vec<u8>> {
        self.images.bytes(src)
    }
}

impl EpubBackend {
    /// Parse `bytes` and paginate for the initial `viewport`. Maps parse failures to a typed error.
    pub fn open(bytes: Vec<u8>, viewport: crate::render::Viewport) -> CoreResult<Self> {
        let pkg = EpubPackage::open(bytes)
            .map_err(|e| CoreError::RenderBackend(format!("epub open: {e}")))?;
        // Deliberately not parsed here: see [`Chapter`]. `open` only has to know how many chapters
        // there are; what they contain is the reader's business, one chapter at a time.
        let chapters: Vec<Chapter> = pkg
            .chapters
            .iter()
            .map(|c| Chapter::Source(c.html.clone()))
            .collect();
        let chapter_keys: Vec<String> = pkg.chapters.iter().map(|c| basename(&c.href)).collect();
        let meta = DocumentMetadata {
            title: pkg.title.clone(),
            author: pkg.author.clone(),
        };
        Ok(Self {
            chapters: RefCell::new(chapters),
            chapter_keys,
            stylesheet: pkg.stylesheet,
            images: pkg.images,
            image_sizes: RefCell::new(HashMap::new()),
            nav: pkg.toc,
            meta,
            font: RefCell::new(AbFont::default_font()),
            hyph: EnHyphenator::new(),
            font_id: Cell::new(0),
            scale: Cell::new(1.0),
            line_spacing: Cell::new(DEFAULT_LINE_SPACING),
            align: Cell::new(Align::default()),
            columns: Cell::new(1),
            viewport: Cell::new((viewport.width, viewport.height)),
            // Deferred: the first read paginates, so the saved typography applied right after open
            // is folded into that single pass rather than triggering one pass per setting.
            laid: RefCell::new(None),
            chapter_pages: RefCell::new(Vec::new()),
            pagination_cache: RefCell::new(None),
            progress: RefCell::new(None),
        })
    }

    /// A backend over `chapters` of pre-parsed blocks, bypassing the container. Lets a test build a
    /// book with enough chapters to exercise the chapter cache's eviction, which the small bundled
    /// fixture cannot. Everything downstream — indexing, materialization, caching — is the real path.
    #[cfg(test)]
    fn from_chapters(chapters: Vec<Vec<Block>>, viewport: crate::render::Viewport) -> Self {
        let chapters: Vec<Chapter> = chapters.into_iter().map(Chapter::Parsed).collect();
        let chapter_keys = (0..chapters.len())
            .map(|i| format!("ch{i}.xhtml"))
            .collect();
        Self {
            chapters: RefCell::new(chapters),
            chapter_keys,
            stylesheet: Stylesheet::default(),
            images: ImageStore::default(),
            image_sizes: RefCell::new(HashMap::new()),
            nav: Vec::new(),
            meta: DocumentMetadata {
                title: Some("Synthetic".into()),
                author: None,
            },
            font: RefCell::new(AbFont::default_font()),
            hyph: EnHyphenator::new(),
            font_id: Cell::new(0),
            scale: Cell::new(1.0),
            line_spacing: Cell::new(DEFAULT_LINE_SPACING),
            align: Cell::new(Align::default()),
            columns: Cell::new(1),
            viewport: Cell::new((viewport.width, viewport.height)),
            laid: RefCell::new(None),
            chapter_pages: RefCell::new(Vec::new()),
            pagination_cache: RefCell::new(None),
            progress: RefCell::new(None),
        }
    }

    /// How many chapters the book has — the one thing about them that needs no parsing.
    fn chapter_count(&self) -> usize {
        self.chapters.borrow().len()
    }

    /// Parse chapter `index` if it has not been parsed yet. Idempotent and cheap on a repeat call.
    fn parse_chapter(&self, index: usize) {
        let mut chapters = self.chapters.borrow_mut();
        let Some(slot) = chapters.get_mut(index) else {
            return;
        };
        if let Chapter::Source(html) = slot {
            // `parse_blocks_with` takes the source by reference, so move it out first and let the
            // old `Chapter` (and the XHTML inside it) drop as soon as the blocks exist.
            let html = std::mem::take(html);
            #[cfg(test)]
            CHAPTER_PARSES.with(|c| c.set(c.get() + 1));
            *slot = Chapter::Parsed(parse_blocks_with(&html, &self.stylesheet));
        }
    }

    /// Parse every chapter. Needed only where the whole book is unavoidably in play — building a
    /// pagination index from scratch — which is why it is a distinct, obvious call rather than
    /// something `open` does silently.
    fn parse_all_chapters(&self) {
        for index in 0..self.chapter_count() {
            self.parse_chapter(index);
        }
    }

    /// Lay chapter `index` out as far as `want` whole pages, returning `(pages, complete)`.
    ///
    /// Pagination is per chapter by construction — a chapter always starts a fresh page and nothing
    /// carries across the boundary — so a chapter laid out alone is identical to its slice of a
    /// whole-book pass. That is what makes materializing one chapter at a time sound rather than
    /// merely convenient, and it extends to a *partial* chapter: `want` pages are identical to the
    /// first `want` pages of the full pass, because a page break depends only on what precedes it.
    ///
    /// Materializing a whole chapter to show one of its pages is the dominant cost of opening a
    /// book, and a reader resuming at a chapter start needs exactly one page of it (#186).
    fn lay_out_chapter_upto(
        &self,
        index: usize,
        opts: &LayoutOpts,
        want: usize,
    ) -> (Vec<Page>, bool) {
        self.parse_chapter(index);
        let chapters = self.chapters.borrow();
        let Some(chapter) = chapters.get(index) else {
            return (vec![Page::default()], true);
        };
        let blocks = chapter.blocks();
        let face = self.font.borrow();
        let font = CachedMetrics::new(&*face);
        let hyph = CachedHyphenator::new(&self.hyph);
        let (mut pages, complete) = paginate_upto(blocks, opts, &font, &hyph, self, want);
        #[cfg(test)]
        {
            CHAPTER_LAYOUTS.with(|c| c.set(c.get() + 1));
            CHAPTER_PAGES_LAID.with(|c| c.set(c.get() + pages.len()));
        }
        // Only a *complete* empty pagination means an empty chapter; an empty partial one just
        // means nothing was asked for.
        if pages.is_empty() && complete {
            pages.push(Page::default()); // an empty chapter still occupies its one page
        }
        (pages, complete)
    }

    /// Run `f` against global `page` and the layout parameters it was laid out with, materializing
    /// its chapter if it is not already cached. `None` for a page past the end of the book.
    fn with_page<R>(&self, page: usize, f: impl FnOnce(&Page, &LayoutOpts) -> R) -> Option<R> {
        let (chapter, offset, chapter_len, opts) = {
            let laid = self.laid();
            if page >= laid.total_pages {
                return None;
            }
            let chapter = laid.chapter_of(page);
            let start = laid.chapter_start.get(chapter).copied().unwrap_or(0);
            // The chapter's length per the pagination index — what decides whether a prefix is
            // worth having at all (see below).
            let end = laid
                .chapter_start
                .get(chapter + 1)
                .copied()
                .unwrap_or(laid.total_pages);
            (chapter, page - start, end.saturating_sub(start), laid.opts)
        };

        // A cached chapter serves the page if it was laid out that far, or if we know there is no
        // more to lay out. `f` runs while `chapter_pages` is borrowed, so it must not re-enter
        // `with_page` — every caller passes a pure read of the page.
        let cached = {
            let cache = self.chapter_pages.borrow();
            match cache.iter().find(|c| c.chapter == chapter) {
                Some(entry) if entry.pages.len() > offset => {
                    return entry.pages.get(offset).map(|p| f(p, &opts));
                }
                // Defence in depth: an in-range page always falls inside its chapter's counted
                // length, so this is unreachable unless the pagination index and the layout have
                // diverged. Cheap to keep, and it fails closed rather than re-paginating.
                Some(entry) if entry.complete => return None,
                Some(_) => true, // materialized, but not this far yet
                None => false,
            }
        };

        // Extending a partial chapter lays out the rest of it in one pass rather than one page at a
        // time: reading forward through a long chapter would otherwise re-lay a growing prefix on
        // every turn, which is quadratic and worse than the whole-chapter pass this replaces. So a
        // chapter costs at most two passes — the cheap prefix that opened it, then the rest.
        //
        // A prefix is only worth taking when it is a small part of the chapter. Resuming deep into
        // one — page 225 of 229, say — the prefix costs almost as much as the whole chapter, and
        // the extending pass is then pure overhead: measured at +66% total work over laying it out
        // once. Past the halfway mark, do the whole chapter now and be done.
        let want = if cached || offset + 1 >= chapter_len.div_ceil(2) {
            usize::MAX
        } else {
            offset + 1
        };
        // Drop any partial entry for this chapter BEFORE laying out the rest of it, not after:
        // otherwise the prefix and the full pass are both live at once, which on a deep resume of a
        // long chapter is tens of megabytes of laid-out pages held for no reason.
        self.chapter_pages
            .borrow_mut()
            .retain(|c| c.chapter != chapter);
        let (pages, complete) = self.lay_out_chapter_upto(chapter, &opts, want);
        let result = pages.get(offset).map(|p| f(p, &opts));
        let mut cache = self.chapter_pages.borrow_mut();
        if cache.len() >= CHAPTER_CACHE {
            cache.remove(0); // oldest out; the tail is what reading is walking through
        }
        cache.push(ChapterPages {
            chapter,
            pages,
            complete,
        });
        result
    }

    /// The settings currently asked for. Comparing this against [`Laid::request`] is what makes a
    /// redundant repagination free.
    fn current_request(&self) -> LayoutRequest {
        LayoutRequest {
            viewport: self.viewport.get(),
            scale: self.scale.get(),
            line_spacing: self.line_spacing.get(),
            align: self.align.get(),
            columns: self.columns.get(),
            font_id: self.font_id.get(),
        }
    }

    /// Resolve a request into the layout engine's parameters.
    fn opts_for(request: &LayoutRequest) -> LayoutOpts {
        let (w, h) = request.viewport;
        let mut opts = LayoutOpts::new(w as f32, h as f32, BASE_FONT_PX * request.scale);
        opts.line_spacing = request.line_spacing;
        opts.align = request.align;
        opts.columns = request.columns;
        opts
    }

    /// The current pagination, built on first use and rebuilt **only** when the requested layout
    /// parameters or the reading face actually differ from the cached ones.
    fn laid(&self) -> Ref<'_, Laid> {
        let request = self.current_request();
        let stale = match self.laid.borrow().as_ref() {
            None => true,
            Some(laid) => laid.request != request,
        };
        if stale {
            let opts = Self::opts_for(&request);
            let key = layout_key(&opts, request.font_id, self.chapter_count());
            let cache = self.pagination_cache.borrow();

            // A stored pagination is only usable if it describes this book's chapters — a count
            // mismatch means the key collided or the content moved under it, and trusting it would
            // put the reader on the wrong page rather than merely make the book slow to open.
            let cached = cache
                .as_ref()
                .and_then(|c| c.load(&key))
                .filter(|pages| pages.len() == self.chapter_count());

            let chapter_pages = match cached {
                Some(hit) => Some(hit),
                None => {
                    // The only place the whole book is unavoidably in play: counting a book's pages
                    // means laying every chapter out. A cache hit skips this entirely, and with it
                    // the parse of every chapter the reader is not reading (#186).
                    self.parse_all_chapters();
                    let chapters = self.chapters.borrow();
                    let blocks: Vec<&[Block]> = chapters.iter().map(Chapter::blocks).collect();
                    let progress = self.progress.borrow();
                    // A book being laid out for the first time has nothing to fall back to, so
                    // that pass reports progress but cannot be cancelled — only a re-layout can.
                    let cancellable = self.laid.borrow().is_some();
                    let counted = index_chapters(
                        &blocks,
                        &self.font.borrow(),
                        &self.hyph,
                        self,
                        &opts,
                        progress.as_deref(),
                        cancellable,
                    );
                    if let (Some(counted), Some(c)) = (counted.as_ref(), cache.as_ref()) {
                        c.save(&key, counted);
                    }
                    counted
                }
            };
            drop(cache);

            match chapter_pages {
                Some(pages) => {
                    // Every cached page belongs to the layout being replaced.
                    self.chapter_pages.borrow_mut().clear();
                    *self.laid.borrow_mut() = Some(Laid::from_chapter_pages(request, opts, &pages));
                }
                // Abandoned: put the requested parameters back to the ones the surviving
                // pagination was built with. Leaving them changed would make it look permanently
                // stale, and every later read would restart the work the reader just cancelled.
                None => self.revert_request_to_laid(),
            }
        }
        // Materialized directly above and never taken back out, so the borrow is always `Some`.
        Ref::map(self.laid.borrow(), |slot| {
            slot.as_ref().expect("pagination materialized above")
        })
    }

    /// Roll the requested settings back to those of the pagination still in force, after a
    /// re-layout was cancelled (#161). Restores the very value the staleness check compares, so the
    /// surviving pagination reads as current again and the abandoned work is not retried.
    fn revert_request_to_laid(&self) {
        let Some(request) = self.laid.borrow().as_ref().map(|laid| laid.request) else {
            return;
        };
        self.viewport.set(request.viewport);
        self.scale.set(request.scale);
        self.line_spacing.set(request.line_spacing);
        self.align.set(request.align);
        self.apply_font(request.font_id as i32);
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
        // Shared with the PDF-reflow backend so the glyph→CharBox + anchor mapping lives once.
        self.with_page(index, |page, opts| {
            crate::document::reflow_view::page_charboxes(page, opts, &self.font.borrow())
        })
        .unwrap_or_default()
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
            .unwrap_or(laid.total_pages);
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
        self.laid().chapter_of(page)
    }
}

impl Document for EpubBackend {
    fn page_count(&self) -> usize {
        self.laid().total_pages
    }

    fn metadata(&self) -> DocumentMetadata {
        self.meta.clone()
    }

    fn render_page(&self, index: usize, buf: &mut PixelBuffer<'_>) -> CoreResult<()> {
        self.viewport.set((buf.width(), buf.height()));
        let (w, h) = (buf.width(), buf.height());
        let canvas = self
            .with_page(index, |page, opts| {
                let mut canvas = GrayCanvas::new(w, h);
                raster_page(page, opts, &self.font.borrow(), self, &mut canvas);
                canvas
            })
            .ok_or(CoreError::PageOutOfRange {
                requested: index,
                available: self.laid().total_pages,
            })?;
        buf.fill_white();
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

    fn set_columns(&self, columns: i32, current_page: usize) -> Option<usize> {
        // Stored as asked, clamped to what the engine models. Whether two columns are actually used
        // is the layout's call — a page too narrow for a readable measure declines them — so the
        // request survives a font-size change that later makes them viable.
        self.columns.set(columns.clamp(1, 2) as u8);
        self.repaginate_keeping_chapter(current_page)
    }

    fn set_font(&self, font_id: i32, current_page: usize) -> Option<usize> {
        // Swap the reading face, then repaginate (new metrics → new line breaks), keeping the chapter.
        self.apply_font(font_id);
        self.repaginate_keeping_chapter(current_page)
    }

    fn set_pagination_cache(&self, cache: Box<dyn PaginationCache>) {
        *self.pagination_cache.borrow_mut() = Some(cache);
    }

    fn set_pagination_progress(&self, progress: Box<dyn PaginationProgress>) {
        *self.progress.borrow_mut() = Some(progress);
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
    /// Chapters parsed on this thread — the counter behind the laziness test (#186).
    static CHAPTER_PARSES: Cell<usize> = const { Cell::new(0) };
    /// Chapter layout passes on this thread, and the pages each produced (#186). Materializing a
    /// chapter is cost a correctness test cannot see, so laziness has to be asserted directly.
    static CHAPTER_LAYOUTS: Cell<usize> = const { Cell::new(0) };
    static CHAPTER_PAGES_LAID: Cell<usize> = const { Cell::new(0) };
}

/// Chapter layout passes on this thread since [`reset_chapter_layouts`].
#[cfg(test)]
pub(crate) fn chapter_layouts() -> usize {
    CHAPTER_LAYOUTS.with(Cell::get)
}

/// Pages produced by those passes — what the laziness is actually about.
#[cfg(test)]
pub(crate) fn chapter_pages_laid() -> usize {
    CHAPTER_PAGES_LAID.with(Cell::get)
}

#[cfg(test)]
pub(crate) fn reset_chapter_layouts() {
    CHAPTER_LAYOUTS.with(|c| c.set(0));
    CHAPTER_PAGES_LAID.with(|c| c.set(0));
}

/// Chapters parsed on this thread since [`reset_chapter_parses`].
#[cfg(test)]
pub(crate) fn chapter_parses() -> usize {
    CHAPTER_PARSES.with(Cell::get)
}

#[cfg(test)]
pub(crate) fn reset_chapter_parses() {
    CHAPTER_PARSES.with(|c| c.set(0));
}

/// Pagination passes performed on this thread since [`reset_layout_passes`].
#[cfg(test)]
pub(crate) fn layout_passes() -> usize {
    LAYOUT_PASSES.with(Cell::get)
}

#[cfg(test)]
pub(crate) fn reset_layout_passes() {
    LAYOUT_PASSES.with(|c| c.set(0));
}

/// The cache key for one exact layout of one book (ADR-INKREAD-0013 D1 / RR9-FR3, #162).
///
/// [`LayoutOpts::layout_digest`] is the canonical discriminator — FNV-1a-64 over every
/// layout-affecting field, pinned by a regression test precisely because it keys persisted data and
/// must stay stable across toolchains. Two things the digest cannot know are appended here: the
/// **reading face**, which changes the metrics without appearing in `LayoutOpts`, and the **chapter
/// count**, so a stored pagination can never be read back against a book of a different shape.
///
/// The leading version exists for a third thing neither of those can see: a change to the layout
/// *engine* itself. The inputs can be identical and the output different, so **bump it whenever a
/// change alters how content is broken into lines or pages**. Without that, a reader who has already
/// opened a book keeps being served the page index the old engine built, and the fix appears not to
/// have worked — worse, positions resolved against a stale index land on the wrong page.
///
/// - `v1` → `v2`: the paragraph first-line indent stopped applying to every line (#163), which
///   changes how much text fits on a line and therefore every page count in the cache.
/// - `v2` → `v3`: the book's stylesheet is honoured (#188). A declared `text-indent: 0`, or a
///   centred block, drops the first-line indent, so lines fit differently than a `v2` pass assumed.
/// - `v3` → `v4`: illustrations occupy a real box instead of one line of `[image]` text (#187),
///   which changes the page count of every illustrated chapter.
fn layout_key(opts: &LayoutOpts, font_id: usize, chapters: usize) -> String {
    format!("v4|{:016x}|{font_id}|{chapters}", opts.layout_digest())
}

/// A materialized chapter: the pages laid out so far, and whether that is all of them (#186).
///
/// A chapter is first materialized only as far as the page being read, so `pages` is usually a
/// prefix. `complete` is what stops a later page being served from a partial layout.
struct ChapterPages {
    chapter: usize,
    pages: Vec<Page>,
    complete: bool,
}

/// Paginate every chapter for `opts` and return how many pages each occupies.
///
/// The pages themselves are dropped as they are counted: this pass exists to build the index, and
/// retaining every page of a long book is exactly the memory cost lazy materialization removes.
/// Peak memory here is one chapter, not the whole book.
/// Progress is reported throughout. `cancellable` says whether `progress` is also allowed to stop
/// the pass — a book being laid out for the first time has nothing to fall back to, so that one
/// always runs to completion. `None` means the pass was abandoned part-way (#161).
#[allow(clippy::too_many_arguments)]
fn index_chapters(
    chapters: &[&[Block]],
    font: &AbFont,
    hyph: &dyn Hyphenator,
    images: &dyn ImageSizer,
    opts: &LayoutOpts,
    progress: Option<&dyn PaginationProgress>,
    cancellable: bool,
) -> Option<Vec<usize>> {
    #[cfg(test)]
    LAYOUT_PASSES.with(|c| c.set(c.get() + 1));
    // Memoize measurement across the *whole book*, not per chapter: prose repeats itself, and the
    // widths a later chapter needs have almost all been computed by an earlier one (#161/#162).
    let font = CachedMetrics::new(font);
    let hyph = CachedHyphenator::new(hyph);
    let total = chapters.len();
    let mut counts = Vec::with_capacity(total);
    for blocks in chapters {
        // Asked before each chapter rather than after, so a cancel taken during the last chapter
        // still avoids the work rather than only the bookkeeping.
        if cancellable && progress.is_some_and(PaginationProgress::cancelled) {
            return None;
        }
        counts.push(
            paginate_with_images(blocks, opts, &font, &hyph, images)
                .len()
                .max(1),
        );
        if let Some(p) = progress {
            p.chapter_done(counts.len(), total);
        }
    }
    Some(counts)
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
#[path = "reflow_tests.rs"]
mod tests;
