//! Persistence port + domain types (RR12, ADR Decision 4).
//!
//! The core depends only on the [`ReaderStore`] **port** (ports-and-adapters): all reading-state
//! logic is testable against an in-memory adapter, and `reader-core` stays free of any IO
//! assumption at the type level (RR1-AC3). The production adapter is [`sqlite::SqliteStore`].
//!
//! M1a persists only the **integer-page reading position** (RR12-FR3) — fixed-layout PDF has no
//! `PinPosition`; the [`ReadingPosition::resume_blob`] slot is reserved for the M2 reflow locator.

use crate::error::{CoreError, CoreResult};

/// A stable identity for a book (a content hash or path-derived id the shell supplies).
///
/// Validated at the boundary (RR21-FR3): non-empty after trim, bounded length. Used as the
/// primary key for reading position (and later annotations, RR12).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BookId(String);

impl BookId {
    /// Maximum accepted id length (bytes) — a sanity bound, not a storage limit.
    pub const MAX_LEN: usize = 512;

    /// Build a validated `BookId`, trimming surrounding whitespace.
    ///
    /// Rejects an empty (post-trim) or over-long id with [`CoreError::InvalidArgument`].
    pub fn new(id: impl Into<String>) -> CoreResult<Self> {
        let id = id.into();
        let trimmed = id.trim();
        if trimmed.is_empty() {
            return Err(CoreError::InvalidArgument(
                "book id must not be empty".into(),
            ));
        }
        if trimmed.len() > Self::MAX_LEN {
            return Err(CoreError::InvalidArgument(format!(
                "book id too long ({} > {})",
                trimmed.len(),
                Self::MAX_LEN
            )));
        }
        Ok(Self(trimmed.to_string()))
    }

    /// The id as a string slice (for use as a SQL parameter).
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A reading position for a fixed-layout document (RR12-FR3).
///
/// `page_index` is zero-based; `total` is the page count at save time. `resume_blob` is an
/// opaque, reserved slot the M2 reflow locator (PinPosition JSON) will use — `None` in M1a.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadingPosition {
    /// Zero-based current page.
    pub page_index: usize,
    /// Page count when the position was saved.
    pub total: usize,
    /// Reserved opaque resume token (M2 PinPosition); always `None` in M1a.
    pub resume_blob: Option<Vec<u8>>,
}

impl ReadingPosition {
    /// A position at `page_index` of `total` pages, with no resume blob.
    #[must_use]
    pub fn new(page_index: usize, total: usize) -> Self {
        Self {
            page_index,
            total,
            resume_blob: None,
        }
    }

    /// Human-facing progress as `"current/total"` (1-based current), e.g. `"3/12"` (RR12-FR3).
    /// An empty document reads `"0/0"`.
    #[must_use]
    pub fn progress(&self) -> String {
        if self.total == 0 {
            return "0/0".to_string();
        }
        let current = (self.page_index + 1).min(self.total);
        format!("{current}/{}", self.total)
    }
}

/// The persistence **port** (ADR Decision 4). Reading-state logic depends on this trait, not on
/// rusqlite; `Send` so the store can move onto the single engine worker thread (RR21).
///
/// M1a exposes the reading-position + schema-version surface; the settings surface (RR23) is
/// added in the settings module.
pub trait ReaderStore: Send {
    /// Load the saved reading position for `book`, or `None` if none was stored.
    fn load_position(&self, book: &BookId) -> CoreResult<Option<ReadingPosition>>;

    /// Persist (insert or replace) the reading position for `book`.
    fn save_position(&self, book: &BookId, pos: &ReadingPosition) -> CoreResult<()>;

    /// The on-disk schema version (`PRAGMA user_version`) — the migration discriminator.
    fn schema_version(&self) -> CoreResult<u32>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn book_id_validates_and_trims() {
        assert!(BookId::new("").is_err());
        assert!(BookId::new("   ").is_err());
        assert!(BookId::new("x".repeat(BookId::MAX_LEN + 1)).is_err());
        let id = BookId::new("  book-42  ").unwrap();
        assert_eq!(id.as_str(), "book-42");
        // A max-length id is accepted.
        assert!(BookId::new("y".repeat(BookId::MAX_LEN)).is_ok());
    }

    #[test]
    fn progress_formats_one_based_current_over_total() {
        assert_eq!(ReadingPosition::new(0, 12).progress(), "1/12");
        assert_eq!(ReadingPosition::new(2, 12).progress(), "3/12");
        // Current is clamped to total; an empty document is 0/0.
        assert_eq!(ReadingPosition::new(99, 5).progress(), "5/5");
        assert_eq!(ReadingPosition::new(0, 0).progress(), "0/0");
    }

    #[test]
    fn reading_position_resume_blob_defaults_none() {
        assert_eq!(ReadingPosition::new(1, 3).resume_blob, None);
    }
}
