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
}
