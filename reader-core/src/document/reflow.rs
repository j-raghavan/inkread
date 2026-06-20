//! Reflowable-format backend (EPUB) behind the [`Document`] trait (RR2-FR5, RR2-AC2).
//!
//! Adapts [`inkread_epub`] (parse → content model → layout → raster) to the core's [`Document`]
//! seam. Unlike the fixed PDF backend, a reflowable document's **page count depends on the
//! viewport + font size**: pages are (re)computed when the render buffer's dimensions change, held
//! behind a [`RefCell`] so the trait's `&self` render path can repaginate lazily. Each spine chapter
//! starts a new page (book convention), which also anchors TOC targets to page indices.
//!
//! `word_at`/`text_in_rect` (dictionary + selection on reflow text) and font-size control are
//! follow-ups; this backend delivers open → paginate → render → navigate → TOC.

use std::cell::RefCell;

use inkread_epub::layout::{paginate, LayoutOpts, Page};
use inkread_epub::render::{render_page as raster_page, AbFont, GrayCanvas};
use inkread_epub::{parse_blocks, Block, EpubPackage, NavPoint};

use crate::document::{Document, DocumentMetadata, TocEntry};
use crate::error::{CoreError, CoreResult};
use crate::render::PixelBuffer;

/// Default body font size in device pixels. Tuned for a Supernote-class panel; Phase 6 makes this a
/// user setting that triggers repagination.
const DEFAULT_FONT_PX: f32 = 38.0;

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
    /// The current pagination; recomputed when the viewport changes.
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
            DEFAULT_FONT_PX,
        );
        Ok(Self {
            chapters,
            chapter_keys,
            nav: pkg.toc,
            meta,
            font,
            laid: RefCell::new(laid),
        })
    }

    /// Repaginate if the requested buffer dimensions differ from the cached layout's viewport.
    fn ensure_laid(&self, w: u32, h: u32) {
        let needs = {
            let laid = self.laid.borrow();
            laid.opts.page_w as u32 != w || laid.opts.page_h as u32 != h
        };
        if needs {
            let font_px = self.laid.borrow().opts.font_px;
            let fresh = layout_all(&self.chapters, &self.font, w, h, font_px);
            *self.laid.borrow_mut() = fresh;
        }
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
}

/// Paginate every chapter for `(w, h, font_px)`; each chapter starts a fresh page.
fn layout_all(chapters: &[Vec<Block>], font: &AbFont, w: u32, h: u32, font_px: f32) -> Laid {
    let opts = LayoutOpts::new(w as f32, h as f32, font_px);
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
