//! Tests for the document-open paths (RR5, RR12-FR3, RR27).
//!
//! Every format reaches a session through its own `open_*`, and each has a `_with_store` twin that
//! additionally attaches persistence and resumes the saved position. What is worth testing is not
//! that a file parses — the backends have their own suites for that — but the three things
//! `open_*` adds on top, none of which the backend can do for itself:
//!
//! - the document is **fingerprinted before its bytes move into the backend**, so a sidecar can be
//!   re-associated with the same file later even if it was renamed;
//! - the session starts at page 0 with a policy sized to the viewport;
//! - the `_with_store` twin resumes the saved page, **clamped** to the document it actually opened.
//!
//! That last one is the one with teeth. The stored page came from a previous session, possibly of a
//! different build with a different pagination, so it can legitimately point past the end of the
//! document now — and an unclamped resume renders `PageOutOfRange` on the first frame after open.

use super::*;
use crate::persistence::sqlite::SqliteStore;
use crate::persistence::ReadingPosition;
use std::io::Write;

/// The pdfium tests skip without a bound library, exactly as `pdf_tests` does; CI fetches one so
/// they execute there. `scripts/gate.sh` warns when they are skipping.
fn pdfium_available() -> bool {
    crate::document::fixed::PdfBackend::open(minimal_pdf()).is_ok()
}

fn minimal_pdf() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/minimal.pdf"
    ))
    .expect("fixture present")
}

fn sample_epub() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/sample.epub"
    ))
    .expect("fixture present")
}

/// A one-page CBZ: a stored ZIP holding the shared JPEG fixture.
fn cbz_bytes() -> Vec<u8> {
    let jpg = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/cbz_solid.jpg"
    ))
    .expect("fixture present");
    let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zw.start_file("001.jpg", opts).unwrap();
    zw.write_all(&jpg).unwrap();
    zw.finish().unwrap().into_inner()
}

fn txt_bytes() -> Vec<u8> {
    let mut s = String::new();
    for i in 0..40 {
        s.push_str(&format!(
            "Paragraph {i}. The quick brown fox jumps over the lazy dog.\n\n"
        ));
    }
    s.into_bytes()
}

fn caps() -> DeviceCapabilities {
    DeviceCapabilities::controllable_epd()
}

fn viewport() -> Viewport {
    Viewport::new(400, 600, 226)
}

fn store() -> Arc<dyn ReaderStore> {
    Arc::new(SqliteStore::open_in_memory().unwrap())
}

// ============================ plain text ============================

#[test]
fn a_text_file_opens_at_the_first_page() {
    let s = ReaderSession::open_txt(txt_bytes(), caps(), viewport()).unwrap();
    assert!(s.page_count() > 0, "paginated to the viewport");
    assert_eq!(s.current_page(), 0);
}

/// `supports_reflow` is narrower than its name suggests, and the distinction is worth pinning
/// because it is easy to read backwards: it asks whether the shell should offer a **Reflow toggle**
/// (ADR-INKREAD-0011), which is a question only about a fixed-layout PDF with a text layer. A format
/// that is *already* reflowed — plain text, EPUB — answers `false`, because there is nothing to
/// toggle. Asserting the intuitive reading here would have pinned the opposite of the contract.
#[test]
fn an_already_reflowed_format_offers_no_reflow_toggle_and_does_not_magnify() {
    let s = ReaderSession::open_txt(txt_bytes(), caps(), viewport()).unwrap();
    assert!(
        !s.supports_reflow(),
        "text is already reflowed; there is no toggle to offer"
    );
    assert!(!s.is_magnifiable(), "and a reflowed view honours no zoom");
}

#[test]
fn an_empty_text_file_still_opens() {
    let s = ReaderSession::open_txt(Vec::new(), caps(), viewport()).unwrap();
    assert_eq!(s.current_page(), 0);
}

#[test]
fn a_text_file_with_a_store_resumes_the_saved_page() {
    let st = store();
    let book = BookId::new("txt-book").unwrap();
    let full = ReaderSession::open_txt(txt_bytes(), caps(), viewport()).unwrap();
    let last = full.page_count() - 1;
    st.save_position(&book, &ReadingPosition::new(last, full.page_count()))
        .unwrap();

    let resumed = ReaderSession::open_txt_with_store(
        txt_bytes(),
        caps(),
        viewport(),
        Arc::clone(&st),
        book,
        Typography::default(),
    )
    .unwrap();
    assert_eq!(resumed.current_page(), last, "resumed where it left off");
}

/// A position saved by an earlier build can point past the end of the document this one paginates.
/// Unclamped, the first render after open is `PageOutOfRange`.
#[test]
fn a_stale_saved_position_past_the_end_is_clamped_on_resume() {
    let st = store();
    let book = BookId::new("stale").unwrap();
    st.save_position(&book, &ReadingPosition::new(9_999, 10_000))
        .unwrap();

    let s = ReaderSession::open_txt_with_store(
        txt_bytes(),
        caps(),
        viewport(),
        Arc::clone(&st),
        book,
        Typography::default(),
    )
    .unwrap();
    assert!(
        s.current_page() < s.page_count(),
        "clamped into range, was {} of {}",
        s.current_page(),
        s.page_count()
    );
}

#[test]
fn a_book_with_no_saved_position_opens_at_the_start() {
    let s = ReaderSession::open_txt_with_store(
        txt_bytes(),
        caps(),
        viewport(),
        store(),
        BookId::new("unread").unwrap(),
        Typography::default(),
    )
    .unwrap();
    assert_eq!(s.current_page(), 0);
}

// ============================ CBZ ============================

#[test]
fn a_cbz_opens_with_its_images_as_pages() {
    let s = ReaderSession::open_cbz(cbz_bytes(), caps(), viewport()).unwrap();
    assert_eq!(s.page_count(), 1);
    assert_eq!(s.current_page(), 0);
}

#[test]
fn a_cbz_is_fixed_layout() {
    let s = ReaderSession::open_cbz(cbz_bytes(), caps(), viewport()).unwrap();
    assert!(!s.supports_reflow(), "a comic page is an image, not text");
    assert!(s.is_magnifiable(), "so it zooms");
}

#[test]
fn a_corrupt_cbz_is_a_typed_error_not_a_panic() {
    let err = ReaderSession::open_cbz(b"not a zip at all".to_vec(), caps(), viewport());
    assert!(err.is_err());
}

#[test]
fn a_cbz_with_a_store_resumes_and_clamps() {
    let st = store();
    let book = BookId::new("comic").unwrap();
    st.save_position(&book, &ReadingPosition::new(500, 900))
        .unwrap();
    let s = ReaderSession::open_cbz_with_store(
        cbz_bytes(),
        caps(),
        viewport(),
        Arc::clone(&st),
        book,
        Typography::default(),
    )
    .unwrap();
    assert_eq!(
        s.current_page(),
        0,
        "one page, so the stale 500 clamps to it"
    );
}

// ============================ EPUB ============================

#[test]
fn an_epub_opens_paginated_at_the_first_page() {
    let s = ReaderSession::open_epub(sample_epub(), caps(), viewport()).unwrap();
    assert_eq!(s.current_page(), 0);
    assert!(s.page_count() > 0, "paginated to the viewport on open");
    assert!(
        !s.supports_reflow(),
        "already reflowed — see the note on the text case"
    );
    assert!(!s.is_magnifiable());
}

#[test]
fn an_epub_with_a_store_applies_the_saved_typography_in_one_pass() {
    let s = ReaderSession::open_epub_with_store(
        sample_epub(),
        caps(),
        viewport(),
        store(),
        BookId::new("epub-book").unwrap(),
        Typography {
            scale: 1.4,
            font_id: 1,
            line_spacing: 1.3,
            align_code: 1,
            columns: 1,
            margin_pct: 6,
        },
    )
    .unwrap();
    assert!(s.page_count() > 0, "repaginated at the restored settings");
    assert_eq!(s.current_page(), 0);
}

#[test]
fn a_corrupt_epub_is_a_typed_error() {
    assert!(ReaderSession::open_epub(b"PK\x03\x04garbage".to_vec(), caps(), viewport()).is_err());
}

// ============================ PDF (needs a bound libpdfium) ============================

#[test]
fn a_pdf_opens_fixed_layout_at_the_first_page() {
    if !pdfium_available() {
        eprintln!("SKIP a_pdf_opens_fixed_layout_at_the_first_page: host libpdfium UNVERIFIED");
        return;
    }
    let s = ReaderSession::open_pdf(minimal_pdf(), caps(), viewport()).unwrap();
    assert!(s.page_count() > 0);
    assert_eq!(s.current_page(), 0);
    assert!(s.is_magnifiable(), "a fixed page zooms");
}

#[test]
fn a_pdf_with_a_store_resumes_the_saved_page() {
    if !pdfium_available() {
        eprintln!("SKIP a_pdf_with_a_store_resumes_the_saved_page: host libpdfium UNVERIFIED");
        return;
    }
    let st = store();
    let book = BookId::new("pdf-book").unwrap();
    st.save_position(&book, &ReadingPosition::new(4_242, 5_000))
        .unwrap();
    let s = ReaderSession::open_pdf_with_store(
        minimal_pdf(),
        caps(),
        viewport(),
        Arc::clone(&st),
        book,
        Typography::default(),
    )
    .unwrap();
    assert!(s.current_page() < s.page_count(), "stale position clamped");
}

#[test]
fn a_corrupt_pdf_is_a_typed_error_not_a_panic() {
    if !pdfium_available() {
        eprintln!("SKIP a_corrupt_pdf_is_a_typed_error_not_a_panic: host libpdfium UNVERIFIED");
        return;
    }
    assert!(ReaderSession::open_pdf(b"%PDF-1.7 truncated".to_vec(), caps(), viewport()).is_err());
}
