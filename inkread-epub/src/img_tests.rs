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

fn rgba(width: u32, height: u32, px: &[[u8; 4]]) -> RgbaImage {
    RgbaImage {
        width,
        height,
        rgba: px.iter().flatten().copied().collect(),
    }
}

#[test]
fn scaling_preserves_a_flat_tone_and_the_target_size() {
    let img = rgba(4, 4, &[[100, 100, 100, 255]; 16]);
    let out = scale_to_gray(&img, 2, 2);
    assert_eq!(out.len(), 4);
    assert!(out.iter().all(|&v| v == 100), "{out:?}");
}

/// Averaging rather than point sampling is the point: a checkerboard downscaled 2:1 must go grey,
/// not pick one phase and alias.
#[test]
fn downscaling_averages_the_pixels_it_covers() {
    let b = [0u8, 0, 0, 255];
    let w = [255u8, 255, 255, 255];
    let img = rgba(2, 2, &[b, w, w, b]);
    let out = scale_to_gray(&img, 1, 1);
    assert_eq!(out.len(), 1);
    assert!(
        (120..=135).contains(&out[0]),
        "expected mid-grey, got {out:?}"
    );
}

#[test]
fn a_degenerate_target_or_source_yields_nothing_rather_than_panicking() {
    let img = rgba(2, 1, &[[0, 0, 0, 255], [255, 255, 255, 255]]);
    assert!(scale_to_gray(&img, 0, 4).is_empty());
    assert!(scale_to_gray(&img, 4, 0).is_empty());
    let empty = RgbaImage {
        width: 0,
        height: 0,
        rgba: Vec::new(),
    };
    assert!(scale_to_gray(&empty, 4, 4).is_empty());
    // A truncated pixel buffer must not index out of bounds.
    let short = RgbaImage {
        width: 4,
        height: 4,
        rgba: vec![0u8; 8],
    };
    assert_eq!(scale_to_gray(&short, 2, 2).len(), 4);
}

/// Scaling up is never asked for by layout, but must still be well defined.
#[test]
fn scaling_up_is_defined_and_sized_correctly() {
    let img = rgba(1, 1, &[[10, 10, 10, 255]]);
    let out = scale_to_gray(&img, 3, 2);
    assert_eq!(out.len(), 6);
    assert!(out.iter().all(|&v| v == 10), "{out:?}");
}
