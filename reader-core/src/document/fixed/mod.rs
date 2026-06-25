//! Fixed-layout document backends (trivial integer page model — RR5-FR2).
//!
//! Both backends here render a fixed page raster aspect-fit into the viewport, so they share the
//! fit/letterbox/pan math below ([`fit_dims`]/[`fit_place`]/[`composite_centered`]) — PDF via pdfium
//! and CBZ via a decoded page image.

mod cbz;
mod pdf;

pub use cbz::CbzBackend;
pub use pdf::{PdfBackend, PDFIUM_LIB_PATH_ENV};

use crate::document::FitMode;
use crate::render::PixelBuffer;

/// The aspect-preserving render size for `aspect` (w/h) inside a `bw`×`bh` buffer under [`FitMode`]
/// (RR4). `Page` contains; `Width`/`Height` fill that axis (the other may overflow). Clamped sane.
pub(crate) fn fit_dims(aspect: f32, bw: i32, bh: i32, mode: FitMode) -> (i32, i32) {
    let (tw, th) = match mode {
        FitMode::Page => {
            if (bw as f32 / bh as f32) > aspect {
                (((bh as f32) * aspect).round() as i32, bh)
            } else {
                (bw, ((bw as f32) / aspect).round() as i32)
            }
        }
        FitMode::Width => (bw, ((bw as f32) / aspect).round() as i32),
        FitMode::Height => (((bh as f32) * aspect).round() as i32, bh),
    };
    (tw.clamp(1, 1 << 15), th.clamp(1, 1 << 15))
}

/// Composite a `tw`×`th` RGBA `tmp` image into `buf` (RR4): centered with white letterbox when it
/// fits, panned by the normalized `pan_*` when it overflows. Shared by fit + crop renders.
pub(crate) fn composite_centered(
    buf: &mut PixelBuffer<'_>,
    tmp: &[u8],
    tw: i32,
    th: i32,
    pan_x: f32,
    pan_y: f32,
) {
    let bw = i32::try_from(buf.width()).unwrap_or(0);
    let bh = i32::try_from(buf.height()).unwrap_or(0);
    let (sx, dx, cw) = fit_place(tw, bw, pan_x);
    let (sy, dy, ch) = fit_place(th, bh, pan_y);
    let dst = buf.bytes_mut();
    for row in 0..ch {
        let s = (((sy + row) * tw + sx) * 4) as usize;
        let d = (((dy + row) * bw + dx) * 4) as usize;
        let n = (cw * 4) as usize;
        dst[d..d + n].copy_from_slice(&tmp[s..s + n]);
    }
}

/// Placement of a fitted dimension `fit` inside a buffer dimension `buf` (RR4 Fit). Returns
/// `(src_offset, dst_offset, count)`: **centered** with white letterbox when it fits, **panned** by
/// the normalized `pan` when it overflows. Always in-bounds for both buffers.
pub(crate) fn fit_place(fit: i32, buf: i32, pan: f32) -> (i32, i32, i32) {
    if fit <= buf {
        (0, (buf - fit) / 2, fit)
    } else {
        let over = fit - buf;
        let src = (pan.clamp(0.0, 1.0) * over as f32).round() as i32;
        (src.clamp(0, over), 0, buf)
    }
}
