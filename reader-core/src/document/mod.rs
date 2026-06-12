//! The `Document` trait — the M0 subset every format implements (RR5-FR2).
//!
//! M0 needs only metadata + page count + render-page (Amendment 4 / scope fence): no
//! `text_runs`/`toc`/`search`/`hint_page`/`page_hash` (those are M1+). The PDF backend
//! ([`fixed::PdfBackend`]) is the one implementation in M0.

pub mod fixed;

use crate::error::CoreResult;
use crate::render::PixelBuffer;

/// Document metadata (title/author) — the M0 subset (RR5-FR2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocumentMetadata {
    /// Document title, if present.
    pub title: Option<String>,
    /// Document author, if present.
    pub author: Option<String>,
}

/// The core trait every format implements (M0 subset).
///
/// Render targets a borrowed [`PixelBuffer`] (Fork 4); the backend white-fills before
/// rasterizing (RR4-FR3) and resolves the channel order so the buffer ends up RGBA
/// (Amendment 3).
pub trait Document {
    /// Total page count (fixed-layout: a trivial integer model — RR5-FR2).
    fn page_count(&self) -> usize;

    /// Title/author metadata.
    fn metadata(&self) -> DocumentMetadata;

    /// Render page `index` into `buf` at the buffer's resolution.
    ///
    /// Returns [`CoreError::PageOutOfRange`](crate::error::CoreError::PageOutOfRange) for a
    /// bad index and a typed backend error on render failure — never panics (RR21-FR3).
    fn render_page(&self, index: usize, buf: &mut PixelBuffer<'_>) -> CoreResult<()>;

    /// Prefetch hint (RR4-FR7): the core may call this after rendering the current page so a
    /// backend can warm an internal handle for the likely-next page, making a page turn blit a
    /// ready buffer. Default: a no-op (backends opt in). Must never panic on a bad index.
    fn hint_page(&self, _next: usize) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Plain;
    impl Document for Plain {
        fn page_count(&self) -> usize {
            1
        }
        fn metadata(&self) -> DocumentMetadata {
            DocumentMetadata::default()
        }
        fn render_page(&self, _index: usize, _buf: &mut PixelBuffer<'_>) -> CoreResult<()> {
            Ok(())
        }
    }

    // RR4-FR7: a backend that does not override hint_page gets the no-op default (no panic).
    #[test]
    fn hint_page_default_is_noop() {
        Plain.hint_page(0);
        Plain.hint_page(99); // out-of-range hint must not panic either
    }

    // RR4-FR7: a backend that overrides hint_page receives the requested page.
    #[test]
    fn hint_page_override_is_called() {
        use std::cell::Cell;
        struct Hinter {
            last: Cell<Option<usize>>,
        }
        impl Document for Hinter {
            fn page_count(&self) -> usize {
                3
            }
            fn metadata(&self) -> DocumentMetadata {
                DocumentMetadata::default()
            }
            fn render_page(&self, _index: usize, _buf: &mut PixelBuffer<'_>) -> CoreResult<()> {
                Ok(())
            }
            fn hint_page(&self, next: usize) {
                self.last.set(Some(next));
            }
        }
        let h = Hinter {
            last: Cell::new(None),
        };
        h.hint_page(2);
        assert_eq!(h.last.get(), Some(2));
    }
}
