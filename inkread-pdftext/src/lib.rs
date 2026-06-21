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
//! ## Pipeline
//! - **Multi-page** ([`reconstruct_pages`]): strip recurring running headers/footers and page
//!   numbers across pages, then reconstruct each page.
//! - **Per page** ([`reconstruct`]): segment into reading-order column/band regions (XY-cut), then
//!   per region: cluster glyphs into baseline **lines** → split into **words** by inter-glyph x-gap
//!   → group lines into **paragraphs** (vertical gap / first-line indent) → join end-of-line
//!   **hyphenation** → classify **headings** (font-size outliers and short bold lines).

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
    /// Whether this glyph's font is bold — used to classify body-sized **bold section headings**
    /// that font-size-outlier detection alone would miss (the adapter reads this from the PDF font
    /// weight; a non-PDF caller may leave it `false`).
    pub bold: bool,
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
    /// On a line with **no explicit space glyphs**, synthesize a space when the gap between adjacent
    /// glyphs exceeds this × the line's median glyph **height** (font size) — a space is ~0.25 em.
    pub word_gap_mult: f32,
    /// On a line that **does** have explicit space glyphs (the common case — they already mark every
    /// word boundary), only synthesize across a gap this much larger × the median height. Kept high
    /// so loose tracking, bold headings, or punctuation side-bearing can't fabricate intra-word
    /// splits; genuine large gaps (tabs/columns missing a space glyph) still break.
    pub word_gap_mult_spaced: f32,
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
            word_gap_mult: 0.25,
            word_gap_mult_spaced: 1.0,
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
    // Keep explicit space glyphs (PDFs emit them as zero-width boxes and they are sometimes the only
    // word-boundary signal — letters can butt together with no gap), dropping only non-finite boxes.
    // The ordering hazard they pose (a zero-width space shares an x0 with the next letter and, at a
    // slightly different baseline, can be tie-broken *after* it) is handled where lines are ordered:
    // the x-sort orders by glyph center, and the pen tracks a running max. Median metrics below
    // ignore these as a minority (spaces are < half of glyphs), so zero heights don't skew them.
    // Also drop control characters (CR/LF/TAB): PDFs emit them at line ends with an unreliable box
    // — the device's pdfium places the line-break glyph *back over the last letter* (negative gap),
    // so ordering by x-center would sort that zero-width whitespace *before* the final glyph and
    // inject a mid-word space ("value" → "valu e"). Lines are formed by geometry, not newlines, so
    // these carry no information and are pure noise.
    let glyphs: Vec<&Glyph> = glyphs
        .iter()
        .filter(|g| {
            g.x0.is_finite()
                && g.y0.is_finite()
                && g.x1.is_finite()
                && g.y1.is_finite()
                && !g.ch.is_control()
        })
        .collect();
    if glyphs.is_empty() {
        return Vec::new();
    }
    // Body line height and glyph width are estimated **globally** from raw glyph geometry (so
    // heading detection has a stable body baseline and gutter/band thresholds a stable scale) and
    // held fixed across the recursive segmentation below. Segmentation must run on glyphs *before*
    // line clustering — otherwise glyphs sharing a baseline across columns would merge into one
    // full-width line and erase the gutter.
    let body_h = median(glyphs.iter().map(|g| g.height())).max(f32::EPSILON);
    let glyph_w = median(glyphs.iter().map(|g| g.width())).max(f32::EPSILON);
    // Typical body line width (median across all baseline lines) — the reference a *short* bold
    // section heading is measured against. Global, so a heading later isolated into its own
    // single-line region still reads as short relative to the body. (For multi-column pages this
    // over-estimates, since cross-column lines merge; bold body text — the only false-positive risk
    // — is rare, so the trade-off is acceptable for v1.)
    let body_width = median(cluster_lines(&glyphs, opts).iter().map(Line::width)).max(f32::EPSILON);
    let metrics = Metrics {
        body_h,
        body_width,
        glyph_w,
    };
    let mut out = Vec::new();
    xy_cut(&glyphs, opts, &metrics, 0, &mut out);
    out
}

/// Page-global reference metrics held fixed across the recursive segmentation, so per-region leaves
/// classify headings and gutters against a stable document-wide baseline rather than their own
/// (possibly single-line) contents.
struct Metrics {
    body_h: f32,
    body_width: f32,
    glyph_w: f32,
}

/// Recursion depth bound for [`xy_cut`] — far beyond any real page's column/band nesting; a guard
/// against pathological geometry, never reached in practice.
const MAX_CUT_DEPTH: usize = 16;

/// Fraction of page height at the top/bottom treated as the header/footer margin band.
const MARGIN_BAND_FRAC: f32 = 0.12;
/// Below this page count there is no basis to call a margin line a *recurring* header/footer, so
/// stripping is skipped (avoids removing a one-off margin line from a short document).
const MIN_PAGES_FOR_MARGIN_STRIP: usize = 3;

/// Which page margin a candidate header/footer line sits in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Band {
    Top,
    Bottom,
}

/// Reconstruct a **multi-page document** (one [`Glyph`] vec per source page), stripping recurring
/// running **headers/footers and page numbers** before per-page reconstruction (ADR-INKREAD-0011).
/// A margin-band line whose alphabetic-normalized text recurs across enough pages — a running title,
/// or every page number once digits are normalized away — is chrome, not content, and is removed.
#[must_use]
pub fn reconstruct_pages(pages: &[Vec<Glyph>]) -> Vec<Vec<Block>> {
    reconstruct_pages_with(pages, &ReconstructOpts::default())
}

/// [`reconstruct_pages`] with explicit options.
#[must_use]
pub fn reconstruct_pages_with(pages: &[Vec<Glyph>], opts: &ReconstructOpts) -> Vec<Vec<Block>> {
    if pages.len() < MIN_PAGES_FOR_MARGIN_STRIP {
        return pages.iter().map(|g| reconstruct_with(g, opts)).collect();
    }

    // One candidate per margin line: which band, its alphabetic-normalized text, and its y-extent.
    let per_page: Vec<Vec<(Band, String, f32, f32)>> =
        pages.iter().map(|g| margin_lines(g, opts)).collect();

    // Tally how many distinct pages each (band, normalized-text) appears on.
    let mut tally: std::collections::HashMap<(Band, String), usize> =
        std::collections::HashMap::new();
    for cands in &per_page {
        let mut seen = std::collections::HashSet::new();
        for (band, norm, _, _) in cands {
            if seen.insert((*band, norm.clone())) {
                *tally.entry((*band, norm.clone())).or_default() += 1;
            }
        }
    }
    let threshold = (pages.len() / 2).max(2);

    pages
        .iter()
        .zip(&per_page)
        .map(|(glyphs, cands)| {
            // y-bands of this page's margin lines that recur across the document → strip them.
            let strip: Vec<(f32, f32)> = cands
                .iter()
                .filter(|(b, n, _, _)| {
                    tally.get(&(*b, n.clone())).copied().unwrap_or(0) >= threshold
                })
                .map(|(_, _, y0, y1)| (*y0, *y1))
                .collect();
            if strip.is_empty() {
                return reconstruct_with(glyphs, opts);
            }
            let kept: Vec<Glyph> = glyphs
                .iter()
                .filter(|g| {
                    let yc = g.y_center();
                    !strip.iter().any(|&(t, b)| yc >= t && yc <= b)
                })
                .copied()
                .collect();
            reconstruct_with(&kept, opts)
        })
        .collect()
}

/// The page's header/footer-candidate lines: lines lying wholly within the top or bottom margin
/// band, as `(band, alphabetic-normalized text, y_top, y_bottom)`.
fn margin_lines(glyphs: &[Glyph], opts: &ReconstructOpts) -> Vec<(Band, String, f32, f32)> {
    let refs: Vec<&Glyph> = glyphs
        .iter()
        .filter(|g| {
            g.x0.is_finite()
                && g.y0.is_finite()
                && g.x1.is_finite()
                && g.y1.is_finite()
                && !g.ch.is_control()
        })
        .collect();
    if refs.is_empty() {
        return Vec::new();
    }
    let y_min = refs.iter().map(|g| g.y0).fold(f32::INFINITY, f32::min);
    let y_max = refs.iter().map(|g| g.y1).fold(f32::NEG_INFINITY, f32::max);
    let h = (y_max - y_min).max(f32::EPSILON);
    let top_limit = y_min + MARGIN_BAND_FRAC * h;
    let bottom_limit = y_max - MARGIN_BAND_FRAC * h;

    group_by_baseline(&refs, opts)
        .into_iter()
        .filter_map(|g| {
            let line = line_from_glyphs(&g, opts)?;
            let band = if line.y_bottom <= top_limit {
                Band::Top
            } else if line.y_top >= bottom_limit {
                Band::Bottom
            } else {
                return None; // body line, never chrome
            };
            Some((band, margin_norm(&line.text), line.y_top, line.y_bottom))
        })
        .collect()
}

/// Normalize a margin line for recurrence comparison: keep only lowercase letters. This makes every
/// page number collapse to the same empty key (so they tally together) while a running title keeps
/// its alphabetic signature.
fn margin_norm(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphabetic())
        .flat_map(char::to_lowercase)
        .collect()
}

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
    metrics: &Metrics,
    depth: usize,
    out: &mut Vec<Block>,
) {
    if glyphs.is_empty() {
        return;
    }
    let vert = (depth < MAX_CUT_DEPTH)
        .then(|| best_vertical_gutter(glyphs, opts, metrics.glyph_w))
        .flatten();
    let horiz = (depth < MAX_CUT_DEPTH)
        .then(|| best_horizontal_band(glyphs, opts, metrics.body_h))
        .flatten();

    // Prefer the separator with the larger axis-normalized score; vertical wins ties (a clean
    // column gutter is a stronger reading-order signal than an incidental band gap).
    let take_vertical = match (&vert, &horiz) {
        (Some(v), Some(h)) => v.1 >= h.1,
        (Some(_), None) => true,
        (None, Some(_)) => false,
        (None, None) => {
            let lines = cluster_lines(glyphs, opts);
            out.extend(group_blocks(&lines, metrics, opts));
            return;
        }
    };

    if take_vertical {
        let (mid, _) = vert.unwrap();
        let (left, right): (Vec<&Glyph>, Vec<&Glyph>) =
            glyphs.iter().copied().partition(|g| g.x_center() < mid);
        xy_cut(&left, opts, metrics, depth + 1, out);
        xy_cut(&right, opts, metrics, depth + 1, out);
    } else {
        let (mid, _) = horiz.unwrap();
        let (top, bottom): (Vec<&Glyph>, Vec<&Glyph>) =
            glyphs.iter().copied().partition(|g| g.y_center() < mid);
        xy_cut(&top, opts, metrics, depth + 1, out);
        xy_cut(&bottom, opts, metrics, depth + 1, out);
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
/// needs (vertical extent, representative height, horizontal extent, and whether it is mostly bold).
struct Line {
    text: String,
    y_top: f32,
    y_bottom: f32,
    height: f32,
    x_left: f32,
    x_right: f32,
    /// Majority of the line's visible glyphs are bold — a body-sized **section heading** signal.
    bold: bool,
}

impl Line {
    fn width(&self) -> f32 {
        (self.x_right - self.x_left).max(0.0)
    }
}

/// Group glyphs into baseline lines (top→bottom); each line's glyphs are ordered left→right by
/// **center** (a zero-width space sits at the word gap, its center falling cleanly between the
/// neighbouring letters' — whereas its left edge ties to sub-pixel noise and would sort it mid-word).
/// Shared by line reconstruction ([`cluster_lines`]) and margin/header-footer detection.
fn group_by_baseline<'a>(glyphs: &[&'a Glyph], opts: &ReconstructOpts) -> Vec<Vec<&'a Glyph>> {
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
    for g in &mut groups {
        g.sort_by(|a, b| {
            a.x_center()
                .partial_cmp(&b.x_center())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
    groups
}

/// Stage 1+2 — cluster glyphs into baseline lines and render each to text with x-gap-synthesized
/// spaces (empty lines dropped).
fn cluster_lines(glyphs: &[&Glyph], opts: &ReconstructOpts) -> Vec<Line> {
    group_by_baseline(glyphs, opts)
        .into_iter()
        .filter_map(|g| line_from_glyphs(&g, opts))
        .collect()
}

/// Build a [`Line`] from one cluster's left-ordered glyphs, synthesizing inter-word spaces from
/// horizontal gaps. Returns `None` for an all-whitespace line (contributes no block content).
fn line_from_glyphs(glyphs: &[&Glyph], opts: &ReconstructOpts) -> Option<Line> {
    // The word gap is scaled by the line's **font size** (median glyph *height*), not glyph width:
    // a space is ~0.25 em regardless of which letters border it, and a width-median is dragged down
    // by narrow glyphs (i, l, t, f, .) on text-heavy lines, over-splitting words. Height is uniform
    // across a line's font and aspect-correct in point space.
    let h_med = median(glyphs.iter().map(|g| g.height())).max(f32::EPSILON);
    // PDFs that emit explicit space glyphs (most) already mark every word boundary; trust them and
    // only *synthesize* across a clearly large gap, so loose tracking / bold headings / punctuation
    // side-bearing can't fabricate intra-word splits. Lines with no space glyphs use the sensitive
    // positional threshold.
    let has_space = glyphs.iter().any(|g| g.ch.is_whitespace());
    let mult = if has_space {
        opts.word_gap_mult_spaced
    } else {
        opts.word_gap_mult
    };
    let word_gap = mult * h_med;

    let mut text = String::new();
    // Running max of right edges seen so far — a glyph that sits behind a wider predecessor (kerning
    // overlap, or a stray narrow box) can't regress the pen and fabricate a gap before the next one.
    let mut pen_x1: Option<f32> = None;
    for g in glyphs {
        if let Some(px1) = pen_x1 {
            // Never synthesize a space *before* closing punctuation — a space before '.'/',' is
            // always wrong (the glyph's left side-bearing reads as a gap).
            if g.x0 - px1 > word_gap && !is_closing_punct(g.ch) {
                text.push(' ');
            }
        }
        if g.ch.is_whitespace() {
            text.push(' ');
        } else {
            text.push(g.ch);
        }
        pen_x1 = Some(pen_x1.map_or(g.x1, |p| p.max(g.x1)));
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
    let x_right = glyphs
        .iter()
        .map(|g| g.x1)
        .fold(f32::NEG_INFINITY, f32::max);
    // Majority of the *visible* (non-space) glyphs bold → a section-heading signal.
    let visible = glyphs.iter().filter(|g| !g.ch.is_whitespace()).count();
    let bold_n = glyphs
        .iter()
        .filter(|g| !g.ch.is_whitespace() && g.bold)
        .count();
    let bold = visible > 0 && bold_n * 2 > visible;
    Some(Line {
        text,
        y_top,
        y_bottom,
        height,
        x_left,
        x_right,
        bold,
    })
}

/// Stages 3–5 — group a single region's lines into paragraphs (vertical gap / first-line indent),
/// join hyphenation, and split out headings (font-size outliers and short bold lines) as their own
/// blocks. `metrics` carries the **global** body line height/width (so heading detection is stable
/// even when a region holds only a heading); the left margin is taken **locally** so a right column's
/// indent is measured against its own edge, not the page's.
fn group_blocks(lines: &[Line], metrics: &Metrics, opts: &ReconstructOpts) -> Vec<Block> {
    if lines.is_empty() {
        return Vec::new();
    }
    let body_h = metrics.body_h;
    let body_width = metrics.body_width;
    let left_margin = lines.iter().map(|l| l.x_left).fold(f32::INFINITY, f32::min);
    let para_gap = opts.para_gap_mult * body_h;
    let indent = opts.indent_mult * body_h; // body height ≈ font size — the indent length scale

    let mut out: Vec<Block> = Vec::new();
    let mut para = String::new();
    let mut prev_bottom: Option<f32> = None;

    for line in lines {
        // A heading is either a font-size outlier, or a *short, mostly-bold* line — a body-sized
        // bold section heading. The shortness guard (well under the widest line) keeps a fully-bold
        // body paragraph, whose lines run full width, from being misread as a stack of headings.
        let size_ratio = line.height / body_h;
        let bold_heading = line.bold && line.width() < 0.6 * body_width;
        if size_ratio > opts.heading_ratio || bold_heading {
            flush_paragraph(&mut out, &mut para);
            out.push(text_block(&line.text, Some(heading_level(size_ratio))));
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

/// Closing punctuation that must never be preceded by a synthesized space (its left side-bearing can
/// read as a word gap). Limited to unambiguous closers — quotes/apostrophes are excluded (they open
/// as often as they close).
fn is_closing_punct(c: char) -> bool {
    matches!(
        c,
        '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '%' | '…'
    )
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
                bold: false,
            });
        }
        out
    }

    /// Mark every glyph bold (a bold line/heading fixture).
    fn bold(mut glyphs: Vec<Glyph>) -> Vec<Glyph> {
        for g in &mut glyphs {
            g.bold = true;
        }
        glyphs
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
                bold: false,
            },
            Glyph {
                ch: 'b',
                x0: 1.0,
                y0: 1.0,
                x1: 0.5,
                y1: 0.5,
                bold: false,
            },
        ];
        let _ = reconstruct(&g); // must return without panicking
    }

    #[test]
    fn short_bold_line_is_a_heading() {
        // A body-sized but bold short line above full-width body prose → a section heading, even
        // though it is not a font-size outlier.
        let mut g = bold(lay_line("Model Architecture", 0.0, 0.0, 10.0));
        g.extend(lay_line(
            "this is a much longer body line of ordinary prose",
            0.0,
            30.0,
            10.0,
        ));
        g.extend(lay_line(
            "that wraps and continues across the column width",
            0.0,
            42.0,
            10.0,
        ));
        let blocks = reconstruct(&g);
        assert!(
            matches!(blocks[0], Block::Heading { .. }),
            "bold short line is a heading: {blocks:?}"
        );
        assert_eq!(block_text(&blocks[0]), "Model Architecture");
        assert!(blocks[1..]
            .iter()
            .all(|b| matches!(b, Block::Paragraph { .. })));
    }

    #[test]
    fn fully_bold_paragraph_is_not_all_headings() {
        // A bold paragraph whose lines run the full body width must stay a paragraph, not become a
        // stack of headings (the shortness guard).
        let mut g = bold(lay_line(
            "this entire paragraph is set in bold yet it is",
            0.0,
            0.0,
            10.0,
        ));
        g.extend(bold(lay_line(
            "ordinary running prose spanning the whole width",
            0.0,
            12.0,
            10.0,
        )));
        let blocks = reconstruct(&g);
        assert!(
            blocks.iter().all(|b| matches!(b, Block::Paragraph { .. })),
            "full-width bold lines stay paragraphs: {blocks:?}"
        );
    }

    #[test]
    fn recurring_page_numbers_and_running_header_are_stripped() {
        // Five pages, each with a running header line at top, a body line, and a page number at
        // bottom. The header (same text) and the page numbers (different digits) both recur in their
        // margin bands → stripped; only the body survives.
        let pages: Vec<Vec<Glyph>> = (1..=5)
            .map(|n| {
                let mut g = lay_line("Running Header Title", 0.0, 0.0, 10.0); // top band
                g.extend(lay_line("body content of this page", 0.0, 150.0, 10.0)); // middle
                g.extend(lay_line(&format!("{n}"), 0.0, 290.0, 10.0)); // bottom page number
                g
            })
            .collect();
        let docs = reconstruct_pages(&pages);
        assert_eq!(docs.len(), 5);
        for (i, blocks) in docs.iter().enumerate() {
            let text: String = blocks
                .iter()
                .map(block_text)
                .collect::<Vec<_>>()
                .join(" | ");
            assert!(
                text.contains("body content"),
                "page {i} keeps its body: {text:?}"
            );
            assert!(
                !text.contains("Running Header"),
                "page {i} strips the running header: {text:?}"
            );
            assert!(
                !text.chars().any(|c| c.is_ascii_digit()),
                "page {i} strips the page number: {text:?}"
            );
        }
    }

    #[test]
    fn few_pages_keep_margin_lines() {
        // With only two pages there is no basis to call a margin line recurring chrome — keep it.
        let pages: Vec<Vec<Glyph>> = (1..=2)
            .map(|_| {
                let mut g = lay_line("Header", 0.0, 0.0, 10.0);
                g.extend(lay_line("body content here", 0.0, 150.0, 10.0));
                g
            })
            .collect();
        let docs = reconstruct_pages(&pages);
        let text: String = docs[0].iter().map(block_text).collect();
        assert!(
            text.contains("Header"),
            "short doc keeps margin line: {text:?}"
        );
    }

    /// A glyph with explicit geometry on a single line (y 0..10), not bold.
    fn gx(ch: char, x0: f32, x1: f32) -> Glyph {
        Glyph {
            ch,
            x0,
            y0: 0.0,
            x1,
            y1: 10.0,
            bold: false,
        }
    }
    /// A zero-width explicit space glyph at `x` (as pdfium emits).
    fn sp(x: f32) -> Glyph {
        Glyph {
            ch: ' ',
            x0: x,
            y0: 5.0,
            x1: x,
            y1: 5.0,
            bold: false,
        }
    }

    #[test]
    fn explicit_spaces_suppress_intraword_split() {
        // "hello world" with an explicit space and a wide intra-word gap inside "world" (loose
        // tracking / a bold heading). Because the line carries explicit spaces, the wide gap must
        // NOT be read as a word break — the word stays whole. (At the sensitive no-space threshold
        // it would split into "wor ld".)
        let g = vec![
            gx('h', 0.0, 8.0),
            gx('e', 8.0, 16.0),
            gx('l', 16.0, 24.0),
            gx('l', 24.0, 32.0),
            gx('o', 32.0, 40.0),
            sp(43.0),
            gx('w', 43.0, 51.0),
            gx('o', 51.0, 59.0),
            gx('r', 59.0, 67.0),
            gx('l', 75.0, 83.0),
            gx('d', 83.0, 91.0), // 8px gap before 'l' (< 1.0×height)
        ];
        let blocks = reconstruct(&g);
        assert_eq!(block_text(&blocks[0]), "hello world", "{blocks:?}");
    }

    #[test]
    fn line_end_control_glyph_does_not_split_last_word() {
        // Device repro: "value" at a line end, followed by a CR/LF glyph whose box sits *back over*
        // the last letter (negative gap) — at a slightly lower baseline. Ordered by x-center it would
        // land between 'u' and 'e' and inject a space ("valu e"); dropping control chars fixes it.
        let g = vec![
            gx('v', 0.0, 9.0),
            gx('a', 9.0, 18.0),
            gx('l', 18.0, 27.0),
            gx('u', 27.0, 36.0),
            gx('e', 36.0, 45.0),
            // CR/LF: zero-width, x backed up 8.3 over 'e', baseline 5.4 lower (h≈9 → still same line).
            Glyph {
                ch: '\n',
                x0: 36.7,
                y0: 5.4,
                x1: 36.7,
                y1: 5.4,
                bold: false,
            },
        ];
        let blocks = reconstruct(&g);
        assert_eq!(block_text(&blocks[0]), "value", "{blocks:?}");
    }

    #[test]
    fn no_synthesized_space_before_period() {
        // "loss." with no explicit spaces and a gap before the period (its left side-bearing). The
        // closing-punctuation guard must suppress a synthesized "loss ." → keep "loss.".
        let g = vec![
            gx('l', 0.0, 8.0),
            gx('o', 8.0, 16.0),
            gx('s', 16.0, 24.0),
            gx('s', 24.0, 32.0),
            gx('.', 35.0, 38.0), // 3px gap before '.' (> 0.25×height)
        ];
        let blocks = reconstruct(&g);
        assert_eq!(block_text(&blocks[0]), "loss.", "{blocks:?}");
    }
}
