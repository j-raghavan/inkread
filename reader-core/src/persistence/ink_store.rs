//! The annotation-persistence **port** + adapters (RR10, RR20).
//!
//! [`InkStore`] is to ink what [`super::ReaderStore`] is to reading position: a narrow port the
//! session depends on, with a production filesystem adapter ([`FsInkStore`]) and an in-memory
//! adapter ([`MemInkStore`]) for tests / a store-less session. A store is scoped to **one
//! document's** `book.inkread/` sidecar, so the port is keyed only by page.
//!
//! Crash-safety (RR10-FR7, RR20-FR3/FR4/FR6): every write goes to a `*.tmp` sibling that is
//! flushed (`sync_all`) and then **atomically renamed** over the target. A crash therefore leaves
//! the committed `.inkbin` either fully old or fully new — never half-written — and only an
//! orphan `.tmp` (ignored by load + scan) is ever lost.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use inkread_ink::{decode_layer, encode_layer, InkLayer};

use crate::error::{CoreError, CoreResult};
use crate::persistence::sidecar::{parse_page_file_name, SidecarMetadata, SidecarPaths};

/// The annotation-persistence port for a single document's sidecar. `Send + Sync` so the session
/// can hold it behind an `Arc` across the engine worker thread (RR21), mirroring `ReaderStore`.
pub trait InkStore: Send + Sync {
    /// Load the ink layer for `page`, or an **empty** layer if the page has no sidecar file
    /// (never an error for an un-annotated page).
    fn load_page(&self, page: usize) -> CoreResult<InkLayer>;

    /// Persist `layer` for `page` atomically. An empty layer **removes** the page's file (all
    /// strokes erased), keeping the sidecar tidy.
    fn save_page(&self, page: usize, layer: &InkLayer) -> CoreResult<()>;

    /// The zero-based indices of pages that currently have stored ink, ascending.
    fn pages_with_ink(&self) -> CoreResult<Vec<usize>>;

    /// Load the sidecar metadata, or `None` if absent. A *corrupt* metadata file is an error the
    /// caller may treat as non-fatal (the strokes still load).
    fn load_metadata(&self) -> CoreResult<Option<SidecarMetadata>>;

    /// Persist the sidecar metadata atomically.
    fn save_metadata(&self, meta: &SidecarMetadata) -> CoreResult<()>;

    /// Move a page's stored ink aside (rename to `*.corrupt`) when it fails to decode, so the
    /// reader can continue with an empty page WITHOUT destroying the unreadable bytes — they stay
    /// recoverable (RR20-FR1). Default: a no-op for stores without on-disk files.
    fn quarantine_page(&self, _page: usize) -> CoreResult<()> {
        Ok(())
    }

    /// Move the whole `annotations/` directory aside when the sidecar belongs to a *different*
    /// document (identity mismatch, RR10-AC3), so the opened document starts with a clean sidecar
    /// and the stale ink is preserved rather than adopted. Default: a no-op.
    fn reset_stale_annotations(&self) -> CoreResult<()> {
        Ok(())
    }
}

/// Wrap an IO error as a persistence error (RR21-FR3).
fn io_err(e: io::Error) -> CoreError {
    CoreError::Persistence(e.to_string())
}

/// Hard ceiling on a single page's `.inkbin` sidecar file (RR20 / RR21-FR3). Mirrors the document
/// open cap ([`crate::jni`]'s `read_document_file`): we **stat before read** so a crafted/oversized
/// sidecar can't force a huge allocation, complementing the codec's per-count decode limits. One
/// page of ink is kilobytes; 64 MiB is far above any real page while bounding a hostile file.
const MAX_INKBIN_BYTES: u64 = 64 << 20;

/// Read a page `.inkbin`, capping the size before the read (RR21-FR3). `Ok(None)` if the file is
/// absent (an un-annotated page). A file over `max_bytes` is reported as **corrupt** so the caller
/// quarantines it and degrades the page to empty — exactly like an undecodable file — rather than
/// attempting the allocation. `max_bytes` is a parameter only so the cap is unit-testable cheaply.
fn read_page_capped(path: &Path, max_bytes: u64) -> CoreResult<Option<Vec<u8>>> {
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_err(e)),
    };
    if meta.len() > max_bytes {
        return Err(CoreError::CorruptDocument(format!(
            "sidecar page is {} bytes, over the {max_bytes}-byte limit",
            meta.len()
        )));
    }
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_err(e)),
    }
}

/// Best-effort directory fsync so a rename/unlink is itself durable across power loss (RR20-FR6):
/// `sync_all` on a file flushes its *data*, but the **directory entry** created/removed by the
/// rename/unlink is only guaranteed durable once the parent dir is fsync'd. A platform or
/// filesystem that rejects opening/fsyncing a directory is tolerated — the rename still happened.
fn sync_dir(dir: &Path) {
    if let Ok(f) = fs::File::open(dir) {
        let _ = f.sync_all();
    }
}

/// Atomically replace `path` with `bytes`: write a flushed `*.tmp` sibling, rename over the target,
/// then fsync the parent directory so the rename survives power loss. The parent must exist. Shared
/// with the pagination cache (ADR-INKREAD-0013) so both sidecar writers are crash-safe the same way.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> CoreResult<()> {
    let mut tmp_name: OsString = path.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp = Path::new(&tmp_name);
    {
        let mut f = fs::File::create(tmp).map_err(io_err)?;
        f.write_all(bytes).map_err(io_err)?;
        f.sync_all().map_err(io_err)?;
    }
    // Rename is atomic on the same filesystem; on failure drop the temp so no orphan lingers.
    if let Err(e) = fs::rename(tmp, path) {
        let _ = fs::remove_file(tmp);
        return Err(io_err(e));
    }
    if let Some(parent) = path.parent() {
        sync_dir(parent);
    }
    Ok(())
}

/// Filesystem adapter — the production [`InkStore`] over a `book.inkread/` directory.
#[derive(Debug, Clone)]
pub struct FsInkStore {
    paths: SidecarPaths,
}

impl FsInkStore {
    /// A store over the given sidecar layout.
    #[must_use]
    pub fn new(paths: SidecarPaths) -> Self {
        Self { paths }
    }
}

impl InkStore for FsInkStore {
    fn load_page(&self, page: usize) -> CoreResult<InkLayer> {
        let path = self.paths.page_file(page);
        match read_page_capped(&path, MAX_INKBIN_BYTES)? {
            Some(bytes) => Ok(decode_layer(&bytes)?),
            None => Ok(InkLayer::new()),
        }
    }

    fn save_page(&self, page: usize, layer: &InkLayer) -> CoreResult<()> {
        let file = self.paths.page_file(page);
        if layer.is_empty() {
            // Erased to nothing → remove the file (absence == no ink). Missing is fine; fsync the
            // dir so the unlink is durable (RR20-FR6).
            return match fs::remove_file(&file) {
                Ok(()) => {
                    if let Some(parent) = file.parent() {
                        sync_dir(parent);
                    }
                    Ok(())
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(io_err(e)),
            };
        }
        fs::create_dir_all(self.paths.annotations_dir()).map_err(io_err)?;
        atomic_write(&file, &encode_layer(layer))
    }

    fn pages_with_ink(&self) -> CoreResult<Vec<usize>> {
        let dir = self.paths.annotations_dir();
        let rd = match fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_err(e)),
        };
        let mut pages = Vec::new();
        for entry in rd {
            let entry = entry.map_err(io_err)?;
            if let Some(page) = entry.file_name().to_str().and_then(parse_page_file_name) {
                pages.push(page);
            }
        }
        pages.sort_unstable();
        Ok(pages)
    }

    fn load_metadata(&self) -> CoreResult<Option<SidecarMetadata>> {
        match fs::read(self.paths.metadata_file()) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| CoreError::Persistence(format!("metadata.json: {e}"))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_err(e)),
        }
    }

    fn save_metadata(&self, meta: &SidecarMetadata) -> CoreResult<()> {
        fs::create_dir_all(self.paths.root()).map_err(io_err)?;
        let bytes = serde_json::to_vec_pretty(meta)
            .map_err(|e| CoreError::Persistence(format!("metadata.json: {e}")))?;
        atomic_write(&self.paths.metadata_file(), &bytes)
    }

    fn quarantine_page(&self, page: usize) -> CoreResult<()> {
        let src = self.paths.page_file(page);
        let mut dst: OsString = src.as_os_str().to_owned();
        dst.push(".corrupt"); // ".inkbin.corrupt" no longer matches the page-scan suffix
        match fs::rename(&src, Path::new(&dst)) {
            Ok(()) => {
                if let Some(parent) = src.parent() {
                    sync_dir(parent);
                }
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_err(e)),
        }
    }

    fn reset_stale_annotations(&self) -> CoreResult<()> {
        let src = self.paths.annotations_dir();
        let mut dst: OsString = src.as_os_str().to_owned();
        dst.push(".stale");
        let dst = Path::new(&dst);
        let _ = fs::remove_dir_all(dst); // drop any prior quarantine so the rename can land
        match fs::rename(&src, dst) {
            Ok(()) => {
                if let Some(parent) = src.parent() {
                    sync_dir(parent);
                }
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_err(e)),
        }
    }
}

/// In-memory adapter — a store-less session's backing and the unit-test double. Holds encoded
/// `.inkbin` bytes per page so it exercises the same codec path as [`FsInkStore`].
#[derive(Debug, Default)]
pub struct MemInkStore {
    pages: Mutex<BTreeMap<usize, Vec<u8>>>,
    meta: Mutex<Option<SidecarMetadata>>,
}

impl MemInkStore {
    /// An empty in-memory store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl InkStore for MemInkStore {
    fn load_page(&self, page: usize) -> CoreResult<InkLayer> {
        let pages = self.pages.lock().unwrap_or_else(|e| e.into_inner());
        match pages.get(&page) {
            Some(bytes) => Ok(decode_layer(bytes)?),
            None => Ok(InkLayer::new()),
        }
    }

    fn save_page(&self, page: usize, layer: &InkLayer) -> CoreResult<()> {
        let mut pages = self.pages.lock().unwrap_or_else(|e| e.into_inner());
        if layer.is_empty() {
            pages.remove(&page);
        } else {
            pages.insert(page, encode_layer(layer));
        }
        Ok(())
    }

    fn pages_with_ink(&self) -> CoreResult<Vec<usize>> {
        let pages = self.pages.lock().unwrap_or_else(|e| e.into_inner());
        Ok(pages.keys().copied().collect())
    }

    fn load_metadata(&self) -> CoreResult<Option<SidecarMetadata>> {
        Ok(self.meta.lock().unwrap_or_else(|e| e.into_inner()).clone())
    }

    fn save_metadata(&self, meta: &SidecarMetadata) -> CoreResult<()> {
        *self.meta.lock().unwrap_or_else(|e| e.into_inner()) = Some(meta.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::identity::DocIdentity;
    use inkread_ink::{InkColor, InkPoint, Tool};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A dependency-free RAII temp directory under the OS temp dir.
    struct TempDir {
        path: PathBuf,
    }
    impl TempDir {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let mut path = std::env::temp_dir();
            path.push(format!("inkread-test-{}-{n}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn one_stroke_layer() -> InkLayer {
        let mut l = InkLayer::new();
        l.start_stroke(Tool::Pen, InkColor::BLACK, 0.01, 0).unwrap();
        l.push_point(InkPoint::new(0.1, 0.1, 1.0, None, None, 0).unwrap())
            .unwrap();
        l.push_point(InkPoint::new(0.2, 0.2, 1.0, None, None, 8).unwrap())
            .unwrap();
        l.finish_stroke().unwrap();
        l
    }

    fn fs_store(tmp: &TempDir) -> FsInkStore {
        FsInkStore::new(SidecarPaths::from_root(tmp.path.join("book.inkread")))
    }

    #[test]
    fn save_then_load_round_trips_strokes() {
        let tmp = TempDir::new();
        let store = fs_store(&tmp);
        let layer = one_stroke_layer();
        store.save_page(3, &layer).unwrap();
        let back = store.load_page(3).unwrap();
        assert_eq!(back.strokes(), layer.strokes());
    }

    #[test]
    fn load_absent_page_is_empty() {
        let tmp = TempDir::new();
        assert!(fs_store(&tmp).load_page(0).unwrap().is_empty());
    }

    #[test]
    fn empty_layer_removes_the_file() {
        let tmp = TempDir::new();
        let store = fs_store(&tmp);
        store.save_page(0, &one_stroke_layer()).unwrap();
        assert_eq!(store.pages_with_ink().unwrap(), vec![0]);
        store.save_page(0, &InkLayer::new()).unwrap(); // erased to nothing
        assert!(store.pages_with_ink().unwrap().is_empty());
        assert!(store.load_page(0).unwrap().is_empty());
    }

    #[test]
    fn pages_with_ink_lists_sorted_indices() {
        let tmp = TempDir::new();
        let store = fs_store(&tmp);
        for p in [5usize, 0, 2] {
            store.save_page(p, &one_stroke_layer()).unwrap();
        }
        assert_eq!(store.pages_with_ink().unwrap(), vec![0, 2, 5]);
    }

    #[test]
    fn orphan_tmp_is_ignored_and_committed_file_survives() {
        // Simulates a crash mid-write: a stray *.tmp must not be seen as ink, and the previously
        // committed page must still load intact (RR20-AC3 / RR10-AC2).
        let tmp = TempDir::new();
        let store = fs_store(&tmp);
        store.save_page(1, &one_stroke_layer()).unwrap();
        let stray = store.paths.page_file(1);
        let mut orphan: OsString = stray.as_os_str().to_owned();
        orphan.push(".tmp");
        fs::write(&orphan, b"garbage-half-write").unwrap();
        assert_eq!(store.pages_with_ink().unwrap(), vec![1], "tmp ignored");
        assert_eq!(
            store.load_page(1).unwrap().strokes().len(),
            1,
            "committed survives"
        );
    }

    #[test]
    fn oversized_sidecar_is_rejected_before_read() {
        // A page file larger than the cap must be reported corrupt (so the caller quarantines and
        // degrades to empty) WITHOUT being read into memory. Tested with a tiny cap so the file
        // stays small (RR21-FR3).
        let tmp = TempDir::new();
        let store = fs_store(&tmp);
        store.save_page(0, &one_stroke_layer()).unwrap();
        let path = store.paths.page_file(0);
        assert!(matches!(
            read_page_capped(&path, 4),
            Err(CoreError::CorruptDocument(_))
        ));
        // An absent page is still None (not an error), regardless of the cap.
        assert!(read_page_capped(&store.paths.page_file(9), 4)
            .unwrap()
            .is_none());
    }

    #[test]
    fn corrupt_inkbin_is_a_typed_error_not_a_panic() {
        let tmp = TempDir::new();
        let store = fs_store(&tmp);
        store.save_page(0, &one_stroke_layer()).unwrap();
        fs::write(store.paths.page_file(0), b"NOTINKBIN").unwrap();
        assert!(matches!(
            store.load_page(0),
            Err(CoreError::CorruptDocument(_))
        ));
    }

    #[test]
    fn quarantine_clears_the_page_but_preserves_the_bytes() {
        let tmp = TempDir::new();
        let store = fs_store(&tmp);
        store.save_page(0, &one_stroke_layer()).unwrap();
        fs::write(store.paths.page_file(0), b"NOTINKBIN").unwrap(); // corrupt it
        store.quarantine_page(0).unwrap();
        assert!(
            store.load_page(0).unwrap().is_empty(),
            "page now reads empty"
        );
        assert!(store.pages_with_ink().unwrap().is_empty(), "no live ink");
        let mut q: OsString = store.paths.page_file(0).into_os_string();
        q.push(".corrupt");
        assert_eq!(
            fs::read(&q).unwrap(),
            b"NOTINKBIN",
            "corrupt bytes preserved for recovery"
        );
    }

    #[test]
    fn reset_stale_annotations_moves_the_dir_aside() {
        let tmp = TempDir::new();
        let store = fs_store(&tmp);
        store.save_page(0, &one_stroke_layer()).unwrap();
        store.reset_stale_annotations().unwrap();
        assert!(store.pages_with_ink().unwrap().is_empty(), "starts clean");
        let mut stale: OsString = store.paths.annotations_dir().into_os_string();
        stale.push(".stale");
        assert!(
            std::path::Path::new(&stale)
                .join("page-0001.inkbin")
                .exists(),
            "old ink preserved in the .stale dir"
        );
    }

    #[test]
    fn metadata_round_trips_on_disk() {
        let tmp = TempDir::new();
        let store = fs_store(&tmp);
        assert_eq!(store.load_metadata().unwrap(), None);
        let id =
            DocIdentity::from_bytes(b"doc-bytes", &crate::document::DocumentMetadata::default());
        let meta = SidecarMetadata::from_identity(&id, 12);
        store.save_metadata(&meta).unwrap();
        assert_eq!(store.load_metadata().unwrap(), Some(meta));
    }

    #[test]
    fn corrupt_metadata_is_an_error() {
        let tmp = TempDir::new();
        let store = fs_store(&tmp);
        fs::create_dir_all(store.paths.root()).unwrap();
        fs::write(store.paths.metadata_file(), b"{not json").unwrap();
        assert!(store.load_metadata().is_err());
    }

    #[test]
    fn mem_store_matches_fs_semantics() {
        let store = MemInkStore::new();
        assert!(store.load_page(0).unwrap().is_empty());
        store.save_page(2, &one_stroke_layer()).unwrap();
        assert_eq!(store.pages_with_ink().unwrap(), vec![2]);
        assert_eq!(
            store.load_page(2).unwrap().strokes(),
            one_stroke_layer().strokes()
        );
        store.save_page(2, &InkLayer::new()).unwrap();
        assert!(store.pages_with_ink().unwrap().is_empty());
    }
}
