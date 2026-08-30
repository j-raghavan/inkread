//! Tests for the text-selection modules (RR11 / ADR-INKREAD-0009 D1).
//!
//! Kept as one suite across the split: these exercise the selection *behaviour*, which is what
//! `word`, `span` and `columns` produce together, not any one of them alone.

use super::columns::{column_gutters, median_glyph_width, COLUMN_GAP_MULT};
use super::*;

/// Build a single line of glyphs from a string, evenly spaced across `[x0, x1]` at row `y`.
fn line(s: &str, x0: f32, x1: f32, y: f32, h: f32) -> Vec<CharBox> {
    let n = s.chars().count().max(1);
    let w = (x1 - x0) / n as f32;
    s.chars()
        .enumerate()
        .map(|(i, ch)| CharBox {
            ch,
            rect: NormRect {
                x0: x0 + i as f32 * w,
                y0: y,
                x1: x0 + (i as f32 + 1.0) * w,
                y1: y + h,
            },
            anchor: None,
            wrap: None,
        })
        .collect()
}

/// The same, but as a **reflow** backend emits it: every glyph anchored (so [`wrap_of`] trusts
/// the layout rather than reading the page), with `wrap` on the line's last glyph.
fn laid_out(s: &str, x0: f32, x1: f32, y: f32, h: f32, wrap: Option<Wrap>) -> Vec<CharBox> {
    let mut chars = line(s, x0, x1, y, h);
    for (i, c) in chars.iter_mut().enumerate() {
        c.anchor = Some(TextAnchor {
            block: 0,
            char_offset: i,
        });
    }
    if let Some(last) = chars.last_mut() {
        last.wrap = wrap;
    }
    chars
}

fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> NormRect {
    NormRect { x0, y0, x1, y1 }
}

#[test]
fn norm_rect_contains_is_inclusive_on_the_edge() {
    let r = rect(0.2, 0.2, 0.8, 0.8);
    assert!(r.contains(0.5, 0.5), "interior point");
    assert!(r.contains(0.2, 0.2), "top-left corner is inclusive");
    assert!(r.contains(0.8, 0.8), "bottom-right corner is inclusive");
    assert!(r.contains(0.2, 0.5), "left edge is inclusive");
    assert!(!r.contains(0.19, 0.5), "just left of the rect");
    assert!(!r.contains(0.5, 0.81), "just below the rect");
}

#[test]
fn norm_rect_intersects_counts_a_touching_edge_and_excludes_a_gap() {
    let a = rect(0.0, 0.0, 0.5, 0.5);
    assert!(a.intersects(&rect(0.4, 0.4, 0.9, 0.9)), "overlapping area");
    assert!(
        a.intersects(&rect(0.5, 0.0, 0.9, 0.5)),
        "edges touching counts (shared x=0.5)"
    );
    let gap = rect(0.6, 0.0, 0.9, 0.5);
    assert!(!a.intersects(&gap), "x gap → disjoint");
    assert!(!a.intersects(&rect(0.0, 0.6, 0.5, 0.9)), "y gap → disjoint");
    // Symmetric for both the overlapping AND the disjoint case: a∩b == b∩a.
    let b = rect(0.4, 0.4, 0.9, 0.9);
    assert_eq!(a.intersects(&b), b.intersects(&a), "overlap is symmetric");
    assert_eq!(
        a.intersects(&gap),
        gap.intersects(&a),
        "disjoint is symmetric"
    );
    // A zero-area rect (a point) still intersects a rect that covers it.
    let point = rect(0.25, 0.25, 0.25, 0.25);
    assert!(
        a.intersects(&point) && point.intersects(&a),
        "degenerate point inside"
    );
}

#[test]
fn norm_rect_union_is_the_smallest_covering_rect() {
    let a = rect(0.1, 0.2, 0.4, 0.5);
    let b = rect(0.3, 0.0, 0.9, 0.6);
    assert_eq!(a.union(&b), rect(0.1, 0.0, 0.9, 0.6));
    // Union with self is identity; union is commutative.
    assert_eq!(a.union(&a), a);
    assert_eq!(a.union(&b), b.union(&a));
    // The union covers both operands' corners.
    let u = a.union(&b);
    assert!(u.contains(a.x0, a.y0) && u.contains(b.x1, b.y1));
}

/// A single-row line whose glyphs carry consecutive chapter-relative anchors in `block`.
fn anchored_line(s: &str, block: usize, start_off: usize) -> Vec<CharBox> {
    line(s, 0.0, 0.8, 0.10, 0.03)
        .into_iter()
        .enumerate()
        .map(|(i, mut c)| {
            c.anchor = Some(TextAnchor {
                block,
                char_offset: start_off + i,
            });
            c
        })
        .collect()
}

#[test]
fn anchored_span_returns_first_and_last_selected_anchors() {
    let chars = anchored_line("hello world", 2, 100);
    // A rect covering the whole line selects every glyph.
    let full = NormRect {
        x0: 0.0,
        y0: 0.0,
        x1: 1.0,
        y1: 1.0,
    };
    let (s, e) = anchored_span(&chars, full).expect("span");
    assert_eq!(
        s,
        TextAnchor {
            block: 2,
            char_offset: 100
        }
    );
    assert_eq!(
        e,
        TextAnchor {
            block: 2,
            char_offset: 100 + "hello world".chars().count() - 1,
        }
    );
}

#[test]
fn anchored_span_is_none_for_unanchored_or_empty() {
    let bare = line("abc", 0.0, 0.8, 0.10, 0.03); // anchor: None
    let full = NormRect {
        x0: 0.0,
        y0: 0.0,
        x1: 1.0,
        y1: 1.0,
    };
    assert!(anchored_span(&bare, full).is_none(), "unanchored → None");
    let empty = NormRect {
        x0: 0.0,
        y0: 0.9,
        x1: 0.1,
        y1: 1.0,
    };
    assert!(
        anchored_span(&anchored_line("abc", 0, 0), empty).is_none(),
        "no glyph selected → None"
    );
}

#[test]
fn word_at_tap_selects_whole_word() {
    let chars = line("the quick fox", 0.0, 0.6, 0.10, 0.03);
    // tap inside "quick"
    let sel = word_at(&chars, 0.25, 0.115).unwrap();
    assert_eq!(sel.text, "quick");
    assert_eq!(sel.boxes.len(), 1);
    assert!(sel.boxes[0].x0 < 0.25 && sel.boxes[0].x1 > 0.25);
}

#[test]
fn word_at_handles_internal_apostrophe_and_hyphen() {
    let a = line("don't", 0.0, 0.2, 0.1, 0.03);
    assert_eq!(word_at(&a, 0.1, 0.115).unwrap().text, "don't");
    let b = line("well-known", 0.0, 0.4, 0.1, 0.03);
    assert_eq!(word_at(&b, 0.2, 0.115).unwrap().text, "well-known");
}

#[test]
fn word_at_on_space_or_empty_returns_none() {
    let chars = line("a b", 0.0, 0.3, 0.1, 0.03);
    // the middle glyph is the space
    assert!(word_at(&chars, 0.15, 0.115).is_none());
    // far away from any glyph
    assert!(word_at(&chars, 0.9, 0.9).is_none());
}

#[test]
fn word_at_snaps_a_near_miss_tap() {
    let chars = line("hi", 0.4, 0.5, 0.10, 0.03);
    // tap slightly below the line but within LINE_MARGIN and near in x
    let sel = word_at(&chars, 0.45, 0.14);
    assert_eq!(sel.unwrap().text, "hi");
}

/// A word soft-hyphenated across a line break: "pontifi-" then "cate" on the next line.
fn split_word() -> Vec<CharBox> {
    let mut chars = line("the pontifi-", 0.0, 0.6, 0.10, 0.03);
    chars.extend(line("cate rule", 0.0, 0.45, 0.16, 0.03));
    chars
}

#[test]
fn word_at_joins_a_word_the_line_break_split() {
    let chars = split_word();
    // Tapping the first half ("pontifi-", second token on the top line)...
    let head = word_at(&chars, 0.35, 0.115).expect("a glyph of the first half");
    assert_eq!(
        head.text, "pontificate",
        "the hyphen joins the halves, it isn't a character"
    );
    assert_eq!(
        head.boxes.len(),
        2,
        "one highlight box per line the word spans"
    );
    // ...and tapping the continuation gives the same word.
    let tail = word_at(&chars, 0.04, 0.175).expect("a glyph of the second half");
    assert_eq!(tail.text, "pontificate");
    assert_eq!(
        tail.boxes, head.boxes,
        "either half selects the same two boxes"
    );
}

#[test]
fn word_at_leaves_a_neighbouring_word_of_a_split_alone() {
    let chars = split_word();
    assert_eq!(word_at(&chars, 0.08, 0.115).unwrap().text, "the");
    assert_eq!(word_at(&chars, 0.35, 0.175).unwrap().text, "rule");
}

#[test]
fn word_at_drops_only_the_hyphen_the_layout_inserted() {
    let mut chars = laid_out("the pontifi-", 0.0, 0.6, 0.10, 0.03, Some(Wrap::SoftHyphen));
    chars.extend(laid_out("cate rule", 0.0, 0.45, 0.16, 0.03, None));
    assert_eq!(word_at(&chars, 0.35, 0.115).unwrap().text, "pontificate");
    assert_eq!(word_at(&chars, 0.04, 0.175).unwrap().text, "pontificate");
}

#[test]
fn word_at_keeps_a_compounds_own_hyphen_across_the_break() {
    // The layout broke "well-known" at the hyphen it already had, so it added none: both halves
    // rejoin with that hyphen intact. Identical on the page to the case above.
    let mut chars = laid_out("well-", 0.0, 0.25, 0.10, 0.03, Some(Wrap::Kept));
    chars.extend(laid_out("known", 0.0, 0.25, 0.16, 0.03, None));
    assert_eq!(word_at(&chars, 0.05, 0.115).unwrap().text, "well-known");
    assert_eq!(word_at(&chars, 0.10, 0.175).unwrap().text, "well-known");
}

#[test]
fn word_at_believes_a_layout_that_reports_no_split() {
    // The same two lines, but the layout says it split nothing — the source really is "well-"
    // followed by "known" (two tokens). Guessing from the hyphen would fuse them.
    let mut chars = laid_out("well-", 0.0, 0.25, 0.10, 0.03, None);
    chars.extend(laid_out("known", 0.0, 0.25, 0.16, 0.03, None));
    assert_eq!(word_at(&chars, 0.05, 0.115).unwrap().text, "well");
    assert_eq!(word_at(&chars, 0.10, 0.175).unwrap().text, "known");
}

#[test]
fn word_at_joins_an_unspaced_script_break() {
    // A CJK line break needs no hyphen at all, so there is nothing to drop.
    let mut chars = laid_out("\u{6f22}\u{5b57}", 0.0, 0.2, 0.10, 0.03, Some(Wrap::Kept));
    chars.extend(laid_out("\u{6e2c}\u{8a66}", 0.0, 0.2, 0.16, 0.03, None));
    let sel = word_at(&chars, 0.05, 0.115).unwrap();
    assert_eq!(sel.text, "\u{6f22}\u{5b57}\u{6e2c}\u{8a66}");
    assert_eq!(sel.boxes.len(), 2);
}

#[test]
fn selection_and_search_follow_the_layout_across_a_kept_hyphen() {
    let mut chars = laid_out("a well-", 0.0, 0.35, 0.10, 0.03, Some(Wrap::Kept));
    chars.extend(laid_out("known fact", 0.0, 0.5, 0.16, 0.03, None));
    let sel = text_line_span(&chars, (0.02, 0.115), (0.48, 0.175));
    assert_eq!(
        sel.text, "a well-known fact",
        "copied text keeps the hyphen"
    );
    assert_eq!(find_matches(&chars, "well-known").len(), 1);
    assert!(
        find_matches(&chars, "wellknown").is_empty(),
        "the hyphen is real, so it is searched for"
    );
}

#[test]
fn word_at_rebuilds_a_compound_broken_at_its_own_hyphen() {
    // en-US patterns offer "well-known" exactly one break — at byte 5, right after its own
    // hyphen — so the layout appends a second one and the line ends "well--". Only the appended
    // hyphen is the join; the word keeps the one it came with.
    let mut chars = line("well--", 0.0, 0.3, 0.10, 0.03);
    chars.extend(line("known", 0.0, 0.25, 0.16, 0.03));
    assert_eq!(word_at(&chars, 0.05, 0.115).unwrap().text, "well-known");
    assert_eq!(word_at(&chars, 0.10, 0.175).unwrap().text, "well-known");
}

#[test]
fn word_at_rebuilds_a_compound_broken_before_its_own_hyphen() {
    // "self-evident" breaks at byte 4, so the hyphen it keeps opens the continuation.
    let mut chars = line("self-", 0.0, 0.25, 0.10, 0.03);
    chars.extend(line("-evident", 0.0, 0.4, 0.16, 0.03));
    assert_eq!(word_at(&chars, 0.05, 0.115).unwrap().text, "self-evident");
    assert_eq!(word_at(&chars, 0.2, 0.175).unwrap().text, "self-evident");
}

#[test]
fn word_at_does_not_join_across_a_dash_ending_a_line() {
    // A dash used as punctuation has a space in front of it — not a split word.
    let mut chars = line("one -", 0.0, 0.25, 0.10, 0.03);
    chars.extend(line("two", 0.0, 0.15, 0.16, 0.03));
    assert_eq!(word_at(&chars, 0.05, 0.175).unwrap().text, "two");
    assert_eq!(word_at(&chars, 0.03, 0.115).unwrap().text, "one");
}

#[test]
fn word_at_keeps_a_trailing_hyphen_off_the_word_at_a_page_end() {
    // Nothing follows the hyphen (the wrap continues on the next page) — unchanged behaviour.
    let chars = line("pontifi-", 0.0, 0.4, 0.10, 0.03);
    assert_eq!(word_at(&chars, 0.2, 0.115).unwrap().text, "pontifi");
}

#[test]
fn word_at_joins_a_word_split_across_two_columns() {
    // A two-column page (#194) breaks a word at the foot of column 1 and continues it at the
    // head of column 2 — the continuation is to the right and *above*, but next in reading
    // order, which is what the join follows.
    let mut chars = line("the pontifi-", 0.05, 0.45, 0.90, 0.03);
    chars.extend(line("cate rule", 0.55, 0.90, 0.05, 0.03));
    let sel = word_at(&chars, 0.30, 0.915).expect("a glyph of the first half");
    assert_eq!(sel.text, "pontificate");
    assert_eq!(sel.boxes.len(), 2, "one box in each column");
    assert_eq!(word_at(&chars, 0.58, 0.065).unwrap().text, "pontificate");
}

#[test]
fn drag_selection_heals_a_word_the_line_break_split() {
    let chars = split_word();
    let sel = text_line_span(&chars, (0.02, 0.115), (0.44, 0.175));
    assert_eq!(
        sel.text, "the pontificate rule",
        "copied text reads as the source does"
    );
}

#[test]
fn find_matches_spans_a_word_the_line_break_split() {
    let chars = split_word();
    let hits = find_matches(&chars, "pontificate");
    assert_eq!(hits.len(), 1, "the split word is still findable whole");
    assert_eq!(hits[0].boxes.len(), 2, "highlighted on both lines");
    assert!(hits[0].snippet.contains("pontificate rule"));
    // The wrap still separates two whole words.
    assert_eq!(find_matches(&chars, "pontificate rule").len(), 1);
    assert!(find_matches(&chars, "pontifi-cate").is_empty());
}

#[test]
fn text_in_rect_collects_a_span_in_order() {
    let chars = line("hello world", 0.0, 0.55, 0.10, 0.03);
    // rect over "hello"
    let sel = text_in_rect(
        &chars,
        NormRect {
            x0: 0.0,
            y0: 0.09,
            x1: 0.26,
            y1: 0.14,
        },
    );
    assert!(sel.text.starts_with("hello"));
    assert_eq!(sel.boxes.len(), 1, "single line → one highlight box");
}

#[test]
fn text_in_rect_spans_two_lines_into_two_boxes() {
    let mut chars = line("first line", 0.0, 0.5, 0.10, 0.03);
    chars.extend(line("second line", 0.0, 0.5, 0.16, 0.03));
    let sel = text_in_rect(
        &chars,
        NormRect {
            x0: 0.0,
            y0: 0.08,
            x1: 0.5,
            y1: 0.20,
        },
    );
    assert_eq!(sel.boxes.len(), 2, "two lines → two highlight boxes");
    assert!(sel.text.contains("first") && sel.text.contains("second"));
}

#[test]
fn text_line_span_full_lines_then_partial_last_line() {
    // Three lines; a diagonal drag that starts mid-line-1 and lifts partway through line-3.
    let mut chars = line("the first line here", 0.0, 0.8, 0.10, 0.03);
    chars.extend(line("the middle line two", 0.0, 0.8, 0.16, 0.03));
    chars.extend(line("the last line three", 0.0, 0.8, 0.22, 0.03));
    // Start mid-line-1; lift over "line" on line-3 (x ≈ 0.45, before "three").
    let sel = text_line_span(&chars, (0.30, 0.115), (0.45, 0.235));
    assert_eq!(sel.boxes.len(), 3, "three line boxes");
    // Lines 1 and 2 are taken WHOLE (full text), regardless of the start x.
    assert!(sel.text.contains("the first line here"));
    assert!(sel.text.contains("the middle line two"));
    // Line 3 is clipped at the lift point: "the last line" but NOT "three".
    assert!(sel.text.contains("the last line"));
    assert!(
        !sel.text.contains("three"),
        "last line clipped to the lift word"
    );
    // Whole lines span the full width; consecutive boxes touch (gaps filled).
    assert!(sel.boxes[0].x0 <= 0.01 && sel.boxes[0].x1 >= 0.79);
    assert!(
        sel.boxes[0].y1 >= sel.boxes[1].y0 - 1e-6,
        "no gap between lines 1 and 2"
    );
    assert!(
        sel.boxes[1].y1 >= sel.boxes[2].y0 - 1e-6,
        "no gap between lines 2 and 3"
    );
}

#[test]
fn text_line_span_skips_degenerate_margin_glyphs() {
    // A real PDF emits zero-width glyphs at the right margin (line-break hyphen artifacts). They
    // must not fragment the lines or defeat the gap-fill (the on-device "stripes" bug).
    let mut chars = line("first line one", 0.0, 0.8, 0.10, 0.03);
    // Zero-width artifact at the margin, at a y between the two lines.
    chars.push(CharBox {
        ch: '\u{00AD}',
        rect: NormRect {
            x0: 0.81,
            y0: 0.12,
            x1: 0.81,
            y1: 0.13,
        },
        anchor: None,
        wrap: None,
    });
    chars.extend(line("second line two", 0.0, 0.8, 0.16, 0.03));
    let sel = text_line_span(&chars, (0.1, 0.115), (0.9, 0.175));
    assert_eq!(
        sel.boxes.len(),
        2,
        "degenerate glyph must not become its own box"
    );
    assert!(
        sel.boxes[0].y1 >= sel.boxes[1].y0 - 1e-6,
        "inter-line gap filled (not striped)"
    );
    assert_eq!(sel.text, "first line one second line two");
}

#[test]
fn text_line_span_lift_past_the_last_line_takes_it_whole() {
    // Lift lands in the gap BELOW line 2 (the pen dragged past it) — line 2 must be taken whole,
    // not clipped to the lift x (the "too little" bug: last line cut short).
    let mut chars = line("line one alpha", 0.0, 0.7, 0.10, 0.03);
    chars.extend(line("line two omega", 0.0, 0.7, 0.16, 0.03));
    let sel = text_line_span(&chars, (0.1, 0.115), (0.2, 0.22)); // lift y=0.22 is below line 2 (..0.19)
    assert_eq!(sel.boxes.len(), 2);
    assert_eq!(
        sel.text, "line one alpha line two omega",
        "whole last line, not clipped at x=0.2"
    );
}

#[test]
fn text_line_span_single_line_drag_takes_the_whole_line() {
    let chars = line("alpha beta gamma", 0.0, 0.6, 0.10, 0.03);
    // Start and lift on the same line (lo == hi) → one whole-line box, no clip.
    let sel = text_line_span(&chars, (0.1, 0.115), (0.4, 0.115));
    assert_eq!(sel.boxes.len(), 1);
    assert_eq!(sel.text, "alpha beta gamma");
}

#[test]
fn text_in_rect_empty_when_nothing_inside() {
    let chars = line("abc", 0.0, 0.3, 0.1, 0.03);
    let sel = text_in_rect(
        &chars,
        NormRect {
            x0: 0.8,
            y0: 0.8,
            x1: 0.9,
            y1: 0.9,
        },
    );
    assert!(sel.is_empty());
}

#[test]
fn text_in_rect_selects_a_thin_horizontal_drag_on_one_line() {
    // Regression (#51 → Define drag-select): a single-line drag is a THIN rect along the text;
    // its y-range sits inside the glyphs but need not straddle their centres. Centre-point alone
    // dropped it (nothing selected); a glyph whose box contains the drag's mid-line is selected.
    let chars = line("hello world", 0.0, 0.55, 0.10, 0.03); // glyphs y[.10,.13], centres y=.115
    let rect = NormRect {
        x0: 0.0,
        y0: 0.118,
        x1: 0.30,
        y1: 0.126,
    }; // a swipe just below the glyph centres
    let sel = text_in_rect(&chars, rect);
    assert!(
        sel.text.starts_with("hello"),
        "thin single-line drag selects the line's words, got '{}'",
        sel.text
    );
}

#[test]
fn text_in_rect_excludes_an_edge_grazed_glyph_in_the_next_column() {
    // #51 precision: glyph "a" is fully inside; "b" only straddles the rect's right edge (its box
    // overlaps but its CENTRE is outside). The old bbox-intersect rule grabbed "b" too — the
    // "too generous"/"picks the wrong stuff" lasso. Centre-point containment drops it.
    let chars = line("ab", 0.0, 0.20, 0.10, 0.03); // a:[0,.10] c=.05, b:[.10,.20] c=.15
    let rect = NormRect {
        x0: 0.0,
        y0: 0.09,
        x1: 0.12, // right edge sits between a's centre (.05) and b's centre (.15)
        y1: 0.14,
    };
    assert!(
        rect.intersects(&chars[1].rect),
        "b's box DOES graze the rect"
    );
    let sel = text_in_rect(&chars, rect);
    assert_eq!(
        sel.text, "a",
        "only the glyph whose centre is inside is taken"
    );
}

#[test]
fn text_in_rect_keeps_a_glyph_whose_box_pokes_out_but_center_is_in() {
    // The positive complement: the rule is centre-IN, not full-box-containment. "a"'s box runs to
    // x=0.10 (well past the rect's right edge at 0.06) yet its centre (0.05) is inside → kept.
    // Guards against a future over-tightening to box-containment (which would drop it).
    let chars = line("ab", 0.0, 0.20, 0.10, 0.03); // a:[0,.10] c=.05, b:[.10,.20] c=.15
    let rect = NormRect {
        x0: 0.0,
        y0: 0.09,
        x1: 0.06, // past a's centre (.05) but well short of a's right edge (.10)
        y1: 0.14,
    };
    assert!(
        chars[0].rect.x1 > rect.x1,
        "a's box pokes past the rect edge"
    );
    assert_eq!(
        text_in_rect(&chars, rect).text,
        "a",
        "centre-in glyph is kept"
    );
}

#[test]
fn text_in_rect_center_exactly_on_the_edge_is_inclusive() {
    // Boundary: a glyph whose centre lands exactly on the rect edge is selected (contains is
    // inclusive). Pins the deterministic edge behaviour.
    let chars = line("ab", 0.0, 0.20, 0.10, 0.03); // a centre .05, b centre .15
    let rect = NormRect {
        x0: 0.0,
        y0: 0.09,
        x1: 0.05, // exactly a's centre
        y1: 0.14,
    };
    assert_eq!(
        text_in_rect(&chars, rect).text,
        "a",
        "centre on the edge counts as inside"
    );
}

#[test]
fn text_in_rect_excludes_a_grazed_neighbouring_line() {
    // The vertical analogue: a rect that fully covers line 1 but only grazes line 2's top edge
    // must not sweep line 2 in (the multi-column/line bleed users reported).
    let mut chars = line("top", 0.0, 0.30, 0.100, 0.03); // y centre .115
    chars.extend(line("bot", 0.0, 0.30, 0.135, 0.03)); // y centre .150
    let rect = NormRect {
        x0: 0.0,
        y0: 0.09,
        x1: 0.40,
        y1: 0.137, // grazes "bot" (top .135) but is below its centre (.150)
    };
    let sel = text_in_rect(&chars, rect);
    assert_eq!(sel.boxes.len(), 1, "only line 1, not the grazed line 2");
    assert_eq!(sel.text, "top");
}

#[test]
fn anchored_span_uses_the_same_center_point_predicate() {
    // The stored [start,end] anchors must agree with text_in_rect on which glyphs are in — an
    // edge-grazed glyph is neither the start nor the end anchor.
    let chars = anchored_line("ab", 2, 100); // a c=.2 {2,100}, b c=.6 {2,101}
    let rect = NormRect {
        x0: 0.0,
        y0: 0.0,
        x1: 0.5, // between a's centre (.2) and b's centre (.6)
        y1: 1.0,
    };
    let (start, end) = anchored_span(&chars, rect).expect("a is selected");
    let a = TextAnchor {
        block: 2,
        char_offset: 100,
    };
    assert_eq!(start, a);
    assert_eq!(
        end, a,
        "the grazed glyph b is not pulled into the anchor span"
    );
}

/// A two-column page: `n` shared baselines, left column in `[0.05,0.45]`, right in `[0.55,0.95]`
/// (a ~0.10-wide gutter). Row `i` reads "left row i" / "right row i".
fn two_columns(n: usize) -> Vec<CharBox> {
    let mut chars = Vec::new();
    for i in 0..n {
        let y = 0.10 + i as f32 * 0.06;
        chars.extend(line(&format!("left row {i}"), 0.05, 0.45, y, 0.03));
        chars.extend(line(&format!("right row {i}"), 0.55, 0.95, y, 0.03));
    }
    chars
}

#[test]
fn column_gutters_finds_the_interior_gutter_and_none_for_one_column() {
    let mut chars = line("aaaa", 0.05, 0.40, 0.10, 0.03);
    chars.extend(line("bbbb", 0.60, 0.95, 0.10, 0.03)); // 0.20-wide gap
    let band: Vec<&CharBox> = chars.iter().collect();
    let g = column_gutters(&band, COLUMN_GAP_MULT * median_glyph_width(&band));
    assert_eq!(g.len(), 1, "one interior gutter");
    assert!(
        g[0] > 0.40 && g[0] < 0.60,
        "gutter midpoint in the gap: {}",
        g[0]
    );
    // A single contiguous column has no interior gutter.
    let one = line("aaaabbbb", 0.05, 0.95, 0.10, 0.03);
    let b1: Vec<&CharBox> = one.iter().collect();
    assert!(column_gutters(&b1, COLUMN_GAP_MULT * median_glyph_width(&b1)).is_empty());
}

#[test]
fn text_line_span_confines_a_lasso_to_one_column() {
    // The reported bug: a closed lasso down the LEFT column of a two-column PDF took every shared
    // baseline WHOLE, sweeping the right column in. Confinement keeps only the lassoed column.
    let chars = two_columns(3);
    // Lasso the left column; lift past the last row so it's taken whole (no end-word clip).
    let sel = text_line_span(&chars, (0.05, 0.09), (0.45, 0.29));
    assert!(sel.text.contains("left row 0") && sel.text.contains("left row 2"));
    assert!(
        !sel.text.contains("right"),
        "right column must not be swept in: {:?}",
        sel.text
    );
    assert_eq!(sel.boxes.len(), 3, "three left-column line boxes");
    assert!(
        sel.boxes.iter().all(|b| b.x1 <= 0.5),
        "every box stays left of the gutter"
    );
}

#[test]
fn text_line_span_wide_lasso_still_selects_both_columns() {
    // A deliberately wide lasso spanning both columns is intended to take both — confinement must
    // not clip it (both bands overlap the drag's x-range).
    let chars = two_columns(2);
    let sel = text_line_span(&chars, (0.05, 0.09), (0.95, 0.23));
    assert!(sel.text.contains("left row 0"), "{:?}", sel.text);
    assert!(sel.text.contains("right row 0"), "{:?}", sel.text);
}

#[test]
fn text_line_span_ignores_a_spanning_title_when_confining() {
    // A full-width heading bridges the gutter — but it sits ABOVE the selection, so it is outside
    // the selection's y-band and can't defeat column detection (why the band is y-restricted).
    let mut chars = line("A WIDE SPANNING TITLE", 0.05, 0.95, 0.03, 0.03);
    chars.extend(two_columns(2));
    // Lasso the left column body, below the title.
    let sel = text_line_span(&chars, (0.05, 0.09), (0.45, 0.23));
    assert!(sel.text.contains("left row 0"), "{:?}", sel.text);
    assert!(!sel.text.contains("TITLE"), "title is outside the y-band");
    assert!(
        !sel.text.contains("right"),
        "gutter still detected despite the spanning title: {:?}",
        sel.text
    );
}

#[test]
fn text_in_rect_confines_when_the_rect_reaches_across_the_gutter() {
    // A single-line lasso whose bbox overshoots into the gutter (but whose centre-of-mass is the
    // left column) previously merged both columns' baseline glyphs into one run. Restricting the
    // x-range to the left column keeps only its text.
    let mut chars = line("alpha beta", 0.05, 0.45, 0.10, 0.03);
    chars.extend(line("gamma delta", 0.55, 0.95, 0.10, 0.03));
    // Rect over the left column only (right edge short of the right column's glyph centres).
    let sel = text_in_rect(&chars, rect(0.05, 0.09, 0.47, 0.14));
    assert!(sel.text.contains("alpha"), "{:?}", sel.text);
    assert!(
        !sel.text.contains("gamma"),
        "right column excluded: {:?}",
        sel.text
    );
    assert_eq!(sel.boxes.len(), 1, "one line box, left column only");
    assert!(sel.boxes[0].x1 <= 0.5, "box confined to the left column");
}

#[test]
fn find_matches_is_case_insensitive_and_non_overlapping() {
    let chars = line("the Cat sat on the cat mat", 0.0, 1.0, 0.10, 0.03);
    let m = find_matches(&chars, "cat");
    assert_eq!(m.len(), 2, "both 'Cat' and 'cat' match, case-insensitively");
    assert!(m[0].boxes.len() == 1 && m[1].boxes.len() == 1);
}

#[test]
fn find_matches_spans_words_with_normalized_whitespace() {
    let chars = line("the quick fox", 0.0, 0.6, 0.10, 0.03);
    // a multi-word query matches across the inter-word space
    let m = find_matches(&chars, "quick fox");
    assert_eq!(m.len(), 1);
    assert!(m[0].snippet.contains("quick fox"));
}

#[test]
fn find_matches_spans_two_lines_into_two_boxes() {
    let mut chars = line("hello", 0.0, 0.3, 0.10, 0.03);
    chars.extend(line("world", 0.0, 0.3, 0.16, 0.03));
    // The two words sit on different lines; "hello world" (normalized) spans both.
    let m = find_matches(&chars, "hello world");
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].boxes.len(), 2, "a match across two lines → two boxes");
}

#[test]
fn find_matches_empty_or_absent_query_is_empty() {
    let chars = line("anything", 0.0, 0.4, 0.1, 0.03);
    assert!(find_matches(&chars, "").is_empty());
    assert!(find_matches(&chars, "   ").is_empty());
    assert!(find_matches(&chars, "zzz").is_empty());
}

#[test]
fn find_matches_snippet_has_ellipses_when_trimmed() {
    let chars = line(
        "a very long line of text that completely surrounds the needle that is buried \
         deep inside the middle of a long body of running text on the page",
        0.0,
        1.0,
        0.1,
        0.03,
    );
    let m = find_matches(&chars, "needle");
    assert_eq!(m.len(), 1);
    assert!(
        m[0].snippet.starts_with('…') && m[0].snippet.ends_with('…'),
        "snippet trimmed on both sides: {:?}",
        m[0].snippet
    );
    assert!(m[0].snippet.contains("needle"));
}

#[test]
fn rect_helpers() {
    let r = NormRect {
        x0: 0.1,
        y0: 0.1,
        x1: 0.3,
        y1: 0.3,
    };
    assert!(r.contains(0.2, 0.2));
    assert!(!r.contains(0.5, 0.2));
    assert!(r.intersects(&NormRect {
        x0: 0.25,
        y0: 0.25,
        x1: 0.4,
        y1: 0.4
    }));
    let u = r.union(&NormRect {
        x0: 0.0,
        y0: 0.0,
        x1: 0.2,
        y1: 0.2,
    });
    assert_eq!(
        u,
        NormRect {
            x0: 0.0,
            y0: 0.0,
            x1: 0.3,
            y1: 0.3
        }
    );
}
