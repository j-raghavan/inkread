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
use crate::css::BlockStyle;

/// Glyph-advance measurement for a font (Phase 4 supplies a real implementation; tests use a
/// fixed-pitch fake). `bold`/`italic` may select a different face/metrics.
pub trait Metrics {
    /// The advance width, in pixels, of `text` rendered at `size_px` with the given emphasis.
    fn advance(&self, text: &str, size_px: f32, bold: bool, italic: bool) -> f32;
}

/// Soft-hyphenation opportunities for a word, so justified/narrow lines break long words like a book
/// rather than leaving loose gaps (KOReader uses Knuth-Liang patterns; Phase 4 supplies them).
/// Abstracted like [`Metrics`] so layout stays host-testable without a pattern dictionary.
pub trait Hyphenator {
    /// Byte offsets within `word` where a hyphen may be inserted (each `0 < i < word.len()`), in
    /// ascending order. An empty result means "never break this word".
    fn opportunities(&self, word: &str) -> Vec<usize>;
}

/// A hyphenator that never breaks a word — the default ([`paginate`]) and what the pure layout tests
/// use for deterministic wrapping.
pub struct NoHyphen;

impl Hyphenator for NoHyphen {
    fn opportunities(&self, _word: &str) -> Vec<usize> {
        Vec::new()
    }
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

/// Which alignment a block is actually laid out with, given what the book declared and the reader's
/// global preference (#188).
///
/// The book wins only for `center` and `right`. Those signal *decorative* intent — a title page, an
/// epigraph, a verse block, a colophon — which is the damage #188 reports, and no reader's
/// alignment preference is really a statement about those. `left` and `justify` are what books
/// declare for ordinary body prose (`p { text-align: justify }` is near-universal), and honouring
/// those would silently override the setting the reader chose for the text they actually read. So
/// they defer to `user`.
#[must_use]
fn effective_align(style: &BlockStyle, user: Align) -> Align {
    match style.align {
        Some(Align::Center) => Align::Center,
        Some(Align::Right) => Align::Right,
        _ => user,
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
    /// it, while a non-layout change (e.g. a colour theme — none exist in `LayoutOpts`) could not.
    ///
    /// Uses **FNV-1a-64**, not `std::hash::DefaultHasher`: this value keys a persisted pagination, so
    /// it must be **stable forever across builds and toolchains** — `DefaultHasher`'s algorithm is
    /// explicitly allowed to change between releases. (Mirrors the FNV-1a fingerprint policy in
    /// `reader-core`'s `persistence::identity`.) f32s are folded in by bit pattern so the digest is
    /// exact and deterministic (ADR-INKREAD-0013 D1).
    #[must_use]
    pub fn layout_digest(&self) -> u64 {
        const FNV_OFFSET_64: u64 = 0xcbf2_9ce4_8422_2325;
        const FNV_PRIME_64: u64 = 0x0000_0100_0000_01b3;
        let mut h = FNV_OFFSET_64;
        let mut eat = |byte: u8| {
            h ^= u64::from(byte);
            h = h.wrapping_mul(FNV_PRIME_64);
        };
        for field in [
            self.page_w,
            self.page_h,
            self.margin,
            self.font_px,
            self.line_spacing,
            self.para_gap,
        ] {
            for b in field.to_bits().to_le_bytes() {
                eat(b);
            }
        }
        eat(self.align as u8);
        h
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

/// Paginate a chapter's blocks into pages for the viewport `opts`, measuring text via `m`. No
/// hyphenation (use [`paginate_with`] to soft-hyphenate justified/narrow lines like a book).
#[must_use]
pub fn paginate(blocks: &[Block], opts: &LayoutOpts, m: &dyn Metrics) -> Vec<Page> {
    paginate_with(blocks, opts, m, &NoHyphen)
}

/// As [`paginate`], but with a [`Hyphenator`] so long words can be soft-hyphenated to tighten
/// justified/narrow lines (book typography, matching KOReader).
#[must_use]
pub fn paginate_with(
    blocks: &[Block],
    opts: &LayoutOpts,
    m: &dyn Metrics,
    hyph: &dyn Hyphenator,
) -> Vec<Page> {
    let mut pager = Pager::new(opts, hyph);
    // Chapter-relative character cursor, advanced as source text is consumed in reading order, so
    // every placed run/glyph carries a font-invariant offset (ADR-INKREAD-0012).
    let mut cursor = 0usize;
    // Book typography (KOReader/crengine epub.css model): paragraphs are set dense — a first-line
    // indent of ~1.2em distinguishes them with NO blank line between (avoids the "too many white
    // lines" web look). Headings are bold + scaled with a margin before (0.7em) and after (0.5em);
    // the before-margin collapses at the top of a page.
    let indent = opts.font_px * 1.2;
    for (block_index, block) in blocks.iter().enumerate() {
        match block {
            Block::Heading {
                level,
                content,
                style,
            } => {
                pager.gap_before(opts.font_px * 0.7);
                let size = opts.font_px * heading_scale(*level);
                // Headings are bold by default, but that is inkread's typography, not a rule: a
                // book that says `font-weight: normal` on its title gets a normal-weight title.
                let bold = style.bold.unwrap_or(true);
                pager.add_paragraph(
                    content,
                    size,
                    0.0,
                    0.0,
                    bold,
                    effective_align(style, opts.align),
                    block_index,
                    &mut cursor,
                    m,
                );
                pager.gap(opts.font_px * 0.5);
            }
            Block::Paragraph { content, style } => {
                // First line indented, the rest flush left, no trailing gap — dense and book-like.
                // The indent is the *only* thing marking where one paragraph ends and the next
                // begins, since this typography deliberately omits the blank line between them; an
                // indent applied to every line would leave prose with no paragraph breaks at all
                // (#163) and would also spend 1.2em of every line's width on nothing.
                let align = effective_align(style, opts.align);
                // Two ways a paragraph loses that indent (#188): the book zeroes `text-indent`, or
                // the block is centred/right-aligned. The indent is inkread's own device for
                // marking where prose paragraphs start — on a decorative centred line it marks
                // nothing, and it would push the text off-centre by half the indent.
                let centred = matches!(align, Align::Center | Align::Right);
                let first_indent = if style.indent == Some(false) || centred {
                    0.0
                } else {
                    indent
                };
                pager.add_paragraph(
                    content,
                    opts.font_px,
                    first_indent,
                    0.0,
                    style.bold.unwrap_or(false),
                    align,
                    block_index,
                    &mut cursor,
                    m,
                );
            }
            Block::ListItem {
                ordered,
                index,
                content,
                style,
            } => {
                let marker = if *ordered {
                    format!("{index}.")
                } else {
                    "•".to_string()
                };
                // A list item keeps its flush-left hanging indent whatever the book declares:
                // centring text that hangs off a marker has no sensible reading. Weight still
                // applies.
                pager.add_list_item(
                    &marker,
                    content,
                    opts.font_px,
                    style.bold.unwrap_or(false),
                    block_index,
                    &mut cursor,
                    m,
                );
                pager.gap(opts.font_px * 0.15);
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
                pager.gap_before(opts.font_px * 0.4);
                pager.add_paragraph(
                    &run,
                    opts.font_px,
                    0.0,
                    0.0,
                    false,
                    opts.align,
                    block_index,
                    &mut cursor,
                    m,
                );
                pager.gap(opts.font_px * 0.4);
            }
            Block::Rule => pager.add_rule(opts.para_gap),
        }
    }
    pager.finish()
}

/// Accumulates lines into pages, breaking when the content box is full.
struct Pager<'o> {
    opts: &'o LayoutOpts,
    hyph: &'o dyn Hyphenator,
    pages: Vec<Page>,
    current: Vec<LayoutLine>,
    cursor_y: f32,
}

impl<'o> Pager<'o> {
    fn new(opts: &'o LayoutOpts, hyph: &'o dyn Hyphenator) -> Self {
        Self {
            opts,
            hyph,
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

    /// A gap inserted BEFORE a block (heading/image), collapsed to nothing at the top of a page so
    /// the page's top margin isn't doubled (margin-collapse, matching browser/crengine behaviour).
    fn gap_before(&mut self, dy: f32) {
        if self.cursor_y > 0.0 {
            self.cursor_y += dy;
        }
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
        first_indent: f32,
        rest_indent: f32,
        bold_all: bool,
        align: Align,
        block: usize,
        cursor: &mut usize,
        m: &dyn Metrics,
    ) {
        let lines = break_lines(
            inlines,
            size,
            first_indent,
            rest_indent,
            self.opts.content_w(),
            bold_all,
            block,
            cursor,
            m,
            self.hyph,
        );
        let line_h = size * self.opts.line_spacing;
        let n = lines.len();
        for (i, mut runs) in lines.into_iter().enumerate() {
            align_line(&mut runs, align, self.opts.content_w(), i + 1 == n, m);
            self.emit(runs, line_h, false);
        }
    }

    /// Lay out a list item with a hanging marker and indented body.
    #[allow(clippy::too_many_arguments)]
    fn add_list_item(
        &mut self,
        marker: &str,
        inlines: &[Inline],
        size: f32,
        bold: bool,
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
        // A hanging indent: the body is inset on *every* line and the marker hangs out to the left
        // of the first, so both indents are the same here — unlike a paragraph's first-line indent.
        let mut lines = break_lines(
            inlines,
            size,
            indent,
            indent,
            self.opts.content_w(),
            bold,
            block,
            cursor,
            m,
            self.hyph,
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
/// body is offset by `indent`). A word that won't fit is soft-hyphenated via `hyph` when a break
/// point fits the remaining space; otherwise it wraps whole (a word too wide even alone overflows).
#[allow(clippy::too_many_arguments)]
fn break_lines(
    inlines: &[Inline],
    size: f32,
    first_indent: f32,
    rest_indent: f32,
    content_w: f32,
    bold_all: bool,
    block: usize,
    cursor: &mut usize,
    m: &dyn Metrics,
    hyph: &dyn Hyphenator,
) -> Vec<Vec<PlacedRun>> {
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
                // Place the token, splitting it across lines at hyphenation points as needed. `rest`
                // is the suffix still to place; `off` is how many of the token's chars precede it (so
                // the suffix run keeps a font-invariant anchor); only the `first` fragment carries the
                // inter-word space (ADR-INKREAD-0012).
                let mut rest = text;
                let mut off = 0usize;
                let mut first = true;
                while !rest.is_empty() {
                    // `lines` holds the lines already finished, so its length is the index of the
                    // one being built — which is what decides whether this is the indented first
                    // line or a subsequent one. Both the offset and the room available move with it.
                    let indent = if lines.is_empty() {
                        first_indent
                    } else {
                        rest_indent
                    };
                    let avail = (content_w - indent).max(1.0);
                    let lead = if first && need_space && !cur.is_empty() {
                        space_w
                    } else {
                        0.0
                    };
                    let start = if cur.is_empty() { 0.0 } else { x + lead };
                    let room = avail - start;
                    let rest_w = m.advance(rest, size, bold, italic);
                    let anchor = SourceAnchor {
                        block: anchor.block,
                        char_offset: anchor.char_offset + off,
                    };
                    let push = |cur: &mut Vec<PlacedRun>, txt: String, px: f32| {
                        cur.push(PlacedRun {
                            x: indent + px,
                            text: txt,
                            size_px: size,
                            bold,
                            italic,
                            href: href.map(str::to_string),
                            anchor,
                        });
                    };
                    if rest_w <= room {
                        push(&mut cur, rest.to_string(), start);
                        x = start + rest_w;
                        break;
                    }
                    // Two ways to split the token mid-word: an unspaced-script (CJK) break at a
                    // UAX #14 opportunity (no hyphen — for a spaceless paragraph this is the only
                    // way it wraps), else a soft-hyphenated Latin break. Same head-and-wrap tail.
                    let split = unspaced_break_fit(rest, room, m, size, bold, italic)
                        .map(|(head, chars)| (head.to_string(), head.len(), chars))
                        .or_else(|| {
                            hyphenate_fit(rest, room, hyph, m, size, bold, italic)
                                .map(|(head, chars)| (format!("{head}-"), head.len(), chars))
                        });
                    if let Some((fragment, head_len, head_chars)) = split {
                        push(&mut cur, fragment, start);
                        lines.push(std::mem::take(&mut cur));
                        rest = &rest[head_len..];
                        off += head_chars;
                        first = false;
                        x = 0.0;
                        continue;
                    }
                    if !cur.is_empty() {
                        // Wrap the whole remaining word to a fresh line, then retry it there.
                        lines.push(std::mem::take(&mut cur));
                        first = false;
                        x = 0.0;
                        continue;
                    }
                    // Fresh line, no break point, still too wide → place it overflowing.
                    push(&mut cur, rest.to_string(), 0.0);
                    x = rest_w;
                    break;
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

/// Whether `ch` belongs to a script written without inter-word spaces (Han, kana, Hangul, and the
/// CJK punctuation/fullwidth blocks). These are the characters around which a mid-token line break
/// is typographically normal — the gate that keeps [`unspaced_break_fit`] away from Latin tokens,
/// whose wrapping (hyphenation-only) must not change.
fn is_unspaced_script(ch: char) -> bool {
    matches!(u32::from(ch),
        0x1100..=0x11FF      // Hangul Jamo
        | 0x2E80..=0x303F    // CJK Radicals … CJK Symbols and Punctuation (。、「」)
        | 0x3040..=0x30FF    // Hiragana, Katakana
        | 0x3130..=0x318F    // Hangul Compatibility Jamo
        | 0x31F0..=0x31FF    // Katakana Phonetic Extensions
        | 0x3400..=0x4DBF    // CJK Extension A
        | 0x4E00..=0x9FFF    // CJK Unified Ideographs
        | 0xA960..=0xA97F    // Hangul Jamo Extended-A
        | 0xAC00..=0xD7FF    // Hangul Syllables (+ Jamo Extended-B)
        | 0xF900..=0xFAFF    // CJK Compatibility Ideographs
        | 0xFE30..=0xFE4F    // CJK Compatibility Forms
        | 0xFF00..=0xFFEF    // Halfwidth and Fullwidth Forms（，！？）
        | 0x20000..=0x2FA1F  // CJK Extensions B–F, Compatibility Supplement
    )
}

/// The longest prefix of `word` ending at a **UAX #14** line-break opportunity adjacent to an
/// unspaced-script character, whose text fits `room` px as-is (no hyphen inserted); returns
/// `(head, head_char_count)`, or `None` if the token has no such break point that fits.
///
/// This is how CJK wraps: a Chinese paragraph has no spaces, so it reaches [`break_lines`] as one
/// token and — before this — overflowed the line as an unbreakable "word". UAX #14 supplies the
/// between-character opportunities *and* the prohibition (kinsoku) rules: it never allows a break
/// before a closing form like 。、」or after an opening 「, so those hang onto the right line for
/// free. Opportunities not adjacent to an unspaced-script character are ignored so Latin tokens
/// (e.g. `self-evident`) keep their existing hyphenation-only wrapping bit-for-bit.
///
/// Width accumulates segment-by-segment between opportunities (one O(len) pass, so a whole-chapter
/// token stays linear). Cross-segment kerning is dropped by the summation; kerning only tightens,
/// so a prefix that fits by the sum also fits when rendered — and CJK faces don't kern.
fn unspaced_break_fit<'w>(
    word: &'w str,
    room: f32,
    m: &dyn Metrics,
    size: f32,
    bold: bool,
    italic: bool,
) -> Option<(&'w str, usize)> {
    if room <= 0.0 || !word.chars().any(is_unspaced_script) {
        return None;
    }
    let mut best: Option<(&str, usize)> = None;
    let mut width = 0.0f32;
    let mut measured = 0usize; // byte offset up to which `width`/`chars` account
    let mut chars = 0usize;
    for (b, _) in unicode_linebreak::linebreaks(word) {
        if b >= word.len() {
            break; // the mandatory end-of-text "opportunity" is not a split point
        }
        let seg = &word[measured..b];
        width += m.advance(seg, size, bold, italic);
        chars += seg.chars().count();
        measured = b;
        if width > room {
            break; // widths only grow — no later prefix can fit
        }
        let adjacent_unspaced = word[..b]
            .chars()
            .next_back()
            .is_some_and(is_unspaced_script)
            || word[b..].chars().next().is_some_and(is_unspaced_script);
        if adjacent_unspaced {
            best = Some((&word[..b], chars));
        }
    }
    best
}

/// The longest prefix of `word` ending at a hyphenation opportunity whose text plus a trailing
/// hyphen fits within `room` px; returns `(head, head_char_count)`, or `None` if none fits.
fn hyphenate_fit<'w>(
    word: &'w str,
    room: f32,
    hyph: &dyn Hyphenator,
    m: &dyn Metrics,
    size: f32,
    bold: bool,
    italic: bool,
) -> Option<(&'w str, usize)> {
    if room <= 0.0 {
        return None;
    }
    let hyphen_w = m.advance("-", size, bold, italic);
    let mut best: Option<(&str, usize)> = None;
    // Opportunities are ascending, so heads grow; keep the largest that fits, stop once one overflows.
    for b in hyph.opportunities(word) {
        if b == 0 || b >= word.len() {
            continue;
        }
        let head = &word[..b];
        if m.advance(head, size, bold, italic) + hyphen_w <= room {
            best = Some((head, head.chars().count()));
        } else {
            break;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    #[test]
    fn layout_digest_is_stable_and_sensitive_to_layout_fields() {
        let base = LayoutOpts::new(400.0, 600.0, 16.0);
        // Deterministic: identical opts → identical digest (same process AND across
        // processes — the persisted cache key contract).
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
    fn layout_digest_is_pinned_against_algorithm_drift() {
        // The digest keys persisted paginations (ADR-INKREAD-0013 D1); pin a known value so a future
        // change to the FNV constants/algorithm — which would silently orphan every cached
        // pagination — is caught here, exactly as reader-core's identity fingerprint is pinned.
        let opts = LayoutOpts {
            page_w: 400.0,
            page_h: 600.0,
            margin: 24.0,
            font_px: 16.0,
            line_spacing: 1.4,
            para_gap: 11.2,
            align: Align::Left,
        };
        assert_eq!(opts.layout_digest(), 17_685_407_801_978_826_572);
    }

    use super::*;
    use crate::content::{parse_blocks, TextRun};
    use crate::css::BlockStyle;

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
            style: BlockStyle::default(),
        }
    }

    /// Build the opts used by the #188 declared-style tests: 10px Mono font (5px/char), content
    /// width 100 → 20 chars per line, and a reader whose global alignment is the default Left.
    fn style_opts() -> LayoutOpts {
        LayoutOpts {
            page_w: 100.0,
            page_h: 10_000.0,
            margin: 0.0,
            font_px: 10.0,
            line_spacing: 1.0,
            para_gap: 0.0,
            align: Align::Left,
        }
    }

    fn styled_para(text: &str, style: BlockStyle) -> Block {
        let Block::Paragraph { content, .. } = para(text) else {
            unreachable!()
        };
        Block::Paragraph { content, style }
    }

    fn styled_heading(text: &str, style: BlockStyle) -> Block {
        let Block::Paragraph { content, .. } = para(text) else {
            unreachable!()
        };
        Block::Heading {
            level: 1,
            content,
            style,
        }
    }

    /// #188: the title page rendered hard-left because `text-align: center` never reached layout.
    #[test]
    fn a_book_declared_centre_centres_even_when_the_reader_prefers_left() {
        let opts = style_opts();
        let style = BlockStyle {
            align: Some(Align::Center),
            ..Default::default()
        };
        let pages = paginate(&[styled_heading("PRIDE", style)], &opts, &Mono);
        let x = pages[0].lines[0].runs[0].x;
        // "PRIDE" at heading scale is narrower than the content box, so it must be inset.
        assert!(x > 0.0, "declared centre was ignored: x = {x}");

        // …and an undeclared heading still honours the reader's Left.
        let plain = paginate(
            &[styled_heading("PRIDE", BlockStyle::default())],
            &opts,
            &Mono,
        );
        assert_eq!(plain[0].lines[0].runs[0].x, 0.0);
    }

    /// The other half of the #188 policy: a book that justifies its prose must not override the
    /// alignment the reader chose for the text they actually read.
    #[test]
    fn a_book_declared_justify_or_left_defers_to_the_reader() {
        let opts = style_opts();
        for declared in [Align::Justify, Align::Left] {
            let style = BlockStyle {
                align: Some(declared),
                ..Default::default()
            };
            // A long paragraph so justification would visibly spread the first line's runs.
            let text = "alpha bravo charlie delta echo foxtrot golf hotel";
            let book = paginate(&[styled_para(text, style)], &opts, &Mono);
            let user = paginate(&[styled_para(text, BlockStyle::default())], &opts, &Mono);
            assert_eq!(
                book[0].lines[0]
                    .runs
                    .iter()
                    .map(|r| r.x)
                    .collect::<Vec<_>>(),
                user[0].lines[0]
                    .runs
                    .iter()
                    .map(|r| r.x)
                    .collect::<Vec<_>>(),
                "book-declared {declared:?} should defer to the reader's Left"
            );
        }
    }

    /// The reader's own Justify still applies to blocks the book says nothing about — the policy
    /// suppresses the *book's* left/justify, not the setting.
    #[test]
    fn the_readers_justify_still_applies_to_undeclared_blocks() {
        let opts = LayoutOpts {
            align: Align::Justify,
            ..style_opts()
        };
        let text = "alpha bravo charlie delta echo foxtrot golf hotel";
        let pages = paginate(&[styled_para(text, BlockStyle::default())], &opts, &Mono);
        let first = &pages[0].lines[0];
        assert!(first.runs.len() > 1);
        let last_x = first.runs.last().unwrap().x;
        let flush = paginate(
            &[styled_para(text, BlockStyle::default())],
            &LayoutOpts {
                align: Align::Left,
                ..style_opts()
            },
            &Mono,
        );
        assert!(
            last_x > flush[0].lines[0].runs.last().unwrap().x,
            "justification did not spread the line"
        );
    }

    /// #188 / #163: the paragraph indent is applied even to blocks whose stylesheet turns it off.
    #[test]
    fn a_declared_zero_text_indent_drops_the_first_line_indent() {
        let opts = style_opts();
        let indented = paginate(
            &[styled_para("short line", BlockStyle::default())],
            &opts,
            &Mono,
        );
        assert_eq!(indented[0].lines[0].runs[0].x, 12.0, "1.2em of 10px");

        let style = BlockStyle {
            indent: Some(false),
            ..Default::default()
        };
        let flat = paginate(&[styled_para("short line", style)], &opts, &Mono);
        assert_eq!(
            flat[0].lines[0].runs[0].x, 0.0,
            "text-indent: 0 was ignored"
        );
    }

    /// A centred block must not also carry the synthetic prose indent, which would push it off
    /// centre by half the indent.
    #[test]
    fn a_centred_paragraph_drops_the_indent_it_would_otherwise_carry() {
        let opts = style_opts();
        let centred = BlockStyle {
            align: Some(Align::Center),
            ..Default::default()
        };
        let text = "abcd"; // 4 chars * 5px = 20px wide in a 100px box
        let pages = paginate(&[styled_para(text, centred)], &opts, &Mono);
        assert_eq!(
            pages[0].lines[0].runs[0].x, 40.0,
            "expected (100 - 20) / 2 with no indent skewing it"
        );
    }

    /// #188: headings were laid out with `bold_all = true` unconditionally.
    #[test]
    fn a_declared_normal_font_weight_unbolds_a_heading() {
        let opts = style_opts();
        let bold = paginate(
            &[styled_heading("Title", BlockStyle::default())],
            &opts,
            &Mono,
        );
        assert!(bold[0].lines[0].runs[0].bold, "headings default to bold");

        let style = BlockStyle {
            bold: Some(false),
            ..Default::default()
        };
        let normal = paginate(&[styled_heading("Title", style)], &opts, &Mono);
        assert!(
            !normal[0].lines[0].runs[0].bold,
            "font-weight: normal was ignored"
        );

        // …and the converse: a paragraph the book bolds comes out bold.
        let style = BlockStyle {
            bold: Some(true),
            ..Default::default()
        };
        let p = paginate(&[styled_para("x", style)], &opts, &Mono);
        assert!(p[0].lines[0].runs[0].bold);
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

    /// The narrow test viewport for the unspaced-script (CJK) wrapping tests: 10px font → 5px/char
    /// (Mono); content 50px. The paragraph's 12px (1.2em) indent applies to the **first line only**,
    /// so 7 chars fit there and 10 on every line after it.
    fn cjk_opts() -> LayoutOpts {
        LayoutOpts {
            page_w: 50.0,
            page_h: 10_000.0,
            margin: 0.0,
            font_px: 10.0,
            line_spacing: 1.0,
            para_gap: 0.0,
            align: Align::Left,
        }
    }

    /// Each line's runs joined (the CJK tests place one run per line).
    fn line_texts(pages: &[Page]) -> Vec<String> {
        pages
            .iter()
            .flat_map(|p| &p.lines)
            .map(|l| l.runs.iter().map(|r| r.text.as_str()).collect())
            .collect()
    }

    /// #163: the first-line indent must land on the first line and nowhere else.
    ///
    /// This typography omits the blank line between paragraphs on purpose, so the indent is the
    /// *only* thing marking where one paragraph ends and the next begins. Applying it to every line
    /// — which is what the code did — left prose with no paragraph breaks at all, and spent 1.2em of
    /// every line's width on nothing.
    #[test]
    fn only_a_paragraphs_first_line_is_indented() {
        let pages = paginate(
            &[para("alpha bravo charlie delta echo foxtrot")],
            &cjk_opts(),
            &Mono,
        );
        let lines: Vec<&LayoutLine> = pages.iter().flat_map(|p| &p.lines).collect();
        assert!(
            lines.len() > 1,
            "need a wrapped paragraph to test: {lines:?}"
        );

        assert_eq!(
            lines[0].runs[0].x, 12.0,
            "the first line carries the 1.2em indent"
        );
        for (i, line) in lines.iter().enumerate().skip(1) {
            assert_eq!(line.runs[0].x, 0.0, "line {i} must start flush left");
        }
    }

    /// The counterpart: a list item's indent is a *hanging* indent, so it applies to every line and
    /// the marker hangs to the left of the first. The two must not be conflated.
    #[test]
    fn a_list_items_body_stays_indented_on_every_line() {
        let item = Block::ListItem {
            ordered: false,
            index: 1,
            content: vec![Inline::Run(TextRun {
                text: "alpha bravo charlie delta echo".into(),
                bold: false,
                italic: false,
                href: None,
            })],
            style: BlockStyle::default(),
        };
        let pages = paginate(&[item], &cjk_opts(), &Mono);
        let lines: Vec<&LayoutLine> = pages.iter().flat_map(|p| &p.lines).collect();
        assert!(lines.len() > 1, "need a wrapped item to test");

        // The marker hangs at the content origin; the body starts indented past it.
        assert_eq!(lines[0].runs[0].text, "•");
        assert_eq!(lines[0].runs[0].x, 0.0, "the marker hangs left");
        let body_x = lines[0].runs[1].x;
        assert!(body_x > 0.0, "the body is inset past the marker");
        for (i, line) in lines.iter().enumerate().skip(1) {
            assert_eq!(
                line.runs[0].x, body_x,
                "wrapped line {i} stays under the body, not the marker"
            );
        }
    }

    #[test]
    fn cjk_paragraph_wraps_between_characters() {
        // No spaces, so the whole paragraph is one token — before UAX #14 it overflowed as a single
        // unbreakable "word". 22 Han chars wrap greedily to 7 (the indented first line) + 10 + 5,
        // nothing lost.
        let text = "书山有路勤为径学海无涯苦作舟读万卷书行万里路";
        let pages = paginate(&[para(text)], &cjk_opts(), &Mono);
        let lines = line_texts(&pages);
        assert_eq!(lines.len(), 3, "22 chars as 7+10+5: {lines:?}");
        assert_eq!(
            lines[0].chars().count(),
            7,
            "the indented first line is narrower"
        );
        assert_eq!(
            lines[1].chars().count(),
            10,
            "later lines get the full column"
        );
        assert_eq!(lines.concat(), text, "no characters dropped or reordered");
        // No line overflows the content box, indented or not.
        for line in pages.iter().flat_map(|p| &p.lines) {
            for r in &line.runs {
                assert!(r.x + r.text.chars().count() as f32 * 5.0 <= 50.0 + 0.01);
            }
        }
    }

    #[test]
    fn cjk_break_honors_kinsoku_and_anchors_stay_font_invariant() {
        // Naive 7-chars-per-line breaking would start line 2 with 。— UAX #14 forbids a break
        // before a closing form, so the break retreats to after 六 and 。hangs onto 七's line.
        let pages = paginate(&[para("一二三四五六七。八九十")], &cjk_opts(), &Mono);
        let lines = line_texts(&pages);
        assert_eq!(lines, ["一二三四五六", "七。八九十"]);
        assert!(lines.iter().all(|l| !l.starts_with('。')));
        // The continuation run keeps a chapter-relative anchor = chars consumed before it
        // (ADR-INKREAD-0012 — a highlight re-resolves across reflow).
        let runs: Vec<&PlacedRun> = pages
            .iter()
            .flat_map(|p| &p.lines)
            .flat_map(|l| &l.runs)
            .collect();
        assert_eq!(runs[0].anchor.char_offset, 0);
        assert_eq!(runs[1].anchor.char_offset, 6);
    }

    #[test]
    fn mixed_latin_cjk_breaks_at_the_script_boundary_opportunities() {
        // "Hello你好世界" is one token (no spaces). Latin-internal positions are not eligible —
        // only UAX #14 opportunities adjacent to an unspaced-script char — so the line fills to
        // "Hello你好" (7 chars, 35px ≤ 38px) and wraps cleanly, no overflow, no hyphen.
        let pages = paginate(&[para("Hello你好世界")], &cjk_opts(), &Mono);
        let lines = line_texts(&pages);
        assert_eq!(lines, ["Hello你好", "世界"]);
        assert!(!lines[0].ends_with('-'), "no hyphen for an unspaced break");
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
                style: BlockStyle::default(),
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
                style: BlockStyle::default(),
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
                    style: BlockStyle::default(),
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

    /// A test hyphenator allowing breaks at fixed byte offsets (regardless of the word).
    struct HyphenAt(Vec<usize>);
    impl Hyphenator for HyphenAt {
        fn opportunities(&self, _word: &str) -> Vec<usize> {
            self.0.clone()
        }
    }

    fn narrow_para() -> LayoutOpts {
        // Mono 10px → 5px/char; content 60 wide, paragraph indent 12 → 48px usable.
        LayoutOpts {
            page_w: 60.0,
            page_h: 10_000.0,
            margin: 0.0,
            font_px: 10.0,
            line_spacing: 1.0,
            para_gap: 0.0,
            align: Align::Left,
        }
    }

    #[test]
    fn long_word_hyphenates_and_suffix_keeps_its_anchor() {
        // "hyphenation" (11 chars = 55px) overflows the 48px column; a break after 5 bytes fits
        // ("hyphe" 25px + hyphen 5px = 30 ≤ 48), so it splits and the suffix anchors after the prefix.
        let pages = paginate_with(
            &[para("hyphenation")],
            &narrow_para(),
            &Mono,
            &HyphenAt(vec![5]),
        );
        let runs: Vec<_> = pages[0].lines.iter().flat_map(|l| l.runs.iter()).collect();
        assert_eq!(runs[0].text, "hyphe-", "prefix carries a trailing hyphen");
        assert_eq!(runs[1].text, "nation", "suffix continues on the next line");
        assert_eq!(runs[0].anchor.char_offset, 0);
        assert_eq!(
            runs[1].anchor.char_offset, 5,
            "suffix anchored at prefix length (reflow-stable)"
        );
    }

    #[test]
    fn default_paginate_never_hyphenates() {
        // The default ([`NoHyphen`]) places an overflowing word whole, on its own line.
        let pages = paginate(&[para("hyphenation")], &narrow_para(), &Mono);
        let runs: Vec<_> = pages[0].lines.iter().flat_map(|l| l.runs.iter()).collect();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "hyphenation");
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
