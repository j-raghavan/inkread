//! Resource budget + bounded caches (RR24).
//!
//! M1a bounds the **render** and **cover** caches (the pagination cache is reflow-only — M2)
//! and caps the per-page render scale so a very large page can't OOM. The session is
//! parameterized by a [`ResourceBudget`] and exposes a back-pressure trim hook the shell calls
//! on `onTrimMemory`. All pure logic, host-tested; the wall-clock latency targets (RR24-FR4)
//! are device-measured and out of scope here.

/// Committed cache sizes + the page-pixel cap (RR24-FR1/FR2). Tunable; the defaults are the
/// Supernote baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceBudget {
    /// Byte ceiling for the rendered-page (RR4-FR6) cache.
    pub render_cache_bytes: usize,
    /// Byte ceiling for the cover-thumbnail (RR13-FR2) cache.
    pub cover_cache_bytes: usize,
    /// Hard cap on a single rendered page's pixel count — the OOM guard (RR24-FR2).
    pub max_page_pixels: u64,
}

impl ResourceBudget {
    /// The committed Supernote defaults: ~64 MiB render cache (a few full 1920×2560 pages),
    /// ~8 MiB cover cache, and a 16 Mpx page cap (~3× the panel area, room for zoom).
    #[must_use]
    pub fn default_supernote() -> Self {
        Self {
            render_cache_bytes: 64 << 20,
            cover_cache_bytes: 8 << 20,
            max_page_pixels: 16_000_000,
        }
    }

    /// Clamp a requested render `zoom` so the rendered page `(native_w·zoom)×(native_h·zoom)`
    /// stays within [`Self::max_page_pixels`] (RR24-FR2). A non-finite/non-positive zoom is
    /// treated as 1.0; the result is never below a tiny floor (avoids a zero-size render). A
    /// page already over the cap at 1× is scaled *down* to fit.
    #[must_use]
    pub fn clamp_render_scale(&self, native_w: u32, native_h: u32, zoom: f32) -> f32 {
        let zoom = if zoom.is_finite() && zoom > 0.0 {
            zoom
        } else {
            1.0
        };
        let pixels_at_1x = u64::from(native_w) * u64::from(native_h);
        if pixels_at_1x == 0 {
            return zoom; // nothing to cap (degenerate page)
        }
        let max_zoom = (self.max_page_pixels as f64 / pixels_at_1x as f64).sqrt() as f32;
        zoom.min(max_zoom).max(0.01)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_keeps_a_within_budget_zoom_unchanged() {
        let b = ResourceBudget::default_supernote();
        // 1000×1000 = 1 Mpx; at 2× = 4 Mpx, under the 16 Mpx cap → unchanged.
        assert_eq!(b.clamp_render_scale(1000, 1000, 2.0), 2.0);
    }

    #[test]
    fn clamp_caps_excessive_zoom_to_the_pixel_budget() {
        let b = ResourceBudget::default_supernote();
        // 2000×2000 = 4 Mpx; 16 Mpx cap → max zoom sqrt(4) = 2.0. A request of 5× clamps to 2×.
        let z = b.clamp_render_scale(2000, 2000, 5.0);
        assert!((z - 2.0).abs() < 1e-3, "expected ~2.0, got {z}");
        // The capped render is within budget.
        let px = (2000.0 * z) as u64 * (2000.0 * z) as u64;
        assert!(px <= b.max_page_pixels + 1);
    }

    #[test]
    fn clamp_scales_an_oversized_page_below_one() {
        let b = ResourceBudget::default_supernote();
        // 8000×8000 = 64 Mpx at 1× — already over the 16 Mpx cap → max zoom 0.5.
        let z = b.clamp_render_scale(8000, 8000, 1.0);
        assert!(z < 1.0 && z > 0.0, "oversized page scaled down, got {z}");
    }

    #[test]
    fn clamp_handles_degenerate_inputs() {
        let b = ResourceBudget::default_supernote();
        // Zero dimension → passthrough of the sanitized zoom.
        assert_eq!(b.clamp_render_scale(0, 100, 3.0), 3.0);
        // Non-finite / non-positive zoom → treated as 1.0, then capped.
        assert!(b.clamp_render_scale(1000, 1000, f32::NAN) > 0.0);
        assert!(b.clamp_render_scale(1000, 1000, -2.0) > 0.0);
    }
}
