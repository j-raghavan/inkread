//! Tests for the navigation and text-query half of [`ReaderSession`] (RR11, RR6).
//!
//! The queries here look like pass-throughs and mostly are, except for one thing that is not: the
//! **viewport↔page mapping**.
//!
//! The shell speaks *viewport*-normalized coordinates — where it renders, and where it reads a
//! touch. The text layer speaks *page*-normalized coordinates. When a page is letterboxed into the
//! viewport (a portrait page on a squarer panel, or an auto-cropped page) those two spaces differ,
//! so a tap has to be mapped **down** into page space before the lookup and the resulting highlight
//! boxes mapped **back up** before they are drawn.
//!
//! Get that wrong and nothing crashes: the dictionary defines a word one line above the one you
//! pressed, and the highlight lands next to the text it highlights. That is the whole reason this
//! file exists — the mapping has a forward and an inverse, and they have to be each other's.
//!
//! `view_transform` deliberately returns `None` for the render paths its affine does not model
//! (pinch-zoom, which renders through `render_zoom`), and every query falls back to the
//! untransformed pass-through in that case. Both branches are covered here.

use super::*;
use crate::document::{LinkTarget, PageLink, SearchMatch};
use crate::position::PinPosition;
use crate::render::PixelBuffer;

/// A page letterboxed into the viewport: page space is squeezed to 80% and offset by 10%, which is
/// what fitting a portrait page onto a squarer panel actually does.
const SX: f32 = 0.8;
const OX: f32 = 0.1;
const SY: f32 = 0.5;
const OY: f32 = 0.25;

/// A text-bearing stub. Its text queries **echo the page-space coordinates they were handed** into
/// the selection's text, so a test can assert exactly what reached the text layer, and return a box
/// at a known page position so the mapping back up can be checked too.
struct TextDoc {
    /// When false, `page_fit_transform` returns `None` — the pass-through branch.
    letterboxed: bool,
}

impl Document for TextDoc {
    fn page_count(&self) -> usize {
        4
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
    fn page_fit_transform(
        &self,
        _index: usize,
        _vw: u32,
        _vh: u32,
        _mode: FitMode,
        _pan_x: f32,
        _pan_y: f32,
        _crop: Option<NormRect>,
    ) -> Option<(f32, f32, f32, f32)> {
        if self.letterboxed {
            Some((SX, OX, SY, OY))
        } else {
            None
        }
    }
    fn word_at(&self, _page: usize, x: f32, y: f32) -> Option<TextSelection> {
        Some(TextSelection {
            text: format!("{x:.4},{y:.4}"),
            boxes: vec![NormRect {
                x0: 0.0,
                y0: 0.0,
                x1: 1.0,
                y1: 1.0,
            }],
        })
    }
    fn text_in_rect(&self, _page: usize, rect: NormRect) -> TextSelection {
        TextSelection {
            text: format!(
                "{:.4},{:.4},{:.4},{:.4}",
                rect.x0, rect.y0, rect.x1, rect.y1
            ),
            boxes: vec![rect],
        }
    }
    fn text_line_span(&self, _page: usize, start: (f32, f32), end: (f32, f32)) -> TextSelection {
        TextSelection {
            text: format!("{:.4},{:.4}->{:.4},{:.4}", start.0, start.1, end.0, end.1),
            boxes: Vec::new(),
        }
    }
    fn page_links(&self, page: usize) -> Vec<PageLink> {
        vec![PageLink {
            x0: 0.1,
            y0: 0.1,
            x1: 0.2,
            y1: 0.2,
            target: LinkTarget::Page(page + 1),
        }]
    }
    fn search_page(&self, _page: usize, query: &str) -> Vec<SearchMatch> {
        if query.is_empty() {
            return Vec::new();
        }
        vec![SearchMatch {
            boxes: Vec::new(),
            snippet: format!("…{query}…"),
        }]
    }
    fn selection_pins(&self, page: usize, _rect: NormRect) -> Option<(PinPosition, PinPosition)> {
        let pin = |o: i32| PinPosition {
            chapter_index: page as i32,
            chapter_id: "c".into(),
            chapter_start: 0,
            chapter_end: 100,
            node_position: o,
            text_offset: 0,
            xpath: Vec::new(),
        };
        Some((pin(0), pin(10)))
    }
}

fn session(letterboxed: bool) -> ReaderSession {
    ReaderSession::with_document(
        Box::new(TextDoc { letterboxed }),
        DeviceCapabilities::controllable_epd(),
        Viewport::new(100, 120, 226),
    )
}

/// The page coordinate a viewport coordinate maps down to, per the letterbox affine.
fn page_x(view_x: f32) -> f32 {
    (view_x - OX) / SX
}
fn page_y(view_y: f32) -> f32 {
    (view_y - OY) / SY
}

// ============================ the mapping, both directions ============================

#[test]
fn a_tap_is_mapped_down_into_page_space_before_the_lookup() {
    let s = session(true);
    let sel = s.word_at(0, 0.5, 0.5).unwrap();
    assert_eq!(
        sel.text,
        format!("{:.4},{:.4}", page_x(0.5), page_y(0.5)),
        "the text layer must receive page coords, not the raw viewport tap"
    );
}

#[test]
fn the_resulting_boxes_are_mapped_back_up_into_viewport_space() {
    let s = session(true);
    let sel = s.word_at(0, 0.5, 0.5).unwrap();
    let b = sel.boxes[0];
    // The stub returns the whole page (0,0)-(1,1); in viewport space that is the letterbox.
    assert!((b.x0 - OX).abs() < 1e-4, "x0 {} vs {OX}", b.x0);
    assert!((b.y0 - OY).abs() < 1e-4, "y0 {} vs {OY}", b.y0);
    assert!((b.x1 - (SX + OX)).abs() < 1e-4, "x1 {}", b.x1);
    assert!((b.y1 - (SY + OY)).abs() < 1e-4, "y1 {}", b.y1);
}

/// The property that matters: mapping down and back up is the identity, so a highlight lands on the
/// text it highlights. If these two drifted apart nothing would fail — the box would simply be in
/// the wrong place.
#[test]
fn mapping_down_then_back_up_is_the_identity() {
    let s = session(true);
    for (vx, vy) in [(0.15f32, 0.3f32), (0.5, 0.5), (0.85, 0.7)] {
        let sel = s.text_in_rect(
            0,
            NormRect {
                x0: vx,
                y0: vy,
                x1: vx,
                y1: vy,
            },
        );
        let b = sel.boxes[0];
        assert!(
            (b.x0 - vx).abs() < 1e-4,
            "x round trip at {vx}: got {}",
            b.x0
        );
        assert!(
            (b.y0 - vy).abs() < 1e-4,
            "y round trip at {vy}: got {}",
            b.y0
        );
    }
}

#[test]
fn a_drag_span_maps_both_of_its_endpoints() {
    let s = session(true);
    let sel = s.text_line_span(0, (0.2, 0.3), (0.8, 0.9));
    assert_eq!(
        sel.text,
        format!(
            "{:.4},{:.4}->{:.4},{:.4}",
            page_x(0.2),
            page_y(0.3),
            page_x(0.8),
            page_y(0.9)
        )
    );
}

// ============================ the pass-through branch ============================

#[test]
fn without_a_letterbox_the_coordinates_pass_straight_through() {
    let s = session(false);
    let sel = s.word_at(0, 0.5, 0.5).unwrap();
    assert_eq!(sel.text, "0.5000,0.5000", "no mapping applied");
    assert_eq!(sel.boxes[0].x1, 1.0, "and the box is untouched");
}

/// A pinch-zoomed page renders through `render_zoom`, which this affine does not model, so the
/// transform reports `None` and every query falls back to the pass-through. Mapping through a fit
/// affine while the page is magnified would put the lookup somewhere else entirely.
#[test]
fn a_magnified_page_falls_back_to_the_pass_through() {
    let mut s = session(true);
    s.set_zoom(2.0, 0.0, 0.0);
    let sel = s.word_at(0, 0.5, 0.5).unwrap();
    assert_eq!(sel.text, "0.5000,0.5000", "no fit mapping while magnified");
}

#[test]
fn returning_to_fit_restores_the_mapping() {
    let mut s = session(true);
    s.set_zoom(2.0, 0.0, 0.0);
    s.set_zoom(1.0, 0.0, 0.0);
    let sel = s.word_at(0, 0.5, 0.5).unwrap();
    assert_eq!(sel.text, format!("{:.4},{:.4}", page_x(0.5), page_y(0.5)));
}

// ============================ plain pass-throughs ============================

#[test]
fn page_links_come_from_the_document() {
    let s = session(true);
    let links = s.page_links(2);
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].target, LinkTarget::Page(3));
}

#[test]
fn search_delegates_and_reports_no_match_for_an_empty_query() {
    let s = session(true);
    assert_eq!(s.search_page(0, "ink").len(), 1);
    assert_eq!(s.search_page(0, "ink")[0].snippet, "…ink…");
    assert!(s.search_page(0, "").is_empty());
}

#[test]
fn selection_pins_come_from_the_document() {
    let s = session(true);
    let rect = NormRect {
        x0: 0.0,
        y0: 0.0,
        x1: 1.0,
        y1: 1.0,
    };
    let (start, end) = s.selection_pins(1, rect).unwrap();
    assert_eq!(start.chapter_index, 1);
    assert!(start < end, "the pair is in reading order");
}

// ============================ TOC navigation ============================

#[test]
fn jumping_to_a_toc_entry_moves_to_its_page() {
    let mut s = session(true);
    let entry = TocEntry {
        title: "Two".into(),
        target_page: Some(2),
        children: Vec::new(),
    };
    let cmds = s.jump_to_toc(&entry);
    assert_eq!(s.current_page(), 2);
    assert!(!cmds.is_empty(), "and the panel is refreshed");
}

/// An outline entry with no resolvable destination must not move the reader — and must not emit a
/// refresh either, or the panel flashes for a jump that did not happen.
#[test]
fn an_unresolved_toc_entry_neither_moves_nor_refreshes() {
    let mut s = session(true);
    s.jump_to_page(1);
    let entry = TocEntry {
        title: "Unresolved".into(),
        target_page: None,
        children: Vec::new(),
    };
    let cmds = s.jump_to_toc(&entry);
    assert_eq!(s.current_page(), 1, "did not move");
    assert!(cmds.is_empty(), "and did not refresh");
}

#[test]
fn a_toc_jump_past_the_end_is_clamped() {
    let mut s = session(true);
    let entry = TocEntry {
        title: "Beyond".into(),
        target_page: Some(9_999),
        children: Vec::new(),
    };
    s.jump_to_toc(&entry);
    assert_eq!(s.current_page(), 3, "clamped to the last page");
}
