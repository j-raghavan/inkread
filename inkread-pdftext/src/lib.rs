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
}

impl Default for ReconstructOpts {
    fn default() -> Self {
        Self {
            line_split_mult: 0.6,
            word_gap_mult: 0.3,
            para_gap_mult: 0.65,
            indent_mult: 1.5,
            heading_ratio: 1.3,
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
    let lines = cluster_lines(glyphs, opts);
    group_blocks(&lines, opts)
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
fn cluster_lines(glyphs: &[Glyph], opts: &ReconstructOpts) -> Vec<Line> {
    if glyphs.is_empty() {
        return Vec::new();
    }
    let h_med = median(glyphs.iter().map(Glyph::height)).max(f32::EPSILON);
    let split = opts.line_split_mult * h_med;

    // Sort by vertical center, then left edge, so same-baseline glyphs are adjacent.
    let mut order: Vec<&Glyph> = glyphs.iter().collect();
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

/// Stages 3–5 — group lines into paragraphs (vertical gap / first-line indent), join hyphenation,
/// and split out headings (font-size outliers) as their own blocks.
fn group_blocks(lines: &[Line], opts: &ReconstructOpts) -> Vec<Block> {
    if lines.is_empty() {
        return Vec::new();
    }
    let body_h = median(lines.iter().map(|l| l.height)).max(f32::EPSILON);
    let left_margin = lines.iter().map(|l| l.x_left).fold(f32::INFINITY, f32::min);
    let w_med = median(lines.iter().map(|l| l.height)).max(f32::EPSILON); // height ≈ font size proxy
    let para_gap = opts.para_gap_mult * body_h;
    let indent = opts.indent_mult * w_med;

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
