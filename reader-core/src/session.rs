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
    /// pressure. The render hot path consumes these in M1a.6 (with the threading rework).
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
            clipboard: Vec::new(),
            identity,
            contrast: 0,
            fit_mode: FitMode::Page,
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
            self.page = pos.page_index.min(last);
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

    /// Persist the current reading position (RR12-FR3). A store-less session is a no-op.
    pub fn save_position(&self) -> CoreResult<()> {
        if let (Some(store), Some(book)) = (&self.store, &self.book) {
            store.save_position(book, &ReadingPosition::new(self.page, self.page_count()))?;
        }
        Ok(())
    }

    /// The bounded render + cover caches (RR24). The render hot path / shell inserts rendered
    /// pages and covers here; M1a.6 wires the render path to consult them.
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
    }

    /// Render the current page into the shell's borrowed buffer (RR4 / Amendment 5).
    ///
    /// The buffer must match the session viewport; the borrow does not outlive this call.
    pub fn render_current(&self, buf: &mut PixelBuffer<'_>) -> CoreResult<()> {
        if buf.width() != self.viewport.width || buf.height() != self.viewport.height {
            return Err(CoreError::BufferMismatch(format!(
                "buffer {}x{} != viewport {}x{}",
                buf.width(),
                buf.height(),
                self.viewport.width,
                self.viewport.height
            )));
        }
        if self.zoom <= 1.0 + 1e-3 {
            // At fit (no pinch-zoom): aspect-preserving fit per the chosen mode (RR4). PDF honors
            // it; reflowable backends fall back to a full-buffer render.
            self.document
                .render_fit(self.page, buf, self.fit_mode, self.pan_x, self.pan_y)?;
        } else {
            // Magnified view: content is buf*zoom; show a buf-sized window panned over the overscan.
            let bw = self.viewport.width as f32;
            let bh = self.viewport.height as f32;
            let off_x = (self.pan_x * bw * (self.zoom - 1.0)).round() as i32;
            let off_y = (self.pan_y * bh * (self.zoom - 1.0)).round() as i32;
            self.document
                .render_zoom(self.page, buf, self.zoom, off_x, off_y)?;
        }
        // Display enhancement (RR4): remap pixels for contrast after the backend renders.
        crate::render::contrast::apply_contrast(
            buf,
            crate::render::contrast::step_to_gamma(self.contrast),
        );
        Ok(())
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

    /// Set the reflow **text scale** (font size; `1.0` = default) for a reflowable document,
    /// repaginating and preserving the reading position by chapter (RR2-FR5). Returns `true` if the
    /// format supports reflow (EPUB); `false` for fixed-layout PDF (no change). The shell re-renders
    /// the (possibly new) current page afterward.
    pub fn set_text_scale(&mut self, scale: f32) -> bool {
        match self.document.set_text_scale(scale, self.page) {
            Some(new_page) => {
                self.page = new_page.min(self.page_count().saturating_sub(1));
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
    /// pass-through to [`Document::word_at`].
    #[must_use]
    pub fn word_at(&self, page: usize, x: f32, y: f32) -> Option<TextSelection> {
        self.document.word_at(page, x, y)
    }

    /// The text within the normalized `rect` on `page` (RR11 / drag-highlight) — a pass-through to
    /// [`Document::text_in_rect`].
    #[must_use]
    pub fn text_in_rect(&self, page: usize, rect: NormRect) -> TextSelection {
        self.document.text_in_rect(page, rect)
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

    /// Finish the in-progress stroke and autosave the page **only if it changed** (RR7-FR6 /
    /// RR20-FR2): an ink stroke that committed at least one point, or an eraser gesture that
    /// removed something. A no-op stroke/erase does not rewrite the page (saves e-ink flash + IO).
    pub fn ink_end_stroke(&mut self) -> CoreResult<()> {
        let changed = if self.active_tool.is_ink() {
            self.layer.finish_stroke().is_some()
        } else {
            self.erase_changed
        };
        if changed {
            self.autosave_ink()?;
        }
        Ok(())
    }

    /// Undo the last ink edit on the current page, autosaving if anything changed (RR6-FR3).
    pub fn ink_undo(&mut self) -> CoreResult<bool> {
        let changed = self.layer.undo();
        if changed {
            self.autosave_ink()?;
        }
        Ok(changed)
    }

    /// Redo the last undone ink edit, autosaving if anything changed (RR6-FR3).
    pub fn ink_redo(&mut self) -> CoreResult<bool> {
        let changed = self.layer.redo();
        if changed {
            self.autosave_ink()?;
        }
        Ok(changed)
    }

    /// Flush the current page's ink to the store (RR20-FR2) — an explicit save for pause/close,
    /// complementing the automatic autosave on stroke-end/undo/redo.
    pub fn save_ink(&self) -> CoreResult<()> {
        self.autosave_ink()
    }

    /// Persist the current page's layer to the store (RR20-FR2). No-op without a store.
    fn autosave_ink(&self) -> CoreResult<()> {
        if let Some(store) = &self.ink {
            store.save_page(self.page, &self.layer)?;
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
        self.autosave_ink()?; // flush the current page's edits to the sidecar first
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

    /// Swap the in-memory layer to the current page's stored ink on a page change. Any pending
    /// stroke is dropped; the load degrades safely (see [`Self::load_layer_for_page`]).
    fn load_ink_for_current_page(&mut self) {
        self.layer.cancel_stroke();
        self.layer = self.load_layer_for_page(self.page);
        // A page turn resets the view to fit (RR5-FR3): the old pan/zoom is meaningless on a new page.
        self.zoom = 1.0;
        self.pan_x = 0.0;
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
