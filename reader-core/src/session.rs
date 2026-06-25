//! `ReaderSession` — the M0 open→render→gesture→commands round-trip (RR21, Amendment 6).
//!
//! Owns the open [`Document`], the current page position, the panel [`Viewport`], and the
//! [`EinkRefreshPolicy`]. A gesture advances/retreats the position then **delegates to the
//! policy's `on_page_turn`** so the Partial/ghost-clear-Full promotion and `partial_count`
//! stay consistent (Amendment 6 — no separately hand-rolled stream).
//!
//! The session is the object the JNI `long` handle points at (Amendment 2): created by
//! open, freed only by close. It never stores a [`PixelBuffer`] (Amendment 5): render
//! borrows the shell's buffer for one call and drops it.

use device_eink::{DeviceCapabilities, Rect, RefreshCommand, RefreshPolicy};

use std::sync::Arc;

use crate::budget::{Caches, ResourceBudget, TrimLevel};
use crate::document::fixed::PdfBackend;
use crate::document::{
    Document, DocumentMetadata, ExportMode, ExportStroke, FitMode, NormRect, PageInk, PageLink,
    TextSelection, TocEntry,
};
use crate::error::{CoreError, CoreResult};
use crate::persistence::identity::DocIdentity;
use crate::persistence::ink_store::InkStore;
use crate::persistence::sidecar::SidecarMetadata;
use crate::persistence::{BookId, ReaderStore, ReadingPosition};
use crate::policy::EinkRefreshPolicy;
use crate::render::{PixelBuffer, Viewport};
use crate::settings::SettingsSnapshot;

use inkread_ink::{
    encode_layer, select_all, select_in_polygon, selection_bounds, InkColor, InkLayer, InkPoint,
    SelectMode, Stroke, StrokeId, Tool,
};

/// Maximum pinch-zoom factor (RR5-FR3) — beyond this, e-ink legibility gains nothing.
const MAX_ZOOM: f32 = 5.0;

/// A navigation gesture (Amendment 6). The int↔enum mapping is defined **once** here and
/// documented at the JNI boundary; `nativeOnGesture` decodes an int into this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gesture {
    /// Advance to the next page.
    NextPage,
    /// Retreat to the previous page.
    PrevPage,
}

impl Gesture {
    /// Decode the wire integer code into a gesture (the single source of truth).
    ///
    /// `0 = NextPage`, `1 = PrevPage`. Unknown codes yield `None` so the boundary can
    /// surface a typed error rather than guess (RR21-FR3).
    #[must_use]
    pub fn from_code(code: i32) -> Option<Gesture> {
        match code {
            0 => Some(Gesture::NextPage),
            1 => Some(Gesture::PrevPage),
            _ => None,
        }
    }

    /// The wire integer code for this gesture (inverse of [`Self::from_code`]).
    #[must_use]
    pub fn code(self) -> i32 {
        match self {
            Gesture::NextPage => 0,
            Gesture::PrevPage => 1,
        }
    }
}

/// A reader session over one open document.
pub struct ReaderSession {
    document: Box<dyn Document>,
    policy: EinkRefreshPolicy,
    viewport: Viewport,
    page: usize,
    /// Persistence store (RR12-FR3); `None` for a store-less session (M0 / tests).
    store: Option<Arc<dyn ReaderStore>>,
    /// The book identity this session persists under (set with the store).
    book: Option<BookId>,
    /// Bounded render + cover caches under the resource budget (RR24); trimmed on memory
    /// pressure. [`Self::render_current`] serves/populates the render cache on the fit path.
    caches: Caches,
    /// The annotation store for this document's sidecar (RR10); `None` = ink not persisted.
    ink: Option<Arc<dyn InkStore>>,
    /// The current page's ink layer (RR6). Reloaded on page change; empty without ink.
    layer: InkLayer,
    /// Pinch-zoom factor (1.0 = fit, the render_page baseline) and normalized pan `[0,1]` of the
    /// off-screen overscan (RR5-FR3). Reset to fit on a page change.
    zoom: f32,
    pan_x: f32,
    pan_y: f32,
    /// The tool of the in-progress stroke — routes [`Self::ink_add_point`] (ink vs. erase).
    active_tool: Tool,
    /// Width of the in-progress ink stroke, or the erase radius for the eraser (normalized).
    active_width: f32,
    /// Whether the in-progress eraser gesture has removed anything yet — gates the autosave so a
    /// no-op erase doesn't rewrite an unchanged page (needless e-ink flash / IO).
    erase_changed: bool,
    /// When true, edits don't fsync the sidecar on every stroke-end; they mark the page dirty and
    /// the shell flushes on a trailing-edge debounce (and on pause/page-change/close). A
    /// power/flash-wear knob (the review's per-stroke-fsync finding) — **off by default** so the
    /// RR7-FR6/RR20-FR2 save-on-stroke-end durability contract holds unless the shell opts in.
    autosave_deferred: bool,
    /// In deferred mode, whether the current page has unsaved edits awaiting [`Self::flush_ink`].
    ink_dirty: bool,
    /// The page index the in-memory [`Self::layer`] belongs to. Saves target this, not `page` — so a
    /// deferred flush triggered *after* `page` has advanced (a page turn) still writes the outgoing
    /// page. Equals `page` during normal editing; updated whenever the layer is (re)loaded.
    layer_page: usize,
    /// The lasso clipboard (ADR-INKREAD-0010): strokes copied/cut from any page, held on the
    /// session so a paste can land on a **different** page (NeoReader's cross-page clipboard).
    clipboard: Vec<Stroke>,
    /// The opened document's content identity (RR10-FR6), computed from its bytes at open. `None`
    /// for a byte-less test session ([`Self::with_document`]). Used to stamp/verify the sidecar.
    identity: Option<DocIdentity>,
    /// Contrast/display-enhancement step (`0` = off; RR4 — KOReader's "Contrast"). Applied as a
    /// per-pixel remap after render so faint scans read better on e-ink.
    contrast: u8,
    /// How a fixed-layout page is fit to the viewport (RR4 — KOReader's "Fit"). Default: contain.
    fit_mode: FitMode,
    /// Auto-crop the page's white margins (RR4 — KOReader Crop = auto). `false` = full page.
    crop_auto: bool,
    /// Margin kept around the auto-crop, in 1%-of-page steps (RR4 — KOReader Margin).
    crop_margin: u8,
    /// Per-page content-bbox memo for auto-crop (recomputed when the page changes). Interior-mutable
    /// so the `&self` render path can cache the probe render.
    crop_cache: std::cell::RefCell<Option<(usize, Option<NormRect>)>>,
    /// Render quality (RR4 — KOReader): `0` = low (sub-sample), `1` = default, `2` = high
    /// (supersample then downscale → smoother e-ink text).
    render_quality: u8,
}

/// Render-quality step → render-scale factor (RR4): low `0.75×`, default `1.0×`, high `1.5×`.
fn render_quality_factor(q: u8) -> f32 {
    match q {
        0 => 0.75,
        2 => 1.5,
        _ => 1.0,
    }
}

impl ReaderSession {
    /// Open a PDF from bytes and build a session for `caps` on `viewport` (RR1-FR3 open).
    ///
    /// The initial page is 0. The policy is sized to the viewport for the full-screen
    /// fallback / Rockchip full quirk (RR2-FR4).
    pub fn open_pdf(
        bytes: Vec<u8>,
        caps: DeviceCapabilities,
        viewport: Viewport,
    ) -> CoreResult<Self> {
        // Fingerprint the bytes before they move into the backend (RR10-FR6); fill title/author
        // from the parsed metadata so the sidecar can be stamped + re-associated.
        let fingerprint = crate::persistence::identity::fingerprint(&bytes);
        let size = bytes.len() as u64;
        let document = PdfBackend::open(bytes)?;
        let meta = document.metadata();
        let identity = Some(DocIdentity {
            fingerprint,
            size,
            title: meta.title,
            author: meta.author,
        });
        Ok(Self::assemble(Box::new(document), caps, viewport, identity))
    }

    /// Open an EPUB from bytes and build a session for `caps` on `viewport` (RR2-FR5). Reflowable:
    /// the backend paginates to the viewport on open and repaginates if it changes.
    pub fn open_epub(
        bytes: Vec<u8>,
        caps: DeviceCapabilities,
        viewport: Viewport,
    ) -> CoreResult<Self> {
        let fingerprint = crate::persistence::identity::fingerprint(&bytes);
        let size = bytes.len() as u64;
        let document = crate::document::reflow::EpubBackend::open(bytes, viewport)?;
        let meta = document.metadata();
        let identity = Some(DocIdentity {
            fingerprint,
            size,
            title: meta.title,
            author: meta.author,
        });
        Ok(Self::assemble(Box::new(document), caps, viewport, identity))
    }

    /// Open an EPUB and attach a persistence store, resuming the saved position for `book`.
    pub fn open_epub_with_store(
        bytes: Vec<u8>,
        caps: DeviceCapabilities,
        viewport: Viewport,
        store: Arc<dyn ReaderStore>,
        book: BookId,
    ) -> CoreResult<Self> {
        let mut session = Self::open_epub(bytes, caps, viewport)?;
        session.attach_store(store, book)?;
        Ok(session)
    }

    /// Open a plain-text file from bytes and build a session (RR2-FR5). Reflowable like EPUB: the
    /// paragraphs are paginated to the viewport and repaginate if it (or the font size) changes.
    pub fn open_txt(
        bytes: Vec<u8>,
        caps: DeviceCapabilities,
        viewport: Viewport,
    ) -> CoreResult<Self> {
        let fingerprint = crate::persistence::identity::fingerprint(&bytes);
        let size = bytes.len() as u64;
        let document = crate::document::plain::PlainBackend::open(bytes, viewport)?;
        let meta = document.metadata();
        let identity = Some(DocIdentity {
            fingerprint,
            size,
            title: meta.title,
            author: meta.author,
        });
        Ok(Self::assemble(Box::new(document), caps, viewport, identity))
    }

    /// Open a plain-text file and attach a persistence store, resuming the saved position for `book`.
    pub fn open_txt_with_store(
        bytes: Vec<u8>,
        caps: DeviceCapabilities,
        viewport: Viewport,
        store: Arc<dyn ReaderStore>,
        book: BookId,
    ) -> CoreResult<Self> {
        let mut session = Self::open_txt(bytes, caps, viewport)?;
        session.attach_store(store, book)?;
        Ok(session)
    }

    /// The single session constructor — every `open_*`/`with_document` path routes through this so
    /// the field initialization lives in one place (initial page 0; policy sized to the viewport).
    fn assemble(
        document: Box<dyn Document>,
        caps: DeviceCapabilities,
        viewport: Viewport,
        identity: Option<DocIdentity>,
    ) -> Self {
        let screen = Rect::full(viewport.width, viewport.height);
        Self {
            document,
            policy: EinkRefreshPolicy::new(caps, screen),
            viewport,
            page: 0,
            store: None,
            book: None,
            caches: Caches::new(&ResourceBudget::default_supernote()),
            ink: None,
            layer: InkLayer::new(),
            zoom: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
            active_tool: Tool::Pen,
            active_width: 0.0,
            erase_changed: false,
            autosave_deferred: false,
            ink_dirty: false,
            layer_page: 0,
            clipboard: Vec::new(),
            identity,
            contrast: 0,
            fit_mode: FitMode::Page,
            crop_auto: false,
            crop_margin: 0,
            crop_cache: std::cell::RefCell::new(None),
            render_quality: 1,
        }
    }

    /// Open a PDF and attach a persistence store, **resuming** the saved reading position for
    /// `book` (clamped to the document range, RR12-AC3). Position is saved via
    /// [`Self::save_position`] on close/background.
    pub fn open_pdf_with_store(
        bytes: Vec<u8>,
        caps: DeviceCapabilities,
        viewport: Viewport,
        store: Arc<dyn ReaderStore>,
        book: BookId,
    ) -> CoreResult<Self> {
        let mut session = Self::open_pdf(bytes, caps, viewport)?;
        session.attach_store(store, book)?;
        Ok(session)
    }

    /// Resume the saved position for `book` (if any), apply persisted e-ink settings to the
    /// policy (RR23 ↔ RR3), and remember the store for saving.
    fn attach_store(&mut self, store: Arc<dyn ReaderStore>, book: BookId) -> CoreResult<()> {
        let settings = store.load_settings()?;
        self.apply_settings(&settings, Some(&book));
        if let Some(pos) = store.load_position(&book)? {
            let last = self.page_count().saturating_sub(1);
            // Prefer the reflow-stable pin (RR12-FR4 / #46): a saved EPUB position re-anchors to the
            // right page under the CURRENT pagination, surviving a font-size change since the last
            // open. The integer page is the fallback (fixed-layout PDF, or a position saved before
            // pins, or a malformed blob).
            self.page = pos
                .resume_blob
                .as_deref()
                .and_then(|b| std::str::from_utf8(b).ok())
                .and_then(|s| crate::position::PinPosition::from_json(s).ok())
                .and_then(|pin| self.document.pin_to_page(&pin))
                .map(|p| p.min(last))
                .unwrap_or_else(|| pos.page_index.min(last));
        }
        self.store = Some(store);
        self.book = Some(book);
        Ok(())
    }

    /// Rebuild the refresh policy from a settings snapshot for `book` — flash interval, night
    /// interval, and avoid-flashing all come from settings (RR23 ↔ RR3-FR3/FR6/FR7). The shell
    /// calls this on open and whenever a relevant setting changes.
    pub fn apply_settings(&mut self, settings: &SettingsSnapshot, book: Option<&BookId>) {
        let caps = self.policy.capabilities();
        let screen = Rect::full(self.viewport.width, self.viewport.height);
        self.policy = EinkRefreshPolicy::with_interval(caps, screen, settings.flash_interval(book))
            .with_night_interval(settings.night_flash_interval(book))
            .with_avoid_flashing(settings.avoid_flashing(book));
    }

    /// Persist the current reading position (RR12-FR3). For a reflowable document it also stores the
    /// page's reflow-stable [`PinPosition`] JSON in `resume_blob` so the next open re-anchors across
    /// a font-size change (RR12-FR4 / #46); fixed-layout PDF stores the integer page only. A
    /// store-less session is a no-op.
    pub fn save_position(&self) -> CoreResult<()> {
        if let (Some(store), Some(book)) = (&self.store, &self.book) {
            let blob = self
                .document
                .page_pin(self.page)
                .map(|pin| pin.to_json().into_bytes());
            let pos = ReadingPosition::new(self.page, self.page_count()).with_resume_blob(blob);
            store.save_position(book, &pos)?;
        }
        Ok(())
    }

    /// The `[start, end]` reflow-stable anchor pair a selection rectangle covers on `page`, for a
    /// reflowable document — the Digest/highlight locator (RR11-FR4 / #46). `None` for fixed-layout
    /// PDF or an empty selection; the caller falls back to a page anchor.
    #[must_use]
    pub fn selection_pins(
        &self,
        page: usize,
        rect: NormRect,
    ) -> Option<(crate::position::PinPosition, crate::position::PinPosition)> {
        self.document.selection_pins(page, rect)
    }

    /// The bounded render + cover caches (RR24). [`Self::render_current`] consults/fills the render
    /// cache; the shell uses the cover cache for the library grid.
    pub fn caches(&mut self) -> &mut Caches {
        &mut self.caches
    }

    /// React to platform memory pressure (`onTrimMemory`, RR24-FR3): trims the caches by
    /// severity. Always leaves the reader usable; never panics.
    pub fn on_trim_memory(&mut self, level: TrimLevel) {
        self.caches.trim(level);
    }

    /// Build a session over an arbitrary [`Document`] (used by the host harness/tests to
    /// drive the policy without a PDF backend).
    pub fn with_document(
        document: Box<dyn Document>,
        caps: DeviceCapabilities,
        viewport: Viewport,
    ) -> Self {
        Self::assemble(document, caps, viewport, None)
    }

    /// Build a session over an arbitrary [`Document`] with a persistence store, resuming the
    /// saved position for `book` (host harness/tests — drives the store path without pdfium).
    pub fn with_document_and_store(
        document: Box<dyn Document>,
        caps: DeviceCapabilities,
        viewport: Viewport,
        store: Arc<dyn ReaderStore>,
        book: BookId,
    ) -> CoreResult<Self> {
        let mut session = Self::with_document(document, caps, viewport);
        session.attach_store(store, book)?;
        Ok(session)
    }

    /// Total page count.
    #[must_use]
    pub fn page_count(&self) -> usize {
        self.document.page_count()
    }

    /// The current page index.
    #[must_use]
    pub fn current_page(&self) -> usize {
        self.page
    }

    /// The session viewport's pixel dimensions `(width, height)` — used by the JNI bridge
    /// to size the render buffer without reaching into private state.
    #[must_use]
    pub fn viewport_dims(&self) -> (u32, u32) {
        (self.viewport.width, self.viewport.height)
    }

    /// Document metadata.
    #[must_use]
    pub fn metadata(&self) -> DocumentMetadata {
        self.document.metadata()
    }

    /// Update the viewport (e.g. `surfaceChanged`/rotation, RR21-FR4); rebuilds the
    /// policy's full-screen rect. Returns nothing; the shell re-renders + re-asks for
    /// a refresh afterward.
    pub fn set_viewport(&mut self, viewport: Viewport) {
        self.viewport = viewport;
        let caps = self.policy.capabilities();
        let screen = Rect::full(viewport.width, viewport.height);
        // Preserve nothing of the partial counter on a metrics change — a fresh full is
        // expected after a viewport change anyway (RR21-FR4).
        self.policy = EinkRefreshPolicy::new(caps, screen);
        // Cached renders are sized to the old viewport and laid out for it — drop them.
        self.invalidate_render_cache();
    }

    /// Render the current page into the shell's borrowed buffer (RR4 / Amendment 5).
    ///
    /// The buffer must match the session viewport; the borrow does not outlive this call. The
    /// non-magnified page render is served from the bounded render cache when an identical
    /// `(page + view-settings)` buffer is held (RR4-FR6 / RR24) — re-rasterization is skipped on a
    /// revisit (e.g. paging back and forth). `&mut self` because a hit/insert mutates the cache.
    pub fn render_current(&mut self, buf: &mut PixelBuffer<'_>) -> CoreResult<()> {
        if buf.width() != self.viewport.width || buf.height() != self.viewport.height {
            return Err(CoreError::BufferMismatch(format!(
                "buffer {}x{} != viewport {}x{}",
                buf.width(),
                buf.height(),
                self.viewport.width,
                self.viewport.height
            )));
        }
        if self.zoom > 1.0 + 1e-3 {
            // Magnified view: content is buf*zoom; show a buf-sized window panned over the overscan.
            // (Render quality is not applied during a transient pinch-zoom.) Not cached — the pan
            // window slides continuously, so a cache would thrash without ever paying off.
            let bw = self.viewport.width as f32;
            let bh = self.viewport.height as f32;
            let off_x = (self.pan_x * bw * (self.zoom - 1.0)).round() as i32;
            let off_y = (self.pan_y * bh * (self.zoom - 1.0)).round() as i32;
            self.document
                .render_zoom(self.page, buf, self.zoom, off_x, off_y)?;
            crate::render::contrast::apply_contrast(
                buf,
                crate::render::contrast::step_to_gamma(self.contrast),
            );
            return Ok(());
        }
        // Non-magnified page render — the page-turn / revisit case. The rendered bytes are a pure
        // function of (page + view-settings): ink is composited by the shell, never baked here, and
        // page content is immutable. So an identical key may be served straight from the cache,
        // skipping the pdfium rasterization. Only the resting view (no pan) is cached; a panned fit
        // window is transient like the zoom case.
        let cacheable = self.pan_x == 0.0 && self.pan_y == 0.0;
        let key = self.render_cache_key();
        if cacheable {
            if let Some(bytes) = self.caches.render().get(&key) {
                if bytes.len() == buf.bytes().len() {
                    buf.bytes_mut().copy_from_slice(bytes);
                    return Ok(());
                }
            }
        }
        let q = render_quality_factor(self.render_quality);
        if (q - 1.0).abs() < 1e-3 {
            self.render_fit_or_crop(buf)?;
        } else {
            // Render at q× the panel resolution, then bilinear-resample down/up to the panel —
            // supersampling (high) smooths e-ink text; sub-sampling (low) is faster/softer.
            let qw = ((self.viewport.width as f32 * q).round() as u32).clamp(1, 8000);
            let qh = ((self.viewport.height as f32 * q).round() as u32).clamp(1, 8000);
            let mut tmp = vec![0u8; (qw as usize) * (qh as usize) * 4];
            {
                let mut tbuf = PixelBuffer::from_rgba(&mut tmp, qw, qh)?;
                self.render_fit_or_crop(&mut tbuf)?;
            }
            crate::render::resample::resample_bilinear(&tmp, qw, qh, buf);
        }
        // Display enhancement (RR4): remap pixels for contrast after the backend renders. The
        // cached buffer is the final displayed pixels (the key carries the contrast step), so a
        // later serve needs no re-apply.
        crate::render::contrast::apply_contrast(
            buf,
            crate::render::contrast::step_to_gamma(self.contrast),
        );
        if cacheable {
            self.caches.render().insert(key, buf.bytes().to_vec());
        }
        Ok(())
    }

    /// The render-cache key for the current page + view-settings (RR4-FR6). The pixel-pipeline axes
    /// (zoom/rotation/invert/dither/gamma) are at their non-magnified defaults — the cache is only
    /// consulted on the fit path — so only the page and the view-settings vary.
    fn render_cache_key(&self) -> crate::render::PageHash {
        crate::render::PageHash::new(
            self.page as u32,
            1.0,
            0,
            false,
            crate::render::DitherMode::None,
            1.0,
        )
        .with_view(
            self.fit_mode.code(),
            self.crop_auto,
            self.crop_margin,
            self.render_quality,
            self.contrast,
        )
    }

    /// Drop cached page renders whose content/geometry changed underneath their key — a reflow
    /// toggle, a repagination, or a viewport resize. The view-setting axes live *in* the key, so a
    /// fit/crop/quality/contrast change needs no invalidation; only a change to what a given page
    /// index renders to (or the buffer size) does.
    fn invalidate_render_cache(&mut self) {
        self.caches.render().clear();
    }

    /// Render the current page fit (or auto-cropped) into `buf` (RR4). With auto-crop on, the white
    /// margins are trimmed to the detected content box; otherwise an aspect-preserving fit. PDF
    /// honors both; reflowable backends fall back to a full-buffer render.
    fn render_fit_or_crop(&self, buf: &mut PixelBuffer<'_>) -> CoreResult<()> {
        match self
            .crop_auto
            .then(|| self.cached_crop_bbox(self.page))
            .flatten()
        {
            Some(b) => {
                let crop = self.expand_crop(b);
                self.document.render_cropped(
                    self.page,
                    buf,
                    crop,
                    self.fit_mode,
                    self.pan_x,
                    self.pan_y,
                )
            }
            None => self
                .document
                .render_fit(self.page, buf, self.fit_mode, self.pan_x, self.pan_y),
        }
    }

    /// Set the contrast/display-enhancement step (`0` = off, clamped to
    /// [`MAX_CONTRAST_STEP`](crate::render::contrast::MAX_CONTRAST_STEP)) — RR4. Re-render to apply.
    pub fn set_contrast(&mut self, step: u8) {
        self.contrast = step.min(crate::render::contrast::MAX_CONTRAST_STEP);
    }

    /// The current contrast step (`0` = off).
    #[must_use]
    pub fn contrast(&self) -> u8 {
        self.contrast
    }

    /// Set the page fit mode (RR4 — KOReader's "Fit"). Re-render to apply.
    pub fn set_fit(&mut self, mode: FitMode) {
        self.fit_mode = mode;
    }

    /// The current page fit mode.
    #[must_use]
    pub fn fit_mode(&self) -> FitMode {
        self.fit_mode
    }

    /// Enable/disable auto-crop of the page's white margins (RR4). Re-render to apply.
    pub fn set_crop_auto(&mut self, auto: bool) {
        self.crop_auto = auto;
    }

    /// Whether auto-crop is on.
    #[must_use]
    pub fn crop_auto(&self) -> bool {
        self.crop_auto
    }

    /// Set the margin kept around the auto-crop (1%-of-page steps, clamped 0..=8). Re-render to apply.
    pub fn set_crop_margin(&mut self, step: u8) {
        self.crop_margin = step.min(8);
    }

    /// The current crop margin step.
    #[must_use]
    pub fn crop_margin(&self) -> u8 {
        self.crop_margin
    }

    /// Set render quality (`0` = low, `1` = default, `2` = high; clamped) — RR4. Re-render to apply.
    pub fn set_render_quality(&mut self, q: u8) {
        self.render_quality = q.min(2);
    }

    /// The current render quality step.
    #[must_use]
    pub fn render_quality(&self) -> u8 {
        self.render_quality
    }

    /// The content bounding box for `page`, memoized per page (recomputed on a page change).
    fn cached_crop_bbox(&self, page: usize) -> Option<NormRect> {
        if let Some((p, b)) = self.crop_cache.borrow().as_ref() {
            if *p == page {
                return *b;
            }
        }
        let b = self.document.content_bbox(page);
        *self.crop_cache.borrow_mut() = Some((page, b));
        b
    }

    /// Expand a content box by the current margin (kept within the page).
    fn expand_crop(&self, b: NormRect) -> NormRect {
        let m = f32::from(self.crop_margin) * 0.01;
        NormRect {
            x0: (b.x0 - m).clamp(0.0, 1.0),
            y0: (b.y0 - m).clamp(0.0, 1.0),
            x1: (b.x1 + m).clamp(0.0, 1.0),
            y1: (b.y1 + m).clamp(0.0, 1.0),
        }
    }

    /// Set the pinch-zoom factor (clamped to `[1.0, MAX_ZOOM]`) and normalized pan `[0,1]`
    /// (RR5-FR3). The shell drives this from pinch + drag; render uses it on the next frame.
    pub fn set_zoom(&mut self, zoom: f32, pan_x: f32, pan_y: f32) {
        self.zoom = if zoom.is_finite() {
            zoom.clamp(1.0, MAX_ZOOM)
        } else {
            1.0
        };
        self.pan_x = pan_x.clamp(0.0, 1.0);
        self.pan_y = pan_y.clamp(0.0, 1.0);
    }

    /// The current zoom factor (1.0 = fit).
    #[must_use]
    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    /// The current horizontal pan `[0,1]` over the magnified overscan (0 at fit).
    #[must_use]
    pub fn pan_x(&self) -> f32 {
        self.pan_x
    }

    /// The current vertical pan `[0,1]` over the magnified overscan (0 at fit / top of page).
    #[must_use]
    pub fn pan_y(&self) -> f32 {
        self.pan_y
    }

    /// Set the reflow **text scale** (font size; `1.0` = default) for a reflowable document,
    /// repaginating and preserving the reading position by chapter (RR2-FR5). Returns `true` if the
    /// format supports reflow (EPUB); `false` for fixed-layout PDF (no change). The shell re-renders
    /// the (possibly new) current page afterward.
    pub fn set_text_scale(&mut self, scale: f32) -> bool {
        match self.document.set_text_scale(scale, self.page) {
            Some(new_page) => {
                self.page = new_page.min(self.page_count().saturating_sub(1));
                self.invalidate_render_cache(); // repagination changes what each page index renders
                self.load_ink_for_current_page();
                true
            }
            None => false,
        }
    }

    /// Set the reflow line-spacing multiplier (RR4); repaginates EPUB preserving the chapter.
    /// `false` for a fixed-layout PDF. Re-render after.
    pub fn set_line_spacing(&mut self, mult: f32) -> bool {
        match self.document.set_line_spacing(mult, self.page) {
            Some(new_page) => {
                self.page = new_page.min(self.page_count().saturating_sub(1));
                self.invalidate_render_cache(); // repagination changes what each page index renders
                self.load_ink_for_current_page();
                true
            }
            None => false,
        }
    }

    /// Set the reflow alignment (`0=Left,1=Justify,2=Center,3=Right`; RR4); repaginates EPUB
    /// preserving the chapter. `false` for a fixed-layout PDF. Re-render after.
    pub fn set_alignment(&mut self, align_code: i32) -> bool {
        match self.document.set_alignment(align_code, self.page) {
            Some(new_page) => {
                self.page = new_page.min(self.page_count().saturating_sub(1));
                self.invalidate_render_cache(); // repagination changes what each page index renders
                self.load_ink_for_current_page();
                true
            }
            None => false,
        }
    }

    /// Whether the open document can be **reflowed** (ADR-INKREAD-0011) — a text-layer PDF. The
    /// shell uses this to enable/disable the Reflow control (disabled for scanned PDFs / EPUB).
    #[must_use]
    pub fn supports_reflow(&self) -> bool {
        self.document.supports_reflow()
    }

    /// Toggle **reflow mode** on the open PDF (ADR-INKREAD-0011): reconstructs the text and flows it
    /// like a book so the font-size/line-spacing/alignment controls take effect; toggling off
    /// restores the fixed page. Preserves the reading position across the changing page count and
    /// invalidates the (now stale-keyed) crop cache. Returns `true` if the toggle applied, `false`
    /// if reflow is unavailable (no text layer / unsupported format). Re-render after.
    pub fn set_reflow(&mut self, on: bool) -> bool {
        match self.document.set_reflow(on, self.page) {
            Some(new_page) => {
                self.page = new_page.min(self.page_count().saturating_sub(1));
                *self.crop_cache.borrow_mut() = None; // page indices change meaning across the toggle
                self.invalidate_render_cache(); // ...so do the cached page renders
                                                // A reflowed view is never magnified (zoom is fixed-layout only, RR25-FR3). Drop a
                                                // zoomed fixed-layout view to fit BEFORE the load (whose page-turn preserve would
                                                // otherwise carry zoom > 1 into reflow mode — keeping the render off the cached fit
                                                // path on every reflowed turn) (#52 review).
                self.zoom = 1.0;
                self.pan_x = 0.0;
                self.pan_y = 0.0;
                self.load_ink_for_current_page();
                true
            }
            None => false,
        }
    }

    /// Apply a navigation gesture: move the position (clamped at the document ends), then
    /// delegate to the policy's `on_page_turn` for the refresh stream (Amendment 6).
    ///
    /// At a boundary (next on the last page, prev on the first) the position does not move,
    /// but the policy is still asked so the panel repaints consistently. Returns the
    /// command stream for the shell to execute.
    pub fn on_gesture(&mut self, gesture: Gesture) -> Vec<RefreshCommand> {
        let last = self.page_count().saturating_sub(1);
        let prev = self.page;
        match gesture {
            Gesture::NextPage => {
                if self.page < last {
                    self.page += 1;
                }
            }
            Gesture::PrevPage => {
                self.page = self.page.saturating_sub(1);
            }
        }
        if self.page != prev {
            self.load_ink_for_current_page();
        }
        let page_rect = Rect::full(self.viewport.width, self.viewport.height);
        self.policy.on_page_turn(page_rect)
    }

    /// Jump to an absolute page index, clamped to `[0, page_count)`, then delegate to the
    /// policy's `on_page_turn` for the refresh stream (RR11-FR1). Used by TOC/scrubber jumps.
    pub fn jump_to_page(&mut self, page: usize) -> Vec<RefreshCommand> {
        let last = self.page_count().saturating_sub(1);
        let prev = self.page;
        self.page = page.min(last);
        if self.page != prev {
            self.load_ink_for_current_page();
        }
        let page_rect = Rect::full(self.viewport.width, self.viewport.height);
        self.policy.on_page_turn(page_rect)
    }

    /// The document outline (RR11-FR2), a pass-through to [`Document::toc`].
    #[must_use]
    pub fn toc(&self) -> Vec<TocEntry> {
        self.document.toc()
    }

    /// The clickable links on `page`, normalized to the rendered page (RR11-FR3) — a
    /// pass-through to [`Document::page_links`]. The shell hit-tests a tap against these.
    #[must_use]
    pub fn page_links(&self, page: usize) -> Vec<PageLink> {
        self.document.page_links(page)
    }

    /// The word under the normalized point `(x, y)` on `page` (RR11 / dictionary tap) — a
    /// pass-through to [`Document::word_at`]. The shell speaks **viewport-normalized** coords (where
    /// it renders + reads touch); the text layer speaks **page-normalized** coords. When the page is
    /// letterboxed in the viewport these differ, so map the input down to page space and the result
    /// boxes back up to viewport space (RR11 — see [`Self::view_transform`]).
    #[must_use]
    pub fn word_at(&self, page: usize, x: f32, y: f32) -> Option<TextSelection> {
        match self.view_transform() {
            Some(t) => {
                let (px, py) = view_to_page_pt((x, y), t);
                self.document
                    .word_at(page, px, py)
                    .map(|s| map_selection_to_view(s, t))
            }
            None => self.document.word_at(page, x, y),
        }
    }

    /// The text within the normalized `rect` on `page` (RR11 / drag-highlight) — viewport↔page mapped
    /// like [`Self::word_at`].
    #[must_use]
    pub fn text_in_rect(&self, page: usize, rect: NormRect) -> TextSelection {
        match self.view_transform() {
            Some(t) => map_selection_to_view(
                self.document.text_in_rect(page, view_to_page_rect(rect, t)),
                t,
            ),
            None => self.document.text_in_rect(page, rect),
        }
    }

    /// Reading-order selection a drag sweeps from `start` to `end` on `page` (RR11 / multi-line
    /// drag) — viewport↔page mapped like [`Self::word_at`].
    #[must_use]
    pub fn text_line_span(&self, page: usize, start: (f32, f32), end: (f32, f32)) -> TextSelection {
        match self.view_transform() {
            Some(t) => map_selection_to_view(
                self.document.text_line_span(
                    page,
                    view_to_page_pt(start, t),
                    view_to_page_pt(end, t),
                ),
                t,
            ),
            None => self.document.text_line_span(page, start, end),
        }
    }

    /// The page→viewport affine `(sx, ox, sy, oy)` for the current fit render (RR11), or `None` when
    /// text coords already equal viewport coords. Returns `None` for the render paths this fit map
    /// doesn't model — pinch-zoom (`zoom > 1`, uses `render_zoom`) and auto-crop (uses
    /// `render_cropped`) — so those fall back to the untransformed pass-through.
    fn view_transform(&self) -> Option<(f32, f32, f32, f32)> {
        // Pinch-zoom renders via render_zoom (different geometry) — skip; fit + auto-crop are both
        // handled by passing the active crop region (matching render_fit_or_crop's choice).
        if self.zoom > 1.0 + 1e-3 {
            return None;
        }
        let crop = if self.crop_auto {
            self.cached_crop_bbox(self.page)
                .map(|b| self.expand_crop(b))
        } else {
            None
        };
        self.document.page_fit_transform(
            self.page,
            self.viewport.width,
            self.viewport.height,
            self.fit_mode,
            self.pan_x,
            self.pan_y,
            crop,
        )
    }

    /// Find `query` on `page` (RR2 in-document search) — a pass-through to
    /// [`Document::search_page`]. The shell drives the scan page-by-page so it stays memory-bounded.
    #[must_use]
    pub fn search_page(&self, page: usize, query: &str) -> Vec<crate::document::SearchMatch> {
        self.document.search_page(page, query)
    }

    /// Navigate to a TOC entry's target page (RR11-AC1). An unresolved entry (no
    /// `target_page`) does not move and returns no refresh commands.
    pub fn jump_to_toc(&mut self, entry: &TocEntry) -> Vec<RefreshCommand> {
        match entry.target_page {
            Some(page) => self.jump_to_page(page),
            None => Vec::new(),
        }
    }

    // ===== Ink annotation lifecycle (RR6/RR7/RR10/RR20) =====

    /// Attach an annotation [`InkStore`], **verify/stamp** the sidecar against the document's
    /// identity (RR10-FR6/AC3), then load the current page's ink (RR7-FR7). A corrupt landing page
    /// degrades to empty rather than blocking open — consistent with a page turn.
    pub fn attach_ink_store(&mut self, store: Arc<dyn InkStore>) -> CoreResult<()> {
        self.ink = Some(store);
        self.verify_or_stamp_identity();
        self.layer = self.load_layer_for_page(self.page);
        self.layer_page = self.page;
        Ok(())
    }

    /// Reconcile the attached sidecar with the open document's identity (RR10-AC3): stamp a fresh
    /// `metadata.json` if absent; if it belongs to a *different* document (same path, different
    /// content) move the stale ink aside and re-stamp, so the open document never adopts foreign
    /// strokes. Best-effort — a write failure here resurfaces on the first real autosave.
    fn verify_or_stamp_identity(&self) {
        let (Some(store), Some(id)) = (&self.ink, &self.identity) else {
            return;
        };
        match store.load_metadata() {
            Ok(Some(meta)) if meta.matches(id) => {} // ours — adopt the existing ink
            Ok(Some(_)) => {
                // Same path, different document: preserve the stale ink and start clean.
                let _ = store.reset_stale_annotations();
                let _ = store.save_metadata(&SidecarMetadata::from_identity(id, self.page_count()));
            }
            _ => {
                // Fresh or unreadable metadata → (re)stamp this document's identity.
                let _ = store.save_metadata(&SidecarMetadata::from_identity(id, self.page_count()));
            }
        }
    }

    /// The current page's committed strokes (RR6).
    #[must_use]
    pub fn ink_strokes(&self) -> &[Stroke] {
        self.layer.strokes()
    }

    /// `.inkbin` bytes for `page` — the open page's live layer, else loaded from the store
    /// (RR7-AC1). The shell decodes these with the same `inkread-ink` codec.
    pub fn ink_strokes_wire(&self, page: usize) -> CoreResult<Vec<u8>> {
        if page == self.page {
            Ok(encode_layer(&self.layer))
        } else if let Some(store) = &self.ink {
            Ok(encode_layer(&store.load_page(page)?))
        } else {
            Ok(encode_layer(&InkLayer::new()))
        }
    }

    /// Draw-wire bytes for `page` (ADR-INKREAD-0010): the open page's live layer, else loaded from
    /// the store. Carries per-stroke id + tool/color/width/path so the shell can bake the strokes
    /// **and** pass selected ids back to the lasso ops. Decode with `WireCodec.decodeStrokes`.
    pub fn ink_draw_wire(&self, page: usize) -> CoreResult<Vec<u8>> {
        if page == self.page {
            Ok(crate::ink_wire::encode_strokes_draw_wire(&self.layer))
        } else if let Some(store) = &self.ink {
            Ok(crate::ink_wire::encode_strokes_draw_wire(
                &store.load_page(page)?,
            ))
        } else {
            Ok(crate::ink_wire::encode_strokes_draw_wire(&InkLayer::new()))
        }
    }

    /// Begin a stroke (RR6). Pen/Highlighter accumulate points; Eraser removes strokes under each
    /// subsequent point. `width` is the stroke width (ink) or the erase radius (eraser).
    pub fn ink_begin_stroke(
        &mut self,
        tool: Tool,
        color: InkColor,
        width: f32,
        created_at_ms: u64,
    ) -> CoreResult<()> {
        if tool == Tool::Eraser && (!width.is_finite() || width <= 0.0) {
            return Err(CoreError::InvalidArgument(format!(
                "eraser radius must be finite and positive, got {width}"
            )));
        }
        self.active_tool = tool;
        self.active_width = width;
        self.erase_changed = false;
        if tool.is_ink() {
            self.layer.start_stroke(tool, color, width, created_at_ms)?;
        } else {
            self.layer.cancel_stroke();
        }
        Ok(())
    }

    /// Add a sample to the in-progress stroke (ink) or erase at the point (eraser) — RR6-FR5.
    pub fn ink_add_point(
        &mut self,
        x: f32,
        y: f32,
        pressure: f32,
        tilt_x: Option<f32>,
        tilt_y: Option<f32>,
        timestamp_ms: u32,
    ) -> CoreResult<()> {
        if self.active_tool.is_ink() {
            self.layer
                .push_point(InkPoint::new(x, y, pressure, tilt_x, tilt_y, timestamp_ms)?)?;
        } else if !self.layer.erase_at(x, y, self.active_width).is_empty() {
            self.erase_changed = true;
        }
        Ok(())
    }

    /// Add a whole run of samples to the in-progress stroke (or erase along them) in one call — the
    /// batched form of [`Self::ink_add_point`]. `xy` is packed `[x0, y0, x1, y1, …]`; pressure
    /// defaults to 1.0 with no tilt/timestamp (the shell's stylus-capture path supplies none). This
    /// lets the shell hand a 200-point stroke across JNI once instead of paying 200 round-trips on
    /// the RK3566 annotation hot path (the review's per-point-JNI finding). A trailing odd float is
    /// ignored; an invalid sample aborts the batch with the same error the per-point call raises.
    pub fn ink_add_points(&mut self, xy: &[f32]) -> CoreResult<()> {
        for pt in xy.chunks_exact(2) {
            self.ink_add_point(pt[0], pt[1], 1.0, None, None, 0)?;
        }
        Ok(())
    }

    /// Finish the in-progress stroke and autosave the page **only if it changed** (RR7-FR6 /
    /// RR20-FR2): an ink stroke that committed at least one point, or an eraser gesture that
    /// removed something. A no-op stroke/erase does not rewrite the page (saves e-ink flash + IO).
    pub fn ink_end_stroke(&mut self) -> CoreResult<()> {
        let changed = if self.active_tool.is_ink() {
            self.layer.finish_stroke().is_some()
        } else {
            self.erase_changed
        };
        // Consume the erase flag: once the gesture ends it's been persisted (or marked dirty), so a
        // lingering `erase_changed` must not later read as an *in-progress* erase on a page turn (#50).
        self.erase_changed = false;
        if changed {
            self.persist_after_edit()?;
        }
        Ok(())
    }

    /// Undo the last ink edit on the current page, autosaving if anything changed (RR6-FR3).
    pub fn ink_undo(&mut self) -> CoreResult<bool> {
        let changed = self.layer.undo();
        if changed {
            self.persist_after_edit()?;
        }
        Ok(changed)
    }

    /// Redo the last undone ink edit, autosaving if anything changed (RR6-FR3).
    pub fn ink_redo(&mut self) -> CoreResult<bool> {
        let changed = self.layer.redo();
        if changed {
            self.persist_after_edit()?;
        }
        Ok(changed)
    }

    /// Enable/disable **deferred autosave** (the shell's per-stroke-fsync power knob). When enabled,
    /// edits mark the page dirty instead of writing on each stroke-end; the shell is then responsible
    /// for flushing on a trailing-edge debounce (and the session itself flushes on page-change /
    /// export / explicit [`Self::save_ink`]). Switching back to immediate mode flushes any pending
    /// edit so nothing is left unsaved.
    pub fn set_autosave_deferred(&mut self, deferred: bool) -> CoreResult<()> {
        if !deferred && self.ink_dirty {
            self.flush_ink()?;
        }
        self.autosave_deferred = deferred;
        Ok(())
    }

    /// Persist after an edit: write now (immediate mode), or just mark the page dirty (deferred
    /// mode) so the shell's debounced [`Self::save_ink`] coalesces the fsync.
    fn persist_after_edit(&mut self) -> CoreResult<()> {
        if self.autosave_deferred {
            self.ink_dirty = true;
            Ok(())
        } else {
            self.autosave_ink()
        }
    }

    /// Flush the current page's ink to the store (RR20-FR2) — an explicit save for pause/close and
    /// the trailing-edge flush in deferred mode, complementing the per-edit autosave.
    pub fn save_ink(&mut self) -> CoreResult<()> {
        self.flush_ink()
    }

    /// Write the current page if it has pending edits (always writes in immediate mode, where
    /// `ink_dirty` is never set), clearing the dirty flag. No-op without a store.
    fn flush_ink(&mut self) -> CoreResult<()> {
        self.autosave_ink()?;
        self.ink_dirty = false;
        Ok(())
    }

    /// Flush the outgoing page's ink on a page turn, re-issuing the write a few times so a transient
    /// failure that clears immediately (e.g. an `EINTR`-interrupted syscall) doesn't cost the user
    /// their ink. The retry is immediate (no backoff), so it does NOT help a sustained condition
    /// (`ENOSPC`, a held lock) — after a bounded number of attempts it gives up so navigation never
    /// blocks on a hard failure. Degrade-safely, RR20 / #50.
    fn flush_ink_retrying(&mut self) {
        const ATTEMPTS: u32 = 3;
        for _ in 0..ATTEMPTS {
            if self.flush_ink().is_ok() {
                return;
            }
        }
    }

    /// Persist the current page's layer to the store (RR20-FR2). No-op without a store.
    fn autosave_ink(&self) -> CoreResult<()> {
        if let Some(store) = &self.ink {
            store.save_page(self.layer_page, &self.layer)?;
        }
        Ok(())
    }

    // ===== Lasso selection over the current page's ink (ADR-INKREAD-0010) =====

    /// Select the strokes a lasso `polygon` encloses/crosses under `mode_code` (`0`=Smart,
    /// `1`=Freehand). Returns the selected stroke ids. Non-destructive (records no edit).
    pub fn ink_select_in_polygon(
        &self,
        polygon: &[(f32, f32)],
        mode_code: u8,
    ) -> CoreResult<Vec<u32>> {
        let mode = SelectMode::from_code(mode_code)
            .ok_or_else(|| CoreError::InvalidArgument(format!("unknown lasso mode {mode_code}")))?;
        Ok(ids_to_u32(&select_in_polygon(&self.layer, polygon, mode)))
    }

    /// Every stroke id on the current page (NeoReader "Select All").
    #[must_use]
    pub fn ink_select_all(&self) -> Vec<u32> {
        ids_to_u32(&select_all(&self.layer))
    }

    /// Selection bounds as `[x0, y0, x1, y1]` (normalized), or empty if the selection is empty —
    /// the anchor/dirty-rect for the floating selection toolbar.
    #[must_use]
    pub fn ink_selection_bounds(&self, ids: &[u32]) -> Vec<f32> {
        match selection_bounds(&self.layer, &u32_to_ids(ids)) {
            Some(b) => vec![b.x0, b.y0, b.x1, b.y1],
            None => Vec::new(),
        }
    }

    /// Move the selection by `(dx, dy)` (clamped on-page), autosaving if anything moved (RR20-FR2).
    pub fn ink_move_selection(&mut self, ids: &[u32], dx: f32, dy: f32) -> CoreResult<bool> {
        let changed = self.layer.move_strokes(&u32_to_ids(ids), dx, dy).is_some();
        if changed {
            self.autosave_ink()?;
        }
        Ok(changed)
    }

    /// Delete the selection, autosaving if anything was removed. Returns the removed ids.
    pub fn ink_delete_selection(&mut self, ids: &[u32]) -> CoreResult<Vec<u32>> {
        let removed = self.layer.delete_strokes(&u32_to_ids(ids));
        if !removed.is_empty() {
            self.autosave_ink()?;
        }
        Ok(ids_to_u32(&removed))
    }

    /// Recolor the selection, autosaving if anything changed.
    pub fn ink_recolor_selection(&mut self, ids: &[u32], color: InkColor) -> CoreResult<bool> {
        let changed = self.layer.recolor_strokes(&u32_to_ids(ids), color);
        if changed {
            self.autosave_ink()?;
        }
        Ok(changed)
    }

    /// Copy the selection into the cross-page clipboard (non-destructive). Returns the count.
    pub fn ink_copy_selection(&mut self, ids: &[u32]) -> usize {
        self.clipboard = self.layer.copy_strokes(&u32_to_ids(ids));
        self.clipboard.len()
    }

    /// Cut = copy to the clipboard, then delete as one undoable edit. Returns the removed ids.
    pub fn ink_cut_selection(&mut self, ids: &[u32]) -> CoreResult<Vec<u32>> {
        self.clipboard = self.layer.copy_strokes(&u32_to_ids(ids));
        self.ink_delete_selection(ids)
    }

    /// Paste the clipboard onto the **current** page offset by `(dx, dy)` (NeoReader's cross-page
    /// paste), autosaving the new strokes. Returns the new ids; empty clipboard → no-op.
    pub fn ink_paste(&mut self, dx: f32, dy: f32) -> CoreResult<Vec<u32>> {
        if self.clipboard.is_empty() {
            return Ok(Vec::new());
        }
        let new_ids = self.layer.paste_strokes(&self.clipboard, dx, dy);
        if !new_ids.is_empty() {
            self.autosave_ink()?;
        }
        Ok(ids_to_u32(&new_ids))
    }

    /// Whether the clipboard holds strokes available to paste (gates the Paste control).
    #[must_use]
    pub fn ink_has_clipboard(&self) -> bool {
        !self.clipboard.is_empty()
    }

    /// Export every page's ink into the PDF at `out_path` (ADR-INKREAD-0005). `flatten` burns the
    /// ink into the page content (visible in every viewer); otherwise editable Ink annotations are
    /// written. Colours are preserved (true RGBA). Gathers all pages from the sidecar after first
    /// flushing the current page, so unsaved edits are included.
    pub fn export_pdf(&mut self, out_path: &str, flatten: bool) -> CoreResult<()> {
        validate_export_path(out_path)?; // contain the write target before touching the filesystem
        self.flush_ink()?; // flush the current page's edits to the sidecar first
        let mode = if flatten {
            ExportMode::Flatten
        } else {
            ExportMode::Annotations
        };
        let mut pages = Vec::new();
        for page in 0..self.page_count() {
            let layer = self.load_layer_for_page(page);
            if layer.strokes().is_empty() {
                continue;
            }
            let strokes = layer
                .strokes()
                .iter()
                .map(|s| ExportStroke {
                    points: s.points.iter().map(|p| (p.x, p.y)).collect(),
                    r: s.color.r,
                    g: s.color.g,
                    b: s.color.b,
                    a: s.color.a,
                    width: s.width,
                })
                .collect();
            pages.push(PageInk { page, strokes });
        }
        if pages.is_empty() {
            return Err(CoreError::RenderBackend(
                "no annotations to export".to_string(),
            ));
        }
        self.document.export_pdf(out_path, &pages, mode)
    }

    /// Swap the in-memory layer to the current page's stored ink on a page change, persisting any
    /// in-progress edit to the outgoing page first; the load degrades safely (see
    /// [`Self::load_layer_for_page`]).
    fn load_ink_for_current_page(&mut self) {
        // A page turn must not silently drop an in-progress edit (#50 — "disappearing strokes"):
        //  - a pending pen/highlighter stroke is COMMITTED to the outgoing page (not cancelled);
        //  - an in-progress eraser gesture has already mutated the layer (`erase_changed`) but isn't
        //    persisted until `ink_end_stroke` — the symmetric case — so it's flushed too;
        //  - deferred mode's pending edits (`ink_dirty`) are flushed, as before.
        // The flush saves under `layer_page` (the outgoing page), not the new one.
        let committed = self.layer.finish_stroke().is_some();
        if committed || self.erase_changed || self.ink_dirty {
            self.flush_ink_retrying();
        }
        // The outgoing layer (and its edit state) is about to be discarded: clear the per-page edit
        // flags so a flush that exhausted its retries can't leak a stale dirty bit onto the new page.
        self.erase_changed = false;
        self.ink_dirty = false;
        self.layer = self.load_layer_for_page(self.page);
        self.layer_page = self.page;
        // Preserve the reading magnification across a page turn (#52 — PDF nav responsiveness):
        // dropping a zoomed-in view back to full-page fit on every turn forced the user to re-zoom,
        // costing a second render to reach their intended view. Keep the zoom and the horizontal
        // column (pan_x), but land at the TOP of the new page (pan_y = 0) so a turn starts the same
        // column afresh. A magnified view is fixed-layout only (zoom > 1 never occurs in reflowed
        // text, RR25-FR3), so reflowed pages — always at fit — still reset cleanly.
        //
        // pan_x is a fraction of the magnified OVERSCAN, not an absolute column — so on a uniform
        // PDF it lands the same column, but if the next page is narrower or a different layout
        // (a title page, the last page) the same pan_x maps elsewhere. It stays numerically in
        // range (clamped [0,1]); only the "same column" intent is approximate across a layout
        // change. A precise mapping would need a column model the fixed-layout backend doesn't have.
        if self.zoom <= 1.0 + 1e-3 {
            self.zoom = 1.0;
            self.pan_x = 0.0;
        }
        self.pan_y = 0.0;
    }

    /// Load `page`'s ink, degrading safely so open/navigation never fails: a **corrupt** page is
    /// quarantined (its bytes preserved aside, RR20-FR1) and returns empty; a transient IO error
    /// also returns empty. The reader thus always opens and always turns.
    fn load_layer_for_page(&self, page: usize) -> InkLayer {
        let Some(store) = &self.ink else {
            return InkLayer::new();
        };
        match store.load_page(page) {
            Ok(layer) => layer,
            Err(CoreError::CorruptDocument(_)) => {
                let _ = store.quarantine_page(page);
                InkLayer::new()
            }
            Err(_) => InkLayer::new(),
        }
    }
}

/// Contain the PDF-export write target before it reaches pdfium's `save_to_file` (IR security, the
/// review's "export path lacks native containment"). The shell chooses the path with all-files
/// access, so the core can't know Android's storage roots — but it *can* reject the shapes a buggy
/// or compromised shell should never produce: a relative path, a `..` traversal component, or a
/// parent directory that doesn't already exist (export creates a file, never a directory tree).
/// This bounds "write anywhere the UID can reach via traversal" without second-guessing legitimate
/// user-chosen destinations.
fn validate_export_path(out_path: &str) -> CoreResult<()> {
    use std::path::{Component, Path};
    let bad = |why: &str| {
        Err(CoreError::InvalidArgument(format!(
            "export path {why}: {out_path}"
        )))
    };
    if out_path.is_empty() {
        return bad("is empty");
    }
    let path = Path::new(out_path);
    if !path.is_absolute() {
        return bad("must be absolute");
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return bad("must not contain `..`");
    }
    match path.parent() {
        Some(dir) if dir.is_dir() => Ok(()),
        _ => bad("parent directory does not exist"),
    }
}

/// Map a **viewport-normalized** point to **page-normalized** using the affine `(sx, ox, sy, oy)`
/// (the inverse of the page→viewport fit map). A zero scale (degenerate) leaves the axis unchanged.
fn view_to_page_pt(p: (f32, f32), t: (f32, f32, f32, f32)) -> (f32, f32) {
    let (sx, ox, sy, oy) = t;
    let px = if sx.abs() > f32::EPSILON {
        (p.0 - ox) / sx
    } else {
        p.0
    };
    let py = if sy.abs() > f32::EPSILON {
        (p.1 - oy) / sy
    } else {
        p.1
    };
    (px, py)
}

/// Map a viewport-normalized rect to page-normalized (corner-wise inverse fit map).
fn view_to_page_rect(r: NormRect, t: (f32, f32, f32, f32)) -> NormRect {
    let (x0, y0) = view_to_page_pt((r.x0, r.y0), t);
    let (x1, y1) = view_to_page_pt((r.x1, r.y1), t);
    NormRect { x0, y0, x1, y1 }
}

/// Map a page-space [`TextSelection`]'s boxes up to viewport space via the affine `(sx, ox, sy, oy)`
/// so they align with the rendered pixels; the text is unchanged.
fn map_selection_to_view(sel: TextSelection, t: (f32, f32, f32, f32)) -> TextSelection {
    let (sx, ox, sy, oy) = t;
    let boxes = sel
        .boxes
        .into_iter()
        .map(|b| NormRect {
            x0: b.x0 * sx + ox,
            y0: b.y0 * sy + oy,
            x1: b.x1 * sx + ox,
            y1: b.y1 * sy + oy,
        })
        .collect();
    TextSelection {
        text: sel.text,
        boxes,
    }
}

/// Stroke ids cross the JNI boundary as plain `u32`; these convert to/from the typed [`StrokeId`].
fn ids_to_u32(ids: &[StrokeId]) -> Vec<u32> {
    ids.iter().map(|s| s.0).collect()
}

fn u32_to_ids(ids: &[u32]) -> Vec<StrokeId> {
    ids.iter().map(|&i| StrokeId(i)).collect()
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
