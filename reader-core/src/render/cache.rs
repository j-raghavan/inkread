//! Render cache (RR4-FR6): a bounded LRU of rendered RGBA buffers keyed by content params.
//!
//! Revisiting a page at the same params (or redrawing after an overlay change) serves the
//! stored buffer instead of re-rasterizing. The cache is a pure, host-tested data structure;
//! its budget is governed by the session's `ResourceBudget` (RR24, M1a.5). Eviction is
//! **deterministic** — the least-recently-used entry goes first, with no dependence on
//! `HashMap` iteration order (every access stamps a unique monotonic tick).

use crate::render::gray::DitherMode;
use std::collections::HashMap;

/// The render-cache key (RR4-FR6): page + zoom + rotation + invert + dither + gamma.
///
/// Floats are integerized (×1000) so the key is `Hash`/`Eq` — a raw `f32` must never enter a
/// hash key (NaN is not `Eq`; precision noise would split logically-equal keys).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PageHash {
    /// Zero-based page index.
    pub page: u32,
    /// Zoom factor ×1000 (e.g. 1.5× → 1500).
    pub zoom_milli: u32,
    /// Rotation in quarter-turns, 0..=3.
    pub rotation_quarters: u8,
    /// Night-invert applied to the cached buffer (RR4-FR8).
    pub invert: bool,
    /// Dither mode the buffer was produced with.
    pub dither: DitherMode,
    /// Gamma ×1000.
    pub gamma_milli: u32,
}

impl PageHash {
    /// Build a key, integerizing the float params. Negative/NaN zoom or gamma clamp to 0.
    #[must_use]
    pub fn new(
        page: u32,
        zoom: f32,
        rotation_quarters: u8,
        invert: bool,
        dither: DitherMode,
        gamma: f32,
    ) -> Self {
        Self {
            page,
            zoom_milli: to_milli(zoom),
            rotation_quarters: rotation_quarters % 4,
            invert,
            dither,
            gamma_milli: to_milli(gamma),
        }
    }
}

/// `value * 1000`, rounded, with NaN/negative mapped to 0 (a key never carries a NaN).
fn to_milli(value: f32) -> u32 {
    if value.is_finite() && value > 0.0 {
        (value * 1000.0).round() as u32
    } else {
        0
    }
}

struct CacheEntry {
    rgba: Vec<u8>,
    last_used: u64,
}

/// A bounded LRU cache of rendered RGBA buffers, capped by total bytes (RR4-FR6 / RR24-FR1).
pub struct RenderCache {
    map: HashMap<PageHash, CacheEntry>,
    max_bytes: usize,
    cur_bytes: usize,
    tick: u64,
    evicted: u64,
}

impl RenderCache {
    /// A cache holding at most `max_bytes` of rendered pixels.
    #[must_use]
    pub fn with_capacity_bytes(max_bytes: usize) -> Self {
        Self {
            map: HashMap::new(),
            max_bytes,
            cur_bytes: 0,
            tick: 0,
            evicted: 0,
        }
    }

    /// Fetch the buffer for `key`, marking it most-recently-used. `None` on a miss.
    pub fn get(&mut self, key: &PageHash) -> Option<&[u8]> {
        self.tick += 1;
        let tick = self.tick;
        let entry = self.map.get_mut(key)?;
        entry.last_used = tick;
        Some(entry.rgba.as_slice())
    }

    /// Insert (or replace) the buffer for `key`, evicting LRU entries until it fits the budget.
    ///
    /// A buffer larger than the whole budget is **not** cached (the budget invariant
    /// `cur_bytes <= max_bytes` always holds) — the caller simply re-renders it next time.
    pub fn insert(&mut self, key: PageHash, rgba: Vec<u8>) {
        self.tick += 1;
        let size = rgba.len();
        // Replacing an existing key: reclaim its bytes first.
        if let Some(old) = self.map.remove(&key) {
            self.cur_bytes -= old.rgba.len();
        }
        if size > self.max_bytes {
            return; // too big to ever fit; leave the budget intact.
        }
        while self.cur_bytes + size > self.max_bytes {
            // Evict the unique least-recently-used entry (ticks are unique → deterministic).
            let victim = self
                .map
                .iter()
                .min_by_key(|(_, e)| e.last_used)
                .map(|(k, _)| *k);
            match victim {
                Some(v) => {
                    if let Some(e) = self.map.remove(&v) {
                        self.cur_bytes -= e.rgba.len();
                        self.evicted += 1;
                    }
                }
                None => break,
            }
        }
        self.cur_bytes += size;
        self.map.insert(
            key,
            CacheEntry {
                rgba,
                last_used: self.tick,
            },
        );
    }

    /// Number of cached entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Total bytes currently held.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.cur_bytes
    }

    /// Lifetime count of evicted entries (inspectable for RR24/RR30 logging).
    #[must_use]
    pub fn evicted_count(&self) -> u64 {
        self.evicted
    }

    /// Drop all entries (the back-pressure trim hook, RR24-FR3).
    pub fn clear(&mut self) {
        self.map.clear();
        self.cur_bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(page: u32) -> PageHash {
        PageHash::new(page, 1.0, 0, false, DitherMode::None, 1.0)
    }

    #[test]
    fn page_hash_integerizes_floats_and_is_stable() {
        let a = PageHash::new(3, 1.5, 1, true, DitherMode::Ordered, 1.0);
        let b = PageHash::new(3, 1.5, 1, true, DitherMode::Ordered, 1.0);
        assert_eq!(a, b);
        assert_eq!(a.zoom_milli, 1500);
        // Non-finite/negative params collapse to 0 rather than poisoning the key.
        let nan = PageHash::new(0, f32::NAN, 0, false, DitherMode::None, -1.0);
        assert_eq!(nan.zoom_milli, 0);
        assert_eq!(nan.gamma_milli, 0);
        // Rotation wraps into 0..=3.
        assert_eq!(
            PageHash::new(0, 1.0, 5, false, DitherMode::None, 1.0).rotation_quarters,
            1
        );
    }

    #[test]
    fn hit_returns_stored_bytes_and_miss_is_none() {
        let mut c = RenderCache::with_capacity_bytes(1024);
        assert!(c.get(&key(0)).is_none());
        c.insert(key(0), vec![1, 2, 3, 4]);
        assert_eq!(c.get(&key(0)), Some(&[1, 2, 3, 4][..]));
        assert_eq!(c.len(), 1);
        assert_eq!(c.bytes(), 4);
    }

    #[test]
    fn evicts_lru_deterministically_under_budget() {
        // Budget holds exactly two 4-byte buffers.
        let mut c = RenderCache::with_capacity_bytes(8);
        c.insert(key(0), vec![0; 4]);
        c.insert(key(1), vec![0; 4]);
        // Touch page 0 so page 1 becomes the LRU.
        assert!(c.get(&key(0)).is_some());
        // Inserting page 2 must evict page 1 (the LRU), not page 0.
        c.insert(key(2), vec![0; 4]);
        assert_eq!(c.len(), 2);
        assert!(c.get(&key(0)).is_some(), "recently-used page 0 retained");
        assert!(c.get(&key(1)).is_none(), "LRU page 1 evicted");
        assert!(c.get(&key(2)).is_some());
        assert_eq!(c.evicted_count(), 1);
        assert!(c.bytes() <= 8);
    }

    #[test]
    fn replacing_a_key_does_not_double_count_bytes() {
        let mut c = RenderCache::with_capacity_bytes(64);
        c.insert(key(0), vec![0; 10]);
        c.insert(key(0), vec![0; 6]); // replace
        assert_eq!(c.len(), 1);
        assert_eq!(c.bytes(), 6);
    }

    #[test]
    fn oversized_buffer_is_not_cached_and_budget_holds() {
        let mut c = RenderCache::with_capacity_bytes(8);
        c.insert(key(0), vec![0; 4]);
        c.insert(key(1), vec![0; 100]); // bigger than the whole budget
        assert!(c.get(&key(1)).is_none(), "oversized buffer not cached");
        assert!(c.bytes() <= 8, "budget invariant held");
    }

    #[test]
    fn clear_drops_everything() {
        let mut c = RenderCache::with_capacity_bytes(64);
        c.insert(key(0), vec![0; 4]);
        c.insert(key(1), vec![0; 4]);
        c.clear();
        assert!(c.is_empty());
        assert_eq!(c.bytes(), 0);
    }
}
