//! Bilinear image resampling (RR4 — KOReader's "Render Quality").
//!
//! Render quality is realized by rendering the page into an off-size buffer (smaller for "low",
//! larger for "high") and resampling it to the panel resolution: rendering **above** panel res then
//! downsampling smooths text/edges on e-ink (supersampling); rendering **below** is faster/softer.
//! Pure pixel math, host-tested, no device types (IR-4).

use crate::render::PixelBuffer;

/// Resample the `sw`×`sh` RGBA `src` into `dst` (its own dimensions) with bilinear filtering.
/// A size match is a straight copy. Out-of-range taps clamp to the edge; never panics on sane sizes.
pub fn resample_bilinear(src: &[u8], sw: u32, sh: u32, dst: &mut PixelBuffer<'_>) {
    let (dw, dh) = (dst.width(), dst.height());
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return;
    }
    let need = (sw as usize) * (sh as usize) * 4;
    if src.len() < need {
        return; // malformed source; leave dst untouched (RR21-FR3)
    }
    let out = dst.bytes_mut();
    if sw == dw && sh == dh {
        out.copy_from_slice(&src[..need]);
        return;
    }
    let fx = sw as f32 / dw as f32;
    let fy = sh as f32 / dh as f32;
    for y in 0..dh {
        let syf = (y as f32 + 0.5) * fy - 0.5;
        let sy0 = syf.floor().clamp(0.0, (sh - 1) as f32) as u32;
        let sy1 = (sy0 + 1).min(sh - 1);
        let wy = (syf - sy0 as f32).clamp(0.0, 1.0);
        for x in 0..dw {
            let sxf = (x as f32 + 0.5) * fx - 0.5;
            let sx0 = sxf.floor().clamp(0.0, (sw - 1) as f32) as u32;
            let sx1 = (sx0 + 1).min(sw - 1);
            let wx = (sxf - sx0 as f32).clamp(0.0, 1.0);
            let i00 = ((sy0 * sw + sx0) * 4) as usize;
            let i10 = ((sy0 * sw + sx1) * 4) as usize;
            let i01 = ((sy1 * sw + sx0) * 4) as usize;
            let i11 = ((sy1 * sw + sx1) * 4) as usize;
            let o = ((y * dw + x) * 4) as usize;
            for c in 0..4 {
                let top = src[i00 + c] as f32 + (src[i10 + c] as f32 - src[i00 + c] as f32) * wx;
                let bot = src[i01 + c] as f32 + (src[i11 + c] as f32 - src[i01 + c] as f32) * wx;
                out[o + c] = (top + (bot - top) * wy).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_size_is_a_copy() {
        let src = vec![10u8, 20, 30, 255, 40, 50, 60, 255]; // 2x1
        let mut dbytes = vec![0u8; 8];
        let mut dst = PixelBuffer::from_rgba(&mut dbytes, 2, 1).unwrap();
        resample_bilinear(&src, 2, 1, &mut dst);
        assert_eq!(dbytes, src);
    }

    #[test]
    fn upscale_preserves_solid_color() {
        let src = vec![100u8, 150, 200, 255]; // 1x1 solid
        let mut dbytes = vec![0u8; 4 * 4 * 4];
        let mut dst = PixelBuffer::from_rgba(&mut dbytes, 4, 4).unwrap();
        resample_bilinear(&src, 1, 1, &mut dst);
        assert!(dbytes.chunks_exact(4).all(|p| p == [100, 150, 200, 255]));
    }

    #[test]
    fn downscale_averages_toward_the_middle() {
        // 2x2: black, white / white, black → 1x1 should be mid-gray-ish.
        let src = vec![
            0, 0, 0, 255, 255, 255, 255, 255, // row 0
            255, 255, 255, 255, 0, 0, 0, 255, // row 1
        ];
        let mut dbytes = vec![0u8; 4];
        let mut dst = PixelBuffer::from_rgba(&mut dbytes, 1, 1).unwrap();
        resample_bilinear(&src, 2, 2, &mut dst);
        assert!(
            dbytes[0] > 80 && dbytes[0] < 175,
            "averaged gray: {}",
            dbytes[0]
        );
    }
}
