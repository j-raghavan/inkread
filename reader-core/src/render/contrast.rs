//! Contrast / display enhancement for the rendered page (RR4 — KOReader's "Contrast" control).
//!
//! A pure per-pixel luminance remap on the final RGBA buffer, applied **after** the backend renders
//! and **before** the shell blits. Format-agnostic (PDF + EPUB) because it works on pixels, not
//! layout. The headline win is legibility of faint/low-contrast scanned PDFs on a grayscale e-ink
//! panel. Host-tested, no device types (IR-4).

use crate::render::PixelBuffer;

/// Number of contrast steps the UI exposes; `0` = off (identity), matching KOReader's slider cells.
pub const MAX_CONTRAST_STEP: u8 = 8;

/// Map a UI step (`0..=MAX_CONTRAST_STEP`) to a multiplicative contrast factor (`1.0` = identity).
#[must_use]
pub fn step_to_factor(step: u8) -> f32 {
    1.0 + f32::from(step.min(MAX_CONTRAST_STEP)) * 0.25 // step 8 → 3.0×
}

/// Apply contrast `factor` (`1.0` = no-op) to the RGBA pixels in `buf`, pivoting around mid-gray so
/// lights stay light and darks get darker. Alpha is untouched. A factor `<= 1.0` is a no-op (cheap
/// early return). Uses a 256-entry LUT so the per-pixel cost is a table lookup.
pub fn apply_contrast(buf: &mut PixelBuffer<'_>, factor: f32) {
    if factor <= 1.0 + 1e-3 || !factor.is_finite() {
        return;
    }
    let mut lut = [0u8; 256];
    for (i, slot) in lut.iter_mut().enumerate() {
        let adj = ((i as f32 - 128.0) * factor + 128.0).round();
        *slot = adj.clamp(0.0, 255.0) as u8;
    }
    for px in buf.bytes_mut().chunks_exact_mut(4) {
        px[0] = lut[px[0] as usize];
        px[1] = lut[px[1] as usize];
        px[2] = lut[px[2] as usize];
        // px[3] (alpha) unchanged
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf_with(bytes: &mut [u8]) -> PixelBuffer<'_> {
        let n = (bytes.len() / 4) as u32;
        PixelBuffer::from_rgba(bytes, n, 1).unwrap()
    }

    #[test]
    fn step_zero_is_identity_factor() {
        assert!((step_to_factor(0) - 1.0).abs() < 1e-6);
        assert!(step_to_factor(8) > step_to_factor(4));
        // saturates beyond the max step
        assert_eq!(step_to_factor(99), step_to_factor(MAX_CONTRAST_STEP));
    }

    #[test]
    fn identity_factor_leaves_pixels_untouched() {
        let mut bytes = [10, 128, 240, 255, 60, 200, 90, 128];
        let before = bytes;
        apply_contrast(&mut buf_with(&mut bytes), 1.0);
        assert_eq!(bytes, before);
    }

    #[test]
    fn higher_contrast_darkens_darks_and_lightens_lights() {
        // dark pixel (50) and light pixel (200); alpha 255.
        let mut bytes = [50, 50, 50, 255, 200, 200, 200, 255];
        apply_contrast(&mut buf_with(&mut bytes), 2.0);
        assert!(bytes[0] < 50, "dark got darker: {}", bytes[0]);
        assert!(bytes[4] > 200, "light got lighter: {}", bytes[4]);
        assert_eq!(bytes[3], 255, "alpha preserved");
        assert_eq!(bytes[7], 255, "alpha preserved");
    }

    #[test]
    fn extreme_values_clamp_not_panic() {
        let mut bytes = [0, 0, 0, 255, 255, 255, 255, 255];
        apply_contrast(&mut buf_with(&mut bytes), 5.0);
        assert_eq!(&bytes[0..3], &[0, 0, 0]); // already at floor
        assert_eq!(&bytes[4..7], &[255, 255, 255]); // already at ceiling
    }
}
