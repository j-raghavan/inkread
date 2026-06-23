//! Document-format detection (RR5 / RR22) — pure, host-testable, no JNI or Android types.
//!
//! The open path used to choose a backend purely from the filename extension, so a file with a
//! missing extension (a `content://` temp file) or a *wrong* one (an `.epub` saved as `.pdf`)
//! opened with the wrong parser and failed confusingly. Detection now leads with the **magic
//! bytes** — which are authoritative — and only falls back to the extension when the leading bytes
//! don't positively identify a format. The whole thing is a few pure functions so the host gate
//! covers it (RR1-AC3); the JNI bridge just calls [`DocFormat::resolve`].

use std::path::Path;

/// The backends the open path can dispatch to. `Pdf` is the fixed-layout backend
/// ([`super::fixed::PdfBackend`]); `Epub` is the reflowable backend ([`super::reflow`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocFormat {
    /// Fixed-layout PDF.
    Pdf,
    /// Reflowable EPUB (a ZIP container).
    Epub,
}

/// PDF header marker (ISO 32000): a conforming file begins with `%PDF-`.
const PDF_MAGIC: &[u8] = b"%PDF-";
/// ZIP local-file-header signature. EPUB is a ZIP, so its first entry starts here.
const ZIP_MAGIC: &[u8] = b"PK\x03\x04";

impl DocFormat {
    /// Identify the format from the **leading bytes**, or `None` when they match no known format.
    ///
    /// This is the authoritative signal: a positive match here is trusted over the extension, so a
    /// mislabeled file still opens with the right backend. A ZIP container is reported as [`Self::Epub`]
    /// (the only ZIP-based backend today). PDFs that carry leading junk before `%PDF-` (tolerated by
    /// the spec) won't match here and fall back to the extension in [`Self::resolve`].
    pub(crate) fn sniff(bytes: &[u8]) -> Option<DocFormat> {
        if bytes.starts_with(PDF_MAGIC) {
            Some(DocFormat::Pdf)
        } else if bytes.starts_with(ZIP_MAGIC) {
            Some(DocFormat::Epub)
        } else {
            None
        }
    }

    /// Identify the format from the filename **extension** (case-insensitive), or `None` when the
    /// extension is absent or unrecognized.
    pub(crate) fn from_extension(path: &str) -> Option<DocFormat> {
        let ext = Path::new(path).extension()?.to_str()?;
        if ext.eq_ignore_ascii_case("pdf") {
            Some(DocFormat::Pdf)
        } else if ext.eq_ignore_ascii_case("epub") {
            Some(DocFormat::Epub)
        } else {
            None
        }
    }

    /// Resolve the backend for an opened document: trust the magic bytes first, fall back to the
    /// extension, and default to [`Self::Pdf`] (the prior behavior) when neither identifies it.
    ///
    /// Correctly-named files resolve exactly as before; the only behavior change is that a file with
    /// a missing or mismatched extension now follows its bytes instead of silently mis-opening.
    pub fn resolve(path: &str, bytes: &[u8]) -> DocFormat {
        DocFormat::sniff(bytes)
            .or_else(|| DocFormat::from_extension(path))
            .unwrap_or(DocFormat::Pdf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pdf_bytes() -> Vec<u8> {
        b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec()
    }

    fn epub_bytes() -> Vec<u8> {
        // ZIP local-file-header magic followed by arbitrary entry bytes.
        b"PK\x03\x04\x14\x00\x00\x00mimetype".to_vec()
    }

    #[test]
    fn sniff_identifies_pdf_magic() {
        assert_eq!(DocFormat::sniff(&pdf_bytes()), Some(DocFormat::Pdf));
    }

    #[test]
    fn sniff_identifies_zip_as_epub() {
        assert_eq!(DocFormat::sniff(&epub_bytes()), Some(DocFormat::Epub));
    }

    #[test]
    fn sniff_rejects_empty_short_and_unknown() {
        assert_eq!(DocFormat::sniff(b""), None);
        assert_eq!(DocFormat::sniff(b"%PD"), None); // shorter than the marker
        assert_eq!(DocFormat::sniff(b"PK\x05\x06"), None); // empty-archive sig, not a local header
        assert_eq!(DocFormat::sniff(b"not a document"), None);
    }

    #[test]
    fn from_extension_is_case_insensitive() {
        assert_eq!(DocFormat::from_extension("book.pdf"), Some(DocFormat::Pdf));
        assert_eq!(DocFormat::from_extension("book.PDF"), Some(DocFormat::Pdf));
        assert_eq!(
            DocFormat::from_extension("/a/b/Novel.EpUb"),
            Some(DocFormat::Epub)
        );
    }

    #[test]
    fn from_extension_none_when_absent_or_unknown() {
        assert_eq!(DocFormat::from_extension("noextension"), None);
        assert_eq!(DocFormat::from_extension("comic.cbz"), None);
        assert_eq!(DocFormat::from_extension(""), None);
    }

    #[test]
    fn resolve_correctly_named_files_are_unchanged() {
        assert_eq!(DocFormat::resolve("book.pdf", &pdf_bytes()), DocFormat::Pdf);
        assert_eq!(
            DocFormat::resolve("book.epub", &epub_bytes()),
            DocFormat::Epub
        );
    }

    #[test]
    fn resolve_magic_overrides_a_wrong_extension() {
        // An EPUB saved as `.pdf` — the bug this issue fixes. The bytes win.
        assert_eq!(
            DocFormat::resolve("mislabeled.pdf", &epub_bytes()),
            DocFormat::Epub
        );
        // The mirror case: a PDF saved as `.epub` resolves to PDF for the same reason.
        assert_eq!(
            DocFormat::resolve("mislabeled.epub", &pdf_bytes()),
            DocFormat::Pdf
        );
    }

    #[test]
    fn resolve_uses_magic_when_extension_is_missing() {
        assert_eq!(DocFormat::resolve("tempfile", &pdf_bytes()), DocFormat::Pdf);
        assert_eq!(
            DocFormat::resolve("tempfile", &epub_bytes()),
            DocFormat::Epub
        );
    }

    #[test]
    fn resolve_falls_back_to_extension_when_bytes_are_unrecognized() {
        // A PDF with leading junk before `%PDF-` (spec-tolerated) won't sniff; the extension saves it.
        let junk_pdf = b"\x00\x00garbage then later %PDF-1.4".to_vec();
        assert_eq!(DocFormat::sniff(&junk_pdf), None);
        assert_eq!(DocFormat::resolve("real.pdf", &junk_pdf), DocFormat::Pdf);
        assert_eq!(DocFormat::resolve("real.epub", &junk_pdf), DocFormat::Epub);
    }

    #[test]
    fn resolve_defaults_to_pdf_when_nothing_matches() {
        assert_eq!(
            DocFormat::resolve("mystery", b"unrecognized bytes"),
            DocFormat::Pdf
        );
    }
}
