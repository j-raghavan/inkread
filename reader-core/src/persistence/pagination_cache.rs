//! Disk pagination cache (ADR-INKREAD-0013 D3 / `SPEC-RUST-READER.md` RR8-FR3).
//!
//! Reflow pagination is deterministic for a given [`layout_digest`](inkread_epub::layout::LayoutOpts::layout_digest)
//! but recomputing a whole book on every reopen is wasteful on a slow e-ink SoC. This caches the
//! **full laid pagination** (the positioned [`Page`]s + per-chapter start indices) to the document's
//! `book.inkread/pagination/` sidecar, keyed by the digest, so reopening at the same viewport + style
//! rehydrates instead of re-laying-out (RR8-AC1).
//!
//! The cache is **advisory**: a miss, an oversize file, or a corrupt/foreign blob simply degrades to
//! a fresh layout — never an error to the reader (RR21-FR3), mirroring [`super::ink_store`]'s posture.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use inkread_epub::layout::Page;

use crate::error::{CoreError, CoreResult};
use crate::persistence::ink_store::atomic_write;
use crate::persistence::sidecar::SidecarPaths;

/// Hard cap on a cache file read (RR21-FR3): a whole book's laid pages, generously bounded so a
/// hostile/corrupt file can't trigger a huge allocation. Over the cap ⇒ treated as a miss.
const MAX_CACHE_BYTES: u64 = 64 * 1024 * 1024;

/// A persisted pagination for one `layout_digest`: the laid pages plus the per-chapter start page
/// indices (`chapter_start`), i.e. everything [`crate::document::reflow`]'s `Laid` needs except the
/// `LayoutOpts`, which the caller already holds (it computed the digest from them).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct CachedPagination {
    /// Global page index where each chapter begins.
    pub chapter_start: Vec<usize>,
    /// Every laid page across the book, in reading order.
    pub pages: Vec<Page>,
}

/// Reads/writes laid paginations under `book.inkread/pagination/`, keyed by `layout_digest`.
pub(crate) struct PaginationCache {
    dir: PathBuf,
}

impl PaginationCache {
    /// The cache rooted at a document's sidecar (`<book>.inkread/pagination/`).
    pub(crate) fn new(paths: &SidecarPaths) -> Self {
        Self {
            dir: paths.root().join("pagination"),
        }
    }

    fn file(&self, digest: u64) -> PathBuf {
        self.dir.join(format!("{digest:016x}.json"))
    }

    /// The cached pagination for `digest`, or `None` on miss / oversize / corrupt — in every
    /// non-hit case the caller lays out fresh. Never panics (RR21-FR3).
    pub(crate) fn load(&self, digest: u64) -> Option<CachedPagination> {
        let path = self.file(digest);
        let meta = std::fs::metadata(&path).ok()?;
        if meta.len() > MAX_CACHE_BYTES {
            return None;
        }
        let bytes = std::fs::read(&path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Persist `cached` for `digest` atomically (crash-safe via [`atomic_write`]). Returns an error
    /// only on a genuine I/O/serialization failure; callers treat caching as best-effort.
    pub(crate) fn store(&self, digest: u64, cached: &CachedPagination) -> CoreResult<()> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| CoreError::Persistence(format!("pagination cache mkdir: {e}")))?;
        let bytes = serde_json::to_vec(cached)
            .map_err(|e| CoreError::Persistence(format!("pagination cache encode: {e}")))?;
        atomic_write(&self.file(digest), &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use inkread_epub::layout::{paginate, LayoutOpts, Metrics};
    use inkread_epub::parse_blocks;

    /// A self-cleaning temp directory (mirrors the `ink_store` test helper).
    struct TempDir {
        path: PathBuf,
    }
    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut path = std::env::temp_dir();
            path.push(format!("inkread-pgcache-{tag}-{:p}", &tag));
            std::fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    struct Mono;
    impl Metrics for Mono {
        fn advance(&self, text: &str, size_px: f32, _b: bool, _i: bool) -> f32 {
            text.chars().count() as f32 * size_px * 0.5
        }
    }

    fn sample() -> CachedPagination {
        let opts = LayoutOpts::new(400.0, 600.0, 16.0);
        let blocks = parse_blocks("<html><body><p>one two three four</p></body></html>");
        CachedPagination {
            chapter_start: vec![0],
            pages: paginate(&blocks, &opts, &Mono),
        }
    }

    #[test]
    fn round_trips_through_disk() {
        let tmp = TempDir::new("rt");
        let cache = PaginationCache::new(&SidecarPaths::from_root(&tmp.path));
        let cached = sample();
        cache.store(0xABCD, &cached).unwrap();
        assert_eq!(cache.load(0xABCD), Some(cached));
    }

    #[test]
    fn miss_and_corrupt_degrade_to_none() {
        let tmp = TempDir::new("miss");
        let cache = PaginationCache::new(&SidecarPaths::from_root(&tmp.path));
        assert_eq!(cache.load(0x1234), None, "absent digest is a miss");

        // A corrupt file at the digest's path is a miss, not a panic.
        std::fs::create_dir_all(&cache.dir).unwrap();
        std::fs::write(cache.file(0x1234), b"{ not valid json").unwrap();
        assert_eq!(cache.load(0x1234), None, "corrupt blob degrades to a miss");
    }

    #[test]
    fn distinct_digests_do_not_collide() {
        let tmp = TempDir::new("keys");
        let cache = PaginationCache::new(&SidecarPaths::from_root(&tmp.path));
        let a = sample();
        let mut b = sample();
        b.chapter_start = vec![0, 1];
        cache.store(1, &a).unwrap();
        cache.store(2, &b).unwrap();
        assert_eq!(cache.load(1), Some(a));
        assert_eq!(cache.load(2), Some(b));
    }
}
