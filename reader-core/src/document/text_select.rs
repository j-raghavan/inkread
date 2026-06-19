//! Pure text-selection logic (RR11 / ADR-INKREAD-0009 D1).
//!
//! The document backend supplies the page's characters as [`CharBox`]es — each a glyph plus its
//! **normalized** box (`[0,1]`, top-left origin, exactly like `PageLink`/ink). This module turns a
//! tap point or a dragged rectangle into a [`TextSelection`] (the text + the boxes to highlight).
//! It is **pure and dependency-free** so it is fully host-tested without pdfium; the backend only
//! has to produce `CharBox`es (see `fixed::pdf`).

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

/// Vertical tolerance (page-height fraction) for "same line" / nearest-on-line tap matching.
const LINE_MARGIN: f32 = 0.012;
/// Horizontal tolerance (page-width fraction) for snapping a near-miss tap to a glyph.
const HIT_TOLERANCE: f32 = 0.03;

/// The word under `(x, y)` (tap / long-press), or `None` if the point isn't on a word glyph
/// (whitespace, punctuation, or empty space). Expands across letters/digits and *internal*
/// apostrophes/hyphens (`don't`, `well-known`).
#[must_use]
pub fn word_at(chars: &[CharBox], x: f32, y: f32) -> Option<TextSelection> {
    let hit = hit_char(chars, x, y)?;
    if !is_word_char(chars[hit].ch) {
        return None;
    }
    let mut start = hit;
    while start > 0 && joins(&chars[start - 1], &chars[start]) {
        start -= 1;
    }
    let mut end = hit;
    while end + 1 < chars.len() && joins(&chars[end], &chars[end + 1]) {
        end += 1;
    }
    let run = &chars[start..=end];
    let text = run
        .iter()
        .map(|c| c.ch)
        .collect::<String>()
        .trim_matches(is_connector)
        .to_string();
    if text.is_empty() {
        return None;
    }
    Some(TextSelection {
        text,
        boxes: line_boxes(run),
    })
}

/// The text whose glyphs fall within `rect` (drag-highlight), in reading order, with one highlight
/// box per line run.
#[must_use]
pub fn text_in_rect(chars: &[CharBox], rect: NormRect) -> TextSelection {
    let selected: Vec<&CharBox> = chars.iter().filter(|c| rect.intersects(&c.rect)).collect();
    if selected.is_empty() {
        return TextSelection::default();
    }
    // Group consecutive glyphs into line runs (a new line breaks the run).
    let mut lines: Vec<Vec<&CharBox>> = Vec::new();
    for c in selected {
        match lines.last_mut() {
            Some(line) if same_line(&line[0].rect, &c.rect) => line.push(c),
            _ => lines.push(vec![c]),
        }
    }
    let mut parts = Vec::with_capacity(lines.len());
    let mut boxes = Vec::with_capacity(lines.len());
    for line in &lines {
        parts.push(
            line.iter()
                .map(|c| c.ch)
                .collect::<String>()
                .trim()
                .to_string(),
        );
        let mut b = line[0].rect;
        for c in &line[1..] {
            b = b.union(&c.rect);
        }
        boxes.push(b);
    }
    TextSelection {
        text: parts.join(" ").trim().to_string(),
        boxes,
    }
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

/// Union the boxes of a single-line glyph run into per-line highlight rects (a word is one line,
/// but guard for a run that wraps).
fn line_boxes(run: &[CharBox]) -> Vec<NormRect> {
    let mut boxes = Vec::new();
    for c in run {
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
    matches!(c, '\'' | '\u{2019}' | '-')
}

fn is_word_or_connector(c: char) -> bool {
    is_word_char(c) || is_connector(c)
}

#[cfg(test)]
mod tests {
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
            })
            .collect()
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
}
