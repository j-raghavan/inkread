//! Ink domain types and the per-page [`InkLayer`] (RR6).

use std::fmt;

/// The error surface of the ink domain. `reader-core` maps these onto its `CoreError` at the
/// JNI boundary (RR21-FR3); the domain itself never panics on bad input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InkError {
    /// A coordinate, pressure, or width was NaN or infinite.
    NonFinite(&'static str),
    /// `finish_stroke` was called but the in-progress stroke had no points.
    EmptyStroke,
    /// `push_point` was called with no stroke in progress.
    NoActiveStroke,
    /// A `.inkbin` blob was malformed or truncated (see [`crate::codec`]).
    BadEncoding(String),
}

impl fmt::Display for InkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InkError::NonFinite(w) => write!(f, "non-finite {w}"),
            InkError::EmptyStroke => write!(f, "stroke has no points"),
            InkError::NoActiveStroke => write!(f, "no stroke in progress"),
            InkError::BadEncoding(m) => write!(f, "malformed ink data: {m}"),
        }
    }
}

impl std::error::Error for InkError {}

/// The ink domain result alias.
pub type InkResult<T> = Result<T, InkError>;

/// A drawing tool. Color is preserved on grayscale panels for export + future color panels
/// (RR6-FR6). The `Eraser` removes strokes it touches rather than depositing ink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// Opaque pen ink.
    Pen,
    /// Translucent highlighter ink (drawn under text where the renderer supports it).
    Highlighter,
    /// Removes strokes it passes over (see [`InkLayer::erase_at`]).
    Eraser,
}

impl Tool {
    /// Decode the wire code (`0=Pen, 1=Highlighter, 2=Eraser`); unknown → `None`.
    #[must_use]
    pub fn from_code(code: u8) -> Option<Tool> {
        match code {
            0 => Some(Tool::Pen),
            1 => Some(Tool::Highlighter),
            2 => Some(Tool::Eraser),
            _ => None,
        }
    }

    /// The stable wire code for this tool (inverse of [`Self::from_code`]).
    #[must_use]
    pub fn code(self) -> u8 {
        match self {
            Tool::Pen => 0,
            Tool::Highlighter => 1,
            Tool::Eraser => 2,
        }
    }

    /// Whether this tool deposits ink (Pen/Highlighter) vs. erases (Eraser).
    #[must_use]
    pub fn is_ink(self) -> bool {
        matches!(self, Tool::Pen | Tool::Highlighter)
    }
}

/// An RGBA ink color, preserved even when the panel renders grayscale (RR6-FR6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InkColor {
    /// Red channel.
    pub r: u8,
    /// Green channel.
    pub g: u8,
    /// Blue channel.
    pub b: u8,
    /// Alpha channel (`255` = opaque).
    pub a: u8,
}

impl InkColor {
    /// Opaque black — the default pen color.
    pub const BLACK: InkColor = InkColor {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };

    /// Build an RGBA color.
    #[must_use]
    pub fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
}

impl Default for InkColor {
    fn default() -> Self {
        InkColor::BLACK
    }
}

/// One captured sample along a stroke. Coordinates + pressure are normalized `[0,1]`; tilt is
/// optional and in radians where the digitizer reports it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InkPoint {
    /// X in normalized page space `[0,1]`, top-left origin.
    pub x: f32,
    /// Y in normalized page space `[0,1]`, top-left origin.
    pub y: f32,
    /// Normalized pen pressure `[0,1]` (`1.0` if the device has no pressure).
    pub pressure: f32,
    /// Pen tilt about the X axis, if reported.
    pub tilt_x: Option<f32>,
    /// Pen tilt about the Y axis, if reported.
    pub tilt_y: Option<f32>,
    /// Sample time in milliseconds relative to stroke start (caller-supplied; the core reads no
    /// clock).
    pub timestamp_ms: u32,
}

impl InkPoint {
    /// Build a validated point. Rejects a non-finite x/y/pressure (`NonFinite`); clamps x, y, and
    /// pressure into `[0,1]`; drops a non-finite tilt to `None`.
    pub fn new(
        x: f32,
        y: f32,
        pressure: f32,
        tilt_x: Option<f32>,
        tilt_y: Option<f32>,
        timestamp_ms: u32,
    ) -> InkResult<Self> {
        if !x.is_finite() || !y.is_finite() || !pressure.is_finite() {
            return Err(InkError::NonFinite("point coordinate"));
        }
        let finite = |t: Option<f32>| t.filter(|v| v.is_finite());
        Ok(Self {
            x: x.clamp(0.0, 1.0),
            y: y.clamp(0.0, 1.0),
            pressure: pressure.clamp(0.0, 1.0),
            tilt_x: finite(tilt_x),
            tilt_y: finite(tilt_y),
            timestamp_ms,
        })
    }
}

/// Identifier for a stroke, unique and monotonic within a page [`InkLayer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StrokeId(pub u32);

/// A vector ink stroke: an ordered, non-empty list of points with a tool, color, and nominal
/// width.
#[derive(Debug, Clone, PartialEq)]
pub struct Stroke {
    /// Stable id within the page layer.
    pub id: StrokeId,
    /// The tool that drew it (always an ink tool — Pen/Highlighter).
    pub tool: Tool,
    /// The ink color (preserved on grayscale, RR6-FR6).
    pub color: InkColor,
    /// Nominal stroke width in normalized page units (modulated per-point by pressure).
    pub width: f32,
    /// The captured samples (always at least one).
    pub points: Vec<InkPoint>,
    /// Caller-supplied creation time (ms since epoch); the core reads no clock.
    pub created_at_ms: u64,
}

impl Stroke {
    /// The axis-aligned bounds of this stroke, expanded by half the nominal width, or `None` if
    /// (impossibly) it has no points.
    #[must_use]
    pub fn bounds(&self) -> Option<BBox> {
        let mut it = self.points.iter();
        let first = it.next()?;
        let mut b = BBox {
            x0: first.x,
            y0: first.y,
            x1: first.x,
            y1: first.y,
        };
        for p in it {
            b.x0 = b.x0.min(p.x);
            b.y0 = b.y0.min(p.y);
            b.x1 = b.x1.max(p.x);
            b.y1 = b.y1.max(p.y);
        }
        let m = self.width * 0.5;
        Some(BBox {
            x0: (b.x0 - m).max(0.0),
            y0: (b.y0 - m).max(0.0),
            x1: (b.x1 + m).min(1.0),
            y1: (b.y1 + m).min(1.0),
        })
    }
}

/// Effective stroke width at a given pressure (RR6-FR3 pressure-to-width). A flat baseline of
/// half the nominal width plus a pressure-scaled half keeps a zero-pressure sample visible.
#[must_use]
pub fn pressure_to_width(nominal: f32, pressure: f32) -> f32 {
    let p = pressure.clamp(0.0, 1.0);
    nominal * (0.5 + 0.5 * p)
}

/// An axis-aligned bounding box in normalized page space `[0,1]` — the dirty rect for a
/// partial refresh (RR6-FR5). The caller maps it to device pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BBox {
    /// Left edge `[0,1]`.
    pub x0: f32,
    /// Top edge `[0,1]`.
    pub y0: f32,
    /// Right edge `[0,1]`.
    pub x1: f32,
    /// Bottom edge `[0,1]`.
    pub y1: f32,
}

/// A reversible edit to an [`InkLayer`] — the unit of undo/redo.
#[derive(Debug, Clone, PartialEq)]
enum Edit {
    /// A stroke was added (at the end of the list).
    Add(Stroke),
    /// A stroke was erased from position `index`.
    Erase { index: usize, stroke: Stroke },
    /// A lasso selection was translated by `(dx, dy)` (ADR-INKREAD-0010). The delta is already
    /// clamped so the selection stays in `[0,1]`, making the inverse `(-dx, -dy)` exact.
    Move {
        ids: Vec<StrokeId>,
        dx: f32,
        dy: f32,
    },
    /// A lasso selection was deleted; `removed` holds `(original_index, stroke)` in **ascending**
    /// index order so undo can reinsert each at its original position.
    DeleteMany { removed: Vec<(usize, Stroke)> },
    /// A lasso selection was recolored to `new`; `old[k]` is `ids[k]`'s previous color (for undo).
    Recolor {
        ids: Vec<StrokeId>,
        old: Vec<InkColor>,
        new: InkColor,
    },
    /// A clipboard paste appended these (already re-id'd, offset) strokes; undo removes them by id.
    AddMany { strokes: Vec<Stroke> },
}

/// All ink on one page, with an undo/redo history (RR6-FR3).
///
/// Strokes are kept in paint order (later strokes draw on top). The undo/redo history is
/// session-only and is **not** serialized — only [`Self::strokes`] persist (RR20: committed
/// strokes survive; the in-progress stroke and history do not).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InkLayer {
    strokes: Vec<Stroke>,
    next_id: u32,
    pending: Option<Stroke>,
    undo: Vec<Edit>,
    redo: Vec<Edit>,
}

impl InkLayer {
    /// An empty layer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a layer from already-committed strokes (the codec/load path). `next_id` is set past
    /// the largest existing id so new strokes never collide.
    #[must_use]
    pub fn from_strokes(strokes: Vec<Stroke>) -> Self {
        let next_id = strokes
            .iter()
            .map(|s| s.id.0)
            .max()
            .map_or(0, |m| m.saturating_add(1));
        Self {
            strokes,
            next_id,
            pending: None,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    /// The committed strokes in paint order (what the renderer draws and the codec persists).
    #[must_use]
    pub fn strokes(&self) -> &[Stroke] {
        &self.strokes
    }

    /// Whether the layer has no committed strokes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.strokes.is_empty()
    }

    /// Begin an ink stroke (Pen/Highlighter). Any previous in-progress stroke is discarded.
    /// Returns the id the stroke will have once finished. Rejects a non-finite width.
    pub fn start_stroke(
        &mut self,
        tool: Tool,
        color: InkColor,
        width: f32,
        created_at_ms: u64,
    ) -> InkResult<StrokeId> {
        if !width.is_finite() || width <= 0.0 {
            return Err(InkError::NonFinite("stroke width"));
        }
        let id = StrokeId(self.next_id);
        self.pending = Some(Stroke {
            id,
            tool,
            color,
            width,
            points: Vec::new(),
            created_at_ms,
        });
        Ok(id)
    }

    /// Append a sample to the in-progress stroke. Errors if no stroke is active or the point is
    /// non-finite (it is otherwise clamped into `[0,1]`).
    pub fn push_point(&mut self, point: InkPoint) -> InkResult<()> {
        let stroke = self.pending.as_mut().ok_or(InkError::NoActiveStroke)?;
        stroke.points.push(point);
        Ok(())
    }

    /// Commit the in-progress stroke. Returns its id, or `None` (and discards it) if it had no
    /// points. Committing clears the redo history (a new edit forks the timeline).
    pub fn finish_stroke(&mut self) -> Option<StrokeId> {
        let stroke = self.pending.take()?;
        if stroke.points.is_empty() {
            return None;
        }
        let id = stroke.id;
        self.next_id = self.next_id.saturating_add(1);
        self.strokes.push(stroke.clone());
        self.undo.push(Edit::Add(stroke));
        self.redo.clear();
        Some(id)
    }

    /// Discard the in-progress stroke without committing (e.g. palm-rejected mid-stroke).
    pub fn cancel_stroke(&mut self) {
        self.pending = None;
    }

    /// The ids of committed strokes within `radius` of `(x, y)` (normalized), nearest segment
    /// first by paint order — stroke hit-testing (RR6-FR3), non-destructive.
    #[must_use]
    pub fn strokes_hit(&self, x: f32, y: f32, radius: f32) -> Vec<StrokeId> {
        self.strokes
            .iter()
            .filter(|s| stroke_hit(s, x, y, radius))
            .map(|s| s.id)
            .collect()
    }

    /// Erase every committed stroke touching `(x, y)` within `radius`. Returns the removed ids
    /// (top-most first) and records one undoable edit per removed stroke. Clears redo.
    pub fn erase_at(&mut self, x: f32, y: f32, radius: f32) -> Vec<StrokeId> {
        let mut removed = Vec::new();
        // Walk from the top (paint order end) so erasing the top-most stroke first feels right
        // and the recorded indices stay valid for undo (LIFO restore).
        let mut i = self.strokes.len();
        while i > 0 {
            i -= 1;
            if stroke_hit(&self.strokes[i], x, y, radius) {
                let stroke = self.strokes.remove(i);
                removed.push(stroke.id);
                self.undo.push(Edit::Erase { index: i, stroke });
            }
        }
        if !removed.is_empty() {
            self.redo.clear();
        }
        removed
    }

    // ---- lasso selection mutations (ADR-INKREAD-0010) — all reversible, all clear redo ----

    /// Translate the strokes in `ids` by `(dx, dy)` (normalized), as one undoable edit. The delta
    /// is clamped so the whole selection stays within `[0,1]`; returns the clamped delta actually
    /// applied, or `None` if nothing moved (empty/unknown ids, non-finite delta, or a zero move).
    pub fn move_strokes(&mut self, ids: &[StrokeId], dx: f32, dy: f32) -> Option<(f32, f32)> {
        if !dx.is_finite() || !dy.is_finite() {
            return None;
        }
        let (x0, y0, x1, y1) = self.raw_point_bounds(ids)?;
        let cdx = dx.clamp(-x0, 1.0 - x1);
        let cdy = dy.clamp(-y0, 1.0 - y1);
        if cdx == 0.0 && cdy == 0.0 {
            return None;
        }
        self.translate(ids, cdx, cdy);
        self.undo.push(Edit::Move {
            ids: ids.to_vec(),
            dx: cdx,
            dy: cdy,
        });
        self.redo.clear();
        Some((cdx, cdy))
    }

    /// Delete every stroke in `ids` as one undoable edit (NeoReader lasso "Delete"/"Cut"). Returns
    /// the removed ids (top-most first); a no-op if none match.
    pub fn delete_strokes(&mut self, ids: &[StrokeId]) -> Vec<StrokeId> {
        let mut removed_ids = Vec::new();
        let mut removed = Vec::new(); // (original_index, stroke), gathered top-down
        let mut i = self.strokes.len();
        while i > 0 {
            i -= 1;
            if ids.contains(&self.strokes[i].id) {
                let stroke = self.strokes.remove(i);
                removed_ids.push(stroke.id);
                removed.push((i, stroke));
            }
        }
        if !removed.is_empty() {
            removed.reverse(); // store ascending so undo reinserts at valid positions
            self.undo.push(Edit::DeleteMany { removed });
            self.redo.clear();
        }
        removed_ids
    }

    /// Recolor every stroke in `ids` to `color` as one undoable edit. Returns `true` if any matched.
    pub fn recolor_strokes(&mut self, ids: &[StrokeId], color: InkColor) -> bool {
        let mut affected = Vec::new();
        let mut old = Vec::new();
        for s in &mut self.strokes {
            if ids.contains(&s.id) {
                affected.push(s.id);
                old.push(s.color);
                s.color = color;
            }
        }
        if affected.is_empty() {
            return false;
        }
        self.undo.push(Edit::Recolor {
            ids: affected,
            old,
            new: color,
        });
        self.redo.clear();
        true
    }

    /// Detached clones of the strokes in `ids`, in paint order — the clipboard payload for
    /// copy/cut (the caller, not the layer, holds the clipboard so it can paste across pages).
    /// Non-destructive: records no edit.
    #[must_use]
    pub fn copy_strokes(&self, ids: &[StrokeId]) -> Vec<Stroke> {
        self.strokes
            .iter()
            .filter(|s| ids.contains(&s.id))
            .cloned()
            .collect()
    }

    /// Paste `strokes` (e.g. from the clipboard) offset by `(dx, dy)`, as one undoable edit. Each
    /// gets a fresh id; points are clamped into `[0,1]`. Returns the new ids; a no-op on empty input
    /// or a non-finite offset.
    pub fn paste_strokes(&mut self, strokes: &[Stroke], dx: f32, dy: f32) -> Vec<StrokeId> {
        if strokes.is_empty() || !dx.is_finite() || !dy.is_finite() {
            return Vec::new();
        }
        let mut added = Vec::new();
        let mut new_ids = Vec::new();
        for s in strokes {
            let id = StrokeId(self.next_id);
            self.next_id = self.next_id.saturating_add(1);
            let mut clone = s.clone();
            clone.id = id;
            for p in &mut clone.points {
                p.x = (p.x + dx).clamp(0.0, 1.0);
                p.y = (p.y + dy).clamp(0.0, 1.0);
            }
            self.strokes.push(clone.clone());
            added.push(clone);
            new_ids.push(id);
        }
        self.undo.push(Edit::AddMany { strokes: added });
        self.redo.clear();
        new_ids
    }

    /// Raw (unexpanded, unclamped) bounds of the points of the strokes in `ids`, or `None` if none
    /// match — used to clamp a move delta to keep the selection on-page.
    fn raw_point_bounds(&self, ids: &[StrokeId]) -> Option<(f32, f32, f32, f32)> {
        let mut b: Option<(f32, f32, f32, f32)> = None;
        for s in &self.strokes {
            if !ids.contains(&s.id) {
                continue;
            }
            for p in &s.points {
                b = Some(match b {
                    None => (p.x, p.y, p.x, p.y),
                    Some((x0, y0, x1, y1)) => (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y)),
                });
            }
        }
        b
    }

    /// Translate the points of the strokes in `ids` by `(dx, dy)` (no clamp — the caller bounds it).
    fn translate(&mut self, ids: &[StrokeId], dx: f32, dy: f32) {
        for s in &mut self.strokes {
            if ids.contains(&s.id) {
                for p in &mut s.points {
                    p.x += dx;
                    p.y += dy;
                }
            }
        }
    }

    /// Whether an undo is available.
    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Whether a redo is available.
    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Undo the most recent edit. Returns `true` if something was undone.
    pub fn undo(&mut self) -> bool {
        let Some(edit) = self.undo.pop() else {
            return false;
        };
        match &edit {
            Edit::Add(stroke) => {
                self.strokes.retain(|s| s.id != stroke.id);
            }
            Edit::Erase { index, stroke } => {
                let at = (*index).min(self.strokes.len());
                self.strokes.insert(at, stroke.clone());
            }
            Edit::Move { ids, dx, dy } => {
                self.translate(ids, -*dx, -*dy);
            }
            Edit::DeleteMany { removed } => {
                for (index, stroke) in removed.iter() {
                    let at = (*index).min(self.strokes.len());
                    self.strokes.insert(at, stroke.clone());
                }
            }
            Edit::Recolor { ids, old, .. } => {
                for (id, color) in ids.iter().zip(old.iter()) {
                    if let Some(s) = self.strokes.iter_mut().find(|s| s.id == *id) {
                        s.color = *color;
                    }
                }
            }
            Edit::AddMany { strokes } => {
                self.strokes
                    .retain(|s| !strokes.iter().any(|a| a.id == s.id));
            }
        }
        self.redo.push(edit);
        true
    }

    /// Redo the most recently undone edit. Returns `true` if something was redone.
    pub fn redo(&mut self) -> bool {
        let Some(edit) = self.redo.pop() else {
            return false;
        };
        match &edit {
            Edit::Add(stroke) => {
                self.strokes.push(stroke.clone());
            }
            Edit::Erase { index, stroke } => {
                if let Some(pos) = self.strokes.iter().position(|s| s.id == stroke.id) {
                    self.strokes.remove(pos);
                } else {
                    let _ = index;
                }
            }
            Edit::Move { ids, dx, dy } => {
                self.translate(ids, *dx, *dy);
            }
            Edit::DeleteMany { removed } => {
                let ids: Vec<StrokeId> = removed.iter().map(|(_, s)| s.id).collect();
                self.strokes.retain(|s| !ids.contains(&s.id));
            }
            Edit::Recolor { ids, new, .. } => {
                for id in ids.iter() {
                    if let Some(s) = self.strokes.iter_mut().find(|s| s.id == *id) {
                        s.color = *new;
                    }
                }
            }
            Edit::AddMany { strokes } => {
                for s in strokes.iter() {
                    self.strokes.push(s.clone());
                }
            }
        }
        self.undo.push(edit);
        true
    }

    /// The union bounds of all committed strokes, or `None` if the layer is empty.
    #[must_use]
    pub fn bounds(&self) -> Option<BBox> {
        let mut acc: Option<BBox> = None;
        for s in &self.strokes {
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
}

/// Whether any segment of `stroke` passes within `radius` of `(x, y)` (normalized).
fn stroke_hit(stroke: &Stroke, x: f32, y: f32, radius: f32) -> bool {
    let r2 = radius * radius;
    let pts = &stroke.points;
    if pts.len() == 1 {
        return dist2_point(pts[0].x, pts[0].y, x, y) <= r2;
    }
    pts.windows(2)
        .any(|w| dist2_to_segment(x, y, w[0].x, w[0].y, w[1].x, w[1].y) <= r2)
}

/// Squared distance between two points.
fn dist2_point(ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let dx = ax - bx;
    let dy = ay - by;
    dx * dx + dy * dy
}

/// Squared distance from `(px,py)` to segment `(ax,ay)-(bx,by)`.
fn dist2_to_segment(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let abx = bx - ax;
    let aby = by - ay;
    let len2 = abx * abx + aby * aby;
    if len2 <= f32::EPSILON {
        return dist2_point(px, py, ax, ay);
    }
    let t = (((px - ax) * abx + (py - ay) * aby) / len2).clamp(0.0, 1.0);
    let cx = ax + t * abx;
    let cy = ay + t * aby;
    dist2_point(px, py, cx, cy)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(x: f32, y: f32) -> InkPoint {
        InkPoint::new(x, y, 1.0, None, None, 0).unwrap()
    }

    fn drawn(layer: &mut InkLayer, pts: &[(f32, f32)]) -> StrokeId {
        layer
            .start_stroke(Tool::Pen, InkColor::BLACK, 0.01, 0)
            .unwrap();
        for &(x, y) in pts {
            layer.push_point(pt(x, y)).unwrap();
        }
        layer.finish_stroke().unwrap()
    }

    #[test]
    fn point_rejects_non_finite_and_clamps_range() {
        assert_eq!(
            InkPoint::new(f32::NAN, 0.0, 1.0, None, None, 0),
            Err(InkError::NonFinite("point coordinate"))
        );
        let p = InkPoint::new(1.5, -0.2, 2.0, Some(f32::INFINITY), None, 7).unwrap();
        assert_eq!((p.x, p.y, p.pressure), (1.0, 0.0, 1.0));
        assert_eq!(p.tilt_x, None); // infinite tilt dropped
        assert_eq!(p.timestamp_ms, 7);
    }

    #[test]
    fn tool_codes_round_trip() {
        for t in [Tool::Pen, Tool::Highlighter, Tool::Eraser] {
            assert_eq!(Tool::from_code(t.code()), Some(t));
        }
        assert_eq!(Tool::from_code(9), None);
        assert!(Tool::Pen.is_ink() && !Tool::Eraser.is_ink());
    }

    #[test]
    fn start_stroke_rejects_bad_width() {
        let mut l = InkLayer::new();
        assert!(l.start_stroke(Tool::Pen, InkColor::BLACK, 0.0, 0).is_err());
        assert!(l
            .start_stroke(Tool::Pen, InkColor::BLACK, f32::NAN, 0)
            .is_err());
    }

    #[test]
    fn empty_stroke_is_discarded() {
        let mut l = InkLayer::new();
        l.start_stroke(Tool::Pen, InkColor::BLACK, 0.01, 0).unwrap();
        assert_eq!(l.finish_stroke(), None);
        assert!(l.is_empty());
        assert!(!l.can_undo());
    }

    #[test]
    fn push_point_without_start_errors() {
        let mut l = InkLayer::new();
        assert_eq!(l.push_point(pt(0.1, 0.1)), Err(InkError::NoActiveStroke));
    }

    #[test]
    fn ids_are_monotonic() {
        let mut l = InkLayer::new();
        let a = drawn(&mut l, &[(0.1, 0.1), (0.2, 0.2)]);
        let b = drawn(&mut l, &[(0.3, 0.3), (0.4, 0.4)]);
        assert_eq!((a, b), (StrokeId(0), StrokeId(1)));
        assert_eq!(l.strokes().len(), 2);
    }

    #[test]
    fn undo_redo_add() {
        let mut l = InkLayer::new();
        drawn(&mut l, &[(0.1, 0.1), (0.2, 0.2)]);
        assert_eq!(l.strokes().len(), 1);
        assert!(l.undo());
        assert!(l.is_empty());
        assert!(l.redo());
        assert_eq!(l.strokes().len(), 1);
        assert!(!l.redo()); // nothing left
    }

    #[test]
    fn new_edit_clears_redo() {
        let mut l = InkLayer::new();
        drawn(&mut l, &[(0.1, 0.1), (0.2, 0.2)]);
        assert!(l.undo());
        drawn(&mut l, &[(0.5, 0.5), (0.6, 0.6)]);
        assert!(!l.can_redo(), "a new stroke forks the timeline");
    }

    #[test]
    fn erase_hits_and_restores_via_undo() {
        let mut l = InkLayer::new();
        drawn(&mut l, &[(0.10, 0.10), (0.20, 0.10)]); // horizontal segment at y=0.1
        drawn(&mut l, &[(0.80, 0.80), (0.90, 0.80)]); // far away
        let removed = l.erase_at(0.15, 0.10, 0.02);
        assert_eq!(removed, vec![StrokeId(0)]);
        assert_eq!(l.strokes().len(), 1);
        assert!(l.undo());
        assert_eq!(l.strokes().len(), 2, "erased stroke restored");
        // restored at original position (paint order preserved: id 0 then id 1)
        assert_eq!(l.strokes()[0].id, StrokeId(0));
    }

    #[test]
    fn erase_miss_changes_nothing() {
        let mut l = InkLayer::new();
        drawn(&mut l, &[(0.1, 0.1), (0.2, 0.1)]);
        assert!(l.erase_at(0.9, 0.9, 0.02).is_empty());
        assert_eq!(l.strokes().len(), 1);
    }

    #[test]
    fn hit_test_is_non_destructive() {
        let mut l = InkLayer::new();
        drawn(&mut l, &[(0.1, 0.1), (0.2, 0.1)]);
        assert_eq!(l.strokes_hit(0.15, 0.1, 0.02), vec![StrokeId(0)]);
        assert_eq!(l.strokes().len(), 1, "hit-test does not remove");
    }

    #[test]
    fn bounds_expand_by_half_width() {
        let mut l = InkLayer::new();
        drawn(&mut l, &[(0.40, 0.40), (0.60, 0.60)]);
        let b = l.bounds().unwrap();
        // width 0.01 → margin 0.005
        assert!((b.x0 - 0.395).abs() < 1e-4 && (b.y1 - 0.605).abs() < 1e-4);
    }

    #[test]
    fn from_strokes_sets_next_id_past_max() {
        let mut seed = InkLayer::new();
        drawn(&mut seed, &[(0.1, 0.1), (0.2, 0.2)]);
        drawn(&mut seed, &[(0.3, 0.3), (0.4, 0.4)]);
        let mut l = InkLayer::from_strokes(seed.strokes().to_vec());
        let id = drawn(&mut l, &[(0.5, 0.5), (0.6, 0.6)]);
        assert_eq!(id, StrokeId(2), "new id is past the largest loaded id");
    }

    #[test]
    fn pressure_to_width_scales() {
        assert!((pressure_to_width(0.02, 0.0) - 0.01).abs() < 1e-6);
        assert!((pressure_to_width(0.02, 1.0) - 0.02).abs() < 1e-6);
    }

    fn ids(l: &InkLayer) -> Vec<StrokeId> {
        l.strokes().iter().map(|s| s.id).collect()
    }

    #[test]
    fn redo_of_erase_re_removes() {
        let mut l = InkLayer::new();
        drawn(&mut l, &[(0.10, 0.10), (0.20, 0.10)]);
        drawn(&mut l, &[(0.10, 0.80), (0.20, 0.80)]);
        l.erase_at(0.15, 0.10, 0.02); // remove S0
        assert_eq!(ids(&l), vec![StrokeId(1)]);
        assert!(l.undo()); // restore S0
        assert_eq!(ids(&l), vec![StrokeId(0), StrokeId(1)]);
        assert!(l.redo()); // re-erase S0
        assert_eq!(ids(&l), vec![StrokeId(1)]);
    }

    #[test]
    fn erase_restores_middle_stroke_in_order() {
        let mut l = InkLayer::new();
        drawn(&mut l, &[(0.10, 0.10), (0.20, 0.10)]); // S0 @ y=0.10
        drawn(&mut l, &[(0.10, 0.50), (0.20, 0.50)]); // S1 @ y=0.50 (middle)
        drawn(&mut l, &[(0.10, 0.90), (0.20, 0.90)]); // S2 @ y=0.90
        assert_eq!(l.erase_at(0.15, 0.50, 0.02), vec![StrokeId(1)]);
        assert_eq!(ids(&l), vec![StrokeId(0), StrokeId(2)]);
        assert!(l.undo());
        assert_eq!(
            ids(&l),
            vec![StrokeId(0), StrokeId(1), StrokeId(2)],
            "middle stroke restored at its original index"
        );
    }

    #[test]
    fn single_gesture_erasing_two_strokes_undoes_one_at_a_time() {
        let mut l = InkLayer::new();
        drawn(&mut l, &[(0.10, 0.10), (0.20, 0.10)]); // S0 horizontal
        drawn(&mut l, &[(0.14, 0.05), (0.14, 0.15)]); // S1 vertical, crosses near (0.15,0.10)
        let removed = l.erase_at(0.15, 0.10, 0.03);
        assert_eq!(removed.len(), 2, "both strokes hit by one erase point");
        assert!(l.is_empty());
        assert!(l.undo()); // restore one
        assert_eq!(l.strokes().len(), 1);
        assert!(l.undo()); // restore the other
        assert_eq!(
            ids(&l),
            vec![StrokeId(0), StrokeId(1)],
            "paint order preserved"
        );
    }

    #[test]
    fn interleaved_add_erase_undo_redo_reconstructs_exactly() {
        let mut l = InkLayer::new();
        drawn(&mut l, &[(0.10, 0.10), (0.20, 0.10)]); // add S0
        l.erase_at(0.15, 0.10, 0.02); // erase S0
        drawn(&mut l, &[(0.50, 0.50), (0.60, 0.50)]); // add S1
        assert_eq!(ids(&l), vec![StrokeId(1)]);
        // undo all three edits in reverse
        assert!(l.undo()); // remove S1
        assert!(l.undo()); // restore S0
        assert_eq!(ids(&l), vec![StrokeId(0)]);
        assert!(l.undo()); // remove S0
        assert!(l.is_empty());
        assert!(!l.undo());
        // redo all three forward
        assert!(l.redo()); // add S0
        assert!(l.redo()); // erase S0
        assert!(l.redo()); // add S1
        assert_eq!(
            ids(&l),
            vec![StrokeId(1)],
            "redo reconstructs original state"
        );
    }
}
