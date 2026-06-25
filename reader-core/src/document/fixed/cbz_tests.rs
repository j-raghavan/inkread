//! Tests for [`CbzBackend`] (#36), split out to keep `cbz.rs` focused. Included via `#[path]` so
//! `super::*` resolves to the cbz module. Fully host-testable — no device, no external fixture for
//! the PNG path (encoded in-test); one tiny JPEG fixture covers the JPEG decoder.

use std::cmp::Ordering;
use std::io::{Cursor, Write};

use super::*;
use crate::document::{Document, FitMode};
use crate::error::CoreError;
use crate::render::PixelBuffer;

/// Encode a solid-color RGBA PNG in memory.
fn png_rgba(w: u32, h: u32, color: [u8; 4]) -> Vec<u8> {
    let data: Vec<u8> = color
        .iter()
        .copied()
        .cycle()
        .take((w * h * 4) as usize)
        .collect();
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut wr = enc.write_header().unwrap();
        wr.write_image_data(&data).unwrap();
    }
    out
}

/// Pack named entries into a CBZ (a stored — uncompressed — ZIP).
fn make_cbz(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut zw = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, bytes) in entries {
        zw.start_file(*name, opts).unwrap();
        zw.write_all(bytes).unwrap();
    }
    zw.finish().unwrap().into_inner()
}

/// Render a page into a `w`×`h` buffer and return the RGBA bytes.
fn render(b: &CbzBackend, page: usize, w: u32, h: u32) -> Vec<u8> {
    let mut px = vec![0u8; (w * h * 4) as usize];
    let mut buf = PixelBuffer::from_rgba(&mut px, w, h).unwrap();
    b.render_page(page, &mut buf).unwrap();
    px
}

/// The average RGB of a rendered buffer (the test images are solid colors).
fn avg_rgb(px: &[u8]) -> [u8; 3] {
    let n = (px.len() / 4).max(1) as u64;
    let (mut r, mut g, mut b) = (0u64, 0u64, 0u64);
    for p in px.chunks_exact(4) {
        r += p[0] as u64;
        g += p[1] as u64;
        b += p[2] as u64;
    }
    [(r / n) as u8, (g / n) as u8, (b / n) as u8]
}

#[test]
fn opens_and_pages_through_in_natural_order() {
    // Filenames whose lexicographic order ("page10" < "page2") DIFFERS from reading order.
    let red = png_rgba(8, 8, [220, 30, 30, 255]); // page1
    let green = png_rgba(8, 8, [30, 200, 30, 255]); // page2
    let blue = png_rgba(8, 8, [30, 30, 220, 255]); // page10
    let cbz = make_cbz(&[
        ("page10.png", blue),
        ("page2.png", green),
        ("page1.png", red),
    ]);
    let b = CbzBackend::open(cbz).expect("opens");
    assert_eq!(b.page_count(), 3);
    let p0 = avg_rgb(&render(&b, 0, 8, 8));
    assert!(
        p0[0] > 150 && p0[1] < 100,
        "page 0 is red (page1.png): {p0:?}"
    );
    let p2 = avg_rgb(&render(&b, 2, 8, 8));
    assert!(
        p2[2] > 150 && p2[0] < 100,
        "page 2 is blue (page10.png), natural order not lexicographic: {p2:?}"
    );
}

#[test]
fn empty_archive_is_unsupported() {
    // A ZIP with no page images opens to a typed UnsupportedFormat, never a panic.
    let cbz = make_cbz(&[("readme.txt", b"no images here".to_vec())]);
    match CbzBackend::open(cbz) {
        Err(CoreError::UnsupportedFormat(_)) => {}
        Ok(_) => panic!("image-less archive must not open"),
        Err(e) => panic!("expected UnsupportedFormat, got {e:?}"),
    }
}

#[test]
fn corrupt_zip_is_typed_error_not_panic() {
    match CbzBackend::open(b"PK\x03\x04 not really a zip".to_vec()) {
        Err(CoreError::CorruptDocument(_)) => {}
        Ok(_) => panic!("garbage must not open as cbz"),
        Err(e) => panic!("expected CorruptDocument, got {e:?}"),
    }
    match CbzBackend::open(Vec::new()) {
        Err(CoreError::CorruptDocument(_)) => {}
        Ok(_) => panic!("empty input must not open"),
        Err(e) => panic!("expected CorruptDocument for empty input, got {e:?}"),
    }
}

#[test]
fn non_image_entries_are_ignored() {
    let red = png_rgba(8, 8, [220, 30, 30, 255]);
    let cbz = make_cbz(&[
        ("page1.png", red),
        ("readme.txt", b"notes".to_vec()),
        ("__MACOSX/._page1.png", b"resource fork junk".to_vec()),
        (".DS_Store", b"finder".to_vec()),
    ]);
    let b = CbzBackend::open(cbz).expect("opens");
    assert_eq!(b.page_count(), 1, "only the real page image counts");
}

#[test]
fn render_out_of_range_is_typed_error() {
    let red = png_rgba(8, 8, [220, 30, 30, 255]);
    let b = CbzBackend::open(make_cbz(&[("p1.png", red)])).unwrap();
    let mut px = vec![0u8; 8 * 8 * 4];
    let mut buf = PixelBuffer::from_rgba(&mut px, 8, 8).unwrap();
    assert!(matches!(
        b.render_page(9, &mut buf),
        Err(CoreError::PageOutOfRange { requested: 9, .. })
    ));
}

#[test]
fn jpeg_page_decodes() {
    // A small baseline JPEG fixture (solid red ~[220,30,30]); proves the JPEG decode path + RGB→RGBA.
    let jpg = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/cbz_solid.jpg"
    ))
    .expect("jpeg fixture present");
    let b = CbzBackend::open(make_cbz(&[("p1.jpg", jpg)])).expect("opens jpeg cbz");
    assert_eq!(b.page_count(), 1);
    let dom = avg_rgb(&render(&b, 0, 8, 8));
    assert!(
        dom[0] > dom[1] + 40 && dom[0] > dom[2] + 40,
        "jpeg page is red-dominant: {dom:?}"
    );
}

#[test]
fn render_fit_letterboxes_a_wide_page() {
    // A 4:1 wide image fit into an 8×8 buffer → height 2, centered, white letterbox above/below.
    let red = png_rgba(16, 4, [220, 30, 30, 255]);
    let b = CbzBackend::open(make_cbz(&[("p1.png", red)])).unwrap();
    let mut px = vec![0u8; 8 * 8 * 4];
    let mut buf = PixelBuffer::from_rgba(&mut px, 8, 8).unwrap();
    b.render_fit(0, &mut buf, FitMode::Page, 0.0, 0.0).unwrap();
    assert_eq!(&px[0..3], &[255, 255, 255], "top row is white letterbox");
    // A central row carries the image (red-dominant).
    let mid = ((4 * 8) * 4) as usize;
    assert!(px[mid] > 150 && px[mid + 1] < 100, "centre row is the page");
}

#[test]
fn cbz_is_magnifiable() {
    // A fixed page raster honors zoom (#61) — unlike a reflowable backend.
    let red = png_rgba(8, 8, [220, 30, 30, 255]);
    let b = CbzBackend::open(make_cbz(&[("p1.png", red)])).unwrap();
    assert!(b.is_magnifiable());
}

#[test]
fn natural_cmp_orders_numbers_numerically() {
    assert_eq!(natural_cmp("page2.png", "page10.png"), Ordering::Less);
    assert_eq!(natural_cmp("p001", "p1"), Ordering::Equal); // leading zeros don't change value
    assert_eq!(natural_cmp("ch1/p2.png", "ch1/p10.png"), Ordering::Less);
    assert_eq!(natural_cmp("Apple", "banana"), Ordering::Less); // case-insensitive
}
