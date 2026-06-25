//! CBZ comic-archive backend (#36): a ZIP of page images, one image per page, **fixed-layout**.
//!
//! A CBZ has no text layer and no reflow — it is a sequence of page rasters. The backend lists the
//! archive's image entries in **natural** filename order (so `page2` precedes `page10`), decodes a
//! page on demand (PNG / JPEG), and renders it aspect-fit into the viewport via the shared
//! fixed-layout fit math ([`super::fit_dims`]/[`super::composite_centered`]). All the
//! reflow/selection/TOC/link methods stay at the [`Document`] trait's no-op defaults — like a scanned
//! PDF, the page *is* the position. Validates at the boundary and never panics (RR21-FR3).

use std::cmp::Ordering;
use std::io::{Cursor, Read};

use zip::ZipArchive;

use super::{composite_centered, fit_dims};
use crate::document::text_select::NormRect;
use crate::document::{Document, DocumentMetadata, FitMode};
use crate::error::{CoreError, CoreResult};
use crate::render::resample::resample_bilinear;
use crate::render::PixelBuffer;

/// Recognized page-image extensions inside the archive (compared lowercased).
const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png"];

/// A decoded page image as tightly-packed RGBA8 plus its pixel dimensions.
struct DecodedImage {
    rgba: Vec<u8>,
    w: u32,
    h: u32,
}

/// CBZ backend: the archive bytes (kept so pages decode lazily — a comic is large, we never hold all
/// pages decoded) plus the image entry names in reading order.
pub struct CbzBackend {
    bytes: Vec<u8>,
    entries: Vec<String>,
}

impl CbzBackend {
    /// Open a CBZ from its bytes: list the image entries, reject a non-ZIP / image-less archive.
    pub fn open(bytes: Vec<u8>) -> CoreResult<Self> {
        let mut archive = ZipArchive::new(Cursor::new(&bytes))
            .map_err(|e| CoreError::CorruptDocument(format!("cbz: not a valid zip: {e}")))?;
        let mut entries: Vec<String> = Vec::new();
        for i in 0..archive.len() {
            let file = archive
                .by_index(i)
                .map_err(|e| CoreError::CorruptDocument(format!("cbz entry {i}: {e}")))?;
            let name = file.name().to_string();
            if is_page_image(&name) {
                entries.push(name);
            }
        }
        if entries.is_empty() {
            return Err(CoreError::UnsupportedFormat(
                "cbz: no page images in the archive".into(),
            ));
        }
        entries.sort_by(|a, b| natural_cmp(a, b));
        Ok(Self { bytes, entries })
    }

    /// Decode the `index`-th page into RGBA8. Out-of-range → [`CoreError::PageOutOfRange`].
    fn decode(&self, index: usize) -> CoreResult<DecodedImage> {
        let name = self.entries.get(index).ok_or(CoreError::PageOutOfRange {
            requested: index,
            available: self.entries.len(),
        })?;
        let mut archive = ZipArchive::new(Cursor::new(&self.bytes))
            .map_err(|e| CoreError::CorruptDocument(format!("cbz: {e}")))?;
        let mut file = archive
            .by_name(name)
            .map_err(|e| CoreError::CorruptDocument(format!("cbz: missing entry {name}: {e}")))?;
        let mut raw = Vec::new();
        file.read_to_end(&mut raw)
            .map_err(|e| CoreError::CorruptDocument(format!("cbz: read {name}: {e}")))?;
        decode_image(&raw)
    }

    /// Aspect-fit the decoded image into `buf` at `(tw, th)` per `mode`, centered/panned. The shared
    /// path behind `render_fit`/`render_cropped`: it resamples `img` (already cropped by the caller)
    /// into a temp then composites it onto the white page.
    fn fit_into(
        img: &DecodedImage,
        buf: &mut PixelBuffer<'_>,
        mode: FitMode,
        pan_x: f32,
        pan_y: f32,
    ) -> CoreResult<()> {
        buf.fill_white();
        let bw = i32::try_from(buf.width()).unwrap_or(0);
        let bh = i32::try_from(buf.height()).unwrap_or(0);
        if img.w == 0 || img.h == 0 || bw <= 0 || bh <= 0 {
            return Ok(()); // nothing sensible to place; leave the white page
        }
        let aspect = img.w as f32 / img.h as f32;
        let (tw, th) = fit_dims(aspect, bw, bh, mode);
        let mut tmp = vec![0u8; (tw as usize) * (th as usize) * 4];
        {
            let mut tbuf = PixelBuffer::from_rgba(&mut tmp, tw as u32, th as u32)?;
            resample_bilinear(&img.rgba, img.w, img.h, &mut tbuf);
        }
        composite_centered(buf, &tmp, tw, th, pan_x, pan_y);
        Ok(())
    }
}

impl Document for CbzBackend {
    fn page_count(&self) -> usize {
        self.entries.len()
    }

    fn metadata(&self) -> DocumentMetadata {
        DocumentMetadata::default() // a CBZ carries no title/author metadata
    }

    fn render_page(&self, index: usize, buf: &mut PixelBuffer<'_>) -> CoreResult<()> {
        // Full-page render: stretch the whole image to the buffer (mirrors the PDF full render; the
        // session uses render_fit for the aspect-correct path).
        let img = self.decode(index)?;
        resample_bilinear(&img.rgba, img.w, img.h, buf);
        Ok(())
    }

    fn render_fit(
        &self,
        index: usize,
        buf: &mut PixelBuffer<'_>,
        mode: FitMode,
        pan_x: f32,
        pan_y: f32,
    ) -> CoreResult<()> {
        let img = self.decode(index)?;
        Self::fit_into(&img, buf, mode, pan_x, pan_y)
    }

    fn render_cropped(
        &self,
        index: usize,
        buf: &mut PixelBuffer<'_>,
        crop: NormRect,
        mode: FitMode,
        pan_x: f32,
        pan_y: f32,
    ) -> CoreResult<()> {
        let img = self.decode(index)?;
        match crop_image(&img, crop) {
            Some(sub) => Self::fit_into(&sub, buf, mode, pan_x, pan_y),
            None => Self::fit_into(&img, buf, mode, pan_x, pan_y), // degenerate crop → whole page
        }
    }

    fn render_zoom(
        &self,
        index: usize,
        buf: &mut PixelBuffer<'_>,
        zoom: f32,
        offset_x: i32,
        offset_y: i32,
    ) -> CoreResult<()> {
        let img = self.decode(index)?;
        let z = if zoom.is_finite() && zoom > 0.0 {
            zoom
        } else {
            1.0
        };
        let bw = buf.width() as f32;
        let bh = buf.height() as f32;
        // The buf-sized window into the z-scaled image is a normalized sub-rect of the source; crop to
        // it and stretch that window to fill the buffer. At z==1, offset==0 this is the whole image →
        // identical to render_page, and the window matches the session's pan + the shell's ink overlay.
        let (sw, sh) = (bw * z, bh * z);
        let crop = NormRect {
            x0: (offset_x as f32 / sw).clamp(0.0, 1.0),
            y0: (offset_y as f32 / sh).clamp(0.0, 1.0),
            x1: ((offset_x as f32 + bw) / sw).clamp(0.0, 1.0),
            y1: ((offset_y as f32 + bh) / sh).clamp(0.0, 1.0),
        };
        match crop_image(&img, crop) {
            Some(sub) => resample_bilinear(&sub.rgba, sub.w, sub.h, buf),
            None => resample_bilinear(&img.rgba, img.w, img.h, buf),
        }
        Ok(())
    }

    fn is_magnifiable(&self) -> bool {
        true // a fixed page raster honors pinch-zoom (like a fixed PDF, RR25-FR3)
    }
}

/// Whether an archive entry is a page image we render: a regular file (not a directory), not a
/// macOS resource-fork / hidden file, with a recognized image extension.
fn is_page_image(name: &str) -> bool {
    if name.ends_with('/') || name.contains("__MACOSX") {
        return false;
    }
    let base = name.rsplit('/').next().unwrap_or(name);
    if base.starts_with('.') {
        return false; // dotfiles (.DS_Store, ._foo)
    }
    match base.rsplit_once('.') {
        Some((_, ext)) => IMAGE_EXTS.contains(&ext.to_ascii_lowercase().as_str()),
        None => false,
    }
}

/// Natural filename order: compare digit runs numerically (so `p2` precedes `p10`) and other runs
/// case-insensitively. Keeps comic pages in reading order regardless of zero-padding.
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut ai, mut bi) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ca), Some(cb)) => {
                if ca.is_ascii_digit() && cb.is_ascii_digit() {
                    let na = take_number(&mut ai);
                    let nb = take_number(&mut bi);
                    match na.cmp(&nb) {
                        Ordering::Equal => continue,
                        ord => return ord,
                    }
                } else {
                    let (la, lb) = (ca.to_ascii_lowercase(), cb.to_ascii_lowercase());
                    match la.cmp(&lb) {
                        Ordering::Equal => {
                            ai.next();
                            bi.next();
                        }
                        ord => return ord,
                    }
                }
            }
        }
    }
}

/// Consume a leading run of ASCII digits as a `u64` (saturating on an absurdly long run).
fn take_number(it: &mut std::iter::Peekable<std::str::Chars<'_>>) -> u64 {
    let mut n: u64 = 0;
    while let Some(c) = it.peek().copied() {
        if let Some(d) = c.to_digit(10) {
            n = n.saturating_mul(10).saturating_add(d as u64);
            it.next();
        } else {
            break;
        }
    }
    n
}

/// Crop `img` to the normalized `crop` sub-rect, returning a tight RGBA sub-image. `None` for a
/// degenerate (empty) crop so the caller can fall back to the whole page.
fn crop_image(img: &DecodedImage, crop: NormRect) -> Option<DecodedImage> {
    let x0 = (crop.x0.clamp(0.0, 1.0) * img.w as f32).floor() as u32;
    let y0 = (crop.y0.clamp(0.0, 1.0) * img.h as f32).floor() as u32;
    let x1 = (crop.x1.clamp(0.0, 1.0) * img.w as f32).ceil() as u32;
    let y1 = (crop.y1.clamp(0.0, 1.0) * img.h as f32).ceil() as u32;
    let (x1, y1) = (x1.min(img.w), y1.min(img.h));
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let (cw, ch) = (x1 - x0, y1 - y0);
    if cw == img.w && ch == img.h {
        return None; // whole image — let the caller use it directly (no copy)
    }
    let mut rgba = vec![0u8; (cw as usize) * (ch as usize) * 4];
    for row in 0..ch {
        let src = (((y0 + row) * img.w + x0) * 4) as usize;
        let dst = (row * cw * 4) as usize;
        let n = (cw * 4) as usize;
        rgba[dst..dst + n].copy_from_slice(&img.rgba[src..src + n]);
    }
    Some(DecodedImage { rgba, w: cw, h: ch })
}

/// Decode a PNG or JPEG page image to RGBA8, sniffing the codec by magic. A non-image / unsupported
/// payload is a typed error, never a panic (RR21-FR3).
fn decode_image(raw: &[u8]) -> CoreResult<DecodedImage> {
    if raw.starts_with(&[0x89, b'P', b'N', b'G']) {
        decode_png(raw)
    } else if raw.starts_with(&[0xFF, 0xD8]) {
        decode_jpeg(raw)
    } else {
        Err(CoreError::UnsupportedFormat(
            "cbz: page entry is not PNG or JPEG".into(),
        ))
    }
}

fn decode_png(raw: &[u8]) -> CoreResult<DecodedImage> {
    let mut dec = png::Decoder::new(Cursor::new(raw));
    // Expand palette/low-bit-depth to 8-bit channels and drop 16-bit down to 8, so the frame is one
    // of Grayscale / GrayscaleAlpha / Rgb / Rgba at 8 bits — the cases we map to RGBA below.
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec
        .read_info()
        .map_err(|e| CoreError::CorruptDocument(format!("cbz png: {e}")))?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| CoreError::CorruptDocument(format!("cbz png frame: {e}")))?;
    let (w, h) = (info.width, info.height);
    let src = &buf[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => src.to_vec(),
        png::ColorType::Rgb => expand(src, 3, |p| [p[0], p[1], p[2], 255]),
        png::ColorType::GrayscaleAlpha => expand(src, 2, |p| [p[0], p[0], p[0], p[1]]),
        png::ColorType::Grayscale => expand(src, 1, |p| [p[0], p[0], p[0], 255]),
        png::ColorType::Indexed => {
            return Err(CoreError::CorruptDocument(
                "cbz png: indexed not expanded".into(),
            ))
        }
    };
    Ok(DecodedImage { rgba, w, h })
}

fn decode_jpeg(raw: &[u8]) -> CoreResult<DecodedImage> {
    let mut dec = jpeg_decoder::Decoder::new(Cursor::new(raw));
    let pixels = dec
        .decode()
        .map_err(|e| CoreError::CorruptDocument(format!("cbz jpeg: {e}")))?;
    let info = dec
        .info()
        .ok_or_else(|| CoreError::CorruptDocument("cbz jpeg: no image info".into()))?;
    let (w, h) = (info.width as u32, info.height as u32);
    let rgba = match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => expand(&pixels, 3, |p| [p[0], p[1], p[2], 255]),
        jpeg_decoder::PixelFormat::L8 => expand(&pixels, 1, |p| [p[0], p[0], p[0], 255]),
        other => {
            return Err(CoreError::UnsupportedFormat(format!(
                "cbz jpeg: unsupported pixel format {other:?}"
            )))
        }
    };
    Ok(DecodedImage { rgba, w, h })
}

/// Map a tightly-packed `stride`-byte-per-pixel buffer to RGBA8 via `f`.
fn expand(src: &[u8], stride: usize, f: impl Fn(&[u8]) -> [u8; 4]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len() / stride * 4);
    for p in src.chunks_exact(stride) {
        out.extend_from_slice(&f(p));
    }
    out
}

#[cfg(test)]
#[path = "cbz_tests.rs"]
mod tests;
