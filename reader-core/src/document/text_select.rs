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
    /// Reflow-stable source anchor (ADR-INKREAD-0012), when the backend is reflowable. `None` for
    /// fixed-layout PDF (its position *is* the page). Reflow backends fill it so a selection or a
    /// page's first glyph can be turned into a `PinPosition` that survives a font-size change.
    pub anchor: Option<TextAnchor>,
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
fn glyph_selected(rect: &NormRect, glyph: &NormRect) -> bool {
    let cx = (glyph.x0 + glyph.x1) * 0.5;
    if cx < rect.x0 || cx > rect.x1 {
        return false;
    }
    let cy = (glyph.y0 + glyph.y1) * 0.5;
    let rect_mid_y = (rect.y0 + rect.y1) * 0.5;
    (cy >= rect.y0 && cy <= rect.y1) || (glyph.y0 <= rect_mid_y && rect_mid_y <= glyph.y1)
}

/// The text whose glyphs fall within `rect` (drag-highlight), in reading order, with one highlight
/// box per line run.
#[must_use]
pub fn text_in_rect(chars: &[CharBox], rect: NormRect) -> TextSelection {
    let selected: Vec<&CharBox> = chars
        .iter()
        .filter(|c| glyph_selected(&rect, &c.rect))
        .collect();
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

/// The reflow-stable anchors of the **first and last** glyphs a rectangle selects, in reading order
/// — the `[start, end]` pin pair for a highlight / note / Digest range (RR11-FR4 / ADR-INKREAD-0012).
/// Uses the same selection predicate as [`text_in_rect`]. Returns `None` when the selection is empty
/// or its glyphs carry no anchor (a fixed-layout backend), so callers fall back to a page anchor.
#[must_use]
pub fn anchored_span(chars: &[CharBox], rect: NormRect) -> Option<(TextAnchor, TextAnchor)> {
    let mut selected = chars.iter().filter(|c| glyph_selected(&rect, &c.rect));
    let start = selected.next()?.anchor?;
    // `end` is the last selected glyph's anchor; a single-glyph selection collapses to `start`.
    let end = selected.next_back().and_then(|c| c.anchor).unwrap_or(start);
    Some((start, end))
}

/// Select the text a **drag** sweeps from `start` to `end` (normalized points), the reading-order
/// multi-line selection (RR11). Mirrors how a desktop selection reads, with the project's twist:
/// the line the drag *starts* on and every line through to the one *before* the lift are taken
/// **whole** (complete characters, full line width); the **last** line (where the pen lifted) is
/// taken only up to the word under `end.x`. Consecutive line boxes are grown to meet the next
/// line's top so the highlight is one continuous block (no inter-line gaps). Word-less edge lines
/// are dropped. Direction-agnostic: the lift point's line is the partial one either way.
pub fn text_line_span(chars: &[CharBox], start: (f32, f32), end: (f32, f32)) -> TextSelection {
    if chars.is_empty() {
        return TextSelection::default();
    }
    // Group glyphs into reading-order line runs (backends emit glyphs in reading order), skipping
    // DEGENERATE glyphs — zero-width/height boxes the backend emits at the right margin (line-break
    // hyphen artifacts). They are invisible, but if grouped they fragment the lines and, sitting
    // between two real lines with a smaller `y`, defeat the gap-fill below — leaving the stripes.
    let mut lines: Vec<Vec<&CharBox>> = Vec::new();
    for c in chars
        .iter()
        .filter(|c| c.rect.x1 > c.rect.x0 && c.rect.y1 > c.rect.y0)
    {
        match lines.last_mut() {
            Some(line) if same_line(&line[0].rect, &c.rect) => line.push(c),
            _ => lines.push(vec![c]),
        }
    }
    if lines.is_empty() {
        return TextSelection::default();
    }
    // A line's vertical span (min y0 / max y1 over its glyphs).
    let line_span = |line: &[&CharBox]| -> (f32, f32) {
        let (mut y0, mut y1) = (line[0].rect.y0, line[0].rect.y1);
        for c in &line[1..] {
            y0 = y0.min(c.rect.y0);
            y1 = y1.max(c.rect.y1);
        }
        (y0, y1)
    };
    // Select the lines the drag's vertical range actually OVERLAPS — never the merely-nearest line.
    // A lift that lands in the blank gap below the last line (above the next paragraph/heading)
    // overlaps the last line but not the next one, so the selection can't overshoot into it.
    let y_top = start.1.min(end.1);
    let y_bot = start.1.max(end.1);
    let downward = end.1 >= start.1;
    let ex = end.0;
    let sel: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| {
            let (y0, y1) = line_span(line);
            y1 >= y_top && y0 <= y_bot
        })
        .map(|(i, _)| i)
        .collect();
    if sel.is_empty() {
        return TextSelection::default(); // both endpoints in gaps — no line truly covered
    }
    // The lift line (the candidate for clipping) is the bottom-most overlap for a downward drag,
    // the top-most for an upward one.
    let focus = if downward {
        *sel.last().unwrap()
    } else {
        sel[0]
    };
    // Clip that line to the lift word ONLY when the pen lifted *on* it (lift y inside the line). If
    // the pen lifted in the gap PAST the line (dragged beyond it), the whole line was meant — taking
    // it whole, not clipped. (This is the "too little" case: lifting just below the last line.)
    let (fy0, fy1) = line_span(&lines[focus]);
    let clip_focus = sel.len() > 1 && end.1 >= fy0 && end.1 <= fy1;

    let mut parts: Vec<String> = Vec::new();
    let mut boxes: Vec<NormRect> = Vec::new();
    for &idx in &sel {
        let line = &lines[idx];
        // The pen-lift line is clipped to the word under `end.x` only when the pen lifted on it;
        // every other line (and a lift past the end) is taken whole.
        let take: &[&CharBox] = if idx == focus && clip_focus {
            // Last glyph whose box starts at/before the lift x, then extend to the word's end.
            let mut last = 0usize;
            for (j, c) in line.iter().enumerate() {
                if c.rect.x0 <= ex {
                    last = j;
                }
            }
            while last + 1 < line.len() && joins(line[last], line[last + 1]) {
                last += 1;
            }
            &line[..=last]
        } else {
            &line[..]
        };
        if take.is_empty() {
            continue;
        }
        let mut bx = take[0].rect;
        for c in &take[1..] {
            bx = bx.union(&c.rect);
        }
        parts.push(
            take.iter()
                .map(|c| c.ch)
                .collect::<String>()
                .trim()
                .to_string(),
        );
        boxes.push(bx);
    }
    // Drop word-less edge lines (a stray blank line clipped at an end).
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
    // Grow each box down to the next line's top so the highlight is one continuous block (fills the
    // inter-line gaps the per-line glyph boxes leave). Boxes are already ordered top to bottom.
    for i in 0..boxes.len().saturating_sub(1) {
        if boxes[i + 1].y0 > boxes[i].y1 {
            boxes[i].y1 = boxes[i + 1].y0;
        }
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
                anchor: None,
            })
            .collect()
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
