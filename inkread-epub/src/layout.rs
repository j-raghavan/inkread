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
    /// An illustration occupying this line's box (#187). Mutually exclusive with `runs` in
    /// practice: an image block emits one line that is nothing but the picture.
    pub image: Option<PlacedImage>,
}

/// An illustration placed in the content box: where it sits and how big it is drawn, not its
/// pixels. The renderer resolves `src` to bytes when it draws — layout never decodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacedImage {
    /// The resource reference from the book's markup, resolved by the renderer.
    pub src: String,
    /// The `alt` text, for the fallback when the image cannot be drawn.
    pub alt: String,
    /// Left offset within the content box (images are centred).
    pub x: i32,
    /// Drawn width in pixels.
    pub width: u32,
    /// Drawn height in pixels.
    pub height: u32,
}

/// Supplies the intrinsic pixel size of an image, so layout can reserve a box without decoding.
/// Injected like [`Metrics`] and [`Hyphenator`] — the layout stage owns no resources.
pub trait ImageSizer {
    /// Intrinsic `(width, height)` of `src`, or `None` when it cannot be resolved.
    fn size(&self, src: &str) -> Option<(u32, u32)>;
}

/// The no-images sizer: every lookup misses, so image blocks fall back to their text placeholder.
pub struct NoImages;

impl ImageSizer for NoImages {
    fn size(&self, _src: &str) -> Option<(u32, u32)> {
        None
    }
}

/// A laid-out page: the lines that fall within the content box.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Page {
    pub lines: Vec<LayoutLine>,
}

/// Fit an image into the content box, preserving its aspect ratio and centring it.
///
/// Never enlarged past its intrinsic size — upscaling a small decorative glyph to the page width
/// looks like a defect — and never larger than one page, so an oversized plate scales down to fit
/// rather than being clipped or forcing an unbreakable overflow.
fn fit_image(
    src: &str,
    alt: &str,
    opts: &LayoutOpts,
    images: &dyn ImageSizer,
) -> Option<PlacedImage> {
    let (iw, ih) = images.size(src)?;
    if iw == 0 || ih == 0 {
        return None;
    }
    let (fw, fh) = (iw as f32, ih as f32);
    let content_w = opts.content_w();
    // The vertical budget is a whole page: `add_image` starts a new page when it will not fit on
    // this one, so an image capped at the content height always has somewhere to go.
    let scale = (content_w / fw).min(opts.content_h() / fh).min(1.0);
    let width = (fw * scale).round().max(1.0);
    let height = (fh * scale).round().max(1.0);
    Some(PlacedImage {
        src: src.to_string(),
        alt: alt.to_string(),
        x: ((content_w - width) * 0.5).round() as i32,
        width: width as u32,
        height: height as u32,
    })
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
    paginate_with_images(blocks, opts, m, hyph, &NoImages)
}

/// As [`paginate_with`], with `images` supplying intrinsic sizes so illustrations are laid out as
/// boxes rather than `[image]` placeholders (#187).
#[must_use]
pub fn paginate_with_images(
    blocks: &[Block],
    opts: &LayoutOpts,
    m: &dyn Metrics,
    hyph: &dyn Hyphenator,
    images: &dyn ImageSizer,
) -> Vec<Page> {
    paginate_upto(blocks, opts, m, hyph, images, usize::MAX).0
}

/// As [`paginate_with_images`], but stops once `max_pages` complete pages exist (#186).
///
/// Returns `(pages, complete)`, where `complete` is false when blocks were left unlaid. Showing one
/// page means laying out only as far as that page: a reader resuming at the top of a long chapter
/// otherwise pays for every page of it to see the first, which is the dominant cost of opening a
/// book. The pages returned are byte-identical to the same prefix of a full pass — a page break
/// depends only on what precedes it — so a partial pagination is a prefix, never an approximation.
///
/// An in-progress page is discarded when stopping early: it is incomplete, and the caller asked
/// only for pages that are whole.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn paginate_upto(
    blocks: &[Block],
    opts: &LayoutOpts,
    m: &dyn Metrics,
    hyph: &dyn Hyphenator,
    images: &dyn ImageSizer,
    max_pages: usize,
) -> (Vec<Page>, bool) {
    let mut pager = Pager::new(opts, hyph);
    // Chapter-relative character cursor, advanced as source text is consumed in reading order, so
    // every placed run/glyph carries a font-invariant offset (ADR-INKREAD-0012).
    let mut cursor = 0usize;
    // Book typography (KOReader/crengine epub.css model): paragraphs are set dense — a first-line
    // indent of ~1.2em distinguishes them with NO blank line between (avoids the "too many white
    // lines" web look). Headings are bold + scaled with a margin before (0.7em) and after (0.5em);
    // the before-margin collapses at the top of a page.
    let indent = opts.font_px * 1.2;
    let mut complete = true;
    for (block_index, block) in blocks.iter().enumerate() {
        // Checked before the block, not after: once enough whole pages exist, laying out one more
        // block is work the caller has said it does not need.
        if pager.finished_page_count() >= max_pages {
            complete = false;
            break;
        }
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
            Block::Image { src, alt } => {
                if let Some(placed) = fit_image(src, alt, opts, images) {
                    // An image occupies one character position, as `<br>` does, so the offsets of
                    // everything after it stay stable whether or not it resolves (ADR-INKREAD-0012).
                    cursor += 1;
                    pager.gap_before(opts.font_px * 0.4);
                    pager.add_image(placed);
                    pager.gap(opts.font_px * 0.4);
                    continue;
                }
                // Unresolvable (a dangling src, an unreadable codec): fall back to naming what is
                // missing rather than dropping it silently.
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
                // The label is synthetic, like a list item's marker: it must not consume source-
                // character budget, or offsets after an image would depend on whether it resolved.
                let at = cursor;
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
                cursor = at + 1;
                pager.gap(opts.font_px * 0.4);
            }
            Block::Rule => pager.add_rule(opts.para_gap),
        }
    }
    if complete {
        (pager.finish(), true)
    } else {
        (pager.into_finished_pages(), false)
    }
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
            image: None,
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

    /// How many *whole* pages have been broken off so far (the in-progress one is not counted).
    fn finished_page_count(&self) -> usize {
        self.pages.len()
    }

    /// The whole pages only, discarding the page still being filled — used when stopping early.
    fn into_finished_pages(self) -> Vec<Page> {
        self.pages
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

    /// Emit an illustration as a line of its own.
    fn add_image(&mut self, image: PlacedImage) {
        let height = image.height as f32;
        if self.cursor_y + height > self.opts.content_h() && !self.current.is_empty() {
            self.break_page();
        }
        let top = self.cursor_y;
        self.current.push(LayoutLine {
            top,
            height,
            runs: Vec::new(),
            rule: false,
            image: Some(image),
        });
        self.cursor_y += height;
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
#[path = "layout_tests.rs"]
mod tests;
