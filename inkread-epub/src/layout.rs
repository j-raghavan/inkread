//! Phase 3 — reflow **layout + pagination** (ADR-INKREAD-0007 / RR2-FR5, RR2-AC2).
//!
//! Turns a chapter's [`Block`](crate::content::Block) sequence into a series of [`Page`]s of
//! positioned text [`PlacedRun`]s for a given viewport + typography ([`LayoutOpts`]). Greedy
//! line-breaking + vertical block stacking with page breaks — a single-column flow that covers the
//! vast majority of EPUB prose.
//!
//! ## Design note (divergence from ADR-RUST-READER Decision 1)
//! That ADR proposed *forking Plato's* engine. Because Phase 2 already lowers XHTML into inkread's
//! own simplified [`content`](crate::content) model (no arbitrary CSS box tree), the layout reduces
//! to line-breaking + block stacking over that model — a few hundred lines, clean-room. Forking
//! Plato's full XML+CSS+box engine (which operates on *its* DOM) would be a poor fit and pull in the
//! AGPL-fork obligation + license checklist for no benefit here. Revisit the fork only if full CSS
//! fidelity becomes a requirement.
//!
//! Text **measurement** is abstracted behind [`Metrics`] so pagination is host-testable without a
//! font rasterizer; Phase 4 plugs a real glyph-advance implementation (skrifa/swash) and renders the
//! [`Page`]s into a `PixelBuffer`.

use crate::content::{Block, Inline};

/// Glyph-advance measurement for a font (Phase 4 supplies a real implementation; tests use a
/// fixed-pitch fake). `bold`/`italic` may select a different face/metrics.
pub trait Metrics {
    /// The advance width, in pixels, of `text` rendered at `size_px` with the given emphasis.
    fn advance(&self, text: &str, size_px: f32, bold: bool, italic: bool) -> f32;
}

/// Viewport + typography for a layout pass (all pixels). Repagination on a font-size or margin
/// Horizontal text alignment for reflowed lines (RR4 — KOReader's "Alignment").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    /// Flush left, ragged right (default).
    #[default]
    Left,
    /// Stretch inter-word gaps to fill the line (last line of a block stays left).
    Justify,
    /// Centered.
    Center,
    /// Flush right.
    Right,
}

impl Align {
    /// Decode the wire integer (`0=Left, 1=Justify, 2=Center, 3=Right`); unknown → `Left`.
    #[must_use]
    pub fn from_code(code: i32) -> Align {
        match code {
            1 => Align::Justify,
            2 => Align::Center,
            3 => Align::Right,
            _ => Align::Left,
        }
    }
}

/// change just reruns [`paginate`] with new opts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutOpts {
    /// Full page width.
    pub page_w: f32,
    /// Full page height.
    pub page_h: f32,
    /// Uniform page margin (content area is inset by this on all sides).
    pub margin: f32,
    /// Base body font size.
    pub font_px: f32,
    /// Line height as a multiple of the run's font size (e.g. 1.4).
    pub line_spacing: f32,
    /// Vertical gap inserted after each block.
    pub para_gap: f32,
    /// Horizontal alignment of reflowed lines (RR4).
    pub align: Align,
}

impl LayoutOpts {
    /// Sensible defaults for a body size on a given page, with a margin proportional to width.
    #[must_use]
    pub fn new(page_w: f32, page_h: f32, font_px: f32) -> Self {
        Self {
            page_w,
            page_h,
            margin: (page_w * 0.06).max(8.0),
            font_px,
            line_spacing: 1.4,
            para_gap: font_px * 0.7,
            align: Align::Left,
        }
    }

    fn content_w(&self) -> f32 {
        (self.page_w - 2.0 * self.margin).max(1.0)
    }

    fn content_h(&self) -> f32 {
        (self.page_h - 2.0 * self.margin).max(1.0)
    }

    /// A stable hash of every layout-affecting field — the pagination-cache discriminator (RR9-FR3,
    /// `SPEC-RUST-READER.md`). Two `LayoutOpts` that paginate identically share a digest; any change
    /// that moves page boundaries (viewport, font size, line/para spacing, alignment, margin) flips
    /// it, while a non-layout change (e.g. a colour theme) would not. f32s are hashed by bit pattern
    /// and the hasher is seeded deterministically, so the digest is reproducible across processes —
    /// a requirement for an on-disk cache key (ADR-INKREAD-0013 D1).
    #[must_use]
    pub fn layout_digest(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        // DefaultHasher::new() seeds with fixed keys (0, 0) — deterministic across runs, unlike
        // RandomState. Bit patterns make the f32s hashable and exact.
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for field in [
            self.page_w,
            self.page_h,
            self.margin,
            self.font_px,
            self.line_spacing,
            self.para_gap,
        ] {
            field.to_bits().hash(&mut h);
        }
        (self.align as u8).hash(&mut h);
        h.finish()
    }
}

/// A reflow-stable source anchor for a placed run/glyph (ADR-INKREAD-0012; feeds RR6 `PinPosition`).
///
/// `block` is the reading-order index of the source [`Block`](crate::content::Block) in the chapter
/// (the v1 `xpath` — stable because reflow never reorders blocks). `char_offset` is the
/// **chapter-relative** character offset of the run's (or this glyph's) first character. Both are
/// derived from character counts, **not pixels**, so they are invariant under a font-size / margin /
/// alignment change — the property a highlight or Digest entry needs to re-resolve to the same text
/// after the page reflows (golden `SPEC-INKREAD.md` RR8-FR2/AC1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SourceAnchor {
    /// Reading-order index of the source block in the chapter.
    pub block: usize,
    /// Chapter-relative character offset of the first character.
    pub char_offset: usize,
}

/// A positioned run of text on a line. `x`/`top` are relative to the page's **content origin** (the
/// top-left after the margin); the renderer adds `opts.margin`. Baseline ≈ `top + size_px`.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacedRun {
    pub x: f32,
    pub text: String,
    pub size_px: f32,
    pub bold: bool,
    pub italic: bool,
    pub href: Option<String>,
    /// Source anchor of this run's first character (ADR-INKREAD-0012).
    pub anchor: SourceAnchor,
}

/// One laid-out line: its `top` (content-relative), `height` (the line box), and positioned runs.
/// A horizontal rule line carries `rule = true` and no runs.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutLine {
    pub top: f32,
    pub height: f32,
    pub runs: Vec<PlacedRun>,
    pub rule: bool,
}

/// A laid-out page: the lines that fall within the content box.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Page {
    pub lines: Vec<LayoutLine>,
}

/// Heading size multipliers by level (`h1`..`h6`).
fn heading_scale(level: u8) -> f32 {
    match level {
        1 => 1.8,
        2 => 1.5,
        3 => 1.3,
        4 => 1.15,
        5 => 1.05,
        _ => 1.0,
    }
}

/// Paginate a chapter's blocks into pages for the viewport `opts`, measuring text via `m`.
#[must_use]
pub fn paginate(blocks: &[Block], opts: &LayoutOpts, m: &dyn Metrics) -> Vec<Page> {
    let mut pager = Pager::new(opts);
    // Chapter-relative character cursor, advanced as source text is consumed in reading order, so
    // every placed run/glyph carries a font-invariant offset (ADR-INKREAD-0012).
    let mut cursor = 0usize;
    for (block_index, block) in blocks.iter().enumerate() {
        match block {
            Block::Heading { level, content } => {
                let size = opts.font_px * heading_scale(*level);
                pager.add_paragraph(content, size, 0.0, true, block_index, &mut cursor, m);
                pager.gap(opts.para_gap);
            }
            Block::Paragraph { content } => {
                pager.add_paragraph(
                    content,
                    opts.font_px,
                    0.0,
                    false,
                    block_index,
                    &mut cursor,
                    m,
                );
                pager.gap(opts.para_gap);
            }
            Block::ListItem {
                ordered,
                index,
                content,
            } => {
                let marker = if *ordered {
                    format!("{index}.")
                } else {
                    "•".to_string()
                };
                pager.add_list_item(&marker, content, opts.font_px, block_index, &mut cursor, m);
                pager.gap(opts.para_gap * 0.4);
            }
            Block::Image { alt, .. } => {
                // Phase 3 reserves a labelled placeholder; Phase 4 renders the decoded image at its
                // intrinsic (viewport-fit) size.
                let label = if alt.is_empty() {
                    "[image]".to_string()
                } else {
                    format!("[image: {alt}]")
                };
                let run = vec![Inline::Run(crate::content::TextRun {
                    text: label,
                    bold: false,
                    italic: true,
                    href: None,
                })];
                pager.add_paragraph(&run, opts.font_px, 0.0, false, block_index, &mut cursor, m);
                pager.gap(opts.para_gap);
            }
            Block::Rule => pager.add_rule(opts.para_gap),
        }
    }
    pager.finish()
}

/// Accumulates lines into pages, breaking when the content box is full.
struct Pager<'o> {
    opts: &'o LayoutOpts,
    pages: Vec<Page>,
    current: Vec<LayoutLine>,
    cursor_y: f32,
}

impl<'o> Pager<'o> {
    fn new(opts: &'o LayoutOpts) -> Self {
        Self {
            opts,
            pages: Vec::new(),
            current: Vec::new(),
            cursor_y: 0.0,
        }
    }

    /// Place a line of `height`, breaking to a new page first if it would overflow a non-empty page.
    /// Run `x` is already content-relative; the line's vertical position is carried by `top`.
    fn emit(&mut self, runs: Vec<PlacedRun>, height: f32, rule: bool) {
        if self.cursor_y + height > self.opts.content_h() && !self.current.is_empty() {
            self.break_page();
        }
        let top = self.cursor_y;
        self.current.push(LayoutLine {
            top,
            height,
            runs,
            rule,
        });
        self.cursor_y += height;
    }

    /// Advance the vertical cursor by a block gap (never itself forces a page break).
    fn gap(&mut self, dy: f32) {
        self.cursor_y += dy;
    }

    fn break_page(&mut self) {
        self.pages.push(Page {
            lines: std::mem::take(&mut self.current),
        });
        self.cursor_y = 0.0;
    }

    fn finish(mut self) -> Vec<Page> {
        if !self.current.is_empty() {
            self.pages.push(Page {
                lines: std::mem::take(&mut self.current),
            });
        }
        self.pages
    }

    /// Lay out a paragraph/heading: greedy-break its inlines to the content width and emit lines.
    /// `cursor` is the chapter-relative character offset, advanced as the inlines are consumed.
    #[allow(clippy::too_many_arguments)]
    fn add_paragraph(
        &mut self,
        inlines: &[Inline],
        size: f32,
        indent: f32,
        bold_all: bool,
        block: usize,
        cursor: &mut usize,
        m: &dyn Metrics,
    ) {
        let lines = break_lines(
            inlines,
            size,
            indent,
            self.opts.content_w(),
            bold_all,
            block,
            cursor,
            m,
        );
        let line_h = size * self.opts.line_spacing;
        let n = lines.len();
        for (i, mut runs) in lines.into_iter().enumerate() {
            align_line(
                &mut runs,
                self.opts.align,
                self.opts.content_w(),
                i + 1 == n,
                m,
            );
            self.emit(runs, line_h, false);
        }
    }

    /// Lay out a list item with a hanging marker and indented body.
    fn add_list_item(
        &mut self,
        marker: &str,
        inlines: &[Inline],
        size: f32,
        block: usize,
        cursor: &mut usize,
        m: &dyn Metrics,
    ) {
        let marker_w = m.advance(marker, size, false, false);
        let indent = marker_w + m.advance("  ", size, false, false);
        // The marker is synthetic (not source text): it shares the body's start offset and does not
        // consume cursor budget, so body offsets still map to source characters.
        let marker_anchor = SourceAnchor {
            block,
            char_offset: *cursor,
        };
        let mut lines = break_lines(
            inlines,
            size,
            indent,
            self.opts.content_w(),
            false,
            block,
            cursor,
            m,
        );
        // Prepend the marker to the first line at the content origin (hanging indent).
        if let Some(first) = lines.first_mut() {
            first.insert(
                0,
                PlacedRun {
                    x: 0.0,
                    text: marker.to_string(),
                    size_px: size,
                    bold: false,
                    italic: false,
                    href: None,
                    anchor: marker_anchor,
                },
            );
        } else {
            lines.push(vec![PlacedRun {
                x: 0.0,
                text: marker.to_string(),
                size_px: size,
                bold: false,
                italic: false,
                href: None,
                anchor: marker_anchor,
            }]);
        }
        let line_h = size * self.opts.line_spacing;
        for runs in lines {
            self.emit(runs, line_h, false);
        }
    }

    /// Emit a horizontal-rule line occupying a small vertical slot.
    fn add_rule(&mut self, gap: f32) {
        self.gap(gap);
        self.emit(Vec::new(), gap.max(2.0), true);
        self.gap(gap);
    }
}

/// A line-breaking token. `Word`s carry their source [`SourceAnchor`] so it can be stamped onto the
/// resulting [`PlacedRun`].
enum Tok<'a> {
    Word {
        text: &'a str,
        bold: bool,
        italic: bool,
        href: Option<&'a str>,
        anchor: SourceAnchor,
    },
    Space,
    Break,
}

/// Flatten inlines into words/spaces/breaks, preserving inter-run spacing (text is already
/// whitespace-collapsed by Phase 2, so a single ASCII space separates words). `cursor` advances by
/// the chapter-relative character count as words and the spaces/breaks between them are consumed, so
/// each word's [`SourceAnchor`] records where its first character sits (ADR-INKREAD-0012).
fn tokenize<'a>(
    inlines: &'a [Inline],
    bold_all: bool,
    block: usize,
    cursor: &mut usize,
) -> Vec<Tok<'a>> {
    let mut toks = Vec::new();
    for inline in inlines {
        match inline {
            Inline::Break => {
                toks.push(Tok::Break);
                *cursor += 1; // the <br> occupies one character position
            }
            Inline::Image { alt, .. } => {
                let label = if alt.is_empty() { "[img]" } else { alt };
                toks.push(Tok::Word {
                    text: label,
                    bold: false,
                    italic: true,
                    href: None,
                    anchor: SourceAnchor {
                        block,
                        char_offset: *cursor,
                    },
                });
                *cursor += label.chars().count();
            }
            Inline::Run(r) => {
                for (i, part) in r.text.split(' ').enumerate() {
                    if i > 0 {
                        toks.push(Tok::Space);
                        *cursor += 1; // the single collapsed space between words
                    }
                    if !part.is_empty() {
                        toks.push(Tok::Word {
                            text: part,
                            bold: r.bold || bold_all,
                            italic: r.italic,
                            href: r.href.as_deref(),
                            anchor: SourceAnchor {
                                block,
                                char_offset: *cursor,
                            },
                        });
                        *cursor += part.chars().count();
                    }
                }
            }
        }
    }
    toks
}

/// Re-position a line's runs for `align` (RR4). Runs come from [`break_lines`] flush-left; this
/// shifts them (Center/Right) or distributes the slack across inter-word gaps (Justify). The last
/// line of a block (`is_last`) stays left under Justify, as in normal typography.
fn align_line(
    runs: &mut [PlacedRun],
    align: Align,
    content_w: f32,
    is_last: bool,
    m: &dyn Metrics,
) {
    if runs.is_empty() || align == Align::Left {
        return;
    }
    let left = runs[0].x; // the line's left edge (indent)
    let right = runs
        .iter()
        .map(|r| r.x + m.advance(&r.text, r.size_px, r.bold, r.italic))
        .fold(0.0f32, f32::max);
    let slack = (content_w - right).max(0.0);
    if slack <= 0.0 {
        return;
    }
    match align {
        Align::Left => {}
        Align::Center => runs.iter_mut().for_each(|r| r.x += slack * 0.5),
        Align::Right => runs.iter_mut().for_each(|r| r.x += slack),
        Align::Justify => {
            // Spread the slack across the N-1 word gaps; skip the block's last line.
            if is_last || runs.len() < 2 {
                let _ = left;
                return;
            }
            let per = slack / (runs.len() - 1) as f32;
            for (k, r) in runs.iter_mut().enumerate() {
                r.x += per * k as f32;
            }
        }
    }
}

/// Greedy line-break: returns each line as its positioned runs (x relative to content origin; the
/// body is offset by `indent`). Words wider than the available width are placed on their own line
/// (not split — hyphenation is a later refinement).
#[allow(clippy::too_many_arguments)]
fn break_lines(
    inlines: &[Inline],
    size: f32,
    indent: f32,
    content_w: f32,
    bold_all: bool,
    block: usize,
    cursor: &mut usize,
    m: &dyn Metrics,
) -> Vec<Vec<PlacedRun>> {
    let avail = (content_w - indent).max(1.0);
    let space_w = m.advance(" ", size, false, false);
    let mut lines: Vec<Vec<PlacedRun>> = Vec::new();
    let mut cur: Vec<PlacedRun> = Vec::new();
    let mut x = 0.0f32; // offset within the body column (excludes indent)
    let mut need_space = false;

    for tok in tokenize(inlines, bold_all, block, cursor) {
        match tok {
            Tok::Break => {
                if !cur.is_empty() {
                    lines.push(std::mem::take(&mut cur));
                }
                x = 0.0;
                need_space = false;
            }
            Tok::Space => need_space = true,
            Tok::Word {
                text,
                bold,
                italic,
                href,
                anchor,
            } => {
                let ww = m.advance(text, size, bold, italic);
                let place = |x: f32, cur: &mut Vec<PlacedRun>| {
                    cur.push(PlacedRun {
                        x: indent + x,
                        text: text.to_string(),
                        size_px: size,
                        bold,
                        italic,
                        href: href.map(str::to_string),
                        anchor,
                    });
                };
                if cur.is_empty() {
                    place(0.0, &mut cur);
                    x = ww;
                } else {
                    let lead = if need_space { space_w } else { 0.0 };
                    if x + lead + ww > avail {
                        lines.push(std::mem::take(&mut cur));
                        place(0.0, &mut cur);
                        x = ww;
                    } else {
                        place(x + lead, &mut cur);
                        x += lead + ww;
                    }
                }
                need_space = false;
            }
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::{parse_blocks, TextRun};

    /// Fixed-pitch metrics: every char advances `0.5 * size` (bold/italic ignored). Deterministic, so
    /// wrapping/pagination can be asserted exactly without a font.
    struct Mono;
    impl Metrics for Mono {
        fn advance(&self, text: &str, size_px: f32, _b: bool, _i: bool) -> f32 {
            text.chars().count() as f32 * size_px * 0.5
        }
    }

    fn para(text: &str) -> Block {
        Block::Paragraph {
            content: vec![Inline::Run(TextRun {
                text: text.into(),
                bold: false,
                italic: false,
                href: None,
            })],
        }
    }

    #[test]
    fn long_paragraph_wraps_to_multiple_lines() {
        // 10px font → 5px/char. content_w = 100 → 20 chars/line. 60-char paragraph → 3 lines.
        let opts = LayoutOpts {
            page_w: 100.0 + 2.0 * 0.0,
            page_h: 10_000.0,
            margin: 0.0,
            font_px: 10.0,
            line_spacing: 1.0,
            para_gap: 0.0,
            align: Align::Left,
        };
        let words = "aaaa ".repeat(12); // 12 words of 4 chars → ~ wraps
        let pages = paginate(&[para(words.trim())], &opts, &Mono);
        assert_eq!(pages.len(), 1);
        assert!(
            pages[0].lines.len() >= 3,
            "wrapped: {}",
            pages[0].lines.len()
        );
        // No run exceeds the content width.
        for line in &pages[0].lines {
            for r in &line.runs {
                let w = r.x + r.text.chars().count() as f32 * 5.0;
                assert!(w <= 100.0 + 0.01, "run overflows: {w}");
            }
        }
    }

    #[test]
    fn content_overflow_breaks_into_pages() {
        // line height 10px, content_h 30px → 3 lines/page. 7 short paragraphs → 3 pages.
        let opts = LayoutOpts {
            page_w: 1000.0,
            page_h: 30.0,
            margin: 0.0,
            font_px: 10.0,
            line_spacing: 1.0,
            para_gap: 0.0,
            align: Align::Left,
        };
        let blocks: Vec<Block> = (0..7).map(|_| para("x")).collect();
        let pages = paginate(&blocks, &opts, &Mono);
        assert_eq!(pages.len(), 3, "7 lines / 3 per page");
        assert_eq!(pages[0].lines.len(), 3);
        assert_eq!(pages[2].lines.len(), 1);
    }

    #[test]
    fn heading_uses_a_larger_line_height() {
        let opts = LayoutOpts {
            page_w: 10_000.0,
            page_h: 10_000.0,
            margin: 0.0,
            font_px: 10.0,
            line_spacing: 1.0,
            para_gap: 0.0,
            align: Align::Left,
        };
        let pages = paginate(
            &[Block::Heading {
                level: 1,
                content: vec![Inline::Run(TextRun {
                    text: "Title".into(),
                    bold: false,
                    italic: false,
                    href: None,
                })],
            }],
            &opts,
            &Mono,
        );
        let line = &pages[0].lines[0];
        assert_eq!(line.height, 18.0, "h1 = 1.8 * 10"); // heading_scale(1)=1.8
        assert!(line.runs[0].bold, "headings render bold");
    }

    #[test]
    fn list_item_has_marker_and_hanging_indent() {
        let opts = LayoutOpts::new(1000.0, 1000.0, 10.0);
        let pages = paginate(
            &[Block::ListItem {
                ordered: true,
                index: 3,
                content: vec![Inline::Run(TextRun {
                    text: "item text".into(),
                    bold: false,
                    italic: false,
                    href: None,
                })],
            }],
            &opts,
            &Mono,
        );
        let runs = &pages[0].lines[0].runs;
        assert_eq!(runs[0].text, "3.", "ordered marker");
        assert_eq!(runs[0].x, 0.0, "marker at content origin");
        assert!(runs[1].x > 0.0, "body hangs past the marker");
    }

    #[test]
    fn integrates_with_phase2_parsing() {
        let blocks = parse_blocks("<html><body><h2>Hi</h2><p>one two three</p></body></html>");
        let opts = LayoutOpts::new(400.0, 600.0, 16.0);
        let pages = paginate(&blocks, &opts, &Mono);
        assert!(!pages.is_empty());
        let all_text: String = pages[0]
            .lines
            .iter()
            .flat_map(|l| l.runs.iter())
            .map(|r| r.text.clone())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(all_text.contains("Hi") && all_text.contains("three"));
    }

    #[test]
    fn empty_blocks_make_no_pages() {
        assert!(paginate(&[], &LayoutOpts::new(400.0, 600.0, 16.0), &Mono).is_empty());
    }

    /// Collect every placed run as `(block, char_offset, text)` in reading order.
    fn run_anchors(pages: &[Page]) -> Vec<(usize, usize, String)> {
        pages
            .iter()
            .flat_map(|p| p.lines.iter())
            .flat_map(|l| l.runs.iter())
            .map(|r| (r.anchor.block, r.anchor.char_offset, r.text.clone()))
            .collect()
    }

    fn wide(font_px: f32) -> LayoutOpts {
        LayoutOpts {
            page_w: 100_000.0,
            page_h: 100_000.0,
            margin: 0.0,
            font_px,
            line_spacing: 1.0,
            para_gap: 0.0,
            align: Align::Left,
        }
    }

    #[test]
    fn layout_digest_is_stable_and_sensitive_to_layout_fields() {
        let base = LayoutOpts::new(400.0, 600.0, 16.0);
        // Deterministic: identical opts → identical digest (same process AND, by fixed seed, across
        // processes — the on-disk cache key contract).
        assert_eq!(
            base.layout_digest(),
            LayoutOpts::new(400.0, 600.0, 16.0).layout_digest()
        );

        // Every layout-affecting field flips the digest.
        let d = base.layout_digest();
        assert_ne!(
            d,
            LayoutOpts {
                page_w: 401.0,
                ..base
            }
            .layout_digest(),
            "width"
        );
        assert_ne!(
            d,
            LayoutOpts {
                page_h: 601.0,
                ..base
            }
            .layout_digest(),
            "height"
        );
        assert_ne!(
            d,
            LayoutOpts {
                margin: base.margin + 1.0,
                ..base
            }
            .layout_digest(),
            "margin"
        );
        assert_ne!(
            d,
            LayoutOpts {
                font_px: 17.0,
                ..base
            }
            .layout_digest(),
            "font"
        );
        assert_ne!(
            d,
            LayoutOpts {
                line_spacing: 1.5,
                ..base
            }
            .layout_digest(),
            "line spacing"
        );
        assert_ne!(
            d,
            LayoutOpts {
                para_gap: base.para_gap + 1.0,
                ..base
            }
            .layout_digest(),
            "para gap"
        );
        assert_ne!(
            d,
            LayoutOpts {
                align: Align::Justify,
                ..base
            }
            .layout_digest(),
            "align"
        );
    }

    #[test]
    fn run_anchors_track_chapter_character_offsets() {
        // "alpha"(5)@0, space@5, "beta"(4)@6, space@10, "gamma"@11.
        let pages = paginate(&[para("alpha beta gamma")], &wide(10.0), &Mono);
        assert_eq!(
            run_anchors(&pages),
            vec![
                (0, 0, "alpha".into()),
                (0, 6, "beta".into()),
                (0, 11, "gamma".into()),
            ]
        );
    }

    #[test]
    fn block_index_increments_and_offset_continues_across_blocks() {
        // block 0 "one two": one@0, two@4 → cursor 7; block 1 "three"@7.
        let pages = paginate(&[para("one two"), para("three")], &wide(10.0), &Mono);
        assert_eq!(
            run_anchors(&pages),
            vec![
                (0, 0, "one".into()),
                (0, 4, "two".into()),
                (1, 7, "three".into()),
            ]
        );
    }

    #[test]
    fn list_marker_shares_body_offset_and_does_not_consume_budget() {
        let pages = paginate(
            &[
                Block::ListItem {
                    ordered: true,
                    index: 1,
                    content: vec![Inline::Run(TextRun {
                        text: "first".into(),
                        bold: false,
                        italic: false,
                        href: None,
                    })],
                },
                para("after"),
            ],
            &LayoutOpts::new(1000.0, 1000.0, 10.0),
            &Mono,
        );
        let anchors = run_anchors(&pages);
        // Marker "1." and body "first" both anchor at offset 0 of block 0; the marker adds no budget,
        // so "after" (block 1) starts at 5 (= len("first")), not 7.
        assert_eq!(anchors[0], (0, 0, "1.".into()), "marker shares body start");
        assert_eq!(anchors[1], (0, 0, "first".into()), "body at block start");
        assert_eq!(
            anchors[2],
            (1, 5, "after".into()),
            "marker consumed no offset"
        );
    }

    #[test]
    fn run_anchors_are_font_size_invariant() {
        // Wrapping differs by size, but each word keeps its (block, char_offset) — the reflow-stable
        // property a highlight/Digest anchor relies on (ADR-INKREAD-0012).
        let blocks = [
            para("the quick brown fox jumps over"),
            para("the lazy dog sleeps soundly"),
        ];
        let narrow = |fp: f32| {
            let opts = LayoutOpts {
                page_w: 60.0,
                ..wide(fp)
            };
            run_anchors(&paginate(&blocks, &opts, &Mono))
        };
        assert_eq!(narrow(10.0), narrow(20.0));
    }
}
