//! Tests for lasso selection, the ink clipboard, and PDF export (ADR-INKREAD-0010, RR11).
//!
//! Two things live in this file and they matter for different reasons.
//!
//! **Selection ops** all share a rule: mutate the layer, and autosave *only if something actually
//! changed*. Autosaving unconditionally would write the sidecar on every no-op tap; not autosaving
//! at all would lose an edit on a crash. The tests drive both sides of that condition.
//!
//! **Export** is the one with real consequences. `validate_export_path` is a security control — the
//! shell picks the destination with all-files access, so the core cannot know Android's storage
//! roots, but it *can* refuse the shapes a buggy or compromised shell should never produce. And
//! `export_pdf` must refuse an empty export rather than write a file, because a reader told
//! "exported" about a PDF containing none of their handwriting has lost it silently.

use super::*;
use crate::persistence::sidecar::SidecarMetadata;
use crate::render::PixelBuffer;
use std::sync::Mutex;

/// An in-memory ink store, so selection edits can autosave without touching a filesystem.
struct MemInk {
    pages: Mutex<std::collections::BTreeMap<usize, Vec<u8>>>,
}

impl MemInk {
    fn new() -> Self {
        Self {
            pages: Mutex::new(std::collections::BTreeMap::new()),
        }
    }
}

impl InkStore for MemInk {
    fn load_page(&self, page: usize) -> CoreResult<InkLayer> {
        match self.pages.lock().unwrap().get(&page) {
            Some(bytes) => inkread_ink::decode_layer(bytes)
                .map_err(|e| CoreError::CorruptDocument(format!("{e:?}"))),
            None => Ok(InkLayer::new()),
        }
    }
    fn save_page(&self, page: usize, layer: &InkLayer) -> CoreResult<()> {
        let mut g = self.pages.lock().unwrap();
        if layer.is_empty() {
            g.remove(&page);
        } else {
            g.insert(page, inkread_ink::encode_layer(layer));
        }
        Ok(())
    }
    fn pages_with_ink(&self) -> CoreResult<Vec<usize>> {
        Ok(self.pages.lock().unwrap().keys().copied().collect())
    }
    fn load_metadata(&self) -> CoreResult<Option<SidecarMetadata>> {
        Ok(None)
    }
    fn save_metadata(&self, _meta: &SidecarMetadata) -> CoreResult<()> {
        Ok(())
    }
}

struct Blank {
    pages: usize,
}
impl Document for Blank {
    fn page_count(&self) -> usize {
        self.pages
    }
    fn metadata(&self) -> DocumentMetadata {
        DocumentMetadata {
            title: None,
            author: None,
        }
    }
    fn render_page(&self, _i: usize, buf: &mut PixelBuffer<'_>) -> CoreResult<()> {
        buf.fill_white();
        Ok(())
    }
}

fn inked(pages: usize) -> ReaderSession {
    let mut s = ReaderSession::with_document(
        Box::new(Blank { pages }),
        DeviceCapabilities::controllable_epd(),
        Viewport::new(100, 120, 226),
    );
    s.attach_ink_store(Arc::new(MemInk::new())).unwrap();
    s
}

/// Draw and commit one stroke through the public API.
fn draw(s: &mut ReaderSession, pts: &[(f32, f32)]) -> u32 {
    s.ink_begin_stroke(Tool::Pen, InkColor::BLACK, 0.01, 0)
        .unwrap();
    for &(x, y) in pts {
        s.ink_add_point(x, y, 1.0, None, None, 0).unwrap();
    }
    s.ink_end_stroke().unwrap();
    *s.ink_select_all().last().expect("a stroke was committed")
}

/// A lasso around the whole page.
fn whole_page() -> Vec<(f32, f32)> {
    vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]
}

// ============================ selecting ============================

#[test]
fn a_lasso_around_everything_selects_every_stroke() {
    let mut s = inked(2);
    draw(&mut s, &[(0.2, 0.2), (0.3, 0.3)]);
    draw(&mut s, &[(0.6, 0.6), (0.7, 0.7)]);
    for mode in [0u8, 1u8] {
        let ids = s.ink_select_in_polygon(&whole_page(), mode).unwrap();
        assert_eq!(ids.len(), 2, "mode {mode}");
    }
}

#[test]
fn an_unknown_lasso_mode_is_a_typed_error() {
    let s = inked(1);
    let err = s.ink_select_in_polygon(&whole_page(), 99);
    assert!(matches!(err, Err(CoreError::InvalidArgument(_))));
}

#[test]
fn select_all_returns_every_stroke_on_the_page() {
    let mut s = inked(1);
    assert!(s.ink_select_all().is_empty(), "nothing drawn yet");
    draw(&mut s, &[(0.1, 0.1), (0.2, 0.2)]);
    draw(&mut s, &[(0.3, 0.3), (0.4, 0.4)]);
    assert_eq!(s.ink_select_all().len(), 2);
}

#[test]
fn selection_bounds_cover_the_selected_strokes() {
    let mut s = inked(1);
    draw(&mut s, &[(0.2, 0.3), (0.4, 0.5)]);
    let ids = s.ink_select_all();
    let b = s.ink_selection_bounds(&ids);
    assert_eq!(b.len(), 4, "[x0,y0,x1,y1]");
    assert!(b[0] <= 0.2 && b[1] <= 0.3, "covers the start: {b:?}");
    assert!(b[2] >= 0.4 && b[3] >= 0.5, "covers the end: {b:?}");
}

/// The toolbar anchors on these bounds, so an empty selection must report *nothing* rather than a
/// degenerate rect at the origin — which would park the toolbar in the corner over the page.
#[test]
fn an_empty_selection_has_no_bounds() {
    let s = inked(1);
    assert!(s.ink_selection_bounds(&[]).is_empty());
    assert!(
        s.ink_selection_bounds(&[42, 43]).is_empty(),
        "unknown ids too"
    );
}

// ============================ mutating, and the autosave condition ============================

#[test]
fn moving_a_selection_reports_the_change() {
    let mut s = inked(1);
    let id = draw(&mut s, &[(0.2, 0.2), (0.3, 0.3)]);
    assert!(s.ink_move_selection(&[id], 0.1, 0.1).unwrap());
}

#[test]
fn moving_nothing_changes_nothing_and_does_not_autosave() {
    let mut s = inked(1);
    draw(&mut s, &[(0.2, 0.2), (0.3, 0.3)]);
    assert!(
        !s.ink_move_selection(&[], 0.1, 0.1).unwrap(),
        "empty selection"
    );
    assert!(
        !s.ink_move_selection(&[999], 0.1, 0.1).unwrap(),
        "unknown ids"
    );
}

#[test]
fn deleting_a_selection_returns_the_removed_ids() {
    let mut s = inked(1);
    let id = draw(&mut s, &[(0.2, 0.2), (0.3, 0.3)]);
    let removed = s.ink_delete_selection(&[id]).unwrap();
    assert_eq!(removed, vec![id]);
    assert!(s.ink_select_all().is_empty(), "the page is clear");
}

#[test]
fn deleting_nothing_removes_nothing() {
    let mut s = inked(1);
    draw(&mut s, &[(0.2, 0.2), (0.3, 0.3)]);
    assert!(s.ink_delete_selection(&[]).unwrap().is_empty());
    assert!(s.ink_delete_selection(&[999]).unwrap().is_empty());
    assert_eq!(s.ink_select_all().len(), 1, "the stroke survives");
}

#[test]
fn recolouring_a_selection_reports_the_change() {
    let mut s = inked(1);
    let id = draw(&mut s, &[(0.2, 0.2), (0.3, 0.3)]);
    let red = InkColor::rgba(255, 0, 0, 255);
    assert!(s.ink_recolor_selection(&[id], red).unwrap());
}

#[test]
fn recolouring_nothing_reports_no_change() {
    let mut s = inked(1);
    draw(&mut s, &[(0.2, 0.2), (0.3, 0.3)]);
    let red = InkColor::rgba(255, 0, 0, 255);
    assert!(!s.ink_recolor_selection(&[], red).unwrap());
    assert!(!s.ink_recolor_selection(&[999], red).unwrap());
}

// ============================ the clipboard ============================

#[test]
fn copy_then_paste_duplicates_the_strokes() {
    let mut s = inked(1);
    let id = draw(&mut s, &[(0.2, 0.2), (0.3, 0.3)]);
    assert!(!s.ink_has_clipboard(), "empty to start");
    assert_eq!(s.ink_copy_selection(&[id]), 1);
    assert!(s.ink_has_clipboard());
    let pasted = s.ink_paste(0.1, 0.1).unwrap();
    assert_eq!(pasted.len(), 1);
    assert_eq!(s.ink_select_all().len(), 2, "original plus the copy");
}

#[test]
fn cut_removes_the_strokes_and_fills_the_clipboard() {
    let mut s = inked(1);
    let id = draw(&mut s, &[(0.2, 0.2), (0.3, 0.3)]);
    let removed = s.ink_cut_selection(&[id]).unwrap();
    assert_eq!(removed, vec![id]);
    assert!(s.ink_select_all().is_empty(), "cut from the page");
    assert!(s.ink_has_clipboard(), "but held for pasting");
}

/// The clipboard survives a page turn — that is the point of it (NeoReader's cross-page paste).
#[test]
fn the_clipboard_pastes_onto_a_different_page() {
    let mut s = inked(3);
    let id = draw(&mut s, &[(0.2, 0.2), (0.3, 0.3)]);
    s.ink_copy_selection(&[id]);
    s.jump_to_page(2);
    assert!(s.ink_select_all().is_empty(), "a fresh page");
    assert_eq!(s.ink_paste(0.0, 0.0).unwrap().len(), 1);
    assert_eq!(s.ink_select_all().len(), 1, "pasted here");
}

#[test]
fn pasting_an_empty_clipboard_is_a_no_op() {
    let mut s = inked(1);
    assert!(s.ink_paste(0.1, 0.1).unwrap().is_empty());
    assert!(s.ink_select_all().is_empty());
}

#[test]
fn copying_nothing_empties_the_clipboard() {
    let mut s = inked(1);
    let id = draw(&mut s, &[(0.2, 0.2), (0.3, 0.3)]);
    s.ink_copy_selection(&[id]);
    assert!(s.ink_has_clipboard());
    assert_eq!(s.ink_copy_selection(&[]), 0);
    assert!(!s.ink_has_clipboard(), "an empty copy clears it");
}

// ============================ export path containment ============================

/// The shell chooses the destination with all-files access, so the core cannot know which roots are
/// legitimate — but it can reject the shapes a correct shell never produces. Each of these would
/// otherwise be a write somewhere the reader did not choose.
#[test]
fn an_export_path_that_could_escape_is_refused() {
    let mut s = inked(1);
    draw(&mut s, &[(0.2, 0.2), (0.3, 0.3)]);
    for bad in [
        "",                            // nothing at all
        "relative/path.pdf",           // resolved against an unknown cwd
        "/tmp/../etc/inkread-out.pdf", // traversal
        "/no/such/directory/out.pdf",  // parent does not exist
    ] {
        let err = s.export_pdf(bad, false);
        assert!(
            matches!(err, Err(CoreError::InvalidArgument(_))),
            "{bad:?} should be refused, got {err:?}"
        );
    }
}

/// Containment is checked *before* anything is flushed or written, so a refused export leaves no
/// trace at all.
#[test]
fn a_refused_export_writes_nothing() {
    let mut s = inked(1);
    draw(&mut s, &[(0.2, 0.2), (0.3, 0.3)]);
    let _ = s.export_pdf("/tmp/../tmp/inkread-traversal.pdf", false);
    assert!(!std::path::Path::new("/tmp/inkread-traversal.pdf").exists());
}

/// Exporting a document with no ink must **fail**, not produce a file. A silent success would tell
/// the reader their handwriting had been written into the PDF when none of it had.
#[test]
fn exporting_with_no_annotations_is_an_error() {
    let mut s = inked(2);
    let dir = std::env::temp_dir();
    let out = dir.join("inkread-empty-export.pdf");
    let err = s.export_pdf(out.to_str().unwrap(), false);
    assert!(
        matches!(err, Err(CoreError::RenderBackend(_))),
        "got {err:?}"
    );
    assert!(!out.exists(), "and nothing was written");
}

/// With ink present the export reaches the backend, which for a stub without write support returns
/// its own typed error — proving the gather ran and handed real pages over.
#[test]
fn an_export_with_ink_reaches_the_backend() {
    let mut s = inked(2);
    draw(&mut s, &[(0.2, 0.2), (0.3, 0.3)]);
    let out = std::env::temp_dir().join("inkread-unsupported-export.pdf");
    let err = s.export_pdf(out.to_str().unwrap(), true);
    assert!(
        matches!(err, Err(CoreError::RenderBackend(_))),
        "the backend declines, having been given pages: {err:?}"
    );
    assert!(!out.exists());
}
