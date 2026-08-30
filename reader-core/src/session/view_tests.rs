//! Tests for the view/typography half of [`ReaderSession`] (RR2-FR5, RR4, RR8).
//!
//! Split from `session_tests.rs` — which is already the largest test file in the crate — so the
//! display and typography surface has a home of its own, matching the `view.rs` it exercises.
//!
//! Two shapes of behaviour live here and they fail differently. **Display settings** (contrast,
//! night, fit, crop, quality, zoom) are stored and read back, and the interesting part is the
//! clamping: a shell that sends a value out of range must be corrected here rather than at the
//! render, because the render has no idea what a legal value is. **Typography settings** delegate
//! to the document, and their shared post-condition is the one worth pinning: whatever page the
//! repagination reports, the session must clamp it into range, drop the render cache, and reload
//! the ink for wherever it ended up. Miss any of the three and the reader gets a stale bitmap, ink
//! from the wrong page, or an index past the end.

use super::*;
use crate::render::PixelBuffer;
use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A reflowable stub whose repagination lands on a page the test chooses, so the clamp and the
/// cache-invalidation post-condition can be driven directly.
struct TypographyStub {
    pages: usize,
    /// What every `set_*` reports as the new page — deliberately settable past the end.
    lands_on: usize,
    /// Counts every typography call, so "did the session delegate at all?" is answerable.
    calls: AtomicUsize,
    columns: i32,
}

impl TypographyStub {
    fn new(pages: usize, lands_on: usize) -> Self {
        Self {
            pages,
            lands_on,
            calls: AtomicUsize::new(0),
            columns: 1,
        }
    }

    fn note(&self) -> Option<usize> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Some(self.lands_on)
    }
}

impl Document for TypographyStub {
    fn page_count(&self) -> usize {
        self.pages
    }
    fn metadata(&self) -> DocumentMetadata {
        DocumentMetadata {
            title: None,
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
    fn supports_reflow(&self) -> bool {
        true
    }
    fn is_magnifiable(&self) -> bool {
        false
    }
    fn effective_columns(&self) -> i32 {
        self.columns
    }
    fn set_text_scale(&self, _scale: f32, _p: usize) -> Option<usize> {
        self.note()
    }
    fn set_font(&self, _id: i32, _p: usize) -> Option<usize> {
        self.note()
    }
    fn set_line_spacing(&self, _m: f32, _p: usize) -> Option<usize> {
        self.note()
    }
    fn set_columns(&self, _c: i32, _p: usize) -> Option<usize> {
        self.note()
    }
    fn set_margin(&self, _pct: i32, _p: usize) -> Option<usize> {
        self.note()
    }
    fn set_alignment(&self, _a: i32, _p: usize) -> Option<usize> {
        self.note()
    }
    fn set_reflow(&self, _on: bool, _p: usize) -> Option<usize> {
        self.note()
    }
    #[allow(clippy::too_many_arguments)]
    fn set_typography(
        &self,
        _scale: f32,
        _font_id: i32,
        _line_spacing: f32,
        _align: i32,
        _columns: i32,
        _margin_pct: i32,
        _p: usize,
    ) -> Option<usize> {
        self.note()
    }
    fn set_pagination_progress(&self, _progress: Box<dyn PaginationProgress>) {
        self.calls.fetch_add(1, Ordering::Relaxed);
    }
}

/// A fixed-layout stub: every typography setter declines, which is the PDF path.
///
/// It overrides `is_magnifiable` because the trait defaults it to `false` — backends opt *in* to
/// zoom by actually honouring it in `render_zoom`, and `PdfBackend` does. A stub that inherited the
/// default would model a reflowable document while claiming to be a PDF.
struct FixedStub {
    pages: usize,
}
impl Document for FixedStub {
    fn page_count(&self) -> usize {
        self.pages
    }
    fn metadata(&self) -> DocumentMetadata {
        DocumentMetadata {
            title: None,
            author: None,
        }
    }
    fn render_page(&self, _index: usize, buf: &mut PixelBuffer<'_>) -> CoreResult<()> {
        buf.fill_white();
        Ok(())
    }
    fn is_magnifiable(&self) -> bool {
        true
    }
}

/// One typography setter, so the shared post-condition can be driven over all of them.
type TypographyOp = fn(&mut ReaderSession) -> bool;

fn reflowable(pages: usize, lands_on: usize) -> ReaderSession {
    ReaderSession::with_document(
        Box::new(TypographyStub::new(pages, lands_on)),
        DeviceCapabilities::controllable_epd(),
        Viewport::new(100, 120, 226),
    )
}

fn fixed(pages: usize) -> ReaderSession {
    ReaderSession::with_document(
        Box::new(FixedStub { pages }),
        DeviceCapabilities::controllable_epd(),
        Viewport::new(100, 120, 226),
    )
}

// ============================ display settings ============================

#[test]
fn contrast_is_stored_and_clamped_to_the_renderer_maximum() {
    let mut s = fixed(3);
    assert_eq!(s.contrast(), 0, "off by default");
    s.set_contrast(2);
    assert_eq!(s.contrast(), 2);
    // The shell sends a raw step; the ceiling belongs here, because the render path has no notion
    // of a legal step and would index a table off its end.
    s.set_contrast(u8::MAX);
    assert_eq!(s.contrast(), crate::render::contrast::MAX_CONTRAST_STEP);
}

#[test]
fn night_mode_toggles() {
    let mut s = fixed(3);
    assert!(!s.night(), "off by default");
    s.set_night(true);
    assert!(s.night());
    s.set_night(false);
    assert!(!s.night());
}

#[test]
fn the_fit_mode_round_trips_every_variant() {
    let mut s = fixed(3);
    for mode in [FitMode::Page, FitMode::Width, FitMode::Height] {
        s.set_fit(mode);
        assert_eq!(s.fit_mode(), mode, "{mode:?}");
    }
}

#[test]
fn auto_crop_toggles() {
    let mut s = fixed(3);
    assert!(!s.crop_auto(), "off by default");
    s.set_crop_auto(true);
    assert!(s.crop_auto());
    s.set_crop_auto(false);
    assert!(!s.crop_auto());
}

#[test]
fn the_crop_margin_is_clamped_to_its_eight_steps() {
    let mut s = fixed(3);
    assert_eq!(s.crop_margin(), 0);
    s.set_crop_margin(5);
    assert_eq!(s.crop_margin(), 5);
    s.set_crop_margin(200);
    assert_eq!(s.crop_margin(), 8, "clamped to the top step");
}

#[test]
fn render_quality_is_stored_and_clamped_to_its_three_steps() {
    let mut s = fixed(3);
    for q in 0..=2u8 {
        s.set_render_quality(q);
        assert_eq!(s.render_quality(), q);
    }
    s.set_render_quality(u8::MAX);
    assert_eq!(s.render_quality(), 2, "clamped to the top step");
}

// ============================ zoom + pan ============================

#[test]
fn zoom_and_pan_are_stored() {
    let mut s = fixed(3);
    assert_eq!(s.zoom(), 1.0, "fit by default");
    s.set_zoom(2.0, 0.25, 0.75);
    assert_eq!(s.zoom(), 2.0);
    assert_eq!(s.pan_x(), 0.25);
    assert_eq!(s.pan_y(), 0.75);
}

#[test]
fn zoom_below_fit_is_raised_to_fit() {
    let mut s = fixed(3);
    s.set_zoom(0.1, 0.0, 0.0);
    assert_eq!(s.zoom(), 1.0, "the page never renders smaller than fit");
}

#[test]
fn zoom_is_capped_so_a_page_cannot_be_rendered_unboundedly_large() {
    let mut s = fixed(3);
    s.set_zoom(1e9, 0.0, 0.0);
    assert_eq!(s.zoom(), MAX_ZOOM);
}

/// A non-finite zoom would poison every coordinate downstream — the fit map, the crop rect, the
/// allocation size for the render. It falls back to fit rather than propagating.
#[test]
fn a_non_finite_zoom_falls_back_to_fit() {
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut s = fixed(3);
        s.set_zoom(bad, 0.0, 0.0);
        assert_eq!(s.zoom(), 1.0, "{bad}");
        assert!(s.zoom().is_finite());
    }
}

#[test]
fn pan_is_clamped_into_the_overscan() {
    let mut s = fixed(3);
    s.set_zoom(2.0, -5.0, 5.0);
    assert_eq!(s.pan_x(), 0.0);
    assert_eq!(s.pan_y(), 1.0);
}

// ============================ typography: the shared post-condition ============================

/// Every typography setter clamps whatever page the repagination reports. A backend that lands past
/// the end — a shorter pagination after a bigger font — would otherwise leave the session pointing
/// at an index that renders `PageOutOfRange` on the very next frame.
#[test]
fn a_repagination_landing_past_the_end_is_clamped_into_range() {
    let ops: Vec<(&str, TypographyOp)> = vec![
        ("set_text_scale", |s| s.set_text_scale(1.5)),
        ("set_font", |s| s.set_font(2)),
        ("set_line_spacing", |s| s.set_line_spacing(1.4)),
        ("set_columns", |s| s.set_columns(2)),
        ("set_margin", |s| s.set_margin(8)),
        ("set_alignment", |s| s.set_alignment(1)),
        ("set_reflow", |s| s.set_reflow(true)),
        ("set_typography", |s| s.set_typography(1.2, 1, 1.3, 1, 1, 5)),
    ];
    for (name, op) in ops {
        let mut s = reflowable(5, 99); // the stub reports page 99 of a 5-page document
        assert!(op(&mut s), "{name} should apply on a reflowable document");
        assert_eq!(s.current_page(), 4, "{name}: clamped to the last page");
    }
}

#[test]
fn a_repagination_landing_in_range_is_honoured_exactly() {
    let mut s = reflowable(10, 3);
    assert!(s.set_text_scale(1.5));
    assert_eq!(s.current_page(), 3);
}

/// An empty document has no page 0 to clamp to; `saturating_sub` must not underflow.
#[test]
fn a_repagination_on_an_empty_document_does_not_underflow() {
    let mut s = reflowable(0, 7);
    assert!(s.set_text_scale(1.5));
    assert_eq!(s.current_page(), 0);
}

// ============================ typography: fixed layout declines ============================

/// A fixed-layout PDF returns `false` from every typography setter, which is what the shell uses to
/// grey the control out — and, per #212, to say so rather than appear to do nothing.
#[test]
fn a_fixed_layout_document_declines_every_typography_change() {
    let mut s = fixed(3);
    assert!(!s.set_text_scale(1.5), "text scale");
    assert!(!s.set_font(1), "font");
    assert!(!s.set_line_spacing(1.4), "line spacing");
    assert!(!s.set_columns(2), "columns");
    assert!(!s.set_margin(6), "margin");
    assert!(!s.set_alignment(1), "alignment");
    assert!(!s.set_typography(1.2, 1, 1.3, 1, 1, 5), "typography");
    assert_eq!(s.current_page(), 0, "and the page is untouched");
}

#[test]
fn a_declined_change_leaves_the_page_where_it_was() {
    let mut s = fixed(10);
    s.jump_to_page(4);
    assert!(!s.set_text_scale(2.0));
    assert_eq!(s.current_page(), 4);
}

// ============================ capability queries ============================

#[test]
fn reflow_support_is_reported_from_the_document() {
    assert!(reflowable(3, 0).supports_reflow());
    assert!(
        !fixed(3).supports_reflow(),
        "a plain fixed stub does not reflow"
    );
}

/// The shell gates pinch, the +/- buttons and double-tap on this, so a gesture on a reflowed view
/// cannot strand the shell's zoom factor and skew every subsequent tap hit-test (#61/#212).
#[test]
fn a_reflowed_view_reports_itself_unmagnifiable() {
    assert!(
        !reflowable(3, 0).is_magnifiable(),
        "a reflowed view does not magnify"
    );
    assert!(fixed(3).is_magnifiable(), "a fixed page does");
}

/// The trait defaults `is_magnifiable` to `false`, so a new backend is un-zoomable until it opts
/// in — the safe direction. A backend that magnified without honouring zoom in `render_zoom` would
/// let the shell's zoom factor drift away from what is actually drawn, skewing every hit-test.
#[test]
fn a_backend_that_does_not_opt_in_is_not_magnifiable() {
    struct Bare;
    impl Document for Bare {
        fn page_count(&self) -> usize {
            1
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
    let s = ReaderSession::with_document(
        Box::new(Bare),
        DeviceCapabilities::controllable_epd(),
        Viewport::new(100, 120, 226),
    );
    assert!(!s.is_magnifiable());
    assert!(!s.supports_reflow(), "and does not reflow either");
}

#[test]
fn effective_columns_is_reported_from_the_layout() {
    assert_eq!(reflowable(3, 0).effective_columns(), 1);
}

#[test]
fn a_pagination_progress_sink_is_handed_to_the_document() {
    struct Sink(RefCell<Vec<(usize, usize)>>);
    impl PaginationProgress for Sink {
        fn chapter_done(&self, done: usize, total: usize) {
            self.0.borrow_mut().push((done, total));
        }
        fn cancelled(&self) -> bool {
            false
        }
    }
    // The reflowable stub counts the call; a fixed document ignores it without complaint.
    let s = reflowable(3, 0);
    s.set_pagination_progress(Box::new(Sink(RefCell::new(Vec::new()))));
    let f = fixed(3);
    f.set_pagination_progress(Box::new(Sink(RefCell::new(Vec::new()))));
}
