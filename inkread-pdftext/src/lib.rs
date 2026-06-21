//! `inkread-pdftext` — PDF text **reflow reconstruction** (ADR-INKREAD-0011 Decision 1).
//!
//! A reflowed PDF is an EPUB-style flow whose [`Block`] sequence is reconstructed from the page's
//! **glyph geometry** instead of from XHTML. This crate owns exactly that reconstruction —
//! `&[Glyph]` → `Vec<Block>` — so the existing `inkread-epub` layout/render/pagination pipeline
//! (font-size, line-spacing, alignment, selection, search) is reused verbatim on PDF text.
//!
//! Pure geometry: no pdfium, no Android, no `reader-core` — host-testable in isolation, names no
//! vendor (IR-7). The `reader-core` adapter maps pdfium's per-page `CharBox`es into [`Glyph`]s.
//!
//! ## Coordinate contract
//! [`Glyph`] coordinates may be in **any consistent units** (pdfium point-space or the normalized
//! `[0,1]` the core already produces), with **`y` increasing downward** (reading order top→bottom)
//! and `x` increasing rightward. Because the core normalizes `x` and `y` independently (by page
//! width/height), all thresholds are derived **per-axis** from glyph medians — horizontal gaps from
//! the median glyph *width*, vertical gaps from the median glyph *height* — so the reconstruction is
//! robust to non-square normalization and to DPI/scale.
//!
//! ## Stages (single column in v1; column segmentation is the next phase, ADR-0011 Decision 3)
//! 1. cluster glyphs into baseline **lines**;
//! 2. split each line into **words** by inter-glyph x-gap (synthesizing spaces);
//! 3. group lines into **paragraphs** by vertical gap / first-line indent;
//! 4. join end-of-line **hyphenation**;
//! 5. classify **headings** by font-size outlier.

use inkread_epub::{Block, Inline, TextRun};

/// One positioned glyph — the reconstruction input. See the crate-level coordinate contract.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Glyph {
    /// The Unicode character.
    pub ch: char,
    /// Left edge.
    pub x0: f32,
    /// Top edge (`y` increases downward).
    pub y0: f32,
    /// Right edge.
    pub x1: f32,
    /// Bottom edge.
    pub y1: f32,
}

impl Glyph {
    fn width(&self) -> f32 {
        (self.x1 - self.x0).max(0.0)
    }
    fn height(&self) -> f32 {
        (self.y1 - self.y0).max(0.0)
    }
    fn x_center(&self) -> f32 {
        (self.x0 + self.x1) * 0.5
    }
    fn y_center(&self) -> f32 {
        (self.y0 + self.y1) * 0.5
    }
}

/// Tunable thresholds for reconstruction. All are **multiples of a per-axis glyph median**, so they
/// transfer across page sizes and DPI. Defaults are tuned for prose; the device phase refines them.
#[derive(Debug, Clone, Copy)]
pub struct ReconstructOpts {
    /// New line when a glyph's vertical center jumps more than this × the median glyph height.
    pub line_split_mult: f32,
    /// Insert a space when the horizontal gap between adjacent glyphs exceeds this × median width.
    pub word_gap_mult: f32,
    /// New paragraph when the vertical gap between lines exceeds this × the median line height.
    pub para_gap_mult: f32,
    /// New paragraph when a line's left edge is indented more than this × median width past the
    /// block's left margin (first-line indent — the common no-spacing paragraph convention).
    pub indent_mult: f32,
    /// A line is a heading when its height exceeds this × the body (median) line height.
    pub heading_ratio: f32,
    /// A vertical **column gutter** must be at least this × the median glyph width (an interior
    /// whitespace band crossed by no line — the separator between columns).
    pub column_gap_mult: f32,
    /// A horizontal **band** cut needs a full-width vertical gap of at least this × the body line
    /// height (clearly larger than a paragraph gap, so it fires only at major separators — e.g. a
    /// spanning title above multiple columns — not between ordinary paragraphs).
    pub band_gap_mult: f32,
}

impl Default for ReconstructOpts {
    fn default() -> Self {
        Self {
            line_split_mult: 0.6,
            word_gap_mult: 0.3,
            para_gap_mult: 0.65,
            indent_mult: 1.5,
            heading_ratio: 1.3,
            column_gap_mult: 1.5,
            band_gap_mult: 1.5,
        }
    }
}

/// Reconstruct a reflowable [`Block`] sequence from a page's glyphs with default options.
#[must_use]
pub fn reconstruct(glyphs: &[Glyph]) -> Vec<Block> {
    reconstruct_with(glyphs, &ReconstructOpts::default())
}

/// Reconstruct with explicit [`ReconstructOpts`]. Never panics; an empty/degenerate page → `[]`.
#[must_use]
pub fn reconstruct_with(glyphs: &[Glyph], opts: &ReconstructOpts) -> Vec<Block> {
    if glyphs.is_empty() {
        return Vec::new();
    }
    // Body line height and glyph width are estimated **globally** from raw glyph geometry (so
    // heading detection has a stable body baseline and gutter/band thresholds a stable scale) and
    // held fixed across the recursive segmentation below. Segmentation must run on glyphs *before*
    // line clustering — otherwise glyphs sharing a baseline across columns would merge into one
    // full-width line and erase the gutter.
    let body_h = median(glyphs.iter().map(Glyph::height)).max(f32::EPSILON);
    let glyph_w = median(glyphs.iter().map(Glyph::width)).max(f32::EPSILON);
    let refs: Vec<&Glyph> = glyphs.iter().collect();
    let mut out = Vec::new();
    xy_cut(&refs, opts, body_h, glyph_w, 0, &mut out);
    out
}

/// Recursion depth bound for [`xy_cut`] — far beyond any real page's column/band nesting; a guard
/// against pathological geometry, never reached in practice.
const MAX_CUT_DEPTH: usize = 16;

/// **Column/band segmentation (XY-cut).** Recursively split a region of glyphs by the most salient
/// *clean* separator — a vertical column gutter (no glyph crosses it) or a full-width horizontal
/// band gap — emitting reading order naturally: a vertical cut reads left region then right; a
/// horizontal cut reads top then bottom. A region with no clean separator is a leaf, where glyphs
/// are finally clustered into lines ([`cluster_lines`]) and grouped into blocks ([`group_blocks`]).
/// This resolves multi-column papers and a spanning title above columns; single-column pages have no
/// interior gutter and fall straight through to one leaf.
fn xy_cut(
    glyphs: &[&Glyph],
    opts: &ReconstructOpts,
    body_h: f32,
    glyph_w: f32,
    depth: usize,
    out: &mut Vec<Block>,
) {
    if glyphs.is_empty() {
        return;
    }
    let vert = (depth < MAX_CUT_DEPTH)
        .then(|| best_vertical_gutter(glyphs, opts, glyph_w))
        .flatten();
    let horiz = (depth < MAX_CUT_DEPTH)
        .then(|| best_horizontal_band(glyphs, opts, body_h))
        .flatten();

    // Prefer the separator with the larger axis-normalized score; vertical wins ties (a clean
    // column gutter is a stronger reading-order signal than an incidental band gap).
    let take_vertical = match (&vert, &horiz) {
        (Some(v), Some(h)) => v.1 >= h.1,
        (Some(_), None) => true,
        (None, Some(_)) => false,
        (None, None) => {
            let lines = cluster_lines(glyphs, opts);
            out.extend(group_blocks(&lines, body_h, opts));
            return;
        }
    };

    if take_vertical {
        let (mid, _) = vert.unwrap();
        let (left, right): (Vec<&Glyph>, Vec<&Glyph>) =
            glyphs.iter().copied().partition(|g| g.x_center() < mid);
        xy_cut(&left, opts, body_h, glyph_w, depth + 1, out);
        xy_cut(&right, opts, body_h, glyph_w, depth + 1, out);
    } else {
        let (mid, _) = horiz.unwrap();
        let (top, bottom): (Vec<&Glyph>, Vec<&Glyph>) =
            glyphs.iter().copied().partition(|g| g.y_center() < mid);
        xy_cut(&top, opts, body_h, glyph_w, depth + 1, out);
        xy_cut(&bottom, opts, body_h, glyph_w, depth + 1, out);
    }
}

/// Find the widest **interior vertical gutter** — an x-interval that no glyph's `[x0, x1]` overlaps —
/// at least `column_gap_mult × glyph_w` wide. Returns `(split_x, score)` where `score` is the gutter
/// width in glyph-width units (comparable to the horizontal band score). `None` if the region's
/// glyphs cover x continuously (a single column): only a true column gutter, empty on *every* line,
/// survives the merge.
fn best_vertical_gutter(
    glyphs: &[&Glyph],
    opts: &ReconstructOpts,
    glyph_w: f32,
) -> Option<(f32, f32)> {
    let min_w = opts.column_gap_mult * glyph_w;
    let mut spans: Vec<(f32, f32)> = glyphs.iter().map(|g| (g.x0, g.x1)).collect();
    spans.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    largest_interior_gap(&spans, min_w).map(|(mid, gap)| (mid, gap / glyph_w))
}

/// Find the widest **full-width horizontal band gap** — a y crossed by no glyph, separating an upper
/// region from a lower one — at least `band_gap_mult × body_h` tall (clearly larger than a paragraph
/// gap, so it fires only at major separators like a spanning title above columns). Returns
/// `(split_y, score)` in body-height units. `None` when glyphs pack vertically with no major break
/// (interleaved columns, or a single column whose gaps are mere line leading / paragraph spacing).
fn best_horizontal_band(
    glyphs: &[&Glyph],
    opts: &ReconstructOpts,
    body_h: f32,
) -> Option<(f32, f32)> {
    let min_h = opts.band_gap_mult * body_h;
    let mut spans: Vec<(f32, f32)> = glyphs.iter().map(|g| (g.y0, g.y1)).collect();
    spans.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    largest_interior_gap(&spans, min_h).map(|(mid, gap)| (mid, gap / body_h))
}

/// Over `[lo, hi]` spans sorted by `lo`, find the widest interior gap (region uncovered by any span)
/// exceeding `min`. Returns `(gap_midpoint, gap_width)`, or `None` if coverage is continuous. Shared
/// by both axes — a vertical gutter and a horizontal band are the same 1-D problem.
fn largest_interior_gap(spans: &[(f32, f32)], min: f32) -> Option<(f32, f32)> {
    let mut best: Option<(f32, f32)> = None;
    let mut cover_end = spans.first()?.1;
    for &(lo, hi) in &spans[1..] {
        let gap = lo - cover_end;
        if gap > min && best.is_none_or(|(_, w)| gap > w) {
            best = Some((cover_end + gap * 0.5, gap));
        }
        cover_end = cover_end.max(hi);
    }
    best
}

/// A reconstructed text line: its glyph-derived text plus the geometry the paragraph/heading stage
/// needs (vertical extent, representative height, left edge).
struct Line {
    text: String,
    y_top: f32,
    y_bottom: f32,
    height: f32,
    x_left: f32,
}

/// Stage 1+2 — cluster glyphs into baseline lines (top→bottom) and render each to text with
/// x-gap-synthesized spaces. Glyphs are grouped by vertical-center proximity (relative to the median
/// glyph height), then ordered left→right within a line.
fn cluster_lines(glyphs: &[&Glyph], opts: &ReconstructOpts) -> Vec<Line> {
    if glyphs.is_empty() {
        return Vec::new();
    }
    let h_med = median(glyphs.iter().map(|g| g.height())).max(f32::EPSILON);
    let split = opts.line_split_mult * h_med;

    // Sort by vertical center, then left edge, so same-baseline glyphs are adjacent.
    let mut order: Vec<&Glyph> = glyphs.to_vec();
    order.sort_by(|a, b| {
        a.y_center()
            .partial_cmp(&b.y_center())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.x0.partial_cmp(&b.x0).unwrap_or(std::cmp::Ordering::Equal))
    });

    // Greedily break into lines when the vertical center jumps past the split threshold.
    let mut groups: Vec<Vec<&Glyph>> = Vec::new();
    let mut mean_y = order[0].y_center();
    let mut cur: Vec<&Glyph> = Vec::new();
    for g in order {
        if cur.is_empty() || (g.y_center() - mean_y).abs() <= split {
            cur.push(g);
            let n = cur.len() as f32;
            mean_y = (mean_y * (n - 1.0) + g.y_center()) / n;
        } else {
            groups.push(std::mem::take(&mut cur));
            cur.push(g);
            mean_y = g.y_center();
        }
    }
    if !cur.is_empty() {
        groups.push(cur);
    }

    groups
        .into_iter()
        .filter_map(|mut g| {
            g.sort_by(|a, b| a.x0.partial_cmp(&b.x0).unwrap_or(std::cmp::Ordering::Equal));
            line_from_glyphs(&g, opts)
        })
        .collect()
}

/// Build a [`Line`] from one cluster's left-ordered glyphs, synthesizing inter-word spaces from
/// horizontal gaps. Returns `None` for an all-whitespace line (contributes no block content).
fn line_from_glyphs(glyphs: &[&Glyph], opts: &ReconstructOpts) -> Option<Line> {
    let w_med = median(glyphs.iter().map(|g| g.width())).max(f32::EPSILON);
    let word_gap = opts.word_gap_mult * w_med;

    let mut text = String::new();
    let mut prev_x1: Option<f32> = None;
    for g in glyphs {
        if let Some(px1) = prev_x1 {
            if g.x0 - px1 > word_gap {
                text.push(' ');
            }
        }
        if g.ch.is_whitespace() {
            text.push(' ');
        } else {
            text.push(g.ch);
        }
        prev_x1 = Some(g.x1);
    }

    let text = collapse_ws(&text);
    if text.is_empty() {
        return None;
    }
    let y_top = glyphs.iter().map(|g| g.y0).fold(f32::INFINITY, f32::min);
    let y_bottom = glyphs
        .iter()
        .map(|g| g.y1)
        .fold(f32::NEG_INFINITY, f32::max);
    let height = median(glyphs.iter().map(|g| g.height())).max(f32::EPSILON);
    let x_left = glyphs.iter().map(|g| g.x0).fold(f32::INFINITY, f32::min);
    Some(Line {
        text,
        y_top,
        y_bottom,
        height,
        x_left,
    })
}

/// Stages 3–5 — group a single region's lines into paragraphs (vertical gap / first-line indent),
/// join hyphenation, and split out headings (font-size outliers) as their own blocks. `body_h` is
/// the **global** body line height (so heading detection is stable even when a region holds only a
/// heading); the left margin is taken **locally** so a right column's indent is measured against its
/// own edge, not the page's.
fn group_blocks(lines: &[Line], body_h: f32, opts: &ReconstructOpts) -> Vec<Block> {
    if lines.is_empty() {
        return Vec::new();
    }
    let left_margin = lines.iter().map(|l| l.x_left).fold(f32::INFINITY, f32::min);
    let para_gap = opts.para_gap_mult * body_h;
    let indent = opts.indent_mult * body_h; // body height ≈ font size — the indent length scale

    let mut out: Vec<Block> = Vec::new();
    let mut para = String::new();
    let mut prev_bottom: Option<f32> = None;

    for line in lines {
        // Headings stand alone — flush any open paragraph first.
        if line.height > opts.heading_ratio * body_h {
            flush_paragraph(&mut out, &mut para);
            out.push(text_block(
                &line.text,
                Some(heading_level(line.height / body_h)),
            ));
            prev_bottom = Some(line.y_bottom);
            continue;
        }

        let starts_paragraph = para.is_empty()
            || prev_bottom.is_some_and(|pb| line.y_top - pb > para_gap)
            || line.x_left - left_margin > indent;

        if starts_paragraph {
            flush_paragraph(&mut out, &mut para);
            para.push_str(&line.text);
        } else {
            join_wrapped(&mut para, &line.text);
        }
        prev_bottom = Some(line.y_bottom);
    }
    flush_paragraph(&mut out, &mut para);
    out
}

/// Append a wrapped continuation line, undoing end-of-line hyphenation: a trailing `-` preceded by
/// an alphabetic char is a split word, so drop the hyphen and join directly; otherwise join with a
/// space.
fn join_wrapped(acc: &mut String, next: &str) {
    let hyphenated =
        acc.ends_with('-') && acc.chars().rev().nth(1).is_some_and(|c| c.is_alphabetic());
    if hyphenated {
        acc.pop(); // drop the '-'
        acc.push_str(next);
    } else {
        if !acc.is_empty() {
            acc.push(' ');
        }
        acc.push_str(next);
    }
}

/// Emit the accumulated paragraph (if any) and clear it.
fn flush_paragraph(out: &mut Vec<Block>, para: &mut String) {
    if !para.is_empty() {
        out.push(text_block(para, None));
        para.clear();
    }
}

/// One reflow [`Block`] from plain `text` — a [`Block::Heading`] when `heading` is set, else a
/// [`Block::Paragraph`]. v1 emits a single unstyled [`TextRun`] (emphasis/links are later
/// refinements once glyph font flags are surfaced).
fn text_block(text: &str, heading: Option<u8>) -> Block {
    let content = vec![Inline::Run(TextRun {
        text: text.to_string(),
        bold: false,
        italic: false,
        href: None,
    })];
    match heading {
        Some(level) => Block::Heading { level, content },
        None => Block::Paragraph { content },
    }
}

/// Map a line-height ratio (line height ÷ body height) to a heading level (bigger ⇒ lower level).
fn heading_level(ratio: f32) -> u8 {
    if ratio >= 2.0 {
        1
    } else if ratio >= 1.6 {
        2
    } else {
        3
    }
}

/// Collapse runs of whitespace to single spaces and trim — normalizes synthesized + source spaces.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The median of an iterator of finite `f32`s (`0.0` if empty). Used for the per-axis glyph metrics
/// the thresholds scale against.
fn median(values: impl Iterator<Item = f32>) -> f32 {
    let mut v: Vec<f32> = values.filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v[v.len() / 2]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Advance + box-width used by [`lay_line`]: each char box is 8 wide on a 10 pitch, so an
    /// intra-word gap is 2 and a (skipped) space slot opens a ~12 gap — unambiguous either side of
    /// the default `word_gap_mult`.
    const ADV: f32 = 10.0;
    const BOX_W: f32 = 8.0;

    /// Lay a string onto one line at `(x_start, y_top)` with glyph height `h`. Space characters
    /// consume a pitch slot but emit no glyph (creating the inter-word x-gap), mirroring how pdfium
    /// reports word spacing as position, not a glyph.
    fn lay_line(text: &str, x_start: f32, y_top: f32, h: f32) -> Vec<Glyph> {
        let mut out = Vec::new();
        for (i, ch) in text.chars().enumerate() {
            let x0 = x_start + i as f32 * ADV;
            if ch == ' ' {
                continue;
            }
            out.push(Glyph {
                ch,
                x0,
                y0: y_top,
                x1: x0 + BOX_W,
                y1: y_top + h,
            });
        }
        out
    }

    /// Pull the plain text out of a block (heading or paragraph) for assertions.
    fn block_text(b: &Block) -> String {
        let content = match b {
            Block::Heading { content, .. } | Block::Paragraph { content } => content,
            _ => return String::new(),
        };
        content
            .iter()
            .filter_map(|i| match i {
                Inline::Run(r) => Some(r.text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn empty_input_yields_no_blocks() {
        assert!(reconstruct(&[]).is_empty());
    }

    #[test]
    fn x_gaps_become_word_spaces() {
        let line = lay_line("ab cd", 0.0, 0.0, 10.0);
        let blocks = reconstruct(&line);
        assert_eq!(blocks.len(), 1);
        assert_eq!(block_text(&blocks[0]), "ab cd");
        assert!(matches!(blocks[0], Block::Paragraph { .. }));
    }

    #[test]
    fn wrapped_lines_join_into_one_paragraph() {
        // Two tightly-stacked lines (leading 2) with no indent, no big gap → one paragraph.
        let mut g = lay_line("Hello world", 0.0, 0.0, 10.0);
        g.extend(lay_line("again here", 0.0, 12.0, 10.0));
        let blocks = reconstruct(&g);
        assert_eq!(blocks.len(), 1, "wrapped lines merge");
        assert_eq!(block_text(&blocks[0]), "Hello world again here");
    }

    #[test]
    fn large_vertical_gap_splits_paragraphs() {
        let mut g = lay_line("first para", 0.0, 0.0, 10.0);
        // Next line sits a full body-height below the previous baseline → paragraph break.
        g.extend(lay_line("second para", 0.0, 30.0, 10.0));
        let blocks = reconstruct(&g);
        assert_eq!(blocks.len(), 2, "gap splits paragraphs");
        assert_eq!(block_text(&blocks[0]), "first para");
        assert_eq!(block_text(&blocks[1]), "second para");
    }

    #[test]
    fn first_line_indent_starts_a_paragraph() {
        // Same tight leading, but the second line is indented well past the margin → new paragraph.
        let mut g = lay_line("opening line", 0.0, 0.0, 10.0);
        g.extend(lay_line("indented start", 40.0, 12.0, 10.0));
        let blocks = reconstruct(&g);
        assert_eq!(blocks.len(), 2, "indent splits paragraphs");
        assert_eq!(block_text(&blocks[1]), "indented start");
    }

    #[test]
    fn end_of_line_hyphenation_is_joined() {
        let mut g = lay_line("an exam-", 0.0, 0.0, 10.0);
        g.extend(lay_line("ple here", 0.0, 12.0, 10.0));
        let blocks = reconstruct(&g);
        assert_eq!(blocks.len(), 1);
        assert_eq!(block_text(&blocks[0]), "an example here");
    }

    #[test]
    fn larger_glyphs_classify_as_heading() {
        // A double-height title line, then several body lines (so the body height is the median the
        // title stands out against — matching real prose where body lines dominate).
        let mut g = lay_line("Title", 0.0, 0.0, 20.0);
        g.extend(lay_line("body line one", 0.0, 40.0, 10.0));
        g.extend(lay_line("body line two", 0.0, 52.0, 10.0));
        g.extend(lay_line("body line three", 0.0, 64.0, 10.0));
        let blocks = reconstruct(&g);
        assert!(
            matches!(blocks[0], Block::Heading { level: 1, .. }),
            "title is an h1: {:?}",
            blocks[0]
        );
        assert_eq!(block_text(&blocks[0]), "Title");
        assert!(
            blocks[1..]
                .iter()
                .all(|b| matches!(b, Block::Paragraph { .. })),
            "body lines are paragraphs: {blocks:?}"
        );
    }

    #[test]
    fn two_columns_read_left_then_right() {
        // Two vertically-stacked lines in each of two columns separated by a wide gutter (left
        // spans x∈[0,~78], right starts at x=120 → ~42 gutter ≫ column threshold). Reading order
        // must be the whole left column, then the whole right column — not interleaved by row.
        let mut g = lay_line("left one", 0.0, 0.0, 10.0);
        g.extend(lay_line("left two", 0.0, 12.0, 10.0));
        g.extend(lay_line("right one", 120.0, 0.0, 10.0));
        g.extend(lay_line("right two", 120.0, 12.0, 10.0));
        let blocks = reconstruct(&g);
        assert_eq!(blocks.len(), 2, "one paragraph per column: {blocks:?}");
        assert_eq!(block_text(&blocks[0]), "left one left two");
        assert_eq!(block_text(&blocks[1]), "right one right two");
    }

    #[test]
    fn spanning_title_above_two_columns_reads_title_then_columns() {
        // A full-width title (crosses the gutter, so no top-level vertical cut) with a large gap to
        // two columns below. XY-cut peels the title band first, then splits the body into columns:
        // reading order = title, left column, right column.
        let mut g = lay_line("the full width title", 0.0, 0.0, 20.0);
        g.extend(lay_line("left alpha", 0.0, 50.0, 10.0));
        g.extend(lay_line("left beta", 0.0, 62.0, 10.0));
        g.extend(lay_line("right alpha", 120.0, 50.0, 10.0));
        g.extend(lay_line("right beta", 120.0, 62.0, 10.0));
        let blocks = reconstruct(&g);
        assert_eq!(blocks.len(), 3, "title + two columns: {blocks:?}");
        assert!(
            matches!(blocks[0], Block::Heading { .. }),
            "spanning title is a heading: {:?}",
            blocks[0]
        );
        assert_eq!(block_text(&blocks[0]), "the full width title");
        assert_eq!(block_text(&blocks[1]), "left alpha left beta");
        assert_eq!(block_text(&blocks[2]), "right alpha right beta");
    }

    #[test]
    fn never_panics_on_degenerate_geometry() {
        // Zero-size and overlapping boxes must not panic or produce NaN-driven ordering crashes.
        let g = vec![
            Glyph {
                ch: 'a',
                x0: 0.0,
                y0: 0.0,
                x1: 0.0,
                y1: 0.0,
            },
            Glyph {
                ch: 'b',
                x0: 1.0,
                y0: 1.0,
                x1: 0.5,
                y1: 0.5,
            },
        ];
        let _ = reconstruct(&g); // must return without panicking
    }
}
