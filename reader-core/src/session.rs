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
use crate::document::{Document, DocumentMetadata, PageLink, TocEntry};
use crate::error::{CoreError, CoreResult};
use crate::persistence::{BookId, ReaderStore, ReadingPosition};
use crate::policy::EinkRefreshPolicy;
use crate::render::{PixelBuffer, Viewport};
use crate::settings::SettingsSnapshot;

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
        let document = PdfBackend::open(bytes)?;
        let screen = Rect::full(viewport.width, viewport.height);
        Ok(Self {
            document: Box::new(document),
            policy: EinkRefreshPolicy::new(caps, screen),
            viewport,
            page: 0,
            store: None,
            book: None,
            caches: Caches::new(&ResourceBudget::default_supernote()),
        })
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
        let screen = Rect::full(viewport.width, viewport.height);
        Self {
            document,
            policy: EinkRefreshPolicy::new(caps, screen),
            viewport,
            page: 0,
            store: None,
            book: None,
            caches: Caches::new(&ResourceBudget::default_supernote()),
        }
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
        self.document.render_page(self.page, buf)
    }

    /// Apply a navigation gesture: move the position (clamped at the document ends), then
    /// delegate to the policy's `on_page_turn` for the refresh stream (Amendment 6).
    ///
    /// At a boundary (next on the last page, prev on the first) the position does not move,
    /// but the policy is still asked so the panel repaints consistently. Returns the
    /// command stream for the shell to execute.
    pub fn on_gesture(&mut self, gesture: Gesture) -> Vec<RefreshCommand> {
        let last = self.page_count().saturating_sub(1);
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
        let page_rect = Rect::full(self.viewport.width, self.viewport.height);
        self.policy.on_page_turn(page_rect)
    }

    /// Jump to an absolute page index, clamped to `[0, page_count)`, then delegate to the
    /// policy's `on_page_turn` for the refresh stream (RR11-FR1). Used by TOC/scrubber jumps.
    pub fn jump_to_page(&mut self, page: usize) -> Vec<RefreshCommand> {
        let last = self.page_count().saturating_sub(1);
        self.page = page.min(last);
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

    /// Navigate to a TOC entry's target page (RR11-AC1). An unresolved entry (no
    /// `target_page`) does not move and returns no refresh commands.
    pub fn jump_to_toc(&mut self, entry: &TocEntry) -> Vec<RefreshCommand> {
        match entry.target_page {
            Some(page) => self.jump_to_page(page),
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
