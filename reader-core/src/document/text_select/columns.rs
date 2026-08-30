//! Column detection within a selection's vertical band (RR11).
//!
//! A drag across a two-column page would otherwise sweep up the neighbouring column's text at the
//! same height. A gutter is an interior vertical whitespace band wider than any inter-word space,
//! measured against the median glyph width — the same rule `inkread_pdftext` uses for reading
//! order, applied here only within the band the selection covers.

use super::*;

/// A column gutter must be at least this multiple of the median glyph width — an interior vertical
/// whitespace band wider than any inter-word space. Mirrors `inkread_pdftext`'s `column_gap_mult`,
/// but applied here only within the selection's own vertical band (see [`confine_to_columns`]).
pub(super) const COLUMN_GAP_MULT: f32 = 1.5;

/// Median width of the non-degenerate glyph boxes (0.0 if there are none).
pub(super) fn median_glyph_width(glyphs: &[&CharBox]) -> f32 {
    let mut ws: Vec<f32> = glyphs
        .iter()
        .map(|c| c.rect.x1 - c.rect.x0)
        .filter(|w| *w > 0.0)
        .collect();
    if ws.is_empty() {
        return 0.0;
    }
    ws.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    ws[ws.len() / 2]
}

/// The x-midpoints of the interior **column gutters** among `glyphs` — every x-interval no glyph's
/// `[x0, x1]` covers that is wider than `min_w`. Sorted left→right. Empty for a single column (glyphs
/// cover x continuously). The 1-D coverage sweep mirrors `inkread_pdftext::largest_interior_gap`, but
/// collects *all* gutters (a 3-column page has two) rather than only the widest.
pub(super) fn column_gutters(glyphs: &[&CharBox], min_w: f32) -> Vec<f32> {
    let mut spans: Vec<(f32, f32)> = glyphs.iter().map(|c| (c.rect.x0, c.rect.x1)).collect();
    spans.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut gutters = Vec::new();
    let mut cover_end = match spans.first() {
        Some(s) => s.1,
        None => return gutters,
    };
    for &(lo, hi) in &spans[1..] {
        let gap = lo - cover_end;
        if gap > min_w {
            gutters.push(cover_end + gap * 0.5);
        }
        cover_end = cover_end.max(hi);
    }
    gutters
}

/// Confine a lasso/drag selection (its x-range `[x0,x1]`, y-range `[y0,y1]`, either order) to the
/// text **column(s)** it actually covers, returning the glyphs to run selection over.
///
/// Two-column PDFs share baselines across the gutter, so the page-wide predicates
/// ([`text_in_rect`] / [`text_line_span`]) otherwise sweep the neighbouring column in (a one-column
/// lasso grabbing both — the reported bug). This finds the vertical gutter(s) among the glyphs on the
/// selection's **own lines** (y overlapping the selection band, so a title spanning both columns
/// above/below can't bridge the gutter) and keeps only glyphs whose column the selection's x-range
/// overlaps. With no interior gutter (a single column) it keeps every glyph — the existing behaviour,
/// bit-for-bit. Non-degenerate, non-whitespace glyphs define the columns (a stray wide space can't
/// bridge a gutter, and trailing spaces can't leak one column into the next).
pub(super) fn confine_to_columns(
    chars: &[CharBox],
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
) -> Vec<CharBox> {
    let (ty0, ty1) = (y0.min(y1), y0.max(y1));
    let band: Vec<&CharBox> = chars
        .iter()
        .filter(|c| {
            c.rect.x1 > c.rect.x0
                && c.rect.y1 > c.rect.y0
                && !c.ch.is_whitespace()
                && c.rect.y1 >= ty0
                && c.rect.y0 <= ty1
        })
        .collect();
    let glyph_w = median_glyph_width(&band);
    if glyph_w <= 0.0 {
        return chars.to_vec();
    }
    let gutters = column_gutters(&band, COLUMN_GAP_MULT * glyph_w);
    if gutters.is_empty() {
        return chars.to_vec(); // single column — unchanged behaviour
    }
    // Column bands are the x-intervals between consecutive gutters, bounded by ±∞ at the page edges.
    // Keep a glyph when the band its centre falls in overlaps the selection's x-range.
    let mut bounds = vec![f32::NEG_INFINITY];
    bounds.extend_from_slice(&gutters);
    bounds.push(f32::INFINITY);
    let (sx0, sx1) = (x0.min(x1), x0.max(x1));
    chars
        .iter()
        .filter(|c| {
            let cx = (c.rect.x0 + c.rect.x1) * 0.5;
            bounds
                .windows(2)
                .find(|w| cx >= w[0] && cx < w[1])
                .is_some_and(|w| sx0 <= w[1] && w[0] <= sx1)
        })
        .cloned()
        .collect()
}

/// Whether a drag/lasso `rect` selects `glyph` — true when the glyph's **centre** lies inside `rect`.
///
/// Precision rule (#51): the predicate used to be bounding-box *intersection*, which selected any
/// glyph the rect merely grazed at an edge — a loose lasso then swept in neighbouring-column or
/// -line glyphs ("too generous", "picks the wrong stuff"). Requiring the centre inside drops
/// edge-grazed glyphs while keeping any glyph at least half-covered, matching what a user means by
/// "inside the loop" (it keeps a glyph at least half-covered along the axis the rect edge cuts —
/// a corner clip can be lower, which is the intended tightening). Shared by [`text_in_rect`] and
/// [`anchored_span`] so the highlight and its stored anchors never disagree on which glyphs are in.
///
/// Horizontal is always strict centre-in-range (precise column boundary). Vertical accepts EITHER
/// the centre inside the rect (the normal case) OR the glyph's box straddling the rect's mid-line —
/// so a **thin single-line drag** (a Define swipe whose bbox is shorter than the glyphs and may not
/// reach their centres) still selects the line it runs along, without re-admitting a neighbouring
/// line a tall lasso bbox only grazes (its mid-line lands on the line it's centred over, not the
/// grazed one).
pub(super) fn glyph_selected(rect: &NormRect, glyph: &NormRect) -> bool {
    let cx = (glyph.x0 + glyph.x1) * 0.5;
    if cx < rect.x0 || cx > rect.x1 {
        return false;
    }
    let cy = (glyph.y0 + glyph.y1) * 0.5;
    let rect_mid_y = (rect.y0 + rect.y1) * 0.5;
    (cy >= rect.y0 && cy <= rect.y1) || (glyph.y0 <= rect_mid_y && rect_mid_y <= glyph.y1)
}
