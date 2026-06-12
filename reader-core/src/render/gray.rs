//! Grayscale + dithering post-step (RR4-FR3).
//!
//! Converts the rendered RGBA buffer to the panel's gray depth in place. Channels are read
//! in **R, G, B** order per [`CHANNEL_ORDER`](crate::render::pixel_buffer::CHANNEL_ORDER) —
//! the explicit half of the BGRA(pdfium)↔RGBA decision (Amendment 3): pdfium is rendered
//! with reverse-byte-order so it emits RGBA, and we read it as RGBA here. The channel-order
//! golden test pins this so a regression (reading B,G,R) is caught on the host.

use crate::render::pixel_buffer::{ChannelOrder, PixelBuffer, BYTES_PER_PIXEL, CHANNEL_ORDER};

/// How to convert tones to the panel's gray depth (RR4-FR3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DitherMode {
    /// Plain luminance quantization (no dithering).
    None,
    /// Ordered (Bayer 4×4) dithering — deterministic, cheap, good for e-ink.
    Ordered,
    /// Floyd–Steinberg error diffusion — higher quality, serial over rows (RR4-FR3).
    FloydSteinberg,
}

/// The Supernote Carta panel is 16-level gray (RR4-FR3).
pub const GRAY_LEVELS: u16 = 16;

/// Rec. 601 luma of an (r,g,b) triple, rounded, 0..=255.
#[must_use]
pub fn luma(r: u8, g: u8, b: u8) -> u8 {
    // 0.299 R + 0.587 G + 0.114 B in fixed point (sum of weights = 1000).
    let y = 299 * u32::from(r) + 587 * u32::from(g) + 114 * u32::from(b);
    ((y + 500) / 1000) as u8
}

/// Quantize `value` (0..=255) to one of `levels` evenly-spaced gray levels, returning the
/// 0..=255 representative of the chosen level.
#[must_use]
fn quantize(value: u8, levels: u16) -> u8 {
    debug_assert!(levels >= 2);
    let levels = u32::from(levels);
    let v = u32::from(value);
    // Map to level index then back to the 0..255 representative.
    let idx = (v * (levels - 1) + 127) / 255;
    ((idx * 255) / (levels - 1)) as u8
}

/// The 4×4 Bayer ordered-dither threshold matrix, normalized to a signed offset in `-8..8`
/// (scaled to the quantization step at use).
const BAYER_4X4: [[i32; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

/// Convert `buf` to `levels`-step grayscale in place, writing the gray value into all of
/// R,G,B (per [`CHANNEL_ORDER`]) and leaving α untouched. Reads channels in R,G,B order.
pub fn to_grayscale(buf: &mut PixelBuffer<'_>, mode: DitherMode, levels: u16) {
    // Floyd–Steinberg needs cross-pixel error diffusion, handled in its own pass (RR4-FR3).
    if mode == DitherMode::FloydSteinberg {
        floyd_steinberg(buf, levels);
        return;
    }

    let order: ChannelOrder = CHANNEL_ORDER;
    let width = buf.width() as usize;
    let bytes = buf.bytes_mut();

    for (i, px) in bytes.chunks_exact_mut(BYTES_PER_PIXEL).enumerate() {
        let r = px[order.r];
        let g = px[order.g];
        let b = px[order.b];
        let y = luma(r, g, b);

        let gray = match mode {
            DitherMode::None => quantize(y, levels),
            DitherMode::Ordered => {
                let x = i % width;
                let row = i / width;
                // Bayer offset centered around 0, scaled to the quantization step.
                let step = 255 / i32::from(levels - 1);
                let threshold = BAYER_4X4[row % 4][x % 4] - 8; // -8..7
                let nudged = (i32::from(y) + threshold * step / 16).clamp(0, 255) as u8;
                quantize(nudged, levels)
            }
            DitherMode::FloydSteinberg => unreachable!("dispatched to floyd_steinberg above"),
        };

        px[order.r] = gray;
        px[order.g] = gray;
        px[order.b] = gray;
        // α (px[order.a]) left as-is.
    }
}

/// Floyd–Steinberg error-diffusion dithering to `levels` gray steps, in place (RR4-FR3).
///
/// Deterministic: a fixed **raster order** (left→right, top→bottom) with integer error weights,
/// so the output is reproducible for golden tests. Allocates two `i32` error rows once (not
/// per pixel) — the only heap allocation on the cache-miss render path.
fn floyd_steinberg(buf: &mut PixelBuffer<'_>, levels: u16) {
    let order: ChannelOrder = CHANNEL_ORDER;
    let width = buf.width() as usize;
    let bytes = buf.bytes_mut();
    if width == 0 {
        return;
    }
    let height = bytes.len() / (width * BYTES_PER_PIXEL);
    // Error rows padded by 1 each side so the x-1 / x+1 spill at the edges lands in a slot
    // we discard rather than wrapping onto the opposite edge.
    let mut err_curr = vec![0i32; width + 2];
    let mut err_next = vec![0i32; width + 2];

    for y in 0..height {
        for x in 0..width {
            let i = (y * width + x) * BYTES_PER_PIXEL;
            let lum = luma(bytes[i + order.r], bytes[i + order.g], bytes[i + order.b]);
            let old = i32::from(lum) + err_curr[x + 1];
            let newc = quantize(old.clamp(0, 255) as u8, levels);
            let e = old - i32::from(newc);
            // Spread the quantization error to the not-yet-visited neighbours (weights /16).
            err_curr[x + 2] += e * 7 / 16; // right
            err_next[x] += e * 3 / 16; // below-left
            err_next[x + 1] += e * 5 / 16; // below
            err_next[x + 2] += e / 16; // below-right
            bytes[i + order.r] = newc;
            bytes[i + order.g] = newc;
            bytes[i + order.b] = newc;
            // α left as-is.
        }
        std::mem::swap(&mut err_curr, &mut err_next);
        err_next.iter_mut().for_each(|v| *v = 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::pixel_buffer::PixelBuffer;

    #[test]
    fn luma_pure_channels() {
        assert_eq!(luma(255, 0, 0), 76); // 0.299*255
        assert_eq!(luma(0, 255, 0), 150); // 0.587*255
        assert_eq!(luma(0, 0, 255), 29); // 0.114*255
        assert_eq!(luma(255, 255, 255), 255);
        assert_eq!(luma(0, 0, 0), 0);
    }

    #[test]
    fn quantize_endpoints_and_midpoint() {
        assert_eq!(quantize(0, 16), 0);
        assert_eq!(quantize(255, 16), 255);
        // 16 levels => step 17; ~mid stays mid-ish.
        assert!(quantize(128, 16) > 100 && quantize(128, 16) < 160);
    }

    // Amendment 3 — the channel-order golden test. A pixel that is pure RED in RGBA byte
    // order (bytes [255,0,0,255]) MUST yield the red luma (76), proving gray.rs reads R from
    // byte 0. If gray.rs erroneously read B,G,R, byte 0 would be treated as blue and give 29.
    #[test]
    fn channel_order_red_pixel_yields_red_luma() {
        let mut buf = vec![255u8, 0, 0, 255]; // RGBA = opaque red
        let mut pb = PixelBuffer::from_rgba(&mut buf, 1, 1).unwrap();
        to_grayscale(&mut pb, DitherMode::None, GRAY_LEVELS);
        let gray = pb.bytes()[CHANNEL_ORDER.r];
        // quantize(76,16): 76 is luma of red. Assert it is NOT the blue luma (29).
        assert_ne!(
            gray,
            quantize(29, GRAY_LEVELS),
            "must not be reading blue as red"
        );
        assert_eq!(gray, quantize(76, GRAY_LEVELS));
        // α preserved.
        assert_eq!(pb.bytes()[CHANNEL_ORDER.a], 255);
    }

    #[test]
    fn channel_order_blue_pixel_yields_blue_luma() {
        let mut buf = vec![0u8, 0, 255, 255]; // RGBA = opaque blue
        let mut pb = PixelBuffer::from_rgba(&mut buf, 1, 1).unwrap();
        to_grayscale(&mut pb, DitherMode::None, GRAY_LEVELS);
        assert_eq!(pb.bytes()[CHANNEL_ORDER.b], quantize(29, GRAY_LEVELS));
    }

    #[test]
    fn grayscale_writes_equal_rgb() {
        // A mid-gray-ish pixel: R,G,B should end up equal after conversion.
        let mut buf = vec![120u8, 130, 140, 255];
        let mut pb = PixelBuffer::from_rgba(&mut buf, 1, 1).unwrap();
        to_grayscale(&mut pb, DitherMode::None, GRAY_LEVELS);
        let b = pb.bytes();
        assert_eq!(b[0], b[1]);
        assert_eq!(b[1], b[2]);
    }

    #[test]
    fn ordered_dither_outputs_valid_levels() {
        // A flat 50% gray field dithered to 16 levels: every output must be a valid level.
        let mut buf = vec![128u8; 8 * 8 * 4];
        for px in buf.chunks_exact_mut(4) {
            px[3] = 255;
        }
        let mut pb = PixelBuffer::from_rgba(&mut buf, 8, 8).unwrap();
        to_grayscale(&mut pb, DitherMode::Ordered, GRAY_LEVELS);
        let valid: Vec<u8> = (0..GRAY_LEVELS)
            .map(|i| quantize((i * 17) as u8, 16))
            .collect();
        for px in pb.bytes().chunks_exact(4) {
            assert!(
                valid.contains(&px[0]),
                "dithered value {} not a level",
                px[0]
            );
        }
    }

    // RR4-FR3: Floyd–Steinberg output is only ever a valid quantized level.
    #[test]
    fn fs_outputs_valid_levels() {
        let mut buf = vec![128u8; 8 * 8 * 4];
        for px in buf.chunks_exact_mut(4) {
            px[3] = 255;
        }
        let mut pb = PixelBuffer::from_rgba(&mut buf, 8, 8).unwrap();
        to_grayscale(&mut pb, DitherMode::FloydSteinberg, GRAY_LEVELS);
        let valid: Vec<u8> = (0..GRAY_LEVELS)
            .map(|i| quantize((i * 17) as u8, 16))
            .collect();
        for px in pb.bytes().chunks_exact(4) {
            assert!(valid.contains(&px[0]), "FS value {} not a level", px[0]);
        }
    }

    // RR4-FR3: FS is deterministic (fixed raster order) — the golden-image contract depends on
    // identical output for identical input.
    #[test]
    fn fs_is_deterministic() {
        let make = || {
            let mut buf = vec![0u8; 16 * 16 * 4];
            for (i, px) in buf.chunks_exact_mut(4).enumerate() {
                let v = (i % 256) as u8;
                px[0] = v;
                px[1] = v;
                px[2] = v;
                px[3] = 255;
            }
            buf
        };
        let mut a = make();
        let mut b = make();
        let mut pa = PixelBuffer::from_rgba(&mut a, 16, 16).unwrap();
        let mut pb = PixelBuffer::from_rgba(&mut b, 16, 16).unwrap();
        to_grayscale(&mut pa, DitherMode::FloydSteinberg, GRAY_LEVELS);
        to_grayscale(&mut pb, DitherMode::FloydSteinberg, GRAY_LEVELS);
        assert_eq!(pa.bytes(), pb.bytes());
    }

    // RR4-FR3: FS writes equal R,G,B and leaves α untouched.
    #[test]
    fn fs_preserves_alpha_and_writes_equal_rgb() {
        let mut buf = vec![120u8, 130, 140, 200];
        let mut pb = PixelBuffer::from_rgba(&mut buf, 1, 1).unwrap();
        to_grayscale(&mut pb, DitherMode::FloydSteinberg, GRAY_LEVELS);
        let b = pb.bytes();
        assert_eq!(b[0], b[1]);
        assert_eq!(b[1], b[2]);
        assert_eq!(b[3], 200);
    }

    // RR4-AC2: a smooth tone gradient dithered to the panel depth must show MULTIPLE distinct
    // levels (tones reproduced, not collapsed) with NO all-black gap in a non-black region
    // (catches banding / black-hole regressions in the gray/dither step).
    #[test]
    fn dither_gradient_has_no_banding_or_black_gaps() {
        // A wide, 1-row horizontal gradient 0..=255 (use a tall band so column patterns mix).
        let w = 256usize;
        let h = 8usize;
        let mut buf = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let v = x as u8; // luminance ramps 0..255 left→right
                let i = (y * w + x) * 4;
                buf[i] = v;
                buf[i + 1] = v;
                buf[i + 2] = v;
                buf[i + 3] = 255;
            }
        }
        let mut pb = PixelBuffer::from_rgba(&mut buf, w as u32, h as u32).unwrap();
        to_grayscale(&mut pb, DitherMode::Ordered, GRAY_LEVELS);

        // (a) The gradient reproduces many distinct gray levels — no banding collapse.
        let mut distinct = std::collections::BTreeSet::new();
        for px in pb.bytes().chunks_exact(4) {
            distinct.insert(px[0]);
        }
        assert!(
            distinct.len() >= GRAY_LEVELS as usize - 1,
            "gradient collapsed to {} levels (banding); expected ~{}",
            distinct.len(),
            GRAY_LEVELS
        );

        // (b) No all-black gap in the bright region. The right half of the gradient (input
        // luminance >= 128) must contain NO black (0) output pixel — a black-hole regression
        // (e.g. reading the wrong channel / underflow) would drop bright pixels to 0.
        let bytes = pb.bytes();
        for y in 0..h {
            for x in (w / 2)..w {
                let i = (y * w + x) * 4;
                assert_ne!(
                    bytes[i], 0,
                    "bright pixel at x={x} dithered to black (banding/black gap)"
                );
            }
        }

        // (c) The darkest input (x=0) stays black and the brightest (x=255) stays white —
        // endpoints are not crushed/blown by the dither offset.
        assert_eq!(bytes[0], 0, "black endpoint should remain black");
        let last = (w - 1) * 4; // brightest pixel of the first row
        assert_eq!(bytes[last], 255, "white endpoint should remain white");
    }
}
