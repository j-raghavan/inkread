//! Selecting a range: a rectangle, or a drag (RR11).
//!
//! [`text_in_rect`] is the lasso/rect case. [`text_line_span`] is the drag, and reads like a
//! desktop selection with one deliberate difference: the line the drag starts on and every line
//! through to the one before the lift are taken whole, and only the last line is clipped to the
//! word under the lift point. Line boxes are grown to meet the next line's top so the highlight is
//! one continuous block rather than a stack with gaps.

use super::columns::{confine_to_columns, glyph_selected};
use super::*;

/// The text whose glyphs fall within `rect` (drag-highlight), in reading order, with one highlight
/// box per line run.
#[must_use]
pub fn text_in_rect(chars: &[CharBox], rect: NormRect) -> TextSelection {
    let confined = confine_to_columns(chars, rect.x0, rect.y0, rect.x1, rect.y1);
    let chars: &[CharBox] = &confined;
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
    let mut parts: Vec<(String, Option<Wrap>)> = Vec::with_capacity(lines.len());
    let mut boxes = Vec::with_capacity(lines.len());
    for line in &lines {
        let text = line
            .iter()
            .map(|c| c.ch)
            .collect::<String>()
            .trim()
            .to_string();
        let wrap = line.last().and_then(|last| wrap_after_part(last, &text));
        parts.push((text, wrap));
        let mut b = line[0].rect;
        for c in &line[1..] {
            b = b.union(&c.rect);
        }
        boxes.push(b);
    }
    TextSelection {
        text: join_lines(&parts).trim().to_string(),
        boxes,
    }
}

/// The reflow-stable anchors of the **first and last** glyphs a rectangle selects, in reading order
/// — the `[start, end]` pin pair for a highlight / note / Digest range (RR11-FR4 / ADR-INKREAD-0012).
/// Uses the same selection predicate as [`text_in_rect`]. Returns `None` when the selection is empty
/// or its glyphs carry no anchor (a fixed-layout backend), so callers fall back to a page anchor.
#[must_use]
pub fn anchored_span(chars: &[CharBox], rect: NormRect) -> Option<(TextAnchor, TextAnchor)> {
    let confined = confine_to_columns(chars, rect.x0, rect.y0, rect.x1, rect.y1);
    let mut selected = confined.iter().filter(|c| glyph_selected(&rect, &c.rect));
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
    // Confine to the column(s) the drag covers so a one-column lasso on a two-column page doesn't
    // take each shared baseline whole across the gutter (the reported bug). No-op on single columns.
    let confined = confine_to_columns(chars, start.0, start.1, end.0, end.1);
    let chars: &[CharBox] = &confined;
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
    // `sel` is non-empty (checked above), so both ends index safely.
    let focus = if downward { sel[sel.len() - 1] } else { sel[0] };
    // Clip that line to the lift word ONLY when the pen lifted *on* it (lift y inside the line). If
    // the pen lifted in the gap PAST the line (dragged beyond it), the whole line was meant — taking
    // it whole, not clipped. (This is the "too little" case: lifting just below the last line.)
    let (fy0, fy1) = line_span(&lines[focus]);
    let clip_focus = sel.len() > 1 && end.1 >= fy0 && end.1 <= fy1;

    let mut parts: Vec<(String, Option<Wrap>)> = Vec::new();
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
        let text = take
            .iter()
            .map(|c| c.ch)
            .collect::<String>()
            .trim()
            .to_string();
        let wrap = take.last().and_then(|last| wrap_after_part(last, &text));
        parts.push((text, wrap));
        boxes.push(bx);
    }
    // Drop word-less edge lines (a stray blank line clipped at an end).
    while parts.last().is_some_and(|p| p.0.is_empty()) {
        parts.pop();
        boxes.pop();
    }
    while parts.first().is_some_and(|p| p.0.is_empty()) {
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
        text: join_lines(&parts).trim().to_string(),
        boxes,
    }
}

/// Join a selection's per-line runs — each its text and what the break after it did ([`Wrap`]) —
/// into one string. A line that broke mid-word runs straight on into the next with no space, minus
/// the hyphen if the layout was the one that put it there: "pontifi-" + "cate" = "pontificate",
/// "well-" + "known" = "well-known". Every other pair of lines is joined by a single space.
fn join_lines(parts: &[(String, Option<Wrap>)]) -> String {
    let mut out = String::new();
    for (i, (text, _)) in parts.iter().enumerate() {
        if i > 0 {
            match parts[i - 1].1.filter(|_| starts_mid_word(text)) {
                Some(Wrap::SoftHyphen) => {
                    out.pop(); // that hyphen is the join, not a character of the word
                }
                Some(Wrap::Kept) => {}
                None => out.push(' '),
            }
        }
        out.push_str(text);
    }
    out
}

/// What the break after a selected line run did to the word it ends. The glyph carries it when the
/// backend laid the text out; otherwise it is read off the run's own text, like [`wrap_of`].
fn wrap_after_part(last: &CharBox, text: &str) -> Option<Wrap> {
    if last.anchor.is_some() {
        return last.wrap;
    }
    ends_mid_word(text).then_some(Wrap::SoftHyphen)
}

/// Whether `s` ends a line mid-word: a hyphen — possibly the second of two, see
/// [`word_before_hyphen`] — with a letter or digit in front of it. A dash used as punctuation
/// ("a word - ") has a space there, so it never joins.
fn ends_mid_word(s: &str) -> bool {
    let mut back = s.chars().rev();
    back.next().is_some_and(is_hyphen) && back.find(|c| !is_hyphen(*c)).is_some_and(is_word_char)
}

/// Whether `s` continues a split word: it starts with a letter or digit, or with the hyphen a
/// compound kept when the break fell in front of it ("self-" / "-evident"). Mirrors [`starts_word`].
fn starts_mid_word(s: &str) -> bool {
    let mut fwd = s.chars();
    match fwd.next() {
        Some(c) if is_word_char(c) => true,
        Some(c) if is_hyphen(c) => fwd.next().is_some_and(is_word_char),
        _ => false,
    }
}
