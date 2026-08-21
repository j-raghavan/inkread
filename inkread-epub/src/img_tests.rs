//! Tests for the shared raster decoder (#187). Included via `#[path]` so `super::*` resolves to
//! the img module.

use super::*;

/// A minimal, valid PNG built by the `png` encoder — round-tripping through the real encoder keeps
/// the fixture honest rather than pinning hand-written bytes.
fn png_bytes(width: u32, height: u32, color: png::ColorType, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, width, height);
        enc.set_color(color);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().expect("header");
        writer.write_image_data(data).expect("data");
    }
    out
}

#[test]
fn a_grayscale_png_decodes_to_rgba_with_its_dimensions() {
    let raw = png_bytes(2, 2, png::ColorType::Grayscale, &[0, 64, 128, 255]);
    let img = decode(&raw).expect("decodes");
    assert_eq!((img.width, img.height), (2, 2));
    assert_eq!(img.rgba.len(), 2 * 2 * 4);
    // Grayscale expands across RGB with opaque alpha.
    assert_eq!(&img.rgba[0..4], &[0, 0, 0, 255]);
    assert_eq!(&img.rgba[4..8], &[64, 64, 64, 255]);
}

#[test]
fn an_rgba_png_keeps_its_alpha() {
    let raw = png_bytes(
        1,
        2,
        png::ColorType::Rgba,
        &[255, 0, 0, 255, 0, 0, 255, 128],
    );
    let img = decode(&raw).expect("decodes");
    assert_eq!((img.width, img.height), (1, 2));
    assert_eq!(&img.rgba[0..4], &[255, 0, 0, 255]);
    assert_eq!(&img.rgba[4..8], &[0, 0, 255, 128]);
}

/// Layout only needs the size, and reading it must not cost a full decode.
#[test]
fn dimensions_reads_the_header_without_decoding() {
    let raw = png_bytes(7, 3, png::ColorType::Grayscale, &[128u8; 21]);
    assert_eq!(dimensions(&raw), Some((7, 3)));
    assert_eq!(dimensions(b"not an image"), None);
    assert_eq!(dimensions(&[]), None);
    // The size comes out of the header, so half a file is enough for it while a decode — which
    // needs the pixel data — fails on the same bytes. That difference is the whole point: layout
    // asks for sizes over a chapter's worth of images and must not pay a decode for each.
    let half = &raw[..raw.len() / 2];
    assert_eq!(dimensions(half), Some((7, 3)));
    assert!(decode(half).is_err());
}

#[test]
fn a_non_image_payload_is_a_typed_error_not_a_panic() {
    assert!(matches!(
        decode(b"plain text"),
        Err(ImageError::Unsupported(_))
    ));
    assert!(matches!(decode(&[]), Err(ImageError::Unsupported(_))));
    // Correct magic, garbage body — the codec's own failure, reported not panicked.
    let mut fake_png = PNG_MAGIC.to_vec();
    fake_png.extend_from_slice(&[0u8; 32]);
    assert!(matches!(decode(&fake_png), Err(ImageError::Corrupt(_))));
    let mut fake_jpeg = JPEG_MAGIC.to_vec();
    fake_jpeg.extend_from_slice(&[0u8; 32]);
    assert!(decode(&fake_jpeg).is_err());
    // Every one of these must also survive the cheap path.
    for raw in [&b"plain text"[..], &fake_png, &fake_jpeg] {
        let _ = dimensions(raw);
    }
}

/// A transparent illustration must read as ink on paper. Compositing over black instead would
/// invert a line drawing with a cut-out background — the common illustrated-EPUB case.
#[test]
fn transparency_composites_over_white() {
    assert_eq!(
        luminance_over_white(&[0, 0, 0, 0]),
        255,
        "fully transparent"
    );
    assert_eq!(luminance_over_white(&[0, 0, 0, 255]), 0, "opaque black");
    assert_eq!(luminance_over_white(&[255, 255, 255, 255]), 255);
    // Half-transparent black sits mid-grey, not black.
    let half = luminance_over_white(&[0, 0, 0, 128]);
    assert!((120..=140).contains(&half), "{half}");
    // Colour uses Rec. 601 luma: pure green is much lighter than pure blue.
    assert!(luminance_over_white(&[0, 255, 0, 255]) > luminance_over_white(&[0, 0, 255, 255]));
}
