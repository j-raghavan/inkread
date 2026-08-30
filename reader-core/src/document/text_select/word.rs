//! The word under a point (RR11 / ADR-INKREAD-0009 D1).
//!
//! A tap or long-press resolves to a whole word, expanding across letters, digits, and *internal*
//! apostrophes and hyphens (`don't`, `well-known`). The wrap handling is the subtle part: a word a
//! line break split is still one word, so either half selects the whole of it — which is what stops
//! a definition lookup asking the dictionary about `well--`.

use super::*;

/// The word under `(x, y)` (tap / long-press), or `None` if the point isn't on a word glyph
/// (whitespace, punctuation, or empty space). Expands across letters/digits and *internal*
/// apostrophes/hyphens (`don't`, `well-known`), and across a line break that split the word
/// (see [`wrap_before`]) so either half defines the whole word.
#[must_use]
pub fn word_at(chars: &[CharBox], x: f32, y: f32) -> Option<TextSelection> {
    let hit = hit_char(chars, x, y)?;
    if !is_word_char(chars[hit].ch) {
        return None;
    }
    let (mut start, mut end) = word_run(chars, hit);
    // Soft hyphenation splits a word across two lines (or, on a two-column page, two columns). Both
    // halves are one word; only a hyphen the layout *inserted* is dropped when they rejoin, so
    // tapping either "pontifi-" or "cate" defines "pontificate" while "well-" / "known" keeps the
    // hyphen it came with.
    let mut breaks = Vec::new();
    if let Some((wrap, brk, head)) = wrap_before(chars, start) {
        if wrap == Wrap::SoftHyphen {
            breaks.push(brk);
        }
        start = word_run(chars, head).0;
    }
    if let Some((wrap, brk, tail)) = wrap_after(chars, end) {
        if wrap == Wrap::SoftHyphen {
            breaks.push(brk);
        }
        end = word_run(chars, tail).1;
    }
    let run = &chars[start..=end];
    let text = run
        .iter()
        .enumerate()
        .filter(|(i, c)| !c.ch.is_whitespace() && !breaks.contains(&(start + i)))
        .map(|(_, c)| c.ch)
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

/// The word run around `chars[i]` as `(start, end)`, inclusive — letters/digits and internal
/// connectors, on one line (a line break ends the run; [`wrap_before`]/[`wrap_after`] cross it).
fn word_run(chars: &[CharBox], i: usize) -> (usize, usize) {
    let mut start = i;
    while start > 0 && joins(&chars[start - 1], &chars[start]) {
        start -= 1;
    }
    let mut end = i;
    while end + 1 < chars.len() && joins(&chars[end], &chars[end + 1]) {
        end += 1;
    }
    (start, end)
}

/// What the line break after `chars[i]` did to the word it split, or `None` if it split none.
///
/// A backend that laid the text out states it outright ([`CharBox::wrap`]), and is believed
/// including when it says nothing happened — it knows, and the alternative is guessing wrong on a
/// line that simply ends in a hyphen ("well- known", two words). A fixed-layout backend fills
/// neither `wrap` nor `anchor`: there the break is read off the page, where a line-ending hyphen
/// with a letter in front of it is a split word by printing convention, and goes when they rejoin.
pub(super) fn wrap_of(chars: &[CharBox], i: usize) -> Option<Wrap> {
    if chars[i].anchor.is_some() {
        return chars[i].wrap;
    }
    word_before_hyphen(chars, i).map(|_| Wrap::SoftHyphen)
}

/// The line break *before* the word run starting at `start`, when it split a word: `(what it did,
/// the glyph at the break, a glyph of the first half)`. `None` when `start` begins a word of its own.
fn wrap_before(chars: &[CharBox], start: usize) -> Option<(Wrap, usize, usize)> {
    let brk = prev_glyph(chars, start)?;
    if same_line(&chars[brk].rect, &chars[start].rect) {
        return None;
    }
    let wrap = wrap_of(chars, brk)?;
    // The first half is whatever run that glyph belongs to: itself when the break needed no hyphen
    // (unspaced script), else the letter in front of the hyphen.
    let head = if is_word_char(chars[brk].ch) {
        brk
    } else {
        word_before_hyphen(chars, brk)?
    };
    Some((wrap, brk, head))
}

/// The mirror of [`wrap_before`]: the run ending at `end` is the first half of a split word.
/// `(what the break did, the glyph at the break, the first glyph of the continuation)`, or `None`
/// when the word really does end there (or the page does).
fn wrap_after(chars: &[CharBox], end: usize) -> Option<(Wrap, usize, usize)> {
    // Step over a second hyphen: `joins` won't pair two connectors, so a page that prints a word's
    // own hyphen *and* a break hyphen ("well--") leaves the run short of the break. Our layout no
    // longer emits that pair, but a fixed-layout page can still show it.
    let mut brk = end;
    while chars
        .get(brk + 1)
        .is_some_and(|c| is_hyphen(c.ch) && same_line(&c.rect, &chars[brk].rect))
    {
        brk += 1;
    }
    let wrap = wrap_of(chars, brk)?;
    let tail = next_glyph(chars, brk)?;
    (starts_word(chars, tail) && !same_line(&chars[brk].rect, &chars[tail].rect))
        .then_some((wrap, brk, tail))
}

/// The letter or digit that the line-ending hyphen at `i` belongs to, scanning back over any
/// further hyphens on its line. `None` when `chars[i]` isn't a hyphen that ends a word — a dash
/// used as punctuation has a space in front of it, and nothing on the line before it counts.
///
/// The scan matters because the layout appends *its own* hyphen to whatever fragment it breaks
/// (`inkread_epub::layout`), so a compound broken at the hyphen it already had ends the line in two
/// ("well-known" → "well--" / "known"). Exactly one of them — the last — is the join.
fn word_before_hyphen(chars: &[CharBox], i: usize) -> Option<usize> {
    if !is_hyphen(chars[i].ch) {
        return None;
    }
    let mut j = i;
    while j > 0 && same_line(&chars[j - 1].rect, &chars[j].rect) {
        j -= 1;
        if is_word_char(chars[j].ch) {
            return Some(j);
        }
        if !is_hyphen(chars[j].ch) {
            return None;
        }
    }
    None
}

/// Whether `chars[i]` starts a word: a letter or digit, or the hyphen a compound kept when the
/// break fell in front of it ("self-evident" → "self-" / "-evident").
fn starts_word(chars: &[CharBox], i: usize) -> bool {
    is_word_char(chars[i].ch)
        || (is_hyphen(chars[i].ch)
            && chars
                .get(i + 1)
                .is_some_and(|c| is_word_char(c.ch) && same_line(&c.rect, &chars[i].rect)))
}
