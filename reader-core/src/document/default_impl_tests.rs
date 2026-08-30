//! The defaulted half of the [`Document`] contract (RR5-FR2).
//!
//! Three methods are required — `page_count`, `metadata`, `render_page` — and everything else a
//! reader expects is **defaulted**, so a new backend inherits a working reader and opts into what
//! it can support. That design is only safe if the defaults are the *conservative* answer in every
//! case, and nothing checked that: the existing suites all exercise backends that override.
//!
//! So this file implements the bare minimum and asserts what a backend gets for free. Each
//! assertion is really a statement about what happens to a format that has not implemented the
//! feature yet:
//!
//! - queries answer **empty or `None`**, never a panic and never a fabricated result (RR21-FR3);
//! - rendering **falls back to the plain page render**, so a backend that cannot crop, zoom or fit
//!   still draws something correct rather than nothing;
//! - mutations **decline** by returning `None`/`false`, which is what the shell reads to grey the
//!   control out rather than appearing to do nothing (#212);
//! - `export_pdf` returns a **typed error**, because silently writing no ink would look like a
//!   successful export of an empty annotation set.
//!
//! Defaulting any of these the other way is the kind of mistake that ships: it would make a new
//! backend look like it supported a feature, and fail somewhere far from here.

use super::*;
use crate::error::CoreError;
use crate::position::PinPosition;
use crate::render::PixelBuffer;

/// The smallest legal backend: the three required methods and nothing else.
struct Bare {
    pages: usize,
}

impl Document for Bare {
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
}

fn bare() -> Bare {
    Bare { pages: 3 }
}

/// The whole page, normalized — the rect a "select everything" query would use.
fn unit_rect() -> NormRect {
    NormRect {
        x0: 0.0,
        y0: 0.0,
        x1: 1.0,
        y1: 1.0,
    }
}

fn scratch() -> Vec<u8> {
    vec![0u8; 8 * 8 * 4]
}

// ============================ queries answer emptily ============================

#[test]
fn a_bare_backend_answers_every_text_query_emptily() {
    let d = bare();
    assert!(d.word_at(0, 0.5, 0.5).is_none(), "no text layer, no word");
    assert_eq!(d.text_in_rect(0, unit_rect()), TextSelection::default());
    assert_eq!(
        d.text_line_span(0, (0.1, 0.1), (0.9, 0.9)),
        TextSelection::default()
    );
    assert!(d.search_page(0, "anything").is_empty(), "not searchable");
}

#[test]
fn a_bare_backend_has_no_outline_and_no_links() {
    let d = bare();
    assert!(d.toc().is_empty());
    assert!(d.page_links(0).is_empty());
}

#[test]
fn a_bare_backend_has_no_reflow_positions() {
    let d = bare();
    assert!(d.page_pin(0).is_none());
    assert!(d.selection_pins(0, unit_rect()).is_none());
}

#[test]
fn a_bare_backend_reports_no_content_bbox_and_no_fit_transform() {
    let d = bare();
    assert!(d.content_bbox(0).is_none(), "nothing to crop to");
    assert!(
        d.page_fit_transform(0, 100, 200, FitMode::Page, 0.0, 0.0, None)
            .is_none(),
        "no fit map to report"
    );
}

/// A query about a page that does not exist must answer like any other unsupported query rather
/// than indexing anything — the shell asks about pages speculatively (link prefetch, search sweep).
#[test]
fn queries_past_the_end_are_still_empty_rather_than_panicking() {
    let d = bare();
    assert!(d.word_at(999, 0.5, 0.5).is_none());
    assert!(d.page_links(999).is_empty());
    assert!(d.search_page(999, "x").is_empty());
    assert!(d.page_pin(999).is_none());
    assert!(d
        .pin_to_page(&PinPosition {
            chapter_index: 0,
            chapter_id: "c0".into(),
            chapter_start: 0,
            chapter_end: 10,
            node_position: 0,
            text_offset: 0,
            xpath: Vec::new(),
        })
        .is_none());
}

#[test]
fn a_bare_backend_reports_a_single_column() {
    assert_eq!(bare().effective_columns(), 1);
}

// ============================ rendering falls back ============================

/// `render_zoom`, `render_fit` and `render_cropped` all default to the plain page render. A backend
/// that cannot magnify or crop therefore still draws the page correctly — the shell gets a right
/// picture at the wrong scale rather than a blank one or an error.
#[test]
fn every_render_variant_falls_back_to_the_plain_page_render() {
    let d = bare();
    for label in ["zoom", "fit", "cropped"] {
        let mut px = scratch();
        let mut buf = PixelBuffer::from_rgba(&mut px, 8, 8).unwrap();
        let r = match label {
            "zoom" => d.render_zoom(0, &mut buf, 2.0, 4, 4),
            "fit" => d.render_fit(0, &mut buf, FitMode::Page, 0.0, 0.0),
            _ => d.render_cropped(0, &mut buf, unit_rect(), FitMode::Page, 0.0, 0.0),
        };
        assert!(r.is_ok(), "{label} render");
        assert!(
            px.iter().all(|&b| b == 0xFF),
            "{label}: page was drawn white"
        );
    }
}

/// ...and the fallback keeps the error contract too: an out-of-range page is a typed error through
/// every variant, not a panic and not a silently blank frame.
#[test]
fn the_render_fallbacks_propagate_an_out_of_range_page() {
    let d = bare();
    let mut px = scratch();
    let mut buf = PixelBuffer::from_rgba(&mut px, 8, 8).unwrap();
    assert!(matches!(
        d.render_zoom(99, &mut buf, 1.0, 0, 0),
        Err(CoreError::PageOutOfRange { .. })
    ));
    assert!(matches!(
        d.render_fit(99, &mut buf, FitMode::Page, 0.0, 0.0),
        Err(CoreError::PageOutOfRange { .. })
    ));
    assert!(matches!(
        d.render_cropped(99, &mut buf, unit_rect(), FitMode::Page, 0.0, 0.0),
        Err(CoreError::PageOutOfRange { .. })
    ));
}

#[test]
fn a_bare_backend_does_not_claim_to_magnify() {
    assert!(
        !bare().is_magnifiable(),
        "opt-in: a backend that ignores zoom must not claim it"
    );
}

// ============================ mutations decline ============================

/// Every typography setter declines by returning `None`, which the session turns into `false` and
/// the shell reads to disable the control. Returning a page instead would make the reader jump.
#[test]
fn every_typography_setter_declines_on_a_bare_backend() {
    let d = bare();
    assert!(d.set_text_scale(1.5, 1).is_none(), "text scale");
    assert!(d.set_line_spacing(1.4, 1).is_none(), "line spacing");
    assert!(d.set_alignment(1, 1).is_none(), "alignment");
    assert!(d.set_columns(2, 1).is_none(), "columns");
    assert!(d.set_margin(6, 1).is_none(), "margin");
    assert!(d.set_font(3, 1).is_none(), "font");
    assert!(
        d.set_typography(1.2, 1, 1.3, 1, 1, 5, 1).is_none(),
        "the batched form declines as one"
    );
}

#[test]
fn a_bare_backend_neither_supports_nor_toggles_reflow() {
    let d = bare();
    assert!(!d.supports_reflow(), "no Reflow toggle is offered");
    assert!(d.set_reflow(true, 0).is_none(), "and toggling declines");
}

/// The two sinks are offered to every backend and ignored by those with nothing slow to report.
/// Accepting them silently is the point — a format without pagination should not have to know the
/// types exist.
#[test]
fn the_pagination_sinks_are_accepted_and_ignored() {
    struct NoCache;
    impl PaginationCache for NoCache {
        fn load(&self, _key: &str) -> Option<Vec<usize>> {
            None
        }
        fn save(&self, _key: &str, _chapter_pages: &[usize]) {}
    }
    struct NoProgress;
    impl PaginationProgress for NoProgress {
        fn chapter_done(&self, _done: usize, _total: usize) {}
        fn cancelled(&self) -> bool {
            false
        }
    }
    let d = bare();
    d.set_pagination_cache(Box::new(NoCache));
    d.set_pagination_progress(Box::new(NoProgress));
}

#[test]
fn a_read_ahead_hint_is_accepted_and_ignored() {
    bare().hint_page(2);
}

// ============================ export refuses, loudly ============================

/// A backend without write support must **fail** the export rather than return `Ok`. Silently
/// writing nothing would present as a successful export of an empty annotation set — the reader
/// would believe their handwriting was saved into the PDF.
#[test]
fn export_is_a_typed_error_on_a_backend_that_cannot_write() {
    let mut d = bare();
    for mode in [ExportMode::Annotations, ExportMode::Flatten] {
        let err = d.export_pdf("/tmp/inkread-should-not-exist.pdf", &[], mode);
        assert!(matches!(err, Err(CoreError::RenderBackend(_))), "{mode:?}");
    }
    assert!(
        !std::path::Path::new("/tmp/inkread-should-not-exist.pdf").exists(),
        "and nothing was written"
    );
}

// ============================ FitMode wire decoding ============================

#[test]
fn fit_mode_decodes_its_wire_codes() {
    assert_eq!(FitMode::from_code(0), FitMode::Page);
    assert_eq!(FitMode::from_code(1), FitMode::Width);
    assert_eq!(FitMode::from_code(2), FitMode::Height);
}

/// The shell sends an int across JNI; an unknown one must land on the safe default rather than
/// leaving the fit mode undefined (RR21-FR3).
#[test]
fn an_unknown_fit_code_falls_back_to_page() {
    for code in [-1, 3, 99, i32::MIN, i32::MAX] {
        assert_eq!(FitMode::from_code(code), FitMode::Page, "code {code}");
    }
}

#[test]
fn fit_mode_defaults_to_page() {
    assert_eq!(FitMode::default(), FitMode::Page);
}
