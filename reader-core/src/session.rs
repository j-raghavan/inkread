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

use crate::document::fixed::PdfBackend;
use crate::document::{Document, DocumentMetadata};
use crate::error::{CoreError, CoreResult};
use crate::policy::EinkRefreshPolicy;
use crate::render::{PixelBuffer, Viewport};

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
        })
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
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::PixelBuffer;
    use device_eink::{MockDeviceRecorder, RefreshIntent};

    /// A stub document with `n` blank pages, for driving the session without pdfium.
    struct StubDoc {
        pages: usize,
    }
    impl Document for StubDoc {
        fn page_count(&self) -> usize {
            self.pages
        }
        fn metadata(&self) -> DocumentMetadata {
            DocumentMetadata {
                title: Some("stub".into()),
                author: None,
            }
        }
        fn render_page(&self, index: usize, buf: &mut PixelBuffer<'_>) -> CoreResult<()> {
            if index >= self.pages {
                return Err(CoreError::PageOutOfRange {
                    requested: index,
                    available: self.pages,
                });
            }
            buf.fill_white();
            Ok(())
        }
    }

    fn session(pages: usize, caps: DeviceCapabilities) -> ReaderSession {
        ReaderSession::with_document(
            Box::new(StubDoc { pages }),
            caps,
            Viewport::new(100, 120, 226),
        )
    }

    #[test]
    fn gesture_code_round_trips() {
        for g in [Gesture::NextPage, Gesture::PrevPage] {
            assert_eq!(Gesture::from_code(g.code()), Some(g));
        }
        assert_eq!(Gesture::from_code(2), None);
        assert_eq!(Gesture::from_code(-1), None);
    }

    #[test]
    fn next_and_prev_advance_and_clamp() {
        let mut s = session(3, DeviceCapabilities::supernote_full());
        assert_eq!(s.current_page(), 0);
        s.on_gesture(Gesture::PrevPage); // clamp at 0
        assert_eq!(s.current_page(), 0);
        s.on_gesture(Gesture::NextPage);
        assert_eq!(s.current_page(), 1);
        s.on_gesture(Gesture::NextPage);
        s.on_gesture(Gesture::NextPage); // clamp at last (2)
        assert_eq!(s.current_page(), 2);
        s.on_gesture(Gesture::PrevPage);
        assert_eq!(s.current_page(), 1);
    }

    // Amendment 6 + RR3-AC1: gestures delegate to the policy so the promotion is consistent.
    #[test]
    fn gestures_drive_the_policy_promotion() {
        let caps = DeviceCapabilities::supernote_full();
        let mut s = session(100, caps);
        let mut rec = MockDeviceRecorder::with_profile(caps);
        // Six forward turns => 5 Partial + (WaitForLast, Full).
        for _ in 0..6 {
            let cmds = s.on_gesture(Gesture::NextPage);
            rec.execute_all(cmds);
        }
        assert_eq!(rec.recorded().len(), 7);
        assert_eq!(rec.recorded()[5], RefreshCommand::WaitForLast);
        assert!(matches!(
            rec.recorded()[6],
            RefreshCommand::Update {
                intent: RefreshIntent::Full,
                ..
            }
        ));
    }

    // RR11-FR1: jump_to_page lands on an absolute page, clamped to the document range, and
    // drives the policy like a page turn.
    #[test]
    fn jump_to_page_clamps_and_drives_policy() {
        let caps = DeviceCapabilities::supernote_full();
        let mut s = session(5, caps);
        let cmds = s.jump_to_page(3);
        assert_eq!(s.current_page(), 3);
        assert!(matches!(
            cmds.as_slice(),
            [RefreshCommand::Update {
                intent: RefreshIntent::Partial,
                ..
            }]
        ));
        // Past the end clamps to the last page.
        s.jump_to_page(99);
        assert_eq!(s.current_page(), 4);
        // Page 0 is reachable.
        s.jump_to_page(0);
        assert_eq!(s.current_page(), 0);
    }

    #[test]
    fn render_rejects_mismatched_buffer() {
        let s = session(1, DeviceCapabilities::supernote_full());
        let mut wrong = vec![0u8; 10 * 10 * 4];
        let mut pb = PixelBuffer::from_rgba(&mut wrong, 10, 10).unwrap();
        assert!(matches!(
            s.render_current(&mut pb),
            Err(CoreError::BufferMismatch(_))
        ));
    }

    #[test]
    fn render_current_into_matching_buffer_ok() {
        let s = session(1, DeviceCapabilities::supernote_full());
        let mut buf = vec![0u8; 100 * 120 * 4];
        let mut pb = PixelBuffer::from_rgba(&mut buf, 100, 120).unwrap();
        assert!(s.render_current(&mut pb).is_ok());
    }

    #[test]
    fn basic_panel_session_collapses_to_full() {
        let caps = DeviceCapabilities::supernote_baseline();
        let mut s = session(10, caps);
        let cmds = s.on_gesture(Gesture::NextPage);
        assert!(matches!(
            cmds.as_slice(),
            [RefreshCommand::Update {
                intent: RefreshIntent::Full,
                ..
            }]
        ));
    }
}
