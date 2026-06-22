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

/// One occurrence of a search query on a page (RR2 in-document search).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchMatch {
    /// Highlight boxes, one per line run the match spans (like a [`TextSelection`]) — for
    /// drawing the on-page highlight and the dirty-rect refresh when the reader jumps to it.
    pub boxes: Vec<NormRect>,
    /// A short context snippet around the match (for the results list), with `…` where trimmed.
    pub snippet: String,
}

/// Context characters kept on each side of a match for its results-list snippet.
const SNIPPET_CONTEXT: usize = 28;

/// Case-insensitive, whitespace-normalized substring search over a page's `chars`. Returns one
/// [`SearchMatch`] per **non-overlapping** occurrence, left to right, each with per-line highlight
/// boxes and a context snippet. An empty or whitespace-only `query` yields no matches. Pure and
/// dependency-free (host-tested) — the backend only supplies the page's `CharBox`es (RR21-FR3:
/// never panics).
#[must_use]
pub fn find_matches(chars: &[CharBox], query: &str) -> Vec<SearchMatch> {
    let needle: Vec<char> = normalize_query(query);
    if needle.is_empty() {
        return Vec::new();
    }
    // Normalized page text as chars, with a parallel map from each normalized char back to its
    // source `chars` index (so a hit's positions resolve to highlight boxes + a snippet).
    let mut hay: Vec<char> = Vec::with_capacity(chars.len());
    let mut src: Vec<usize> = Vec::with_capacity(chars.len());
    let mut prev_space = false;
    let mut prev_rect: Option<NormRect> = None;
    for (i, c) in chars.iter().enumerate() {
        if c.ch.is_whitespace() {
            if !prev_space && !hay.is_empty() {
                hay.push(' ');
                src.push(i);
                prev_space = true;
            }
        } else {
            // A line break with no explicit space glyph (text wrap) still separates words, so the
            // query "foo bar" matches across the wrap.
            if !prev_space {
                if let Some(pr) = prev_rect {
                    if !same_line(&pr, &c.rect) {
                        hay.push(' ');
                        src.push(i);
                    }
                }
            }
            for lc in c.ch.to_lowercase() {
                hay.push(lc);
                src.push(i);
            }
            prev_space = false;
            prev_rect = Some(c.rect);
        }
    }

    let n = needle.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i + n <= hay.len() {
        if hay[i..i + n] == needle[..] {
            let s = src[i];
            let e = src[i + n - 1];
            out.push(SearchMatch {
                boxes: line_boxes(&chars[s..=e]),
                snippet: snippet_around(&hay, i, n),
            });
            i += n; // non-overlapping: resume past this match
        } else {
            i += 1;
        }
    }
    out
}

/// Lowercase + collapse internal whitespace + trim a query into its char sequence.
fn normalize_query(query: &str) -> Vec<char> {
    let mut out: Vec<char> = Vec::new();
    let mut prev_space = false;
    for c in query.chars() {
        if c.is_whitespace() {
            if !out.is_empty() && !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            for lc in c.to_lowercase() {
                out.push(lc);
            }
            prev_space = false;
        }
    }
    while out.last() == Some(&' ') {
        out.pop();
    }
    out
}

/// A `…`-trimmed context window of `hay` around the match at `[start, start+len)`.
fn snippet_around(hay: &[char], start: usize, len: usize) -> String {
    let from = start.saturating_sub(SNIPPET_CONTEXT);
    let to = (start + len + SNIPPET_CONTEXT).min(hay.len());
    let mut s = String::new();
    if from > 0 {
        s.push('…');
    }
    s.extend(&hay[from..to]);
    if to < hay.len() {
        s.push('…');
    }
    s
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

/// Select **whole lines** overlapped by `rect`'s vertical span — the multi-line drag (RR11). Unlike
/// [`text_in_rect`] (which keeps only the glyphs geometrically inside the rect, clipping the first
/// and last lines along a diagonal drag), this takes every glyph of every line the band meaningfully
/// covers, so complete characters are selected: each line becomes one full-width box and the overall
/// selection spans the full height of all lines by the widest line's width. `rect.x` is ignored
/// (lines are taken whole). A line is included when the band covers more than a third of its height,
/// so a band edge that merely grazes the neighbouring line doesn't pull it in. Trailing/leading
/// word-less lines (e.g. a band edge clipping a blank line) are dropped.
pub fn text_lines_in_rect(chars: &[CharBox], rect: NormRect) -> TextSelection {
    if chars.is_empty() {
        return TextSelection::default();
    }
    // Group every glyph into reading-order line runs (backends emit glyphs in reading order).
    let mut lines: Vec<Vec<&CharBox>> = Vec::new();
    for c in chars {
        match lines.last_mut() {
            Some(line) if same_line(&line[0].rect, &c.rect) => line.push(c),
            _ => lines.push(vec![c]),
        }
    }
    let mut parts: Vec<String> = Vec::new();
    let mut boxes: Vec<NormRect> = Vec::new();
    for line in &lines {
        // The line's full vertical span + box (union of all its glyphs — the whole line, not a clip).
        let mut b = line[0].rect;
        for c in &line[1..] {
            b = b.union(&c.rect);
        }
        let overlap = b.y1.min(rect.y1) - b.y0.max(rect.y0);
        if overlap <= 0.3 * b.height().max(1e-4) {
            continue; // the band only grazes this line — don't pull it in
        }
        parts.push(
            line.iter()
                .map(|c| c.ch)
                .collect::<String>()
                .trim()
                .to_string(),
        );
        boxes.push(b);
    }
    // Drop word-less edge lines (a band edge clipping a blank line shows as an empty trailing line).
    while parts.last().is_some_and(String::is_empty) {
        parts.pop();
        boxes.pop();
    }
    while parts.first().is_some_and(String::is_empty) {
        parts.remove(0);
        boxes.remove(0);
    }
    if parts.is_empty() {
        return TextSelection::default();
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
    fn text_lines_in_rect_takes_whole_lines_from_a_diagonal_band() {
        // Three lines; a diagonal drag band that starts mid-line-1 and ends mid-line-3 with a narrow
        // x-range must still select the FULL text of all three lines (not the clipped sub-rect).
        let mut chars = line("the first line here", 0.0, 0.8, 0.10, 0.03);
        chars.extend(line("the middle line two", 0.0, 0.8, 0.16, 0.03));
        chars.extend(line("the last line three", 0.0, 0.8, 0.22, 0.03));
        // Narrow x; starts mid-line-1 (0.10..0.13) and ends mid-line-3 (0.22..0.25).
        let band = NormRect {
            x0: 0.30,
            y0: 0.115,
            x1: 0.55,
            y1: 0.235,
        };
        let sel = text_lines_in_rect(&chars, band);
        assert_eq!(sel.boxes.len(), 3, "three lines → three full-line boxes");
        // Every line's complete text is present (full chars, not the diagonal clip).
        assert!(sel.text.contains("the first line here"));
        assert!(sel.text.contains("the middle line two"));
        assert!(sel.text.contains("the last line three"));
        // Each box spans the full line width, not the narrow band x.
        assert!(sel.boxes[0].x0 <= 0.01 && sel.boxes[0].x1 >= 0.79);
    }

    #[test]
    fn text_lines_in_rect_ignores_a_barely_grazed_neighbour_and_drops_blank_edge() {
        let mut chars = line("alpha beta", 0.0, 0.5, 0.10, 0.03);
        chars.extend(line("gamma delta", 0.0, 0.5, 0.16, 0.03));
        chars.extend(line("   ", 0.0, 0.5, 0.22, 0.03)); // a whitespace-only line below
                                                         // Band covers line 1 fully, line 2 fully, and only grazes the blank line's top (<30%).
        let band = NormRect {
            x0: 0.0,
            y0: 0.10,
            x1: 1.0,
            y1: 0.225,
        };
        let sel = text_lines_in_rect(&chars, band);
        assert_eq!(sel.boxes.len(), 2, "blank/grazed trailing line dropped");
        assert_eq!(sel.text, "alpha beta gamma delta");
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
}
