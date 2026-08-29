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
    let mut prev: Option<usize> = None;
    for (i, c) in chars.iter().enumerate() {
        if c.ch.is_whitespace() {
            if !prev_space && !hay.is_empty() {
                hay.push(' ');
                src.push(i);
                prev_space = true;
            }
        } else {
            // A line break with no explicit space glyph (text wrap) still separates words, so the
            // query "foo bar" matches across the wrap — unless the break split a word, in which
            // case the halves are one word and the search reads them as `word_at` defines them
            // ("pontificate" finds "pontifi-" / "cate", "well-known" finds "well-" / "known").
            if let Some(p) = prev.filter(|&p| !same_line(&chars[p].rect, &c.rect)) {
                match wrap_of(chars, p).filter(|_| is_word_char(c.ch) || is_hyphen(c.ch)) {
                    Some(wrap) => {
                        while hay.last() == Some(&' ') {
                            hay.pop();
                            src.pop();
                        }
                        if wrap == Wrap::SoftHyphen {
                            hay.pop();
                            src.pop();
                        }
                    }
                    None if !prev_space => {
                        hay.push(' ');
                        src.push(i);
                    }
                    None => {}
                }
            }
            for lc in c.ch.to_lowercase() {
                hay.push(lc);
                src.push(i);
            }
            prev_space = false;
            prev = Some(i);
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
fn wrap_of(chars: &[CharBox], i: usize) -> Option<Wrap> {
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

/// A column gutter must be at least this multiple of the median glyph width — an interior vertical
/// whitespace band wider than any inter-word space. Mirrors `inkread_pdftext`'s `column_gap_mult`,
/// but applied here only within the selection's own vertical band (see [`confine_to_columns`]).
const COLUMN_GAP_MULT: f32 = 1.5;

/// Median width of the non-degenerate glyph boxes (0.0 if there are none).
fn median_glyph_width(glyphs: &[&CharBox]) -> f32 {
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
fn column_gutters(glyphs: &[&CharBox], min_w: f32) -> Vec<f32> {
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
fn confine_to_columns(chars: &[CharBox], x0: f32, y0: f32, x1: f32, y1: f32) -> Vec<CharBox> {
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
}
