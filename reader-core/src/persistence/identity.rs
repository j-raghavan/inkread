//! Document identity for sidecar re-association (RR10-FR6).
//!
//! A document is identified by a **content fingerprint + size** (plus title/author for display
//! and interop). The fingerprint lets a moved/renamed file re-associate with its `book.inkread/`
//! sidecar (RR10-AC3): we recompute it from the file's bytes on open and match.
//!
//! The fingerprint is **FNV-1a-128**, deliberately vendored here rather than taken from a crate
//! or `std::hash::DefaultHasher`: it must be **stable forever across builds and toolchains**
//! (a persisted identity can't change when the compiler does — `DefaultHasher`'s algorithm is
//! explicitly allowed to change). It is a content *fingerprint*, not a cryptographic hash.

use crate::document::DocumentMetadata;
use crate::error::CoreResult;
use crate::persistence::BookId;

/// FNV-1a-128 offset basis.
const FNV_OFFSET_128: u128 = 0x6c62272e07bb0142_62b821756295c58d;
/// FNV-1a-128 prime.
const FNV_PRIME_128: u128 = 0x0000000001000000_000000000000013B;

/// FNV-1a-128 content fingerprint over `bytes` — stable across builds (see module docs).
#[must_use]
pub fn fingerprint(bytes: &[u8]) -> u128 {
    let mut h = FNV_OFFSET_128;
    for &b in bytes {
        h ^= u128::from(b);
        h = h.wrapping_mul(FNV_PRIME_128);
    }
    h
}

/// The robust identity of an opened document (RR10-FR6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocIdentity {
    /// Content fingerprint (FNV-1a-128 over the file bytes).
    pub fingerprint: u128,
    /// File size in bytes.
    pub size: u64,
    /// Title metadata, if any (display/interop only).
    pub title: Option<String>,
    /// Author metadata, if any (display/interop only).
    pub author: Option<String>,
}

impl DocIdentity {
    /// Derive identity from a document's bytes + parsed metadata. The fingerprint is computed
    /// over the in-memory bytes the backend already holds (no extra IO).
    #[must_use]
    pub fn from_bytes(bytes: &[u8], meta: &DocumentMetadata) -> Self {
        Self {
            fingerprint: fingerprint(bytes),
            size: bytes.len() as u64,
            title: meta.title.clone(),
            author: meta.author.clone(),
        }
    }

    /// The fingerprint as a fixed-width 32-char lowercase hex string.
    #[must_use]
    pub fn fingerprint_hex(&self) -> String {
        format!("{:032x}", self.fingerprint)
    }

    /// The stable [`BookId`] for this document — `"<fingerprint_hex>-<size>"`. Size is folded in
    /// so two files that happen to collide on the fingerprint still differ if their sizes do.
    pub fn to_book_id(&self) -> CoreResult<BookId> {
        BookId::new(format!("{}-{}", self.fingerprint_hex(), self.size))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(title: Option<&str>, author: Option<&str>) -> DocumentMetadata {
        DocumentMetadata {
            title: title.map(str::to_string),
            author: author.map(str::to_string),
        }
    }

    #[test]
    fn same_bytes_same_identity() {
        let a = DocIdentity::from_bytes(b"hello world", &meta(Some("T"), None));
        let b = DocIdentity::from_bytes(b"hello world", &meta(Some("T"), None));
        assert_eq!(a, b);
        assert_eq!(a.to_book_id().unwrap(), b.to_book_id().unwrap());
    }

    #[test]
    fn different_bytes_different_fingerprint() {
        let a = DocIdentity::from_bytes(b"hello world", &meta(None, None));
        let b = DocIdentity::from_bytes(b"hello worlds", &meta(None, None));
        assert_ne!(a.fingerprint, b.fingerprint);
    }

    #[test]
    fn one_bit_flip_changes_fingerprint() {
        let a = fingerprint(b"page-0001");
        let b = fingerprint(b"page-0002");
        assert_ne!(a, b);
    }

    #[test]
    fn fingerprint_is_stable_and_pinned() {
        // Pin the algorithm against drift: empty input must equal the FNV offset basis (no bytes
        // processed), and the function must be deterministic. If either breaks, persisted
        // identities would silently fail to re-associate.
        assert_eq!(fingerprint(b""), FNV_OFFSET_128);
        // A pinned non-empty vector: a change here means a FNV prime/algorithm regression that
        // would silently break re-association of every already-persisted sidecar.
        assert_eq!(fingerprint(b"inkread"), 0x1e803fe25c4ff78d88226ddd13e0b86b);
        assert_eq!(DocIdentity::from_bytes(b"", &meta(None, None)).size, 0);
    }

    #[test]
    fn book_id_format_includes_size() {
        let id = DocIdentity::from_bytes(b"abc", &meta(None, None));
        let bid = id.to_book_id().unwrap();
        assert!(bid.as_str().ends_with("-3"), "size 3 folded into the id");
        assert_eq!(bid.as_str().len(), 32 + 1 + 1); // hex + '-' + "3"
    }
}
