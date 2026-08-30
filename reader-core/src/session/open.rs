//! Opening a document, and binding it to a store (RR5, RR12-FR3, RR27).
//!
//! One `open_*` per format, each with a `_with_store` twin that also attaches the SQLite store and
//! restores the saved reading position. [`ReaderSession::assemble`] is where they meet: format
//! detection has already happened in the caller, so this is the single place a backend becomes a
//! session.

use super::*;

impl ReaderSession {
    /// Open a PDF from bytes and build a session for `caps` on `viewport` (RR1-FR3 open).
    ///
    /// The initial page is 0. The policy is sized to the viewport for the full-screen
    /// fallback / full-screen-only quirk (RR2-FR4).
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

    /// Open a CBZ comic archive from bytes and build a session for `caps` on `viewport` (#36).
    /// Fixed-layout, like PDF — the page list is the archive's image entries in reading order.
    pub fn open_cbz(
        bytes: Vec<u8>,
        caps: DeviceCapabilities,
        viewport: Viewport,
    ) -> CoreResult<Self> {
        let fingerprint = crate::persistence::identity::fingerprint(&bytes);
        let size = bytes.len() as u64;
        let document = CbzBackend::open(bytes)?;
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
        typography: Typography,
    ) -> CoreResult<Self> {
        let mut session = Self::open_epub(bytes, caps, viewport)?;
        session.attach_store(store, book, typography)?;
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
        typography: Typography,
    ) -> CoreResult<Self> {
        let mut session = Self::open_txt(bytes, caps, viewport)?;
        session.attach_store(store, book, typography)?;
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
            caches: Caches::new(&ResourceBudget::default_tablet_epd()),
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
            night: false,
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
        typography: Typography,
    ) -> CoreResult<Self> {
        let mut session = Self::open_pdf(bytes, caps, viewport)?;
        session.attach_store(store, book, typography)?;
        Ok(session)
    }

    /// Open a CBZ and attach a persistence store, resuming the saved position for `book` (#36).
    pub fn open_cbz_with_store(
        bytes: Vec<u8>,
        caps: DeviceCapabilities,
        viewport: Viewport,
        store: Arc<dyn ReaderStore>,
        book: BookId,
        typography: Typography,
    ) -> CoreResult<Self> {
        let mut session = Self::open_cbz(bytes, caps, viewport)?;
        session.attach_store(store, book, typography)?;
        Ok(session)
    }

    /// Resume the saved position for `book` (if any), apply persisted e-ink settings to the
    /// policy (RR23 ↔ RR3), and remember the store for saving.
    fn attach_store(
        &mut self,
        store: Arc<dyn ReaderStore>,
        book: BookId,
        typography: Typography,
    ) -> CoreResult<()> {
        let settings = store.load_settings()?;
        self.apply_settings(&settings, Some(&book));
        // Before the first pagination, so it can be served from the cache rather than computed
        // (#162). Keyed by content fingerprint as well as book id, so replacing the file behind a
        // book cannot serve a pagination computed for what used to be there.
        if let Some(identity) = &self.identity {
            self.document
                .set_pagination_cache(Box::new(StorePaginationCache::new(
                    store.clone(),
                    book.clone(),
                    identity.fingerprint.to_string(),
                )));
        }
        // Before anything reads a page count. A reflowable document paginates lazily, so applying
        // the typography here means the one pagination that the resume below triggers is built at
        // the settings the book will actually be read at — rather than one at the defaults, then
        // another when the shell applies the real ones (#161/#162).
        if let Some(page) = self.document.apply_typography(typography, self.page) {
            self.page = page;
        }
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
        session.attach_store(store, book, Typography::default())?;
        Ok(session)
    }
}
