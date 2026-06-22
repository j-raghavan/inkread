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

use std::cell::{Cell, RefCell};
use std::sync::{Mutex, OnceLock};

use inkread_epub::Block;
use inkread_pdftext::Glyph;
use pdfium_render::prelude::*;

use crate::document::reflow_view::ReflowView;
use crate::document::text_select::{self, CharBox, NormRect, TextSelection};
use crate::document::{
    Document, DocumentMetadata, ExportMode, FitMode, LinkTarget, PageInk, PageLink, TocEntry,
};
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

/// Whether a pdfium font weight counts as **bold** (≥ 600, i.e. semibold and up) — used to flag
/// body-sized bold section headings during reflow reconstruction (ADR-INKREAD-0011).
fn weight_is_bold(w: PdfFontWeight) -> bool {
    matches!(
        w,
        PdfFontWeight::Weight600
            | PdfFontWeight::Weight700Bold
            | PdfFontWeight::Weight800
            | PdfFontWeight::Weight900
    ) || matches!(w, PdfFontWeight::Custom(n) if n >= 600)
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

/// Drop points closer than `min_dist` (normalized) to the last kept one — removes the dense
/// capture jitter that makes an exported freehand stroke look lumpy. Endpoints are always kept.
fn simplify(points: &[(f32, f32)], min_dist: f32) -> Vec<(f32, f32)> {
    if points.len() <= 2 {
        return points.to_vec();
    }
    let md2 = min_dist * min_dist;
    let mut out = vec![points[0]];
    for &p in &points[1..points.len() - 1] {
        let last = *out.last().unwrap();
        let (dx, dy) = (p.0 - last.0, p.1 - last.1);
        if dx * dx + dy * dy >= md2 {
            out.push(p);
        }
    }
    out.push(points[points.len() - 1]);
    out
}

/// Chaikin corner-cutting: smooths a polyline into a rounded curve, keeping the endpoints. Run on
/// the simplified points so the exported ink reads as a smooth line, not a jagged polygon.
fn chaikin(points: &[(f32, f32)], iterations: u8) -> Vec<(f32, f32)> {
    let mut pts = simplify(points, 0.004);
    if pts.len() < 3 {
        return pts;
    }
    for _ in 0..iterations {
        let mut out = Vec::with_capacity(pts.len() * 2);
        out.push(pts[0]);
        for w in pts.windows(2) {
            let (p, q) = (w[0], w[1]);
            out.push((0.75 * p.0 + 0.25 * q.0, 0.75 * p.1 + 0.25 * q.1));
            out.push((0.25 * p.0 + 0.75 * q.0, 0.25 * p.1 + 0.75 * q.1));
        }
        out.push(*pts.last().unwrap());
        pts = out;
    }
    pts
}

/// A loaded PDF, rendered directly into the shell's buffer (RR5, Amendment 4).
///
/// When **reflow mode** is on (ADR-INKREAD-0011), rendering, page count, selection, search, and the
/// typesetting setters are served by the [`ReflowView`] over the document's reconstructed text
/// instead of the fixed pdfium page; toggling back restores the fixed-layout path.
pub struct PdfBackend {
    document: PdfDocument<'static>,
    /// Reflow view over the reconstructed text, `Some` while reflow mode is on. Interior mutability
    /// so the `&self` render/setter paths can build/repaginate it.
    reflow: RefCell<Option<ReflowView>>,
    /// The last viewport (panel) size rendered at — the initial pagination guess when reflow is
    /// enabled, so the position-preserving jump lands correctly (the first render then confirms it).
    last_viewport: Cell<(u32, u32)>,
}

impl PdfBackend {
    /// Open a PDF from in-memory bytes (the shell reads the file; the core never names a
    /// path scheme). DRM/corrupt files return a typed error, never a panic (RR7-FR5, RR21-FR3).
    pub fn open(bytes: Vec<u8>) -> CoreResult<Self> {
        let pdfium = pdfium()?;
        let document = pdfium
            .load_pdf_from_byte_vec(bytes, None)
            .map_err(map_load_error)?;
        Ok(Self {
            document,
            reflow: RefCell::new(None),
            last_viewport: Cell::new((0, 0)),
        })
    }

    /// The number of source (fixed-layout) pages in the document.
    fn source_page_count(&self) -> usize {
        self.document.pages().len() as usize
    }

    /// Extract a source page's glyphs for reconstruction directly from pdfium, in **aspect-correct
    /// point space**, y-down, carrying the per-char **bold** flag. Point space (not the normalized
    /// `[0,1]` `page_chars` returns) keeps reconstruction's per-axis thresholds in one physical scale;
    /// bold lets it pick out body-sized bold section headings. See `inkread_pdftext`'s contract.
    fn glyphs_for_page(&self, index: usize) -> Vec<Glyph> {
        let Some(page) = i32::try_from(index)
            .ok()
            .and_then(|i| self.document.pages().get(i).ok())
        else {
            return Vec::new();
        };
        let ph = page.height().value;
        let Ok(text) = page.text() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for ch in text.chars().iter() {
            let Some(c) = ch.unicode_char() else { continue };
            let Ok(b) = ch.loose_bounds() else { continue };
            // pdfium origin is bottom-left; flip to y-down (matches page_chars).
            let (x0, x1) = (b.left().value, b.right().value);
            let (y_top, y_bottom) = (ph - b.top().value, ph - b.bottom().value);
            let bold = ch.font_is_bold_reenforced() || ch.font_weight().is_some_and(weight_is_bold);
            out.push(Glyph {
                ch: c,
                x0: x0.min(x1),
                y0: y_top.min(y_bottom),
                x1: x0.max(x1),
                y1: y_top.max(y_bottom),
                bold,
            });
        }
        out
    }

    /// Reconstruct every source page into a reflowable [`Block`] unit (ADR-0011 page-by-page). Runs
    /// the multi-page path so recurring running headers/footers and page numbers are stripped. Text
    /// only — bounded by the document's text size, like the EPUB backend holding all chapters.
    fn extract_units(&self) -> Vec<Vec<Block>> {
        let pages: Vec<Vec<Glyph>> = (0..self.source_page_count())
            .map(|i| self.glyphs_for_page(i))
            .collect();
        inkread_pdftext::reconstruct_pages(&pages)
    }

    /// Whether the PDF carries a usable text layer (sampling the first pages — a cover/blank first
    /// page is common). A pure scan has none and cannot be reflowed without OCR (out of scope).
    fn has_text_layer(&self) -> bool {
        (0..self.source_page_count().min(8)).any(|i| !self.page_chars(i).is_empty())
    }

    /// Borrow the reflow view if reflow mode is on.
    fn reflow_on(&self) -> bool {
        self.reflow.borrow().is_some()
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
    /// falls back to 1.0. (Inherent helper used by tests; the session uses [`Document::render_zoom`].)
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
        match &*self.reflow.borrow() {
            Some(v) => v.page_count(),
            None => self.source_page_count(),
        }
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

    fn export_pdf(
        &mut self,
        out_path: &str,
        page_ink: &[PageInk],
        mode: ExportMode,
    ) -> CoreResult<()> {
        let n = self.page_count();
        for pi in page_ink {
            if pi.page >= n || pi.strokes.is_empty() {
                continue;
            }
            let mut page = self.document.pages().get(pi.page as i32).map_err(|e| {
                CoreError::RenderBackend(format!("export get page {}: {e}", pi.page))
            })?;
            let pw = page.width().value; // page size in PDF points
            let ph = page.height().value;

            // Build a stroke as a PDF path object (normalized [0,1] top-left → PDF points bottom-left).
            let build_path = |s: &crate::document::ExportStroke| -> CoreResult<PdfPagePathObject> {
                let color = PdfColor::new(s.r, s.g, s.b, s.a);
                let px = |nx: f32| PdfPoints::new(nx * pw);
                let py = |ny: f32| PdfPoints::new((1.0 - ny) * ph);
                let stroke_w = PdfPoints::new((s.width * pw).max(0.5));
                // Smooth the raw freehand polyline (Chaikin corner-cutting) so the exported line
                // isn't jagged — and round the caps/joins to match the on-screen ink.
                let pts = chaikin(&s.points, 2);
                let (x0, y0) = pts[0];
                let mut path = PdfPagePathObject::new(
                    &self.document,
                    px(x0),
                    py(y0),
                    Some(color),
                    Some(stroke_w),
                    None,
                )
                .map_err(|e| CoreError::RenderBackend(format!("path obj: {e}")))?;
                for &(nx, ny) in &pts[1..] {
                    path.line_to(px(nx), py(ny))
                        .map_err(|e| CoreError::RenderBackend(format!("line_to: {e}")))?;
                }
                path.set_line_cap(PdfPageObjectLineCap::Round)
                    .map_err(|e| CoreError::RenderBackend(format!("line cap: {e}")))?;
                path.set_line_join(PdfPageObjectLineJoin::Round)
                    .map_err(|e| CoreError::RenderBackend(format!("line join: {e}")))?;
                Ok(path)
            };

            match mode {
                // Editable Ink annotation holding the page's strokes (selectable in PDF viewers).
                ExportMode::Annotations => {
                    let mut annot = page
                        .annotations_mut()
                        .create_ink_annotation()
                        .map_err(|e| CoreError::RenderBackend(format!("create ink annot: {e}")))?;
                    for s in pi.strokes.iter().filter(|s| s.points.len() >= 2) {
                        annot
                            .objects_mut()
                            .add_path_object(build_path(s)?)
                            .map_err(|e| CoreError::RenderBackend(format!("add path: {e}")))?;
                    }
                }
                // Flatten = bake the strokes straight into the page content (shows in every viewer;
                // pdfium-render 0.9.1's `flatten` feature is broken, so we add page objects instead).
                ExportMode::Flatten => {
                    for s in pi.strokes.iter().filter(|s| s.points.len() >= 2) {
                        page.objects_mut()
                            .add_path_object(build_path(s)?)
                            .map_err(|e| CoreError::RenderBackend(format!("add path: {e}")))?;
                    }
                }
            }
        }
        self.document
            .save_to_file(out_path)
            .map_err(|e| CoreError::RenderBackend(format!("save {out_path}: {e}")))
    }

    fn render_page(&self, index: usize, buf: &mut PixelBuffer<'_>) -> CoreResult<()> {
        self.last_viewport.set((buf.width(), buf.height()));
        if let Some(v) = &*self.reflow.borrow() {
            return v.render(index, buf);
        }
        // Full-page render: scale the whole page to the buffer. Amendment 3: reverse byte
        // order so pdfium emits RGBA into our RGBA buffer.
        self.render_with_config(index, buf, |w, h| {
            PdfRenderConfig::new()
                .set_target_size(w, h)
                .set_format(PdfBitmapFormat::BGRA)
                .set_reverse_byte_order(true)
        })
    }

    fn render_fit(
        &self,
        index: usize,
        buf: &mut PixelBuffer<'_>,
        mode: FitMode,
        pan_x: f32,
        pan_y: f32,
    ) -> CoreResult<()> {
        self.last_viewport.set((buf.width(), buf.height()));
        // Reflowed text already fills the viewport — fit modes don't apply.
        if let Some(v) = &*self.reflow.borrow() {
            return v.render(index, buf);
        }
        buf.fill_white();
        let bw = i32::try_from(buf.width()).unwrap_or(0);
        let bh = i32::try_from(buf.height()).unwrap_or(0);
        // The page's native aspect (points). Unknown/degenerate → fall back to the stretch render.
        let aspect = i32::try_from(index)
            .ok()
            .and_then(|i| self.document.pages().get(i).ok())
            .map(|p| {
                let (pw, ph) = (p.width().value, p.height().value);
                if pw > 0.0 && ph > 0.0 {
                    pw / ph
                } else {
                    0.0
                }
            })
            .unwrap_or(0.0);
        if aspect <= 0.0 || bw <= 0 || bh <= 0 {
            return self.render_page(index, buf);
        }

        // Render the page aspect-correct into a temp buffer, then composite it into the white page.
        let (tw, th) = fit_dims(aspect, bw, bh, mode);
        let mut tmp = vec![0u8; (tw as usize) * (th as usize) * 4];
        {
            let mut tbuf = PixelBuffer::from_rgba(&mut tmp, tw as u32, th as u32)?;
            self.render_with_config(index, &mut tbuf, |w, h| {
                PdfRenderConfig::new()
                    .set_target_size(w, h)
                    .set_format(PdfBitmapFormat::BGRA)
                    .set_reverse_byte_order(true)
            })?;
        }
        composite_centered(buf, &tmp, tw, th, pan_x, pan_y);
        Ok(())
    }

    fn content_bbox(&self, index: usize) -> Option<NormRect> {
        // Reflowed text fills the viewport with no white margins — nothing to crop.
        if self.reflow_on() {
            return None;
        }
        let page = i32::try_from(index)
            .ok()
            .and_then(|i| self.document.pages().get(i).ok())?;
        let (pw, ph) = (page.width().value, page.height().value);
        if pw <= 0.0 || ph <= 0.0 {
            return None;
        }
        // Render the page small (aspect-correct) and scan for the non-white content box.
        let aspect = pw / ph;
        let probe_w = 480i32;
        let probe_h = ((probe_w as f32 / aspect).round() as i32).clamp(1, 2000);
        let mut px = vec![0u8; (probe_w as usize) * (probe_h as usize) * 4];
        {
            let mut pbuf = PixelBuffer::from_rgba(&mut px, probe_w as u32, probe_h as u32).ok()?;
            self.render_with_config(index, &mut pbuf, |w, h| {
                PdfRenderConfig::new()
                    .set_target_size(w, h)
                    .set_format(PdfBitmapFormat::BGRA)
                    .set_reverse_byte_order(true)
            })
            .ok()?;
        }
        const INK: u8 = 235; // any channel below this counts as content (not paper white)
        let (mut minx, mut miny, mut maxx, mut maxy) = (probe_w, probe_h, -1i32, -1i32);
        for y in 0..probe_h {
            for x in 0..probe_w {
                let o = ((y * probe_w + x) * 4) as usize;
                if px[o] < INK || px[o + 1] < INK || px[o + 2] < INK {
                    minx = minx.min(x);
                    maxx = maxx.max(x);
                    miny = miny.min(y);
                    maxy = maxy.max(y);
                }
            }
        }
        if maxx < minx || maxy < miny {
            return None; // blank page
        }
        let pad = 0.01f32; // keep a hair of margin so glyph edges aren't clipped
        let x0 = (minx as f32 / probe_w as f32 - pad).clamp(0.0, 1.0);
        let y0 = (miny as f32 / probe_h as f32 - pad).clamp(0.0, 1.0);
        let x1 = ((maxx + 1) as f32 / probe_w as f32 + pad).clamp(0.0, 1.0);
        let y1 = ((maxy + 1) as f32 / probe_h as f32 + pad).clamp(0.0, 1.0);
        if x1 - x0 < 0.05 || y1 - y0 < 0.05 {
            return None; // implausibly tiny — ignore the crop
        }
        Some(NormRect { x0, y0, x1, y1 })
    }

    fn render_cropped(
        &self,
        index: usize,
        buf: &mut PixelBuffer<'_>,
        crop: NormRect,
        mode: FitMode,
        pan_x: f32,
        pan_y: f32,
    ) -> CoreResult<()> {
        self.last_viewport.set((buf.width(), buf.height()));
        // Reflowed text already fills the viewport — there is no crop region to apply.
        if let Some(v) = &*self.reflow.borrow() {
            return v.render(index, buf);
        }
        buf.fill_white();
        let bw = i32::try_from(buf.width()).unwrap_or(0);
        let bh = i32::try_from(buf.height()).unwrap_or(0);
        let page = i32::try_from(index)
            .ok()
            .and_then(|i| self.document.pages().get(i).ok());
        let (pw, ph) = match &page {
            Some(p) => (p.width().value, p.height().value),
            None => return self.render_fit(index, buf, mode, pan_x, pan_y),
        };
        let x0 = crop.x0.clamp(0.0, 1.0);
        let y0 = crop.y0.clamp(0.0, 1.0);
        let x1 = crop.x1.clamp(0.0, 1.0);
        let y1 = crop.y1.clamp(0.0, 1.0);
        let crop_w_pt = (x1 - x0) * pw;
        let crop_h_pt = (y1 - y0) * ph;
        if crop_w_pt <= 0.0 || crop_h_pt <= 0.0 || bw <= 0 || bh <= 0 {
            return self.render_fit(index, buf, mode, pan_x, pan_y);
        }
        let (tw, th) = fit_dims(crop_w_pt / crop_h_pt, bw, bh, mode);
        // Scale so the crop region becomes the temp size, then render just that window.
        let s = tw as f32 / crop_w_pt;
        let off_x = (x0 * pw * s).round() as i32;
        let off_y = (y0 * ph * s).round() as i32;
        let mut tmp = vec![0u8; (tw as usize) * (th as usize) * 4];
        {
            let mut tbuf = PixelBuffer::from_rgba(&mut tmp, tw as u32, th as u32)?;
            self.render_region(index, &mut tbuf, s, off_x, off_y)?;
        }
        composite_centered(buf, &tmp, tw, th, pan_x, pan_y);
        Ok(())
    }

    fn render_zoom(
        &self,
        index: usize,
        buf: &mut PixelBuffer<'_>,
        zoom: f32,
        offset_x: i32,
        offset_y: i32,
    ) -> CoreResult<()> {
        self.last_viewport.set((buf.width(), buf.height()));
        // Reflow has no fixed page to magnify; the reader uses font size instead of pinch-zoom.
        if let Some(v) = &*self.reflow.borrow() {
            return v.render(index, buf);
        }
        let z = if zoom.is_finite() && zoom > 0.0 {
            zoom
        } else {
            1.0
        };
        // Stretch the page independently in X and Y so the content is exactly (w·z)×(h·z) — at z=1
        // this equals render_page (stretch-to-buffer), and the buf-sized window at (offset_x,
        // offset_y) matches the session's pan math and the shell's ink overlay on BOTH axes.
        // (Uniform scale_page_by_factor made content height ≠ h·z, so the Y offset went blank;
        //  set_target_size + clip renders blank in pdfium 0.9.1 — hence per-axis scale + clip.)
        let (pw, ph) = self
            .document
            .pages()
            .get(index as i32)
            .map(|p| (p.width().value, p.height().value))
            .unwrap_or((0.0, 0.0));
        self.render_with_config(index, buf, move |w, h| {
            let fw = if pw > 0.0 { (w as f32 * z) / pw } else { z };
            let fh = if ph > 0.0 { (h as f32 * z) / ph } else { z };
            // PAN by translating the page (clip only masks, it does not pan). translate() is in
            // page points, applied before the per-axis scale, so device-px offset → points = off/f.
            // clip(0,0,w,h) is a full-bitmap mask whose only job is to enable the matrix path.
            let base = PdfRenderConfig::new()
                .scale_page_width_by_factor(fw)
                .scale_page_height_by_factor(fh);
            let panned = base
                .translate(
                    PdfPoints::new(-(offset_x as f32) / fw),
                    PdfPoints::new(-(offset_y as f32) / fh),
                )
                .unwrap_or_else(|_| {
                    PdfRenderConfig::new()
                        .scale_page_width_by_factor(fw)
                        .scale_page_height_by_factor(fh)
                });
            panned
                .clip(0, 0, w, h)
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
        match &*self.reflow.borrow() {
            Some(v) => text_select::word_at(&v.page_chars(page), x, y),
            None => text_select::word_at(&self.page_chars(page), x, y),
        }
    }

    fn text_in_rect(&self, page: usize, rect: NormRect) -> TextSelection {
        match &*self.reflow.borrow() {
            Some(v) => text_select::text_in_rect(&v.page_chars(page), rect),
            None => text_select::text_in_rect(&self.page_chars(page), rect),
        }
    }

    fn text_lines_in_rect(&self, page: usize, rect: NormRect) -> TextSelection {
        match &*self.reflow.borrow() {
            Some(v) => text_select::text_lines_in_rect(&v.page_chars(page), rect),
            None => text_select::text_lines_in_rect(&self.page_chars(page), rect),
        }
    }

    fn search_page(&self, page: usize, query: &str) -> Vec<crate::document::SearchMatch> {
        match &*self.reflow.borrow() {
            Some(v) => text_select::find_matches(&v.page_chars(page), query),
            None => text_select::find_matches(&self.page_chars(page), query),
        }
    }

    fn set_text_scale(&self, scale: f32, current_page: usize) -> Option<usize> {
        self.reflow
            .borrow()
            .as_ref()
            .map(|v| v.set_scale(scale, current_page))
    }

    fn set_line_spacing(&self, mult: f32, current_page: usize) -> Option<usize> {
        self.reflow
            .borrow()
            .as_ref()
            .map(|v| v.set_line_spacing(mult, current_page))
    }

    fn set_alignment(&self, align_code: i32, current_page: usize) -> Option<usize> {
        self.reflow
            .borrow()
            .as_ref()
            .map(|v| v.set_alignment(align_code, current_page))
    }

    fn supports_reflow(&self) -> bool {
        self.has_text_layer()
    }

    fn set_reflow(&self, on: bool, current_page: usize) -> Option<usize> {
        if on {
            // Already on → no-op jump. No text layer → can't reflow (scanned PDF needs OCR).
            if self.reflow_on() {
                return Some(current_page);
            }
            if !self.has_text_layer() {
                return None;
            }
            let units = self.extract_units();
            let (w, h) = self.last_viewport.get();
            let view = ReflowView::new(units, w, h);
            // `current_page` is a source page (fixed mode) → land on its first reflowed page.
            let target = view.unit_start_page(current_page);
            *self.reflow.borrow_mut() = Some(view);
            Some(target)
        } else {
            // Map the current reflowed page back to its source page before tearing the view down.
            let target = {
                self.reflow
                    .borrow()
                    .as_ref()
                    .map(|v| v.unit_of(current_page))
            };
            *self.reflow.borrow_mut() = None;
            Some(target.unwrap_or(current_page))
        }
    }
}

/// The aspect-preserving render size for `aspect` (w/h) inside a `bw`×`bh` buffer under [`FitMode`]
/// (RR4). `Page` contains; `Width`/`Height` fill that axis (the other may overflow). Clamped sane.
fn fit_dims(aspect: f32, bw: i32, bh: i32, mode: FitMode) -> (i32, i32) {
    let (tw, th) = match mode {
        FitMode::Page => {
            if (bw as f32 / bh as f32) > aspect {
                (((bh as f32) * aspect).round() as i32, bh)
            } else {
                (bw, ((bw as f32) / aspect).round() as i32)
            }
        }
        FitMode::Width => (bw, ((bw as f32) / aspect).round() as i32),
        FitMode::Height => (((bh as f32) * aspect).round() as i32, bh),
    };
    (tw.clamp(1, 1 << 15), th.clamp(1, 1 << 15))
}

/// Composite a `tw`×`th` RGBA `tmp` image into `buf` (RR4): centered with white letterbox when it
/// fits, panned by the normalized `pan_*` when it overflows. Shared by fit + crop renders.
fn composite_centered(
    buf: &mut PixelBuffer<'_>,
    tmp: &[u8],
    tw: i32,
    th: i32,
    pan_x: f32,
    pan_y: f32,
) {
    let bw = i32::try_from(buf.width()).unwrap_or(0);
    let bh = i32::try_from(buf.height()).unwrap_or(0);
    let (sx, dx, cw) = fit_place(tw, bw, pan_x);
    let (sy, dy, ch) = fit_place(th, bh, pan_y);
    let dst = buf.bytes_mut();
    for row in 0..ch {
        let s = (((sy + row) * tw + sx) * 4) as usize;
        let d = (((dy + row) * bw + dx) * 4) as usize;
        let n = (cw * 4) as usize;
        dst[d..d + n].copy_from_slice(&tmp[s..s + n]);
    }
}

/// Placement of a fitted dimension `fit` inside a buffer dimension `buf` (RR4 Fit). Returns
/// `(src_offset, dst_offset, count)`: **centered** with white letterbox when it fits, **panned** by
/// the normalized `pan` when it overflows. Always in-bounds for both buffers.
fn fit_place(fit: i32, buf: i32, pan: f32) -> (i32, i32, i32) {
    if fit <= buf {
        (0, (buf - fit) / 2, fit)
    } else {
        let over = fit - buf;
        let src = (pan.clamp(0.0, 1.0) * over as f32).round() as i32;
        (src.clamp(0, over), 0, buf)
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
