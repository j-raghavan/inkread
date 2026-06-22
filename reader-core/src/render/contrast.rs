//! Contrast / display enhancement for the rendered page (RR4 — KOReader's "Contrast" control).
//!
//! A pure per-pixel **gamma-darken** remap on the final RGBA buffer, applied **after** the backend
//! renders and **before** the shell blits. Format-agnostic (PDF + EPUB). A gamma curve
//! `out = 255·(in/255)^γ` with `γ > 1` keeps pure white (255→255) and pure black (0→0) fixed while
//! pushing every gray toward black — so it darkens/thickens faint text without graying the page
//! background. The win is legibility of faint or low-contrast scans on a grayscale e-ink panel; on a
//! crisp digital PDF it still darkens the anti-aliased glyph edges, reading as slightly bolder text.
//! Host-tested, no device types (IR-4).

use crate::render::PixelBuffer;

/// Number of contrast steps the UI exposes; `0` = off (identity), matching KOReader's slider cells.
pub const MAX_CONTRAST_STEP: u8 = 8;

/// Map a UI step (`0..=MAX_CONTRAST_STEP`) to a darkening gamma (`1.0` = identity; higher = darker).
#[must_use]
pub fn step_to_gamma(step: u8) -> f32 {
    1.0 + f32::from(step.min(MAX_CONTRAST_STEP)) * 0.5 // step 8 → γ 5.0 (strong)
}

/// Apply a darkening `gamma` (`1.0` = no-op) to the RGBA pixels in `buf`. `gamma <= 1.0` is a cheap
/// no-op. Endpoints are fixed (white stays white, black stays black); mid grays move toward black.
/// Uses a 256-entry LUT so the per-pixel cost is a table lookup. Alpha is untouched.
pub fn apply_contrast(buf: &mut PixelBuffer<'_>, gamma: f32) {
    if gamma <= 1.0 + 1e-3 || !gamma.is_finite() {
        return;
    }
    let mut lut = [0u8; 256];
    for (i, slot) in lut.iter_mut().enumerate() {
        let v = (i as f32 / 255.0).powf(gamma) * 255.0;
        *slot = v.round().clamp(0.0, 255.0) as u8;
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
    fn step_zero_is_identity_gamma() {
        assert!((step_to_gamma(0) - 1.0).abs() < 1e-6);
        assert!(step_to_gamma(8) > step_to_gamma(4));
        // saturates beyond the max step
        assert_eq!(step_to_gamma(99), step_to_gamma(MAX_CONTRAST_STEP));
    }

    #[test]
    fn identity_gamma_leaves_pixels_untouched() {
        let mut bytes = [10, 128, 240, 255, 60, 200, 90, 128];
        let before = bytes;
        apply_contrast(&mut buf_with(&mut bytes), 1.0);
        assert_eq!(bytes, before);
    }

    #[test]
    fn gamma_darkens_grays_toward_black() {
        // a light gray (200) and a mid gray (128) both move toward black; alpha preserved.
        let mut bytes = [200, 200, 200, 255, 128, 128, 128, 255];
        apply_contrast(&mut buf_with(&mut bytes), step_to_gamma(MAX_CONTRAST_STEP));
        assert!(bytes[0] < 200, "light gray darkened: {}", bytes[0]);
        assert!(bytes[4] < 128, "mid gray darkened: {}", bytes[4]);
        assert_eq!(bytes[3], 255, "alpha preserved");
    }

    #[test]
    fn endpoints_are_fixed_and_clamped() {
        let mut bytes = [0, 0, 0, 255, 255, 255, 255, 255];
        apply_contrast(&mut buf_with(&mut bytes), 5.0);
        assert_eq!(&bytes[0..3], &[0, 0, 0], "black stays black");
        assert_eq!(&bytes[4..7], &[255, 255, 255], "white stays white");
    }
}
