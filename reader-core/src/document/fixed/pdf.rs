//! `PdfBackend` — the M0 fixed-layout PDF [`Document`] over `pdfium-render` (RR5, Amendment 4).
//!
//! ## Single-copy render with explicit channel order (Fork 4 / Amendment 3)
//! The page is rendered **directly into the borrowed [`PixelBuffer`]**: a `PdfBitmap` wraps
//! the buffer's `&mut [u8]` via the (unsafe) `from_bytes`, and `set_reverse_byte_order(true)`
//! makes pdfium emit **RGBA** straight into it (pdfium's native order is BGRA). The buffer is
//! white-filled first (RR4-FR3); the channel order matches [`CHANNEL_ORDER`], asserted by the
//! grayscale golden test.
//!
//! ## Library binding (host vs device)
//! pdfium is a dynamic library. A process-global `Pdfium` (behind a `OnceLock`, `thread_safe`
//! feature) backs `PdfDocument<'static>` so the backend owns the open document without a
//! self-referential type. Binding order: an explicit `PDFIUM_DYNAMIC_LIB_PATH` (host tests /
//! a vendored binary) → the system library → else [`CoreError::BackendUnavailable`]. On
//! Android the loader finds `libpdfium.so` in `jniLibs/` via the system path.

use std::sync::{Mutex, OnceLock};

use pdfium_render::prelude::*;

use crate::document::text_select::{self, CharBox, NormRect, TextSelection};
use crate::document::{Document, DocumentMetadata, LinkTarget, PageLink, TocEntry};
use crate::error::{CoreError, CoreResult};
use crate::render::PixelBuffer;

/// Env var naming an explicit libpdfium to bind (host testing / vendored binary).
pub const PDFIUM_LIB_PATH_ENV: &str = "PDFIUM_DYNAMIC_LIB_PATH";

/// The process-global pdfium binding (owns the library bindings; effectively `'static`).
static PDFIUM: OnceLock<Pdfium> = OnceLock::new();

/// A second, leaked binding to the same library, exposed as `&'static dyn ...`.
///
/// `Pdfium::new` consumes the box it is given and 0.9.1 exposes no accessor to recover it,
/// yet the single-copy render needs a `&dyn PdfiumLibraryBindings` for
/// `PdfBitmap::from_bytes` (Fork 4). Binding the same already-loaded library twice is sound
/// — each call returns a fresh bindings object over the same handle — so we keep one for
/// `Pdfium` and leak one to `'static` for `from_bytes`.
static BINDINGS: OnceLock<&'static dyn PdfiumLibraryBindings> = OnceLock::new();

/// Serializes first-time binding. pdfium's library init is not reentrant, so two threads
/// racing to bind on first use can abort; this lock makes init single-flight (the steady
/// state is lock-free via the `OnceLock::get` fast path).
static INIT_LOCK: Mutex<()> = Mutex::new(());

/// Bind pdfium once and return the process-global handle, or a typed error if no library
/// is available (the host without a vendored libpdfium — RR5-FR1).
fn pdfium() -> CoreResult<&'static Pdfium> {
    // Fast path: already initialized, no lock.
    if let Some(p) = PDFIUM.get() {
        return Ok(p);
    }
    // Slow path: serialize so exactly one thread ever binds the library.
    let _guard = INIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(p) = PDFIUM.get() {
        return Ok(p);
    }
    // Bind twice over the same library: one box for `Pdfium`, one leaked for `from_bytes`.
    let for_pdfium = bind_pdfium()?;
    let for_bitmap = bind_pdfium()?;
    let _ = PDFIUM.set(Pdfium::new(for_pdfium));
    let _ = BINDINGS.set(Box::leak(for_bitmap));
    Ok(PDFIUM.get().expect("pdfium set above"))
}

/// The `&'static dyn PdfiumLibraryBindings` for the single-copy bitmap wrap (Fork 4).
fn bindings() -> CoreResult<&'static dyn PdfiumLibraryBindings> {
    pdfium()?; // ensure both globals are initialized
    BINDINGS
        .get()
        .copied()
        .ok_or_else(|| CoreError::BackendUnavailable("pdfium bindings uninitialized".into()))
}

fn bind_pdfium() -> CoreResult<Box<dyn PdfiumLibraryBindings>> {
    if let Ok(path) = std::env::var(PDFIUM_LIB_PATH_ENV) {
        return Pdfium::bind_to_library(&path).map_err(|e| {
            CoreError::BackendUnavailable(format!("bind_to_library({path}) failed: {e}"))
        });
    }
    Pdfium::bind_to_system_library().map_err(|e| {
        CoreError::BackendUnavailable(format!(
            "no libpdfium: set {PDFIUM_LIB_PATH_ENV} or install a system library ({e})"
        ))
    })
}

/// Map a pdfium load error to a typed [`CoreError`] (DRM vs corrupt vs other — RR7-FR5).
fn map_load_error(e: PdfiumError) -> CoreError {
    match e {
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError)
        | PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::SecurityError) => {
            CoreError::DrmProtected
        }
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::FormatError)
        | PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::FileError) => {
            CoreError::CorruptDocument(format!("pdfium: {e}"))
        }
        other => CoreError::RenderBackend(format!("pdfium load: {other}")),
    }
}

/// A loaded PDF, rendered directly into the shell's buffer (RR5, Amendment 4).
pub struct PdfBackend {
    document: PdfDocument<'static>,
}

impl PdfBackend {
    /// Open a PDF from in-memory bytes (the shell reads the file; the core never names a
    /// path scheme). DRM/corrupt files return a typed error, never a panic (RR7-FR5, RR21-FR3).
    pub fn open(bytes: Vec<u8>) -> CoreResult<Self> {
        let pdfium = pdfium()?;
        let document = pdfium
            .load_pdf_from_byte_vec(bytes, None)
            .map_err(map_load_error)?;
        Ok(Self { document })
    }

    /// Render page `index` into `buf` using a [`PdfRenderConfig`] built from the buffer's
    /// `(w, h)`. Shared by full-page and clipped/scaled region render (DRY): validates the
    /// index, white-fills, and does the single-copy bitmap wrap (Fork 4); never panics on a
    /// bad index/oversize (RR21-FR3).
    fn render_with_config(
        &self,
        index: usize,
        buf: &mut PixelBuffer<'_>,
        make_config: impl FnOnce(i32, i32) -> PdfRenderConfig,
    ) -> CoreResult<()> {
        let available = self.page_count();
        let page_index = i32::try_from(index)
            .ok()
            .filter(|&i| (i as usize) < available)
            .ok_or(CoreError::PageOutOfRange {
                requested: index,
                available,
            })?;

        // White-fill before render so there are no alpha gaps (RR4-FR3).
        buf.fill_white();

        let width = buf.width();
        let height = buf.height();
        let w = i32::try_from(width)
            .map_err(|_| CoreError::BufferMismatch(format!("width {width} exceeds i32")))?;
        let h = i32::try_from(height)
            .map_err(|_| CoreError::BufferMismatch(format!("height {height} exceeds i32")))?;

        let page = self
            .document
            .pages()
            .get(page_index)
            .map_err(|e| CoreError::RenderBackend(format!("get page {index}: {e}")))?;

        let config = make_config(w, h);

        // Single-copy: wrap the borrowed buffer as the pdfium bitmap target (Fork 4).
        // SAFETY: `buf.bytes_mut()` is exactly `width*height*4` bytes (PixelBuffer invariant),
        // matching the bitmap geometry; the borrow lives only for this call (Amendment 5).
        let lib = bindings()?;
        let mut bitmap =
            unsafe { PdfBitmap::from_bytes(w, h, PdfBitmapFormat::BGRA, buf.bytes_mut(), lib) }
                .map_err(|e| CoreError::RenderBackend(format!("wrap bitmap: {e}")))?;

        page.render_into_bitmap_with_config(&mut bitmap, &config)
            .map_err(|e| CoreError::RenderBackend(format!("render page {index}: {e}")))?;

        Ok(())
    }

    /// Render a clipped, scaled viewport window of a page for pan/zoom (RR5-FR3). The page is
    /// scaled by `zoom`, then the `buf`-sized window whose top-left is `(offset_x, offset_y)`
    /// (in scaled-page pixels) is rasterized into `buf`. A non-finite/non-positive `zoom`
    /// falls back to 1.0.
    pub fn render_region(
        &self,
        index: usize,
        buf: &mut PixelBuffer<'_>,
        zoom: f32,
        offset_x: i32,
        offset_y: i32,
    ) -> CoreResult<()> {
        let zoom = if zoom.is_finite() && zoom > 0.0 {
            zoom
        } else {
            1.0
        };
        self.render_with_config(index, buf, move |w, h| {
            PdfRenderConfig::new()
                .scale_page_by_factor(zoom)
                .clip(offset_x, offset_y, offset_x + w, offset_y + h)
                .set_format(PdfBitmapFormat::BGRA)
                .set_reverse_byte_order(true)
        })
    }

    /// The page's glyphs as normalized [`CharBox`]es (RR11 / D1b) — the input to the pure
    /// selection logic. pdfium gives chars in reading order with point-space, bottom-left-origin
    /// bounds; we normalize + flip Y exactly like [`Self::page_links`]. An out-of-range page, a
    /// text-less page, or a glyph with no resolvable bounds simply contributes nothing (never
    /// panics, RR21-FR3).
    fn page_chars(&self, index: usize) -> Vec<CharBox> {
        let page = match i32::try_from(index)
            .ok()
            .and_then(|i| self.document.pages().get(i).ok())
        {
            Some(p) => p,
            None => return Vec::new(),
        };
        let pw = page.width().value;
        let ph = page.height().value;
        if pw <= 0.0 || ph <= 0.0 {
            return Vec::new();
        }
        let text = match page.text() {
            Ok(t) => t,
            Err(_) => return Vec::new(),
        };
        let mut out = Vec::new();
        for ch in text.chars().iter() {
            let Some(c) = ch.unicode_char() else { continue };
            let bounds = match ch.loose_bounds() {
                Ok(b) => b,
                Err(_) => continue,
            };
            let nx0 = (bounds.left().value / pw).clamp(0.0, 1.0);
            let nx1 = (bounds.right().value / pw).clamp(0.0, 1.0);
            let ny_top = ((ph - bounds.top().value) / ph).clamp(0.0, 1.0);
            let ny_bottom = ((ph - bounds.bottom().value) / ph).clamp(0.0, 1.0);
            out.push(CharBox {
                ch: c,
                rect: NormRect {
                    x0: nx0.min(nx1),
                    y0: ny_top.min(ny_bottom),
                    x1: nx0.max(nx1),
                    y1: ny_top.max(ny_bottom),
                },
            });
        }
        out
    }
}

impl Document for PdfBackend {
    fn page_count(&self) -> usize {
        self.document.pages().len() as usize
    }

    fn metadata(&self) -> DocumentMetadata {
        use pdfium_render::prelude::PdfDocumentMetadataTagType as Tag;
        let md = self.document.metadata();
        let get = |tag| md.get(tag).map(|t| t.value().to_string());
        DocumentMetadata {
            title: get(Tag::Title),
            author: get(Tag::Author),
        }
    }

    fn render_page(&self, index: usize, buf: &mut PixelBuffer<'_>) -> CoreResult<()> {
        // Full-page render: scale the whole page to the buffer. Amendment 3: reverse byte
        // order so pdfium emits RGBA into our RGBA buffer.
        self.render_with_config(index, buf, |w, h| {
            PdfRenderConfig::new()
                .set_target_size(w, h)
                .set_format(PdfBitmapFormat::BGRA)
                .set_reverse_byte_order(true)
        })
    }

    fn toc(&self) -> Vec<TocEntry> {
        // Walk the top-level outline (root() = first top-level entry) then its siblings; each
        // entry recurses into its direct children. Never panics: a missing/unresolvable
        // destination yields target_page = None (RR5-FR2 / RR11-FR2).
        let mut out = Vec::new();
        let mut cur = self.document.bookmarks().root();
        while let Some(bm) = cur {
            out.push(bookmark_to_entry(&bm));
            cur = bm.next_sibling();
        }
        out
    }

    fn page_links(&self, index: usize) -> Vec<PageLink> {
        // Resolve the page; an out-of-range index yields no links (never panics, RR21-FR3).
        let page = match i32::try_from(index)
            .ok()
            .and_then(|i| self.document.pages().get(i).ok())
        {
            Some(p) => p,
            None => return Vec::new(),
        };
        // Page dimensions in points (pdfium origin = bottom-left); used to normalize + flip Y.
        let pw = page.width().value;
        let ph = page.height().value;
        if pw <= 0.0 || ph <= 0.0 {
            return Vec::new();
        }
        let mut out = Vec::new();
        for link in page.links().iter() {
            let target = match link_target(&link) {
                Some(t) => t,
                None => continue, // unresolvable / unsupported action → not navigable
            };
            let rect = match link.rect() {
                Ok(r) => r,
                Err(_) => continue,
            };
            // pdfium: bottom-left origin, points → normalized [0,1], top-left origin.
            let nx0 = (rect.left().value / pw).clamp(0.0, 1.0);
            let nx1 = (rect.right().value / pw).clamp(0.0, 1.0);
            let ny_top = ((ph - rect.top().value) / ph).clamp(0.0, 1.0);
            let ny_bottom = ((ph - rect.bottom().value) / ph).clamp(0.0, 1.0);
            out.push(PageLink {
                x0: nx0.min(nx1),
                y0: ny_top.min(ny_bottom),
                x1: nx0.max(nx1),
                y1: ny_top.max(ny_bottom),
                target,
            });
        }
        out
    }

    fn word_at(&self, page: usize, x: f32, y: f32) -> Option<TextSelection> {
        text_select::word_at(&self.page_chars(page), x, y)
    }

    fn text_in_rect(&self, page: usize, rect: NormRect) -> TextSelection {
        text_select::text_in_rect(&self.page_chars(page), rect)
    }
}

/// Resolve a pdfium link's target (RR11-FR3): a direct destination or a GoTo/URI action.
/// Returns `None` for label-only or unsupported actions (the link is then not navigable).
fn link_target(link: &PdfLink<'_>) -> Option<LinkTarget> {
    // A direct /Dest on the link annotation.
    if let Some(page) = link
        .destination()
        .and_then(|d| d.page_index().ok())
        .and_then(|i| usize::try_from(i).ok())
    {
        return Some(LinkTarget::Page(page));
    }
    // Otherwise an action: GoTo (internal) or URI (external).
    match link.action()? {
        PdfAction::LocalDestination(local) => local
            .destination()
            .ok()
            .and_then(|d| d.page_index().ok())
            .and_then(|i| usize::try_from(i).ok())
            .map(LinkTarget::Page),
        PdfAction::Uri(uri) => uri
            .uri()
            .ok()
            .filter(|u| !u.is_empty())
            .map(LinkTarget::Uri),
        _ => None,
    }
}

/// Convert a pdfium bookmark and its subtree into a [`TocEntry`] (RR5-FR2 / RR11-FR2).
fn bookmark_to_entry(bm: &PdfBookmark<'_>) -> TocEntry {
    let title = bm.title().unwrap_or_default();
    // A bookmark may have no destination, or one that doesn't resolve to a page index — the
    // entry is shown but not navigable (target_page = None).
    let target_page = bm
        .destination()
        .and_then(|d| d.page_index().ok())
        .and_then(|i| usize::try_from(i).ok());
    let children = bm
        .iter_direct_children()
        .map(|c| bookmark_to_entry(&c))
        .collect();
    TocEntry {
        title,
        target_page,
        children,
    }
}

#[cfg(test)]
#[path = "pdf_tests.rs"]
mod tests;
