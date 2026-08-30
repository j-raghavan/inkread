//! Pure text-selection logic (RR11 / ADR-INKREAD-0009 D1).
//!
//! The document backend supplies the page's characters as [`CharBox`]es — each a glyph plus its
//! **normalized** box (`[0,1]`, top-left origin, exactly like `PageLink`/ink). This module turns a
//! tap point or a dragged rectangle into a [`TextSelection`] (the text + the boxes to highlight).
//! It is **pure and dependency-free** so it is fully host-tested without pdfium; the backend only
//! has to produce `CharBox`es (see `fixed::pdf`).

// Split by what a query *is*: finding a string, resolving a word, detecting columns, and selecting a
// range. The shared vocabulary — the geometry types, the character predicates, and the line/glyph
// helpers every group leans on — stays here, and each submodule opens with `use super::*`.
mod columns;
mod search;
mod span;
mod word;

// The module's public surface is unchanged by the split: callers still write
// `text_select::word_at(..)`, not `text_select::word::word_at(..)`.
pub use search::find_matches;
pub use span::{anchored_span, text_in_rect, text_line_span};
pub use word::word_at;

/// A normalized rectangle `[0,1]` with a top-left origin. Mirrors `PageLink`'s convention.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NormRect {
    /// Left edge `[0,1]`.
    pub x0: f32,
    /// Top edge `[0,1]`.
    pub y0: f32,
    /// Right edge `[0,1]`.
    pub x1: f32,
    /// Bottom edge `[0,1]`.
    pub y1: f32,
}

impl NormRect {
    /// Whether the point `(x, y)` lies within this rect (inclusive).
    #[must_use]
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }

    /// Whether this rect overlaps `other` (any shared area, edges touching counts).
    #[must_use]
    pub fn intersects(&self, other: &NormRect) -> bool {
        self.x0 <= other.x1 && other.x0 <= self.x1 && self.y0 <= other.y1 && other.y0 <= self.y1
    }

    /// The smallest rect covering both.
    #[must_use]
    pub fn union(&self, other: &NormRect) -> NormRect {
        NormRect {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }

    fn height(&self) -> f32 {
        (self.y1 - self.y0).max(0.0)
    }
}

/// A single glyph with its normalized box — the unit selection works over. Backends emit these in
/// reading order.
#[derive(Debug, Clone, PartialEq)]
pub struct CharBox {
    /// The character.
    pub ch: char,
    /// Its normalized box.
    pub rect: NormRect,
    /// Reflow-stable source anchor (ADR-INKREAD-0012), when the backend is reflowable. `None` for
    /// fixed-layout PDF (its position *is* the page). Reflow backends fill it so a selection or a
    /// page's first glyph can be turned into a `PinPosition` that survives a font-size change.
    pub anchor: Option<TextAnchor>,
    /// Set on the last glyph of a line the layout broke **mid-word** — see [`Wrap`]. `None` on
    /// every other glyph, and on every glyph of a fixed-layout backend, which lays nothing out and
    /// therefore knows nothing (there, selection reads the break off the page; see [`wrap_of`]).
    pub wrap: Option<Wrap>,
}

/// What a line break did to the word it split. Only a backend that performed the layout can say:
/// the two cases print identically — "self-evident" broken before its hyphen and "well-known"
/// broken at its own both end a line "self-"/"well-" — so the fact travels with the glyph instead
/// of being guessed from it.
///
/// Field-identical to `inkread_epub::layout::Wrap` but kept as the core's own selection-domain type
/// so the selection model isn't coupled to the renderer's glyph type, exactly like [`TextAnchor`]
/// (the backends convert at the boundary).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrap {
    /// The layout **inserted** the hyphen shown at the break. It is not in the source text, so
    /// rejoining the halves drops it: "pontifi-" + "cate" = "pontificate".
    SoftHyphen,
    /// The break needed no hyphen — the word already had one there, or it is unspaced script (CJK).
    /// Every character is the source's, so rejoining keeps them all: "well-" + "known".
    Kept,
}

/// A reflow-stable text anchor: the source block (reading-order index in the chapter) and the
/// chapter-relative character offset. Field-identical to `inkread_epub::layout::SourceAnchor` but
/// kept as the core's own selection-domain type so the selection model isn't coupled to the
/// renderer's glyph type (the backends convert at the boundary). The backend frames it into a full
/// `PinPosition` (it owns the chapter index/id).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextAnchor {
    /// Reading-order index of the source block in the chapter.
    pub block: usize,
    /// Chapter-relative character offset.
    pub char_offset: usize,
}

/// A resolved selection: the selected text plus the boxes a shell highlights (one per text line).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TextSelection {
    /// The selected text (trimmed; line runs joined by a single space).
    pub text: String,
    /// One box per line run of the selection (for highlight rendering / dirty-rect refresh).
    pub boxes: Vec<NormRect>,
}

impl TextSelection {
    /// Whether the selection produced no text.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

/// One occurrence of a search query on a page (RR2 in-document search).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchMatch {
    /// Highlight boxes, one per line run the match spans (like a [`TextSelection`]) — for
    /// drawing the on-page highlight and the dirty-rect refresh when the reader jumps to it.
    pub boxes: Vec<NormRect>,
    /// A short context snippet around the match (for the results list), with `…` where trimmed.
    pub snippet: String,
}

/// Vertical tolerance (page-height fraction) for "same line" / nearest-on-line tap matching.
const LINE_MARGIN: f32 = 0.012;
/// Horizontal tolerance (page-width fraction) for snapping a near-miss tap to a glyph.
const HIT_TOLERANCE: f32 = 0.03;

/// The nearest non-whitespace glyph before `i` (a backend may or may not emit a glyph for the line
/// break itself, so neither wrap test may assume adjacency across it).
fn prev_glyph(chars: &[CharBox], i: usize) -> Option<usize> {
    chars[..i].iter().rposition(|c| !c.ch.is_whitespace())
}

/// The nearest non-whitespace glyph after `i`.
fn next_glyph(chars: &[CharBox], i: usize) -> Option<usize> {
    chars
        .get(i + 1..)?
        .iter()
        .position(|c| !c.ch.is_whitespace())
        .map(|j| i + 1 + j)
}

/// The glyph at `(x, y)`: the one whose box contains it, else the nearest on the same line within
/// [`HIT_TOLERANCE`] (so a tap landing just off a glyph still selects it).
fn hit_char(chars: &[CharBox], x: f32, y: f32) -> Option<usize> {
    if let Some(i) = chars.iter().position(|c| c.rect.contains(x, y)) {
        return Some(i);
    }
    let mut best: Option<usize> = None;
    let mut best_d = f32::MAX;
    for (i, c) in chars.iter().enumerate() {
        if y < c.rect.y0 - LINE_MARGIN || y > c.rect.y1 + LINE_MARGIN {
            continue; // not on this glyph's line
        }
        let cx = (c.rect.x0 + c.rect.x1) * 0.5;
        let d = (cx - x).abs();
        if d < best_d {
            best_d = d;
            best = Some(i);
        }
    }
    best.filter(|_| best_d <= HIT_TOLERANCE)
}

/// Union the boxes of a glyph run into per-line highlight rects (a word is one line, but a
/// hyphenated or searched-across wrap spans two). Whitespace glyphs are skipped: a backend may box
/// them degenerately (or off the line), which would split a run into spurious rects.
fn line_boxes(run: &[CharBox]) -> Vec<NormRect> {
    let mut boxes = Vec::new();
    for c in run.iter().filter(|c| !c.ch.is_whitespace()) {
        match boxes.last_mut() {
            Some(b) if same_line(b, &c.rect) => *b = b.union(&c.rect),
            _ => boxes.push(c.rect),
        }
    }
    boxes
}

/// Whether two boxes share enough vertical overlap to be on the same text line.
fn same_line(a: &NormRect, b: &NormRect) -> bool {
    let overlap = a.y1.min(b.y1) - a.y0.max(b.y0);
    let min_h = a.height().min(b.height()).max(1e-4);
    overlap > 0.4 * min_h
}

/// Whether `a` and `b` are part of the same word: same line, both word-ish, not two connectors.
fn joins(a: &CharBox, b: &CharBox) -> bool {
    same_line(&a.rect, &b.rect)
        && is_word_or_connector(a.ch)
        && is_word_or_connector(b.ch)
        && (is_word_char(a.ch) || is_word_char(b.ch))
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric()
}

fn is_connector(c: char) -> bool {
    matches!(c, '\'' | '\u{2019}') || is_hyphen(c)
}

/// Hyphens a line break can split a word across: ASCII, Unicode, and the soft hyphen.
fn is_hyphen(c: char) -> bool {
    matches!(c, '-' | '\u{2010}' | '\u{00ad}')
}

fn is_word_or_connector(c: char) -> bool {
    is_word_char(c) || is_connector(c)
}

#[cfg(test)]
#[path = "text_select_tests.rs"]
mod tests;
