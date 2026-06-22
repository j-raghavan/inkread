//! Resource budget + bounded caches (RR24).
//!
//! M1a bounds the **render** and **cover** caches (the pagination cache is reflow-only — M2)
//! and caps the per-page render scale so a very large page can't OOM. The session is
//! parameterized by a [`ResourceBudget`] and exposes a back-pressure trim hook the shell calls
//! on `onTrimMemory`. All pure logic, host-tested; the wall-clock latency targets (RR24-FR4)
//! are device-measured and out of scope here.

use crate::persistence::BookId;
use crate::render::{ByteLru, RenderCache};

/// A bounded LRU of cover thumbnails keyed by [`BookId`] (RR13-FR2 / RR24-FR1) — a thin wrapper
/// over [`ByteLru`], sized from the [`ResourceBudget`].
pub struct CoverCache {
    inner: ByteLru<BookId>,
}

impl CoverCache {
    /// A cover cache holding at most `max_bytes` of thumbnails.
    #[must_use]
    pub fn with_capacity_bytes(max_bytes: usize) -> Self {
        Self {
            inner: ByteLru::with_capacity_bytes(max_bytes),
        }
    }

    /// Fetch a book's cover bytes, marking it most-recently-used. `None` on a miss.
    pub fn get(&mut self, book: &BookId) -> Option<&[u8]> {
        self.inner.get(book)
    }

    /// Insert (or replace) a book's cover bytes, evicting LRU entries to fit the budget.
    pub fn insert(&mut self, book: BookId, cover: Vec<u8>) {
        self.inner.insert(book, cover);
    }

    /// Number of cached covers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Whether the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Total bytes currently held.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.inner.bytes()
    }

    /// Drop all covers (back-pressure trim, RR24-FR3).
    pub fn clear(&mut self) {
        self.inner.clear();
    }
}

/// Memory-pressure severity from the platform (`onTrimMemory`, RR24-FR3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrimLevel {
    /// Moderate pressure — shed the least-critical caches (covers/prefetch).
    Moderate,
    /// Critical pressure — drop all caches before the platform kills the process.
    Critical,
}

impl TrimLevel {
    /// Decode the JNI severity code: `0` → `Moderate`, anything `>= 1` → `Critical`. The Kotlin
    /// shell maps Android's `onTrimMemory` constants onto this; an unknown/higher value is treated
    /// as the more aggressive shed, which is the safe default under memory pressure.
    #[must_use]
    pub fn from_code(code: i32) -> TrimLevel {
        if code >= 1 {
            TrimLevel::Critical
        } else {
            TrimLevel::Moderate
        }
    }
}

/// The session's cache governor (RR24): owns the render + cover caches, sized by a
/// [`ResourceBudget`], and trims them on memory pressure.
pub struct Caches {
    render: RenderCache,
    cover: CoverCache,
}

impl Caches {
    /// Build caches sized from `budget`.
    #[must_use]
    pub fn new(budget: &ResourceBudget) -> Self {
        Self {
            render: RenderCache::with_capacity_bytes(budget.render_cache_bytes),
            cover: CoverCache::with_capacity_bytes(budget.cover_cache_bytes),
        }
    }

    /// The render cache.
    pub fn render(&mut self) -> &mut RenderCache {
        &mut self.render
    }

    /// The cover cache.
    pub fn cover(&mut self) -> &mut CoverCache {
        &mut self.cover
    }

    /// Total bytes held across both caches.
    #[must_use]
    pub fn total_bytes(&self) -> usize {
        self.render.bytes() + self.cover.bytes()
    }

    /// React to memory pressure (RR24-FR3): `Moderate` drops the cover cache; `Critical` drops
    /// everything. Deterministic and panic-free — always leaves the reader usable.
    pub fn trim(&mut self, level: TrimLevel) {
        match level {
            TrimLevel::Moderate => self.cover.clear(),
            TrimLevel::Critical => {
                self.render.clear();
                self.cover.clear();
            }
        }
    }
}

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

    // RR24-FR1 / RR13-FR2: the cover cache evicts the LRU book deterministically and clears.
    #[test]
    fn cover_cache_evicts_lru_book_and_clears() {
        let mut c = CoverCache::with_capacity_bytes(8); // holds two 4-byte covers
        let a = BookId::new("a").unwrap();
        let b = BookId::new("b").unwrap();
        let d = BookId::new("d").unwrap();
        c.insert(a.clone(), vec![0; 4]);
        c.insert(b.clone(), vec![0; 4]);
        assert!(c.get(&a).is_some()); // touch a → b becomes LRU
        c.insert(d.clone(), vec![0; 4]); // evicts b
        assert_eq!(c.len(), 2);
        assert!(c.get(&a).is_some(), "recently-used a retained");
        assert!(c.get(&b).is_none(), "LRU b evicted");
        assert!(c.get(&d).is_some());
        assert!(c.bytes() <= 8);
        c.clear();
        assert!(c.is_empty());
    }

    // RR24-FR3: the JNI severity code decodes to the matching trim level (unknown → Critical).
    #[test]
    fn trim_level_from_code_maps_severity() {
        assert_eq!(TrimLevel::from_code(0), TrimLevel::Moderate);
        assert_eq!(TrimLevel::from_code(1), TrimLevel::Critical);
        assert_eq!(TrimLevel::from_code(99), TrimLevel::Critical);
        assert_eq!(TrimLevel::from_code(-5), TrimLevel::Moderate);
    }

    // RR24-FR3: trim sheds caches by severity — Moderate drops covers, Critical drops all.
    #[test]
    fn caches_trim_sheds_by_level() {
        use crate::render::{DitherMode, PageHash};
        let budget = ResourceBudget {
            render_cache_bytes: 1 << 20,
            cover_cache_bytes: 1 << 20,
            max_page_pixels: 1_000_000,
        };
        let mut c = Caches::new(&budget);
        c.render().insert(
            PageHash::new(0, 1.0, 0, false, DitherMode::None, 1.0),
            vec![0; 100],
        );
        c.cover().insert(BookId::new("a").unwrap(), vec![0; 100]);
        assert!(c.total_bytes() > 0);

        // Moderate: covers shed, render retained.
        c.trim(TrimLevel::Moderate);
        assert!(c.cover().is_empty());
        assert!(!c.render().is_empty());

        // Critical: everything dropped.
        c.trim(TrimLevel::Critical);
        assert_eq!(c.total_bytes(), 0);
    }
}
