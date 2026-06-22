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
