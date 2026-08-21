//! Raster image decoding for the reflow pipeline (#187).
//!
//! EPUBs carry their illustrations as PNG or JPEG in the manifest. This module turns those bytes
//! into pixels, and — separately and much more cheaply — reads just their intrinsic size, which is
//! all the layout stage needs to reserve a box before anything is drawn.
//!
//! The same two codecs back the CBZ page reader, so this is the one decoder for both paths rather
//! than a second copy. It is deliberately narrow: no colour management, no EXIF orientation, no
//! animation. A payload that is neither PNG nor JPEG, or that fails to decode, is a typed error —
//! never a panic (RR21-FR3), because a book's images are untrusted input.

use std::io::Cursor;

/// A decoded image as RGBA8, row-major.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

/// Why an image could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageError {
    /// Not a codec we support (or not an image at all).
    Unsupported(String),
    /// A supported codec that failed to decode.
    Corrupt(String),
}

impl std::fmt::Display for ImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImageError::Unsupported(m) => write!(f, "unsupported image: {m}"),
            ImageError::Corrupt(m) => write!(f, "corrupt image: {m}"),
        }
    }
}

impl std::error::Error for ImageError {}

/// PNG magic (`\x89PNG`).
const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G'];
/// JPEG start-of-image marker.
const JPEG_MAGIC: &[u8] = &[0xFF, 0xD8];

/// The image's intrinsic pixel size, read from the header alone.
///
/// Layout needs only this to reserve a box, and header parsing is orders of magnitude cheaper than
/// decoding — which matters when a chapter is laid out to find one page's worth of content. Returns
/// `None` for anything unreadable; the caller falls back to a placeholder rather than failing.
#[must_use]
pub fn dimensions(raw: &[u8]) -> Option<(u32, u32)> {
    if raw.starts_with(PNG_MAGIC) {
        let mut dec = png::Decoder::new(Cursor::new(raw));
        dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let reader = dec.read_info().ok()?;
        let info = reader.info();
        Some((info.width, info.height))
    } else if raw.starts_with(JPEG_MAGIC) {
        let mut dec = jpeg_decoder::Decoder::new(Cursor::new(raw));
        dec.read_info().ok()?;
        let info = dec.info()?;
        Some((u32::from(info.width), u32::from(info.height)))
    } else {
        None
    }
}

/// Decode a PNG or JPEG to RGBA8, sniffing the codec by magic.
pub fn decode(raw: &[u8]) -> Result<RgbaImage, ImageError> {
    if raw.starts_with(PNG_MAGIC) {
        decode_png(raw)
    } else if raw.starts_with(JPEG_MAGIC) {
        decode_jpeg(raw)
    } else {
        Err(ImageError::Unsupported("not PNG or JPEG".into()))
    }
}

fn decode_png(raw: &[u8]) -> Result<RgbaImage, ImageError> {
    let mut dec = png::Decoder::new(Cursor::new(raw));
    // Expand palette/low-bit-depth to 8-bit channels and drop 16-bit down to 8, so the frame is one
    // of Grayscale / GrayscaleAlpha / Rgb / Rgba at 8 bits — the cases mapped below.
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec
        .read_info()
        .map_err(|e| ImageError::Corrupt(format!("png: {e}")))?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| ImageError::Corrupt(format!("png frame: {e}")))?;
    let src = &buf[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => src.to_vec(),
        png::ColorType::Rgb => expand(src, 3, |p| [p[0], p[1], p[2], 255]),
        png::ColorType::GrayscaleAlpha => expand(src, 2, |p| [p[0], p[0], p[0], p[1]]),
        png::ColorType::Grayscale => expand(src, 1, |p| [p[0], p[0], p[0], 255]),
        png::ColorType::Indexed => {
            return Err(ImageError::Corrupt("png: indexed not expanded".into()))
        }
    };
    Ok(RgbaImage {
        width: info.width,
        height: info.height,
        rgba,
    })
}

fn decode_jpeg(raw: &[u8]) -> Result<RgbaImage, ImageError> {
    let mut dec = jpeg_decoder::Decoder::new(Cursor::new(raw));
    let pixels = dec
        .decode()
        .map_err(|e| ImageError::Corrupt(format!("jpeg: {e}")))?;
    let info = dec
        .info()
        .ok_or_else(|| ImageError::Corrupt("jpeg: no image info".into()))?;
    let rgba = match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => expand(&pixels, 3, |p| [p[0], p[1], p[2], 255]),
        jpeg_decoder::PixelFormat::L8 => expand(&pixels, 1, |p| [p[0], p[0], p[0], 255]),
        other => {
            return Err(ImageError::Unsupported(format!(
                "jpeg pixel format {other:?}"
            )))
        }
    };
    Ok(RgbaImage {
        width: u32::from(info.width),
        height: u32::from(info.height),
        rgba,
    })
}

/// Map a tightly-packed `stride`-byte-per-pixel buffer to RGBA8 via `f`.
fn expand(src: &[u8], stride: usize, f: impl Fn(&[u8]) -> [u8; 4]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len() / stride * 4);
    for p in src.chunks_exact(stride) {
        out.extend_from_slice(&f(p));
    }
    out
}

/// Luminance of an RGBA pixel, alpha-composited over white.
///
/// White is the page, so a transparent PNG — a line drawing with a cut-out background, which is the
/// common illustrated-EPUB case — reads as ink on paper rather than ink on black.
#[must_use]
pub fn luminance_over_white(px: &[u8]) -> u8 {
    let (r, g, b, a) = (
        f32::from(px[0]),
        f32::from(px[1]),
        f32::from(px[2]),
        f32::from(px[3]) / 255.0,
    );
    // Rec. 601 luma, the same weighting the fixed-page render path uses.
    let luma = 0.299 * r + 0.587 * g + 0.114 * b;
    (luma * a + 255.0 * (1.0 - a)).round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
#[path = "img_tests.rs"]
mod tests;
