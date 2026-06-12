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
        let s = session(1, DeviceCapabilities::supernote_full());
        let mut wrong = vec![0u8; 10 * 10 * 4];
        let mut pb = PixelBuffer::from_rgba(&mut wrong, 10, 10).unwrap();
        assert!(matches!(
            s.render_current(&mut pb),
            Err(CoreError::BufferMismatch(_))
        ));
    }

    #[test]
    fn render_current_into_matching_buffer_ok() {
        let s = session(1, DeviceCapabilities::supernote_full());
        let mut buf = vec![0u8; 100 * 120 * 4];
        let mut pb = PixelBuffer::from_rgba(&mut buf, 100, 120).unwrap();
        assert!(s.render_current(&mut pb).is_ok());
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
