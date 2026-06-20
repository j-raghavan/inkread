//! Tests for [`PdfBackend`] (RR5), split out to keep `pdf.rs` focused. Included via `#[path]`
//! so `super::*` resolves to the pdf module. pdfium-dependent tests SKIP when no host
//! libpdfium is bound (host-binary-UNVERIFIED), they are never deleted.

use super::*;
use std::sync::Mutex;

// pdfium rendering is single-threaded in production (RR21: engine calls are serialized onto
// one worker thread). cargo runs tests in parallel, so concurrent renders across these tests
// can race the library and crash it; serialize them with one lock to match that contract.
static PDFIUM_SERIAL: Mutex<()> = Mutex::new(());

fn pdfium_serial() -> std::sync::MutexGuard<'static, ()> {
    PDFIUM_SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

// Whether a host libpdfium is reachable in this environment. The render/open tests are
// gated on it so a CI box without the binary skips rather than fails (recorded as
// host-binary-UNVERIFIED). They are NOT deleted — they run wherever pdfium is present.
fn host_pdfium_available() -> bool {
    pdfium().is_ok()
}

#[test]
fn missing_library_is_typed_not_panic() {
    let _s = pdfium_serial();
    // When neither the env path nor a system library resolves, binding yields a typed
    // BackendUnavailable error (never a panic) — RR21-FR3. If a library IS present in
    // this environment, the bind simply succeeds; either way, no panic.
    match pdfium() {
        Ok(_) => { /* a library is available here */ }
        Err(CoreError::BackendUnavailable(_)) => { /* expected on a bare host */ }
        Err(other) => panic!("unexpected error binding pdfium: {other}"),
    }
}

#[test]
fn open_and_render_minimal_pdf() {
    let _s = pdfium_serial();
    if !host_pdfium_available() {
        eprintln!("SKIP open_and_render_minimal_pdf: host libpdfium UNVERIFIED (no binding)");
        return;
    }
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/minimal.pdf"
    ))
    .expect("fixture present");
    let doc = PdfBackend::open(bytes).expect("open fixture");
    assert_eq!(doc.page_count(), 1);

    // Render page 0 into a small RGBA buffer; assert it doesn't stay all-white
    // (something was drawn) and the channel order produced sane RGBA.
    let (w, h) = (120u32, 160u32);
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    let mut pb = PixelBuffer::from_rgba(&mut pixels, w, h).unwrap();
    doc.render_page(0, &mut pb).expect("render page 0");

    // Every pixel must be opaque (white-fill set α=255; pdfium keeps it opaque).
    assert!(pb.bytes().chunks_exact(4).all(|p| p[3] == 0xFF));
    // The fixture draws black text on white, so at least one pixel is non-white.
    let any_ink = pb
        .bytes()
        .chunks_exact(4)
        .any(|p| p[0] < 200 || p[1] < 200 || p[2] < 200);
    assert!(
        any_ink,
        "expected some rendered content, got an all-white page"
    );
}

#[test]
fn render_out_of_range_is_typed_error() {
    let _s = pdfium_serial();
    if !host_pdfium_available() {
        eprintln!("SKIP render_out_of_range: host libpdfium UNVERIFIED");
        return;
    }
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/minimal.pdf"
    ))
    .expect("fixture present");
    let doc = PdfBackend::open(bytes).unwrap();
    let mut pixels = vec![0u8; 4 * 4 * 4];
    let mut pb = PixelBuffer::from_rgba(&mut pixels, 4, 4).unwrap();
    assert!(matches!(
        doc.render_page(99, &mut pb),
        Err(CoreError::PageOutOfRange { requested: 99, .. })
    ));
}

// RR5 / RR7-FR5 / RR21-FR3: a garbage/corrupt file opens to a typed error, never a panic.
#[test]
fn corrupt_pdf_open_is_typed_error_not_panic() {
    let _s = pdfium_serial();
    if !host_pdfium_available() {
        eprintln!("SKIP corrupt_pdf_open: host libpdfium UNVERIFIED");
        return;
    }
    // Looks like a PDF (has the header) but is not a valid one. `PdfBackend` is not
    // `Debug`, so inspect the Result without `expect_err`.
    let garbage = b"%PDF-1.7 not-a-real-pdf \x00\x01\x02 trailing junk".to_vec();
    match PdfBackend::open(garbage) {
        Err(CoreError::CorruptDocument(_)) => {}
        Err(other) => panic!("expected CorruptDocument, got {other:?}"),
        Ok(_) => panic!("garbage bytes must not open as a PDF"),
    }

    // Totally empty input is also corrupt/typed, not a panic.
    match PdfBackend::open(Vec::new()) {
        Err(CoreError::CorruptDocument(_)) => {}
        Err(other) => panic!("expected CorruptDocument for empty input, got {other:?}"),
        Ok(_) => panic!("empty input must not open as a PDF"),
    }
}

// RR7-FR5: a password-protected (encrypted) PDF is rejected as DRM-protected, no decrypt.
#[test]
fn encrypted_pdf_open_is_drm_protected() {
    let _s = pdfium_serial();
    if !host_pdfium_available() {
        eprintln!("SKIP encrypted_pdf_open: host libpdfium UNVERIFIED");
        return;
    }
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/encrypted.pdf");
    let bytes = std::fs::read(path).expect("encrypted fixture present");
    match PdfBackend::open(bytes) {
        Err(CoreError::DrmProtected) => {}
        Err(other) => panic!("expected DrmProtected, got {other:?}"),
        Ok(_) => panic!("encrypted file must not open without a password"),
    }
}

// Deviation-2 defense: first-time binding is single-flighted by INIT_LOCK; opening from
// several threads at once must be race-free (no SIGTRAP, all succeed).
#[test]
fn concurrent_open_is_race_free() {
    let _s = pdfium_serial();
    if !host_pdfium_available() {
        eprintln!("SKIP concurrent_open: host libpdfium UNVERIFIED");
        return;
    }
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/minimal.pdf");
    let bytes = std::fs::read(path).expect("fixture present");

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let b = bytes.clone();
            std::thread::spawn(move || {
                let doc = PdfBackend::open(b).expect("concurrent open");
                doc.page_count()
            })
        })
        .collect();

    for h in handles {
        assert_eq!(h.join().expect("thread joined cleanly"), 1);
    }
}

// RR5-FR3: a clipped/scaled region render succeeds and produces an opaque buffer.
#[test]
fn render_region_clips_and_scales() {
    let _s = pdfium_serial();
    if !host_pdfium_available() {
        eprintln!("SKIP render_region: host libpdfium UNVERIFIED");
        return;
    }
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/minimal.pdf"
    ))
    .expect("fixture present");
    let doc = PdfBackend::open(bytes).unwrap();
    let (w, h) = (100u32, 140u32);
    let mut px = vec![0u8; (w * h * 4) as usize];
    let mut pb = PixelBuffer::from_rgba(&mut px, w, h).unwrap();
    // Zoom 2× window at the top-left — must render without error and stay opaque.
    doc.render_region(0, &mut pb, 2.0, 0, 0)
        .expect("render region");
    assert!(pb.bytes().chunks_exact(4).all(|p| p[3] == 0xFF));
}

// RR5-FR3 / RR21-FR3: a region render of a bad page is a typed error, not a panic.
#[test]
fn render_region_out_of_range_is_typed_error() {
    let _s = pdfium_serial();
    if !host_pdfium_available() {
        eprintln!("SKIP render_region_oob: host libpdfium UNVERIFIED");
        return;
    }
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/minimal.pdf"
    ))
    .expect("fixture present");
    let doc = PdfBackend::open(bytes).unwrap();
    let mut px = vec![0u8; 4 * 4 * 4];
    let mut pb = PixelBuffer::from_rgba(&mut px, 4, 4).unwrap();
    assert!(matches!(
        doc.render_region(99, &mut pb, 1.0, 0, 0),
        Err(CoreError::PageOutOfRange { requested: 99, .. })
    ));
}

// RR5-FR2 / RR11-FR2: a PDF with no outline yields an empty TOC, never an error.
#[test]
fn toc_of_outline_less_pdf_is_empty() {
    let _s = pdfium_serial();
    if !host_pdfium_available() {
        eprintln!("SKIP toc_empty: host libpdfium UNVERIFIED");
        return;
    }
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/minimal.pdf"
    ))
    .expect("fixture present");
    let doc = PdfBackend::open(bytes).unwrap();
    assert!(doc.toc().is_empty(), "an outline-less PDF has an empty TOC");
}

// RR5-FR2 / RR11-FR2: a nested outline reads with titles + resolved page targets.
#[test]
fn toc_from_outline_fixture() {
    let _s = pdfium_serial();
    if !host_pdfium_available() {
        eprintln!("SKIP toc_outline: host libpdfium UNVERIFIED");
        return;
    }
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/outline.pdf"
    ))
    .expect("outline fixture present");
    let doc = PdfBackend::open(bytes).unwrap();
    let toc = doc.toc();
    assert_eq!(toc.len(), 2, "two top-level entries");
    assert_eq!(toc[0].title, "Chapter 1");
    assert_eq!(toc[0].target_page, Some(0));
    assert_eq!(toc[0].children.len(), 1);
    assert_eq!(toc[0].children[0].title, "Section 1.1");
    assert_eq!(toc[0].children[0].target_page, Some(1));
    assert_eq!(toc[1].title, "Chapter 2");
    assert_eq!(toc[1].target_page, Some(2));
    assert!(toc[1].children.is_empty());
}

// RR11-FR3: page links read with normalized top-left rects + internal/external targets.
#[test]
fn page_links_from_links_fixture() {
    let _s = pdfium_serial();
    if !host_pdfium_available() {
        eprintln!("SKIP page_links: host libpdfium UNVERIFIED");
        return;
    }
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/links.pdf"
    ))
    .expect("links fixture present");
    let doc = PdfBackend::open(bytes).unwrap();

    let links = doc.page_links(0);
    assert_eq!(links.len(), 2, "two links on page 1");

    // Fixture page is 612x792. link1 [100 600 300 650] -> internal page index 1.
    let internal = links
        .iter()
        .find(|l| matches!(l.target, LinkTarget::Page(_)))
        .expect("an internal link");
    assert_eq!(internal.target, LinkTarget::Page(1));
    assert!(
        (internal.x0 - 100.0 / 612.0).abs() < 1e-3,
        "x0={}",
        internal.x0
    );
    assert!(
        (internal.x1 - 300.0 / 612.0).abs() < 1e-3,
        "x1={}",
        internal.x1
    );
    assert!(
        (internal.y0 - 142.0 / 792.0).abs() < 1e-3,
        "y0={}",
        internal.y0
    ); // (792-650)
    assert!(
        (internal.y1 - 192.0 / 792.0).abs() < 1e-3,
        "y1={}",
        internal.y1
    ); // (792-600)
    assert!(
        internal.y0 < internal.y1 && internal.x0 < internal.x1,
        "top-left normalized"
    );

    // link2 [100 400 300 450] -> external URI.
    let external = links
        .iter()
        .find_map(|l| match &l.target {
            LinkTarget::Uri(u) => Some(u.clone()),
            LinkTarget::Page(_) => None,
        })
        .expect("an external link");
    assert_eq!(external, "https://example.com");

    // RR21-FR3: an out-of-range page never panics and yields no links.
    assert!(doc.page_links(999).is_empty());
}

#[test]
fn export_writes_ink_annotation_and_flatten_into_pdf() {
    let _s = pdfium_serial();
    if !host_pdfium_available() {
        eprintln!("SKIP export_writes_*: host libpdfium UNVERIFIED (no binding)");
        return;
    }
    use crate::document::{ExportMode, ExportStroke, PageInk};
    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/minimal.pdf");

    // One red diagonal stroke on page 0 (normalized coords).
    let page_ink = vec![PageInk {
        page: 0,
        strokes: vec![ExportStroke {
            points: vec![(0.2, 0.2), (0.8, 0.8)],
            r: 255,
            g: 0,
            b: 0,
            a: 255,
            width: 0.01,
        }],
    }];

    // --- Annotations mode: the reopened PDF has an annotation on page 0. ---
    let out_a = std::env::temp_dir().join("inkread_export_annot.pdf");
    let mut doc = PdfBackend::open(std::fs::read(fixture).unwrap()).unwrap();
    doc.export_pdf(out_a.to_str().unwrap(), &page_ink, ExportMode::Annotations)
        .expect("export annotations");
    drop(doc);
    let reopened = PdfBackend::open(std::fs::read(&out_a).unwrap()).expect("reopen annotated");
    let annots = reopened
        .document
        .pages()
        .get(0)
        .unwrap()
        .annotations()
        .len();
    assert!(annots >= 1, "expected >=1 annotation, got {annots}");
    drop(reopened);

    // --- Flatten mode: reopens, renders, and the page now has non-white ink baked in. ---
    let out_f = std::env::temp_dir().join("inkread_export_flat.pdf");
    let mut doc = PdfBackend::open(std::fs::read(fixture).unwrap()).unwrap();
    doc.export_pdf(out_f.to_str().unwrap(), &page_ink, ExportMode::Flatten)
        .expect("export flatten");
    drop(doc);
    let flat = PdfBackend::open(std::fs::read(&out_f).unwrap()).expect("reopen flattened");
    let (w, h) = (120u32, 160u32);
    let mut px = vec![0u8; (w * h * 4) as usize];
    let mut pb = PixelBuffer::from_rgba(&mut px, w, h).unwrap();
    flat.render_page(0, &mut pb).expect("render flattened");
    let any_ink = pb
        .bytes()
        .chunks_exact(4)
        .any(|p| p[0] < 200 || p[1] < 200 || p[2] < 200);
    assert!(any_ink, "flattened page should contain baked ink");

    let _ = std::fs::remove_file(&out_a);
    let _ = std::fs::remove_file(&out_f);
}
