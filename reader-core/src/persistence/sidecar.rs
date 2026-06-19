//! Sidecar layout + portable `metadata.json` (RR10-FR2/FR3/FR5).
//!
//! Annotations live next to the document in a `book.inkread/` directory, never inside the
//! original file (RR10-FR1):
//! ```text
//! book.pdf
//! book.inkread/
//!   metadata.json          # identity + progress + page count (this module)
//!   annotations/
//!     page-0001.inkbin     # one .inkbin per annotated page (inkread-ink codec)
//!   exports/
//!   thumbnails/
//! ```
//! This module is **pure** — it computes paths and (de)serializes `metadata.json`. The
//! filesystem IO lives in [`super::ink_store`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::persistence::identity::DocIdentity;

/// Current `metadata.json` schema version.
pub const METADATA_VERSION: u32 = 1;

/// The set of paths that make up a document's `book.inkread/` sidecar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarPaths {
    root: PathBuf,
}

impl SidecarPaths {
    /// The sidecar for `doc_path`: `book.pdf` → sibling `book.inkread/` (RR10-FR2). Any existing
    /// extension is replaced, so `book.pdf` and `book` both map to `book.inkread`.
    #[must_use]
    pub fn for_document(doc_path: &Path) -> Self {
        Self {
            root: doc_path.with_extension("inkread"),
        }
    }

    /// Build directly from a known sidecar root (tests / a relocated sidecar).
    #[must_use]
    pub fn from_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The `book.inkread/` root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The `annotations/` subdirectory holding the per-page `.inkbin` files.
    #[must_use]
    pub fn annotations_dir(&self) -> PathBuf {
        self.root.join("annotations")
    }

    /// The `.inkbin` path for `page` (zero-based) — `annotations/page-NNNN.inkbin`.
    #[must_use]
    pub fn page_file(&self, page: usize) -> PathBuf {
        self.annotations_dir().join(page_file_name(page))
    }

    /// The `metadata.json` path.
    #[must_use]
    pub fn metadata_file(&self) -> PathBuf {
        self.root.join("metadata.json")
    }

    /// The `exports/` subdirectory (RR11 artifacts).
    #[must_use]
    pub fn exports_dir(&self) -> PathBuf {
        self.root.join("exports")
    }

    /// The `thumbnails/` subdirectory (RR17-FR5).
    #[must_use]
    pub fn thumbnails_dir(&self) -> PathBuf {
        self.root.join("thumbnails")
    }
}

/// The `.inkbin` file name for a zero-based page index. The 1-based, zero-padded form
/// (`page-0001.inkbin`) matches the spec's example layout and sorts correctly.
#[must_use]
pub fn page_file_name(page: usize) -> String {
    format!("page-{:04}.inkbin", page + 1)
}

/// Parse a zero-based page index back out of a `page-NNNN.inkbin` file name, or `None` if the
/// name doesn't match the pattern. Inverse of [`page_file_name`].
#[must_use]
pub fn parse_page_file_name(name: &str) -> Option<usize> {
    let digits = name.strip_prefix("page-")?.strip_suffix(".inkbin")?;
    let one_based: usize = digits.parse().ok()?;
    one_based.checked_sub(1)
}

/// Portable sidecar metadata (RR10-FR3/FR5). Serialized as `metadata.json` for interop; on
/// reopen it lets us verify the sidecar belongs to the opened document (RR10-AC3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidecarMetadata {
    /// Schema version (`METADATA_VERSION`).
    pub version: u32,
    /// Content fingerprint as 32-char hex (see [`DocIdentity`]).
    pub fingerprint: String,
    /// File size in bytes at the time the sidecar was written.
    pub size: u64,
    /// Document title, if known.
    #[serde(default)]
    pub title: Option<String>,
    /// Document author, if known.
    #[serde(default)]
    pub author: Option<String>,
    /// Page count at write time.
    pub page_count: usize,
}

impl SidecarMetadata {
    /// Build metadata from a document identity + page count.
    #[must_use]
    pub fn from_identity(id: &DocIdentity, page_count: usize) -> Self {
        Self {
            version: METADATA_VERSION,
            fingerprint: id.fingerprint_hex(),
            size: id.size,
            title: id.title.clone(),
            author: id.author.clone(),
            page_count,
        }
    }

    /// Whether this metadata identifies the same document as `id` (fingerprint + size match).
    #[must_use]
    pub fn matches(&self, id: &DocIdentity) -> bool {
        self.fingerprint == id.fingerprint_hex() && self.size == id.size
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::DocumentMetadata;

    fn ident() -> DocIdentity {
        DocIdentity::from_bytes(
            b"the quick brown fox",
            &DocumentMetadata {
                title: Some("Fox".into()),
                author: Some("A. Nonymous".into()),
            },
        )
    }

    #[test]
    fn sidecar_root_replaces_extension() {
        let p = SidecarPaths::for_document(Path::new("/books/war-and-peace.pdf"));
        assert_eq!(p.root(), Path::new("/books/war-and-peace.inkread"));
        // extensionless documents also map cleanly
        let q = SidecarPaths::for_document(Path::new("/books/notes"));
        assert_eq!(q.root(), Path::new("/books/notes.inkread"));
    }

    #[test]
    fn page_file_path_and_name() {
        let p = SidecarPaths::from_root("/x/book.inkread");
        assert_eq!(
            p.page_file(0),
            Path::new("/x/book.inkread/annotations/page-0001.inkbin")
        );
        assert_eq!(p.page_file(41).file_name().unwrap(), "page-0042.inkbin");
        assert_eq!(
            p.metadata_file(),
            Path::new("/x/book.inkread/metadata.json")
        );
    }

    #[test]
    fn page_name_round_trips() {
        for page in [0usize, 1, 41, 9999, 12345] {
            assert_eq!(parse_page_file_name(&page_file_name(page)), Some(page));
        }
        assert_eq!(parse_page_file_name("notes.txt"), None);
        assert_eq!(parse_page_file_name("page-0000.inkbin"), None); // 1-based: 0 is invalid
        assert_eq!(parse_page_file_name("page-abc.inkbin"), None);
    }

    #[test]
    fn metadata_json_round_trips() {
        let meta = SidecarMetadata::from_identity(&ident(), 200);
        let json = serde_json::to_string(&meta).unwrap();
        let back: SidecarMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(meta, back);
        assert!(json.contains("\"fingerprint\""));
        assert!(json.contains("\"page_count\":200"));
    }

    #[test]
    fn metadata_matches_same_document_only() {
        let id = ident();
        let meta = SidecarMetadata::from_identity(&id, 10);
        assert!(meta.matches(&id));
        let other = DocIdentity::from_bytes(b"a different document", &DocumentMetadata::default());
        assert!(!meta.matches(&other));
    }

    #[test]
    fn metadata_tolerates_missing_optional_fields() {
        // title/author absent → deserializes with None (forward/back compat).
        let json = r#"{"version":1,"fingerprint":"00","size":5,"page_count":3}"#;
        let meta: SidecarMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(meta.title, None);
        assert_eq!(meta.page_count, 3);
    }
}
