//! Tests for [`ReaderSession`] (RR12/RR21/RR23/RR24), split out to keep `session.rs` under the
//! size guideline. Included via `#[path]` so `super::*` resolves to the session module.

use super::*;
use crate::persistence::sqlite::SqliteStore;
use crate::render::PixelBuffer;
use crate::settings::{Scope, SettingKey, SettingValue};
use device_eink::{MockDeviceRecorder, RefreshIntent};

/// A stub document with `n` blank pages, for driving the session without pdfium.
struct StubDoc {
    pages: usize,
}
impl Document for StubDoc {
    fn page_count(&self) -> usize {
        self.pages
    }
    fn metadata(&self) -> DocumentMetadata {
        DocumentMetadata {
            title: Some("stub".into()),
            author: None,
        }
    }
    fn render_page(&self, index: usize, buf: &mut PixelBuffer<'_>) -> CoreResult<()> {
        if index >= self.pages {
            return Err(CoreError::PageOutOfRange {
                requested: index,
                available: self.pages,
            });
        }
        buf.fill_white();
        Ok(())
    }
    fn toc(&self) -> Vec<TocEntry> {
        vec![TocEntry {
            title: "Stub Chapter".into(),
            target_page: Some(0),
            children: vec![],
        }]
    }
}

fn session(pages: usize, caps: DeviceCapabilities) -> ReaderSession {
    ReaderSession::with_document(
        Box::new(StubDoc { pages }),
        caps,
        Viewport::new(100, 120, 226),
    )
}

#[test]
fn gesture_code_round_trips() {
    for g in [Gesture::NextPage, Gesture::PrevPage] {
        assert_eq!(Gesture::from_code(g.code()), Some(g));
    }
    assert_eq!(Gesture::from_code(2), None);
    assert_eq!(Gesture::from_code(-1), None);
}

#[test]
fn next_and_prev_advance_and_clamp() {
    let mut s = session(3, DeviceCapabilities::supernote_full());
    assert_eq!(s.current_page(), 0);
    s.on_gesture(Gesture::PrevPage); // clamp at 0
    assert_eq!(s.current_page(), 0);
    s.on_gesture(Gesture::NextPage);
    assert_eq!(s.current_page(), 1);
    s.on_gesture(Gesture::NextPage);
    s.on_gesture(Gesture::NextPage); // clamp at last (2)
    assert_eq!(s.current_page(), 2);
    s.on_gesture(Gesture::PrevPage);
    assert_eq!(s.current_page(), 1);
}

// Amendment 6 + RR3-AC1: gestures delegate to the policy so the promotion is consistent.
#[test]
fn gestures_drive_the_policy_promotion() {
    let caps = DeviceCapabilities::supernote_full();
    let mut s = session(100, caps);
    let mut rec = MockDeviceRecorder::with_profile(caps);
    // Six forward turns => 5 Partial + (WaitForLast, Full).
    for _ in 0..6 {
        let cmds = s.on_gesture(Gesture::NextPage);
        rec.execute_all(cmds);
    }
    assert_eq!(rec.recorded().len(), 7);
    assert_eq!(rec.recorded()[5], RefreshCommand::WaitForLast);
    assert!(matches!(
        rec.recorded()[6],
        RefreshCommand::Update {
            intent: RefreshIntent::Full,
            ..
        }
    ));
}

// RR11-FR1: jump_to_page lands on an absolute page, clamped to the document range, and
// drives the policy like a page turn.
#[test]
fn jump_to_page_clamps_and_drives_policy() {
    let caps = DeviceCapabilities::supernote_full();
    let mut s = session(5, caps);
    let cmds = s.jump_to_page(3);
    assert_eq!(s.current_page(), 3);
    assert!(matches!(
        cmds.as_slice(),
        [RefreshCommand::Update {
            intent: RefreshIntent::Partial,
            ..
        }]
    ));
    // Past the end clamps to the last page.
    s.jump_to_page(99);
    assert_eq!(s.current_page(), 4);
    // Page 0 is reachable.
    s.jump_to_page(0);
    assert_eq!(s.current_page(), 0);
}

// RR11-FR2: toc() passes through to the document.
#[test]
fn toc_passthrough() {
    let s = session(3, DeviceCapabilities::supernote_full());
    let toc = s.toc();
    assert_eq!(toc.len(), 1);
    assert_eq!(toc[0].title, "Stub Chapter");
}

// RR11-AC1: jump_to_toc lands on a resolved target; an unresolved entry doesn't move.
#[test]
fn jump_to_toc_lands_on_target_or_stays() {
    let caps = DeviceCapabilities::supernote_full();
    let mut s = session(10, caps);
    let entry = TocEntry {
        title: "Ch".into(),
        target_page: Some(4),
        children: vec![],
    };
    s.jump_to_toc(&entry);
    assert_eq!(s.current_page(), 4);

    let unresolved = TocEntry {
        title: "label".into(),
        target_page: None,
        children: vec![],
    };
    let cmds = s.jump_to_toc(&unresolved);
    assert_eq!(s.current_page(), 4, "unresolved entry does not move");
    assert!(cmds.is_empty(), "unresolved entry emits no refresh");
}

fn store_session(pages: usize, store: Arc<dyn ReaderStore>, book: BookId) -> ReaderSession {
    ReaderSession::with_document_and_store(
        Box::new(StubDoc { pages }),
        DeviceCapabilities::supernote_full(),
        Viewport::new(100, 120, 226),
        store,
        book,
    )
    .unwrap()
}

// RR12-AC3: a session resumes the saved reading position on open.
#[test]
fn resumes_saved_position_on_open() {
    let store: Arc<dyn ReaderStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
    let book = BookId::new("b").unwrap();
    store
        .save_position(&book, &ReadingPosition::new(3, 10))
        .unwrap();
    let s = store_session(10, store, book);
    assert_eq!(s.current_page(), 3);
}

// RR12-FR3: save_position persists the current page through the store.
#[test]
fn save_position_persists_current_page() {
    let store: Arc<dyn ReaderStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
    let book = BookId::new("b").unwrap();
    let mut s = store_session(10, store.clone(), book.clone());
    s.on_gesture(Gesture::NextPage);
    s.on_gesture(Gesture::NextPage); // page index 2
    s.save_position().unwrap();
    let loaded = store.load_position(&book).unwrap().unwrap();
    assert_eq!(loaded.page_index, 2);
    assert_eq!(loaded.total, 10);
}

// RR12-AC3: a saved position past the current document's end clamps to the last page.
#[test]
fn resume_clamps_to_document_range() {
    let store: Arc<dyn ReaderStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
    let book = BookId::new("b").unwrap();
    store
        .save_position(&book, &ReadingPosition::new(99, 200))
        .unwrap();
    let s = store_session(5, store, book);
    assert_eq!(s.current_page(), 4);
}

// A store-less session: saving is a no-op (M0 path stays green).
#[test]
fn store_less_save_is_noop() {
    let s = session(3, DeviceCapabilities::supernote_full());
    assert!(s.save_position().is_ok());
}

// RR23 ↔ RR3: a persisted flash_interval drives the policy promotion through the session.
#[test]
fn settings_drive_the_policy_interval() {
    let store: Arc<dyn ReaderStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
    store
        .put_setting(
            Scope::Global,
            SettingKey::FlashInterval,
            SettingValue::Int(2),
        )
        .unwrap();
    let mut s = store_session(10, store, BookId::new("b").unwrap());
    // Interval 2: turn 1 Partial, turn 2 promotes to WaitForLast + Full.
    assert!(matches!(
        s.on_gesture(Gesture::NextPage).as_slice(),
        [RefreshCommand::Update {
            intent: RefreshIntent::Partial,
            ..
        }]
    ));
    let second = s.on_gesture(Gesture::NextPage);
    assert_eq!(second.len(), 2);
    assert_eq!(second[0], RefreshCommand::WaitForLast);
}

// RR24-FR3: the session's onTrimMemory hook trims the caches.
#[test]
fn on_trim_memory_clears_caches() {
    let mut s = session(3, DeviceCapabilities::supernote_full());
    s.caches()
        .cover()
        .insert(BookId::new("a").unwrap(), vec![0; 100]);
    assert!(s.caches().total_bytes() > 0);
    s.on_trim_memory(TrimLevel::Critical);
    assert_eq!(s.caches().total_bytes(), 0);
}

#[test]
fn render_rejects_mismatched_buffer() {
    let mut s = session(1, DeviceCapabilities::supernote_full());
    let mut wrong = vec![0u8; 10 * 10 * 4];
    let mut pb = PixelBuffer::from_rgba(&mut wrong, 10, 10).unwrap();
    assert!(matches!(
        s.render_current(&mut pb),
        Err(CoreError::BufferMismatch(_))
    ));
}

#[test]
fn render_current_into_matching_buffer_ok() {
    let mut s = session(1, DeviceCapabilities::supernote_full());
    let mut buf = vec![0u8; 100 * 120 * 4];
    let mut pb = PixelBuffer::from_rgba(&mut buf, 100, 120).unwrap();
    assert!(s.render_current(&mut pb).is_ok());
}

// RR4-FR6 / RR24: a revisited page (same view-settings) is served from the render cache without
// re-rasterizing; a settings change keys a fresh render; a repagination/viewport change drops it.
// RR11: a letterboxed page (page coords ≠ viewport coords) must have its text-selection boxes
// mapped from page space up to viewport space, so the highlight lands on the rendered text.
#[test]
fn text_selection_boxes_are_mapped_page_to_viewport() {
    use crate::document::NormRect;

    /// A doc whose page is letterboxed: page→viewport is y' = y*0.9746 + 0.0125 (x unchanged),
    /// matching a ~0.77-aspect page fit-to-width in a 0.75 viewport. text_line_span returns one
    /// PAGE-space box spanning the page; the session must map it to viewport space.
    struct LetterboxDoc;
    impl Document for LetterboxDoc {
        fn page_count(&self) -> usize {
            1
        }
        fn metadata(&self) -> DocumentMetadata {
            DocumentMetadata::default()
        }
        fn render_page(&self, _i: usize, buf: &mut PixelBuffer<'_>) -> CoreResult<()> {
            buf.fill_white();
            Ok(())
        }
        #[allow(clippy::too_many_arguments)]
        fn page_fit_transform(
            &self,
            _i: usize,
            _vw: u32,
            _vh: u32,
            _m: crate::document::FitMode,
            _px: f32,
            _py: f32,
            _crop: Option<crate::document::NormRect>,
        ) -> Option<(f32, f32, f32, f32)> {
            Some((1.0, 0.0, 0.9746, 0.0125))
        }
        fn text_line_span(&self, _i: usize, _s: (f32, f32), _e: (f32, f32)) -> TextSelection {
            TextSelection {
                text: "hello".into(),
                boxes: vec![NormRect {
                    x0: 0.10,
                    y0: 0.20,
                    x1: 0.80,
                    y1: 0.24,
                }],
            }
        }
    }

    let s = ReaderSession::with_document(
        Box::new(LetterboxDoc),
        DeviceCapabilities::supernote_full(),
        Viewport::new(1920, 2560, 226),
    );
    let sel = s.text_line_span(0, (0.1, 0.2), (0.8, 0.24));
    assert_eq!(sel.boxes.len(), 1);
    let b = sel.boxes[0];
    // x unchanged (page fills width); y mapped through the letterbox affine.
    assert!((b.x0 - 0.10).abs() < 1e-4 && (b.x1 - 0.80).abs() < 1e-4);
    assert!(
        (b.y0 - (0.20 * 0.9746 + 0.0125)).abs() < 1e-4,
        "y0 mapped, got {}",
        b.y0
    );
    assert!(
        (b.y1 - (0.24 * 0.9746 + 0.0125)).abs() < 1e-4,
        "y1 mapped, got {}",
        b.y1
    );
}

#[test]
fn render_cache_serves_revisits_and_invalidates_on_change() {
    use std::cell::Cell;
    use std::rc::Rc;

    /// A document that counts how many times a page was actually rasterized.
    struct CountingDoc {
        pages: usize,
        renders: Rc<Cell<usize>>,
    }
    impl Document for CountingDoc {
        fn page_count(&self) -> usize {
            self.pages
        }
        fn metadata(&self) -> DocumentMetadata {
            DocumentMetadata::default()
        }
        fn render_page(&self, index: usize, buf: &mut PixelBuffer<'_>) -> CoreResult<()> {
            if index >= self.pages {
                return Err(CoreError::PageOutOfRange {
                    requested: index,
                    available: self.pages,
                });
            }
            self.renders.set(self.renders.get() + 1);
            buf.fill_white();
            Ok(())
        }
    }

    let renders = Rc::new(Cell::new(0usize));
    let mut s = ReaderSession::with_document(
        Box::new(CountingDoc {
            pages: 3,
            renders: renders.clone(),
        }),
        DeviceCapabilities::supernote_full(),
        Viewport::new(100, 120, 226),
    );
    let mut buf = vec![0u8; 100 * 120 * 4];
    let render = |s: &mut ReaderSession, buf: &mut [u8]| {
        let mut pb = PixelBuffer::from_rgba(buf, 100, 120).unwrap();
        s.render_current(&mut pb).unwrap();
    };

    render(&mut s, &mut buf); // page 0: miss → rasterize
    assert_eq!(renders.get(), 1);
    render(&mut s, &mut buf); // page 0 again: hit → no rasterize
    assert_eq!(renders.get(), 1);
    assert_eq!(s.caches().render().len(), 1);

    s.on_gesture(Gesture::NextPage);
    render(&mut s, &mut buf); // page 1: miss
    assert_eq!(renders.get(), 2);
    s.on_gesture(Gesture::PrevPage);
    render(&mut s, &mut buf); // back to page 0: still cached → hit
    assert_eq!(renders.get(), 2);

    // A contrast change is part of the key → distinct buffer, fresh rasterize.
    s.set_contrast(5);
    render(&mut s, &mut buf);
    assert_eq!(renders.get(), 3);

    // A viewport change invalidates the cache (geometry changed underneath the keys).
    s.set_viewport(Viewport::new(100, 120, 226));
    assert_eq!(s.caches().render().len(), 0);
}

#[test]
fn basic_panel_session_collapses_to_full() {
    let caps = DeviceCapabilities::supernote_baseline();
    let mut s = session(10, caps);
    let cmds = s.on_gesture(Gesture::NextPage);
    assert!(matches!(
        cmds.as_slice(),
        [RefreshCommand::Update {
            intent: RefreshIntent::Full,
            ..
        }]
    ));
}

// ===== Ink annotation lifecycle (RR6/RR7/RR10/RR20) =====

use crate::persistence::ink_store::{FsInkStore, MemInkStore};
use crate::persistence::sidecar::SidecarPaths;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// A dependency-free RAII temp dir (mirrors the one in `ink_store`'s tests).
struct TempDir {
    path: PathBuf,
}
impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!("inkread-session-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// An `InkStore` whose saves always fail — for the no-data-loss-on-failure test.
struct FailingStore;
impl InkStore for FailingStore {
    fn load_page(&self, _page: usize) -> CoreResult<InkLayer> {
        Ok(InkLayer::new())
    }
    fn save_page(&self, _page: usize, _layer: &InkLayer) -> CoreResult<()> {
        Err(CoreError::Persistence("disk full".into()))
    }
    fn pages_with_ink(&self) -> CoreResult<Vec<usize>> {
        Ok(Vec::new())
    }
    fn load_metadata(&self) -> CoreResult<Option<SidecarMetadata>> {
        Ok(None)
    }
    fn save_metadata(&self, _meta: &SidecarMetadata) -> CoreResult<()> {
        Err(CoreError::Persistence("disk full".into()))
    }
}

/// An `FsInkStore` over `<tmp>/book.pdf`'s sidecar.
fn fs_ink(doc: &std::path::Path) -> Arc<dyn InkStore> {
    Arc::new(FsInkStore::new(SidecarPaths::for_document(doc)))
}

/// An `InkStore` that fails its first `fail_n` saves (transient IO), then delegates to an inner
/// [`MemInkStore`] — exercises the bounded page-turn flush retry (#50).
struct FlakyStore {
    remaining_failures: AtomicU64,
    save_attempts: AtomicU64,
    inner: MemInkStore,
}
impl FlakyStore {
    fn new(fail_n: u64) -> Self {
        Self {
            remaining_failures: AtomicU64::new(fail_n),
            save_attempts: AtomicU64::new(0),
            inner: MemInkStore::new(),
        }
    }
    /// Total `save_page` calls seen (failed + succeeded) — for the no-spurious-save guard.
    fn save_attempts(&self) -> u64 {
        self.save_attempts.load(Ordering::Relaxed)
    }
}
impl InkStore for FlakyStore {
    fn load_page(&self, page: usize) -> CoreResult<InkLayer> {
        self.inner.load_page(page)
    }
    fn save_page(&self, page: usize, layer: &InkLayer) -> CoreResult<()> {
        self.save_attempts.fetch_add(1, Ordering::Relaxed);
        if self.remaining_failures.load(Ordering::Relaxed) > 0 {
            self.remaining_failures.fetch_sub(1, Ordering::Relaxed);
            return Err(CoreError::Persistence("transient IO".into()));
        }
        self.inner.save_page(page, layer)
    }
    fn pages_with_ink(&self) -> CoreResult<Vec<usize>> {
        self.inner.pages_with_ink()
    }
    fn load_metadata(&self) -> CoreResult<Option<SidecarMetadata>> {
        self.inner.load_metadata()
    }
    fn save_metadata(&self, meta: &SidecarMetadata) -> CoreResult<()> {
        self.inner.save_metadata(meta)
    }
}

/// Begin a pen stroke and add points but DON'T finish it — leaves a pending (in-progress) stroke,
/// the mid-stroke state a page turn must not drop (#50).
fn begin_pending(s: &mut ReaderSession, pts: &[(f32, f32)]) {
    s.ink_begin_stroke(Tool::Pen, InkColor::BLACK, 0.01, 0)
        .unwrap();
    for &(x, y) in pts {
        s.ink_add_point(x, y, 1.0, None, None, 0).unwrap();
    }
}

/// Draw and commit one pen stroke through the public session API.
fn draw(s: &mut ReaderSession, pts: &[(f32, f32)]) {
    s.ink_begin_stroke(Tool::Pen, InkColor::BLACK, 0.01, 0)
        .unwrap();
    for &(x, y) in pts {
        s.ink_add_point(x, y, 1.0, None, None, 0).unwrap();
    }
    s.ink_end_stroke().unwrap();
}

#[test]
fn ink_saves_and_reloads_across_sessions() {
    // RR7-AC1: a stroke written on a page reappears after reopen.
    let store: Arc<dyn InkStore> = Arc::new(MemInkStore::new());
    let mut s = session(2, DeviceCapabilities::supernote_full());
    s.attach_ink_store(Arc::clone(&store)).unwrap();
    draw(&mut s, &[(0.1, 0.1), (0.2, 0.2)]);
    assert_eq!(s.ink_strokes().len(), 1);

    // A fresh session over the same sidecar reloads the stroke.
    let mut reopened = session(2, DeviceCapabilities::supernote_full());
    reopened.attach_ink_store(store).unwrap();
    assert_eq!(reopened.ink_strokes().len(), 1, "stroke reloaded on reopen");
}

#[test]
fn ink_undo_redo_autosaves_to_store() {
    let store: Arc<dyn InkStore> = Arc::new(MemInkStore::new());
    let mut s = session(1, DeviceCapabilities::supernote_full());
    s.attach_ink_store(Arc::clone(&store)).unwrap();
    draw(&mut s, &[(0.1, 0.1), (0.2, 0.2)]);
    assert_eq!(store.pages_with_ink().unwrap(), vec![0]);

    assert!(s.ink_undo().unwrap());
    assert!(
        store.pages_with_ink().unwrap().is_empty(),
        "undo autosaved the now-empty page (file removed)"
    );
    assert!(s.ink_redo().unwrap());
    assert_eq!(
        store.pages_with_ink().unwrap(),
        vec![0],
        "redo re-persisted"
    );
}

#[test]
fn page_turn_loads_each_pages_own_ink() {
    let store: Arc<dyn InkStore> = Arc::new(MemInkStore::new());
    let mut s = session(3, DeviceCapabilities::supernote_full());
    s.attach_ink_store(store).unwrap();
    draw(&mut s, &[(0.1, 0.1), (0.2, 0.2)]); // page 0

    s.on_gesture(Gesture::NextPage); // → page 1
    assert_eq!(s.ink_strokes().len(), 0, "page 1 starts empty");
    draw(&mut s, &[(0.5, 0.5), (0.6, 0.6)]); // page 1

    s.on_gesture(Gesture::PrevPage); // back to page 0
    assert_eq!(s.ink_strokes().len(), 1, "page 0 ink reloaded on return");
    s.on_gesture(Gesture::NextPage); // page 1 again
    assert_eq!(s.ink_strokes().len(), 1, "page 1 ink intact");
}

#[test]
fn page_turn_commits_an_in_progress_stroke() {
    // #50: a page turn taken mid-stroke (pen still down) used to drop the pending stroke. It must
    // instead be committed to the OUTGOING page and persisted, then reload on return.
    let store: Arc<dyn InkStore> = Arc::new(MemInkStore::new());
    let mut s = session(2, DeviceCapabilities::supernote_full());
    s.attach_ink_store(Arc::clone(&store)).unwrap();

    begin_pending(&mut s, &[(0.1, 0.1), (0.2, 0.2)]); // pen down on page 0, not finished
    assert_eq!(
        s.ink_strokes().len(),
        0,
        "pending stroke isn't committed yet"
    );

    s.on_gesture(Gesture::NextPage); // turn the page mid-stroke
    assert_eq!(
        store.pages_with_ink().unwrap(),
        vec![0],
        "the in-progress stroke was committed + persisted to the outgoing page"
    );

    s.on_gesture(Gesture::PrevPage); // back to page 0
    assert_eq!(
        s.ink_strokes().len(),
        1,
        "the once-pending stroke reloads on return — not lost"
    );
    assert_eq!(
        s.ink_strokes()[0].points.len(),
        2,
        "the committed stroke kept its points, not just its existence"
    );
}

#[test]
fn page_turn_commits_an_in_progress_erase() {
    // #50 (eraser parity): an erase applied but not yet ended (ink_end_stroke) is the symmetric
    // "disappearing edit" — a page turn must persist it, not let the erased stroke reappear.
    let store: Arc<dyn InkStore> = Arc::new(MemInkStore::new());
    let mut s = session(2, DeviceCapabilities::supernote_full());
    s.attach_ink_store(Arc::clone(&store)).unwrap();
    draw(&mut s, &[(0.10, 0.10), (0.20, 0.10)]); // page 0: a committed stroke
    assert_eq!(store.pages_with_ink().unwrap(), vec![0]);

    // Begin an eraser gesture over the stroke but DON'T end it (pen still down).
    s.ink_begin_stroke(Tool::Eraser, InkColor::BLACK, 0.05, 0)
        .unwrap();
    s.ink_add_point(0.15, 0.10, 1.0, None, None, 0).unwrap();
    assert_eq!(
        s.ink_strokes().len(),
        0,
        "erase removed the stroke in-memory"
    );

    s.on_gesture(Gesture::NextPage); // turn mid-erase
    assert!(
        store.pages_with_ink().unwrap().is_empty(),
        "the in-progress erase was persisted to the outgoing page (file removed)"
    );
    s.on_gesture(Gesture::PrevPage);
    assert_eq!(
        s.ink_strokes().len(),
        0,
        "the erased stroke stays gone on return — the erase wasn't lost"
    );
}

#[test]
fn page_turn_proceeds_when_flush_exhausts_its_retries() {
    // #50 / RR20 degrade-safely: if every retry fails (e.g. ENOSPC), navigation must still proceed
    // — never block, never panic — even though the outgoing page's ink is lost.
    let store = Arc::new(FlakyStore::new(u64::MAX)); // never succeeds
    let mut s = session(2, DeviceCapabilities::supernote_full());
    s.attach_ink_store(Arc::clone(&store) as Arc<dyn InkStore>)
        .unwrap();

    begin_pending(&mut s, &[(0.1, 0.1), (0.2, 0.2)]);
    s.on_gesture(Gesture::NextPage); // must not panic / must not hang
    assert_eq!(
        s.current_page(),
        1,
        "navigation proceeded despite a hard write failure"
    );
    assert!(
        store.pages_with_ink().unwrap().is_empty(),
        "nothing persisted on a hard failure — but we degraded safely"
    );
}

#[test]
fn deferred_mode_page_turn_flush_retries_transient_io() {
    // AC#3: in DEFERRED mode the outgoing page's pending edits are flushed on the turn, riding out
    // transient IO with the retry (the immediate-mode retry is covered separately).
    let store = Arc::new(FlakyStore::new(2)); // first 2 saves fail, 3rd (in budget) succeeds
    let mut s = session(2, DeviceCapabilities::supernote_full());
    s.attach_ink_store(Arc::clone(&store) as Arc<dyn InkStore>)
        .unwrap();
    s.set_autosave_deferred(true).unwrap();

    draw(&mut s, &[(0.1, 0.1), (0.2, 0.2)]); // page 0, held in memory (ink_dirty), not yet saved
    assert!(store.pages_with_ink().unwrap().is_empty());

    s.on_gesture(Gesture::NextPage); // flush-with-retry over the 2 transient failures
    assert_eq!(
        store.pages_with_ink().unwrap(),
        vec![0],
        "deferred page-0 edits persisted via the page-turn flush retry"
    );
}

#[test]
fn clean_page_turn_does_not_resave_immediate_mode() {
    // A page turn with no pending stroke and no in-progress erase must not rewrite the outgoing page
    // (immediate mode already saved it per stroke-end) — avoids a needless e-ink-relevant write.
    let store = Arc::new(FlakyStore::new(0)); // never fails; counts save_page calls
    let mut s = session(2, DeviceCapabilities::supernote_full());
    s.attach_ink_store(Arc::clone(&store) as Arc<dyn InkStore>)
        .unwrap();
    draw(&mut s, &[(0.1, 0.1), (0.2, 0.2)]); // exactly one save (immediate stroke-end)
    let before = store.save_attempts();

    s.on_gesture(Gesture::NextPage); // clean turn: nothing to persist
    assert_eq!(
        store.save_attempts(),
        before,
        "a clean page turn did not re-save the outgoing page"
    );
}

#[test]
fn page_turn_commits_an_in_progress_highlighter_stroke() {
    // Highlighter is_ink()==true, so it shares the pen commit-on-turn path — pin it explicitly.
    let store: Arc<dyn InkStore> = Arc::new(MemInkStore::new());
    let mut s = session(2, DeviceCapabilities::supernote_full());
    s.attach_ink_store(Arc::clone(&store)).unwrap();
    s.ink_begin_stroke(Tool::Highlighter, InkColor::BLACK, 0.03, 0)
        .unwrap();
    s.ink_add_point(0.1, 0.1, 1.0, None, None, 0).unwrap();
    s.ink_add_point(0.3, 0.1, 1.0, None, None, 0).unwrap();

    s.on_gesture(Gesture::NextPage);
    s.on_gesture(Gesture::PrevPage);
    assert_eq!(
        s.ink_strokes().len(),
        1,
        "an in-progress highlighter stroke survives a page turn too"
    );
}

#[test]
fn page_turn_preserves_committed_strokes_plus_the_pending_one() {
    // A page with several committed strokes AND a pending one must reload ALL of them.
    let store: Arc<dyn InkStore> = Arc::new(MemInkStore::new());
    let mut s = session(2, DeviceCapabilities::supernote_full());
    s.attach_ink_store(Arc::clone(&store)).unwrap();
    draw(&mut s, &[(0.1, 0.1), (0.2, 0.2)]);
    draw(&mut s, &[(0.3, 0.3), (0.4, 0.4)]);
    begin_pending(&mut s, &[(0.5, 0.5), (0.6, 0.6)]); // 3rd, not finished

    s.on_gesture(Gesture::NextPage);
    s.on_gesture(Gesture::PrevPage);
    assert_eq!(
        s.ink_strokes().len(),
        3,
        "two committed + one once-pending stroke all reload"
    );
}

#[test]
fn committed_stroke_survives_a_simulated_crash() {
    // #50: in immediate mode a finished stroke is atomically written (fsync + rename) on stroke-end,
    // so a hard kill with no graceful close still recovers it. Use a real FS sidecar; "crash" =
    // open a fresh store over the same files without any save/close on the first session.
    let tmp = TempDir::new();
    let doc = tmp.path.join("book.pdf");

    let mut s = session(1, DeviceCapabilities::supernote_full());
    s.attach_ink_store(fs_ink(&doc)).unwrap();
    draw(&mut s, &[(0.1, 0.1), (0.2, 0.2)]);
    drop(s); // ReaderSession has no Drop/flush, so this is exactly a hard kill: nothing flushed here

    let mut reopened = session(1, DeviceCapabilities::supernote_full());
    reopened.attach_ink_store(fs_ink(&doc)).unwrap();
    assert_eq!(
        reopened.ink_strokes().len(),
        1,
        "the committed stroke was durable on disk before the crash"
    );
}

#[test]
fn page_turn_flush_retries_transient_io() {
    // #50: the outgoing-page flush retries a transient IO error so a momentary hiccup doesn't drop
    // the stroke. FlakyStore fails the first 2 saves; the 3rd (within the retry budget) persists.
    let store = Arc::new(FlakyStore::new(2));
    let mut s = session(2, DeviceCapabilities::supernote_full());
    s.attach_ink_store(Arc::clone(&store) as Arc<dyn InkStore>)
        .unwrap();

    begin_pending(&mut s, &[(0.1, 0.1), (0.2, 0.2)]); // page 0, pen down
    s.on_gesture(Gesture::NextPage); // commit + flush with retry over the 2 transient failures

    assert_eq!(
        store.pages_with_ink().unwrap(),
        vec![0],
        "the retry rode out the transient failures and persisted the stroke"
    );
}

#[test]
fn eraser_removes_strokes_and_autosaves() {
    let store: Arc<dyn InkStore> = Arc::new(MemInkStore::new());
    let mut s = session(1, DeviceCapabilities::supernote_full());
    s.attach_ink_store(Arc::clone(&store)).unwrap();
    draw(&mut s, &[(0.10, 0.10), (0.20, 0.10)]);

    s.ink_begin_stroke(Tool::Eraser, InkColor::BLACK, 0.03, 0)
        .unwrap();
    s.ink_add_point(0.15, 0.10, 1.0, None, None, 0).unwrap();
    s.ink_end_stroke().unwrap();
    assert_eq!(s.ink_strokes().len(), 0);
    assert!(
        store.pages_with_ink().unwrap().is_empty(),
        "erase autosaved the empty page"
    );
}

#[test]
fn deferred_autosave_holds_writes_until_an_explicit_flush() {
    // The power knob: in deferred mode a stroke is held in memory and not fsynced on stroke-end;
    // the shell's debounced save_ink flushes it. (Default immediate mode is covered above.)
    let store: Arc<dyn InkStore> = Arc::new(MemInkStore::new());
    let mut s = session(1, DeviceCapabilities::supernote_full());
    s.attach_ink_store(Arc::clone(&store)).unwrap();
    s.set_autosave_deferred(true).unwrap();

    draw(&mut s, &[(0.1, 0.1), (0.2, 0.2)]);
    assert_eq!(s.ink_strokes().len(), 1, "stroke is live in the layer");
    assert!(
        store.pages_with_ink().unwrap().is_empty(),
        "deferred: nothing persisted on stroke-end"
    );

    s.save_ink().unwrap();
    assert_eq!(
        store.pages_with_ink().unwrap(),
        vec![0],
        "explicit flush persists the page"
    );
}

#[test]
fn deferred_autosave_flushes_before_a_page_turn() {
    // A page turn must flush the outgoing page's pending edits, or deferred ink would be lost when
    // the layer is swapped for the new page.
    let store: Arc<dyn InkStore> = Arc::new(MemInkStore::new());
    let mut s = session(2, DeviceCapabilities::supernote_full());
    s.attach_ink_store(Arc::clone(&store)).unwrap();
    s.set_autosave_deferred(true).unwrap();

    draw(&mut s, &[(0.1, 0.1), (0.2, 0.2)]); // page 0, not yet flushed
    assert!(store.pages_with_ink().unwrap().is_empty());

    s.on_gesture(Gesture::NextPage); // flushes page 0 before loading page 1
    assert_eq!(
        store.pages_with_ink().unwrap(),
        vec![0],
        "page 0 flushed on the turn"
    );
    assert_eq!(s.ink_strokes().len(), 0, "page 1 starts empty");
}

#[test]
fn disabling_deferred_autosave_flushes_pending_edits() {
    let store: Arc<dyn InkStore> = Arc::new(MemInkStore::new());
    let mut s = session(1, DeviceCapabilities::supernote_full());
    s.attach_ink_store(Arc::clone(&store)).unwrap();
    s.set_autosave_deferred(true).unwrap();
    draw(&mut s, &[(0.1, 0.1), (0.2, 0.2)]);
    assert!(store.pages_with_ink().unwrap().is_empty());

    s.set_autosave_deferred(false).unwrap(); // turning the knob off must not strand the edit
    assert_eq!(store.pages_with_ink().unwrap(), vec![0]);
}

#[test]
fn ink_add_points_batches_a_whole_stroke() {
    let mut s = session(1, DeviceCapabilities::supernote_full());
    s.ink_begin_stroke(Tool::Pen, InkColor::BLACK, 0.01, 0)
        .unwrap();
    // Packed [x0,y0,x1,y1,x2,y2]; the trailing odd float is ignored.
    s.ink_add_points(&[0.1, 0.1, 0.2, 0.2, 0.3, 0.3, 0.9])
        .unwrap();
    s.ink_end_stroke().unwrap();
    let strokes = s.ink_strokes();
    assert_eq!(strokes.len(), 1);
    assert_eq!(strokes[0].points.len(), 3, "three (x,y) pairs landed");
}

#[test]
fn validate_export_path_contains_the_write_target() {
    let dir = std::env::temp_dir();
    let ok = dir.join("inkread-export.pdf");
    assert!(validate_export_path(ok.to_str().unwrap()).is_ok());

    // Relative, traversal, and non-existent-parent paths are all refused.
    assert!(validate_export_path("export.pdf").is_err());
    assert!(validate_export_path(dir.join("../export.pdf").to_str().unwrap()).is_err());
    assert!(validate_export_path("/no/such/dir/export.pdf").is_err());
    assert!(validate_export_path("").is_err());
}

#[test]
fn ink_works_in_memory_without_a_store() {
    // A store-less session still captures and edits ink (just no persistence) — no panic.
    let mut s = session(1, DeviceCapabilities::supernote_full());
    draw(&mut s, &[(0.1, 0.1), (0.2, 0.2)]);
    assert_eq!(s.ink_strokes().len(), 1);
    assert!(s.ink_undo().unwrap());
    assert_eq!(s.ink_strokes().len(), 0);
}

// RR7-AC1, on the REAL on-disk path (not the in-memory double): a stroke drawn on a virgin
// path — no sidecar dir yet — round-trips through FsInkStore + the .inkbin codec + atomic write.
#[test]
fn ink_round_trips_through_fsinkstore_on_a_virgin_path() {
    let tmp = TempDir::new();
    let doc = tmp.path.join("book.pdf"); // nothing exists beside it
    let mut s = session(2, DeviceCapabilities::supernote_full());
    s.attach_ink_store(fs_ink(&doc)).unwrap();
    draw(&mut s, &[(0.1, 0.1), (0.2, 0.2)]);

    let mut reopened = session(2, DeviceCapabilities::supernote_full());
    reopened.attach_ink_store(fs_ink(&doc)).unwrap();
    assert_eq!(
        reopened.ink_strokes().len(),
        1,
        "stroke persisted to disk and reloaded on reopen"
    );
}

// Inconsistent-degradation fix: a corrupt landing-page .inkbin must NOT block open; the page
// shows empty and the bad bytes are quarantined (not clobbered).
#[test]
fn corrupt_landing_page_still_opens_and_quarantines() {
    let tmp = TempDir::new();
    let doc = tmp.path.join("book.pdf");
    let paths = SidecarPaths::for_document(&doc);
    {
        let mut s = session(2, DeviceCapabilities::supernote_full());
        s.attach_ink_store(fs_ink(&doc)).unwrap();
        draw(&mut s, &[(0.1, 0.1), (0.2, 0.2)]);
    }
    std::fs::write(paths.page_file(0), b"NOTINKBIN").unwrap(); // corrupt page 0

    let mut reopened = session(2, DeviceCapabilities::supernote_full());
    assert!(
        reopened.attach_ink_store(fs_ink(&doc)).is_ok(),
        "document still opens despite a corrupt page"
    );
    assert_eq!(reopened.ink_strokes().len(), 0, "corrupt page shows empty");
}

// RR20-FR1: a failed autosave surfaces an error but must NOT lose the in-memory stroke (retryable).
#[test]
fn autosave_failure_surfaces_but_keeps_strokes_in_memory() {
    let mut s = session(1, DeviceCapabilities::supernote_full());
    s.attach_ink_store(Arc::new(FailingStore)).unwrap();
    s.ink_begin_stroke(Tool::Pen, InkColor::BLACK, 0.01, 0)
        .unwrap();
    s.ink_add_point(0.1, 0.1, 1.0, None, None, 0).unwrap();
    assert!(s.ink_end_stroke().is_err(), "save failure is surfaced");
    assert_eq!(
        s.ink_strokes().len(),
        1,
        "stroke retained in memory for retry — no data loss"
    );
}

// RR10-FR6/AC3 wiring: attach stamps the sidecar with the document's identity.
#[test]
fn attach_stamps_and_matches_document_identity() {
    let mut s = session(3, DeviceCapabilities::supernote_full());
    let id = DocIdentity::from_bytes(b"the-doc-bytes", &DocumentMetadata::default());
    s.identity = Some(id.clone()); // (test reaches into the private field; open_pdf sets it for real)
    let store: Arc<dyn InkStore> = Arc::new(MemInkStore::new());
    s.attach_ink_store(Arc::clone(&store)).unwrap();
    let meta = store
        .load_metadata()
        .unwrap()
        .expect("identity stamped on attach");
    assert!(meta.matches(&id));
    assert_eq!(meta.page_count, 3);
}

#[test]
fn empty_erase_does_not_rewrite_the_page() {
    let store: Arc<dyn InkStore> = Arc::new(MemInkStore::new());
    let mut s = session(1, DeviceCapabilities::supernote_full());
    s.attach_ink_store(Arc::clone(&store)).unwrap();
    draw(&mut s, &[(0.1, 0.1), (0.2, 0.1)]);
    // an eraser gesture that hits nothing must not rewrite/remove the page
    s.ink_begin_stroke(Tool::Eraser, InkColor::BLACK, 0.02, 0)
        .unwrap();
    s.ink_add_point(0.9, 0.9, 1.0, None, None, 0).unwrap();
    s.ink_end_stroke().unwrap();
    assert_eq!(s.ink_strokes().len(), 1);
    assert_eq!(store.pages_with_ink().unwrap(), vec![0]);
}

#[test]
fn eraser_rejects_non_positive_radius() {
    let mut s = session(1, DeviceCapabilities::supernote_full());
    assert!(s
        .ink_begin_stroke(Tool::Eraser, InkColor::BLACK, 0.0, 0)
        .is_err());
    assert!(s
        .ink_begin_stroke(Tool::Eraser, InkColor::BLACK, f32::NAN, 0)
        .is_err());
}

// ===== Lasso selection at the session layer (ADR-INKREAD-0010, M-Lasso-2) =====

/// A box polygon covering the page centre (0.3..0.7), where `draw` strokes are placed.
fn centre_box() -> Vec<(f32, f32)> {
    vec![(0.3, 0.3), (0.7, 0.3), (0.7, 0.7), (0.3, 0.7)]
}

#[test]
fn lasso_select_and_move_persists_and_undoes() {
    let store: Arc<dyn InkStore> = Arc::new(MemInkStore::new());
    let mut s = session(1, DeviceCapabilities::supernote_full());
    s.attach_ink_store(Arc::clone(&store)).unwrap();
    draw(&mut s, &[(0.45, 0.45), (0.55, 0.55)]);
    let sel = s.ink_select_in_polygon(&centre_box(), 0).unwrap(); // Smart
    assert_eq!(sel.len(), 1);

    assert!(s.ink_move_selection(&sel, 0.05, 0.0).unwrap());
    let moved_x = s.ink_strokes()[0].points[0].x;
    assert!((moved_x - 0.50).abs() < 1e-5);
    assert_eq!(store.pages_with_ink().unwrap(), vec![0], "move autosaved");

    assert!(s.ink_undo().unwrap());
    assert!((s.ink_strokes()[0].points[0].x - 0.45).abs() < 1e-5);
}

#[test]
fn lasso_delete_persists_and_leaves_others() {
    let store: Arc<dyn InkStore> = Arc::new(MemInkStore::new());
    let mut s = session(1, DeviceCapabilities::supernote_full());
    s.attach_ink_store(Arc::clone(&store)).unwrap();
    draw(&mut s, &[(0.45, 0.45), (0.55, 0.55)]); // inside the box
    draw(&mut s, &[(0.05, 0.05), (0.08, 0.08)]); // outside
    let sel = s.ink_select_in_polygon(&centre_box(), 1).unwrap(); // Freehand
    assert_eq!(sel.len(), 1);

    let removed = s.ink_delete_selection(&sel).unwrap();
    assert_eq!(removed, sel);
    assert_eq!(s.ink_strokes().len(), 1, "the outside stroke survives");
    assert!(s.ink_undo().unwrap());
    assert_eq!(s.ink_strokes().len(), 2, "delete undoes atomically");
}

#[test]
fn lasso_copy_paste_duplicates_on_the_same_page() {
    let mut s = session(1, DeviceCapabilities::supernote_full());
    s.attach_ink_store(Arc::new(MemInkStore::new())).unwrap();
    draw(&mut s, &[(0.45, 0.45), (0.55, 0.55)]);
    let sel = s.ink_select_all();
    assert_eq!(s.ink_copy_selection(&sel), 1);
    assert!(s.ink_has_clipboard());
    let pasted = s.ink_paste(0.1, 0.1).unwrap();
    assert_eq!(pasted.len(), 1);
    assert_eq!(s.ink_strokes().len(), 2, "paste added a copy");
}

#[test]
fn lasso_cut_then_paste_moves_strokes_across_pages() {
    // NeoReader's cross-page clipboard: cut on page 0, paste on page 1.
    let store: Arc<dyn InkStore> = Arc::new(MemInkStore::new());
    let mut s = session(2, DeviceCapabilities::supernote_full());
    s.attach_ink_store(Arc::clone(&store)).unwrap();
    draw(&mut s, &[(0.45, 0.45), (0.55, 0.55)]); // page 0
    let sel = s.ink_select_all();
    let cut = s.ink_cut_selection(&sel).unwrap();
    assert_eq!(cut.len(), 1);
    assert!(s.ink_strokes().is_empty(), "page 0 emptied by cut");

    s.on_gesture(Gesture::NextPage); // → page 1
    assert!(s.ink_strokes().is_empty());
    let pasted = s.ink_paste(0.0, 0.0).unwrap();
    assert_eq!(pasted.len(), 1, "clipboard survived the page turn");
    assert_eq!(s.ink_strokes().len(), 1);
    assert_eq!(
        store.pages_with_ink().unwrap(),
        vec![1],
        "ink now on page 1"
    );
}

#[test]
fn lasso_rejects_an_unknown_mode_code() {
    let s = session(1, DeviceCapabilities::supernote_full());
    assert!(s.ink_select_in_polygon(&centre_box(), 9).is_err());
}

// RR4: contrast is a post-render pixel remap. A gray page renders darker (below mid-gray) at a
// higher contrast step; step 0 is identity. Uses a gray-filling doc so the effect is visible.
struct GrayDoc(u8);
impl Document for GrayDoc {
    fn page_count(&self) -> usize {
        1
    }
    fn metadata(&self) -> DocumentMetadata {
        DocumentMetadata::default()
    }
    fn render_page(&self, _index: usize, buf: &mut PixelBuffer<'_>) -> CoreResult<()> {
        for px in buf.bytes_mut().chunks_exact_mut(4) {
            px[0] = self.0;
            px[1] = self.0;
            px[2] = self.0;
            px[3] = 0xFF;
        }
        Ok(())
    }
}

#[test]
fn contrast_step_clamps_and_round_trips() {
    let mut s = session(1, DeviceCapabilities::supernote_full());
    assert_eq!(s.contrast(), 0);
    s.set_contrast(99); // clamps to MAX
    assert_eq!(s.contrast(), crate::render::contrast::MAX_CONTRAST_STEP);
    s.set_contrast(3);
    assert_eq!(s.contrast(), 3);
}

#[test]
fn contrast_darkens_a_gray_page_after_render() {
    let mut s = ReaderSession::with_document(
        Box::new(GrayDoc(80)), // below mid-gray → contrast pushes it darker
        DeviceCapabilities::supernote_full(),
        Viewport::new(8, 8, 226),
    );
    let mut bytes = vec![0u8; 8 * 8 * 4];

    s.set_contrast(0);
    {
        let mut buf = PixelBuffer::from_rgba(&mut bytes, 8, 8).unwrap();
        s.render_current(&mut buf).unwrap();
    }
    assert_eq!(bytes[0], 80, "step 0 is identity");

    s.set_contrast(crate::render::contrast::MAX_CONTRAST_STEP);
    {
        let mut buf = PixelBuffer::from_rgba(&mut bytes, 8, 8).unwrap();
        s.render_current(&mut buf).unwrap();
    }
    assert!(
        bytes[0] < 80,
        "higher contrast darkened the gray page: {}",
        bytes[0]
    );
    assert_eq!(bytes[3], 0xFF, "alpha preserved");
}
