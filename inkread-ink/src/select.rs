//! Lasso selection over an [`InkLayer`] (RR6 / ADR-INKREAD-0010, lasso vertical).
//!
//! Mirrors Boox NeoReader's two lasso sub-modes (clean-room — behaviour only, no decompiled code):
//! [`SelectMode::Smart`] grabs any whole stroke the loop **encloses or crosses** (forgiving), and
//! [`SelectMode::Freehand`] grabs only strokes **fully inside** the loop (precise). Selection is
//! always **whole-stroke** — strokes are never split — matching NeoReader and keeping ids stable.
//!
//! This module holds the **query** half of the lasso (read-only over `&InkLayer`): which strokes a
//! drawn loop selects, "select all", and the bounds of a selection (the dirty rect for the floating
//! selection toolbar). The mutating half (move / delete / clipboard / recolor, all reversible) lives
//! on [`InkLayer`] itself so it shares the undo/redo history.
//!
//! Coordinates are normalized page space `[0,1]`, exactly like [`InkPoint`]; the lasso polygon is an
//! open list of vertices `(x, y)` that is treated as **closed** (an implicit last→first edge).

use crate::model::{BBox, InkLayer, InkPoint, StrokeId};

/// The lasso sub-mode (NeoReader: "Smart Lasso" vs "Freehand Lasso").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectMode {
    /// Forgiving: a stroke is selected if the loop encloses **any** of its points or **crosses** it.
    Smart,
    /// Precise: a stroke is selected only if **all** of its points lie inside the loop.
    Freehand,
}

impl SelectMode {
    /// Decode a wire code (`0` = Smart, `1` = Freehand) for the JNI boundary; `None` if unknown.
    #[must_use]
    pub fn from_code(code: u8) -> Option<SelectMode> {
        match code {
            0 => Some(SelectMode::Smart),
            1 => Some(SelectMode::Freehand),
            _ => None,
        }
    }

    /// The wire code for this mode.
    #[must_use]
    pub fn code(self) -> u8 {
        match self {
            SelectMode::Smart => 0,
            SelectMode::Freehand => 1,
        }
    }
}

/// The ids of the strokes a `polygon` loop selects under `mode`, in paint order.
///
/// A polygon with fewer than 3 vertices selects nothing (not a closed loop). Selection is
/// whole-stroke; the result preserves the layer's paint order so downstream ops are deterministic.
#[must_use]
pub fn select_in_polygon(
    layer: &InkLayer,
    polygon: &[(f32, f32)],
    mode: SelectMode,
) -> Vec<StrokeId> {
    if polygon.len() < 3 {
        return Vec::new();
    }
    layer
        .strokes()
        .iter()
        .filter(|s| stroke_selected(&s.points, polygon, mode))
        .map(|s| s.id)
        .collect()
}

/// Every committed stroke id, in paint order (NeoReader "Select All").
#[must_use]
pub fn select_all(layer: &InkLayer) -> Vec<StrokeId> {
    layer.strokes().iter().map(|s| s.id).collect()
}

/// The union of the bounds of the strokes in `ids` — the dirty rect / anchor for the selection
/// toolbar (RR6-FR5). `None` if `ids` is empty or matches no stroke.
#[must_use]
pub fn selection_bounds(layer: &InkLayer, ids: &[StrokeId]) -> Option<BBox> {
    let mut acc: Option<BBox> = None;
    for s in layer.strokes() {
        if !ids.contains(&s.id) {
            continue;
        }
        if let Some(b) = s.bounds() {
            acc = Some(match acc {
                None => b,
                Some(a) => BBox {
                    x0: a.x0.min(b.x0),
                    y0: a.y0.min(b.y0),
                    x1: a.x1.max(b.x1),
                    y1: a.y1.max(b.y1),
                },
            });
        }
    }
    acc
}

/// Whether `points` (a stroke) is selected by `polygon` under `mode`.
fn stroke_selected(points: &[InkPoint], polygon: &[(f32, f32)], mode: SelectMode) -> bool {
    match mode {
        SelectMode::Freehand => {
            // Precise: every point must be inside the loop (and the stroke must have points).
            !points.is_empty() && points.iter().all(|p| point_in_polygon(p.x, p.y, polygon))
        }
        SelectMode::Smart => {
            // Forgiving: any point inside, OR any stroke segment crosses a polygon edge.
            points.iter().any(|p| point_in_polygon(p.x, p.y, polygon))
                || stroke_crosses_polygon(points, polygon)
        }
    }
}

/// Ray-casting point-in-polygon (the polygon is implicitly closed last→first). A point exactly on
/// an edge may report either way; that ambiguity is immaterial for lasso selection.
fn point_in_polygon(px: f32, py: f32, poly: &[(f32, f32)]) -> bool {
    let mut inside = false;
    let n = poly.len();
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = poly[i];
        let (xj, yj) = poly[j];
        // Does the horizontal ray at py cross edge (j→i)?
        if (yi > py) != (yj > py) {
            let t = (py - yi) / (yj - yi);
            if px < xi + t * (xj - xi) {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// Whether any segment of the stroke crosses any edge of the (closed) polygon.
fn stroke_crosses_polygon(points: &[InkPoint], poly: &[(f32, f32)]) -> bool {
    let n = poly.len();
    for w in points.windows(2) {
        let (ax, ay) = (w[0].x, w[0].y);
        let (bx, by) = (w[1].x, w[1].y);
        let mut j = n - 1;
        for i in 0..n {
            if segments_intersect(ax, ay, bx, by, poly[j].0, poly[j].1, poly[i].0, poly[i].1) {
                return true;
            }
            j = i;
        }
    }
    false
}

/// Proper segment-intersection test for (p1→p2) vs (p3→p4) via orientation signs.
#[allow(clippy::too_many_arguments)]
fn segments_intersect(
    p1x: f32,
    p1y: f32,
    p2x: f32,
    p2y: f32,
    p3x: f32,
    p3y: f32,
    p4x: f32,
    p4y: f32,
) -> bool {
    let d1 = cross(p3x, p3y, p4x, p4y, p1x, p1y);
    let d2 = cross(p3x, p3y, p4x, p4y, p2x, p2y);
    let d3 = cross(p1x, p1y, p2x, p2y, p3x, p3y);
    let d4 = cross(p1x, p1y, p2x, p2y, p4x, p4y);
    if ((d1 > 0.0) != (d2 > 0.0)) && ((d3 > 0.0) != (d4 > 0.0)) {
        return true;
    }
    // Collinear-overlap cases are immaterial for lasso; the strict crossing above suffices.
    false
}

/// Z of the cross product of (a→b) × (a→c) — the orientation of c relative to the line a→b.
fn cross(ax: f32, ay: f32, bx: f32, by: f32, cx: f32, cy: f32) -> f32 {
    (bx - ax) * (cy - ay) - (by - ay) * (cx - ax)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{InkColor, Tool};

    /// A pen stroke through the given normalized points.
    fn stroke(layer: &mut InkLayer, pts: &[(f32, f32)]) -> StrokeId {
        layer
            .start_stroke(Tool::Pen, InkColor::default(), 0.01, 0)
            .unwrap();
        for &(x, y) in pts {
            layer
                .push_point(InkPoint::new(x, y, 1.0, None, None, 0).unwrap())
                .unwrap();
        }
        layer.finish_stroke().unwrap()
    }

    /// A square loop covering roughly the centre of the page.
    fn centre_box() -> Vec<(f32, f32)> {
        vec![(0.3, 0.3), (0.7, 0.3), (0.7, 0.7), (0.3, 0.7)]
    }

    #[test]
    fn empty_or_degenerate_polygon_selects_nothing() {
        let mut layer = InkLayer::new();
        stroke(&mut layer, &[(0.5, 0.5), (0.55, 0.55)]);
        assert!(select_in_polygon(&layer, &[], SelectMode::Smart).is_empty());
        assert!(select_in_polygon(&layer, &[(0.1, 0.1), (0.2, 0.2)], SelectMode::Smart).is_empty());
    }

    #[test]
    fn freehand_selects_only_fully_enclosed_strokes() {
        let mut layer = InkLayer::new();
        let inside = stroke(&mut layer, &[(0.4, 0.4), (0.6, 0.6)]); // fully inside
        let _straddle = stroke(&mut layer, &[(0.5, 0.5), (0.9, 0.9)]); // one point outside
        let _outside = stroke(&mut layer, &[(0.05, 0.05), (0.1, 0.1)]); // fully outside
        let sel = select_in_polygon(&layer, &centre_box(), SelectMode::Freehand);
        assert_eq!(sel, vec![inside]);
    }

    #[test]
    fn smart_grabs_enclosed_and_crossing_strokes() {
        let mut layer = InkLayer::new();
        let inside = stroke(&mut layer, &[(0.4, 0.4), (0.6, 0.6)]);
        let straddle = stroke(&mut layer, &[(0.5, 0.5), (0.9, 0.9)]); // a point inside → smart grabs it
        let crossing = stroke(&mut layer, &[(0.1, 0.5), (0.9, 0.5)]); // crosses the box, no vertex inside
        let _outside = stroke(&mut layer, &[(0.05, 0.05), (0.1, 0.1)]);
        let sel = select_in_polygon(&layer, &centre_box(), SelectMode::Smart);
        assert_eq!(sel, vec![inside, straddle, crossing]);
    }

    #[test]
    fn smart_crossing_catches_a_stroke_with_no_vertex_inside() {
        // A horizontal stroke passing straight through the box, vertices well outside on both sides.
        let mut layer = InkLayer::new();
        let crossing = stroke(&mut layer, &[(0.0, 0.5), (1.0, 0.5)]);
        let smart = select_in_polygon(&layer, &centre_box(), SelectMode::Smart);
        let free = select_in_polygon(&layer, &centre_box(), SelectMode::Freehand);
        assert_eq!(smart, vec![crossing]);
        assert!(free.is_empty()); // freehand needs full containment
    }

    #[test]
    fn select_all_returns_every_stroke_in_paint_order() {
        let mut layer = InkLayer::new();
        let a = stroke(&mut layer, &[(0.1, 0.1), (0.2, 0.2)]);
        let b = stroke(&mut layer, &[(0.8, 0.8), (0.9, 0.9)]);
        assert_eq!(select_all(&layer), vec![a, b]);
    }

    #[test]
    fn selection_bounds_unions_selected_strokes_only() {
        let mut layer = InkLayer::new();
        let a = stroke(&mut layer, &[(0.4, 0.4), (0.5, 0.5)]);
        let _b = stroke(&mut layer, &[(0.05, 0.05), (0.06, 0.06)]); // not selected
        let bounds = selection_bounds(&layer, &[a]).expect("a has bounds");
        // The box hugs stroke a (±half width), and excludes b near the origin.
        assert!(bounds.x0 >= 0.3 && bounds.x0 < 0.4);
        assert!(bounds.x1 > 0.5 && bounds.x1 <= 0.6);
        assert!(selection_bounds(&layer, &[]).is_none());
    }
}
