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
use crate::css::{BlockStyle, Length, PageBreak};

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

/// Narrowest column worth setting, in ems of the body size (#194).
///
/// A comfortable single-column measure is 45-75 characters, but newspaper columns are deliberately
/// far tighter — 30-35 is normal, which is the look this feature exists to produce. At roughly half
/// an em per character, 14 em is about **28 characters**: tight, and squarely in newspaper
/// territory.
///
/// The floor is in ems, not pixels, because what matters is characters per line: raising the text
/// size narrows the measure even though the page has not changed. Set from measurement rather than
/// taste — at a comfortable reading size on a 1920px panel (56px text) a column comes out at 14 em,
/// so a floor above that declines the feature exactly where it was asked for. Below this the line
/// breaks every few words and justified text opens rivers, which is worse than one column.
const MIN_COLUMN_EM: f32 = 14.0;

/// The narrowest a table cell may be before the row gives up and stacks its cells instead.
///
/// Lower than [`MIN_COLUMN_EM`] on purpose. Page columns are continuous prose and a short measure
/// there is merely bad typography; a table cell holds a phrase whose *pairing* with the cell beside
/// it is the point, and keeping that pairing is worth a tighter measure than prose would tolerate.
/// Below this the words break every few characters and nothing is gained.
const MIN_CELL_EM: f32 = 6.0;

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

/// Viewport + typography for a layout pass (all pixels). Repagination on a font-size or margin
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
    /// Text columns per page — 1 (default) or 2 (#194). A page too narrow to give two columns a
    /// readable measure falls back to one; see [`LayoutOpts::effective_columns`].
    pub columns: u8,
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
            columns: 1,
        }
    }

    /// The gap between columns. Reuses the page margin rather than adding a knob: it is already
    /// tuned to the panel, and a gutter narrower than the outer margin reads as a mistake.
    fn gutter(&self) -> f32 {
        self.margin
    }

    /// The full text measure, ignoring columns.
    fn text_w(&self) -> f32 {
        (self.page_w - 2.0 * self.margin).max(1.0)
    }

    /// Columns actually used, which is not always what was asked for (#194).
    ///
    /// Two columns on a narrow page produce a measure too short to read — words break every few
    /// characters and justification opens rivers. Below [`MIN_COLUMN_EM`] ems the request is
    /// declined and the page stays single-column, which is what the reader wants even though it is
    /// not what they asked for.
    #[must_use]
    pub fn effective_columns(&self) -> u8 {
        if self.columns <= 1 {
            return 1;
        }
        let each = (self.text_w() - self.gutter()) / f32::from(self.columns);
        if each < MIN_COLUMN_EM * self.font_px {
            1
        } else {
            self.columns
        }
    }

    /// One column's width — what a rule spans, and what a caller drawing column-wide furniture
    /// needs. Same as [`Self::content_w`], exposed because the renderer is outside this module.
    #[must_use]
    pub fn column_width(&self) -> f32 {
        self.content_w()
    }

    /// The measure lines are broken to — one column's width, which for a single-column page is the
    /// whole text width. Line breaking, justification and image fitting all read this, so columns
    /// need no special handling anywhere below this point.
    fn content_w(&self) -> f32 {
        let cols = self.effective_columns();
        if cols <= 1 {
            return self.text_w();
        }
        ((self.text_w() - self.gutter()) / f32::from(cols)).max(1.0)
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
        // Folded in only when it changes the layout. A single-column page lays out exactly as it
        // did before columns existed, so leaving its digest alone keeps every pagination already
        // cached — rebuilding one is the cost #186 is about, and there is nothing to gain by
        // discarding them all for a field most books never set.
        if self.columns > 1 {
            eat(self.columns);
        }
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
    /// Set when this run ends its line **mid-word** — the word continues on the next line, and this
    /// says whether the hyphen shown at the break is one of ours. `None` on every other run, which
    /// is nearly all of them: a line normally ends at a word boundary.
    pub wrap: Option<Wrap>,
}

/// How a line break split the word that ends a line. Only the layout knows this — the two cases
/// print identically ("self-evident" broken before its hyphen and "well-known" broken at its own
/// both show a line ending "self-"/"well-") — so it records the fact for selection, search and copy
/// to rejoin the halves faithfully (RR11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrap {
    /// The break needed a hyphen and the layout **inserted** one. It is not in the source text, so
    /// rejoining the halves drops it: "pontifi-" + "cate" = "pontificate".
    SoftHyphen,
    /// The break needed no hyphen — the word already had one there, or it is unspaced script (CJK).
    /// Every character on the line is the source's, so rejoining keeps them all: "well-" + "known"
    /// = "well-known".
    Kept,
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
    /// Left edge of the column this line sits in, relative to the content origin (#194). Zero for a
    /// single-column page and for the first column. Runs already carry their own absolute-in-content
    /// `x`; this exists for the things that span a whole column rather than sitting at an `x` — the
    /// horizontal rule, which would otherwise run across both columns.
    pub column_x: f32,
    /// Width of that column. A rule spans `column_x .. column_x + column_w`; without it the
    /// renderer has to assume the page's column width, which is wrong for any line laid out to a
    /// narrower measure — a `<hr/>` inside a table cell would run across its neighbours (#251).
    pub column_w: f32,
    /// The page may not break between this line and the one above it: they are inside a block the
    /// book marked `page-break-inside: avoid` (#251).
    ///
    /// The property is a *block* one, but a table row is merged into shared line boxes before it is
    /// paged, and nothing of the blocks survives that merge. Carrying the request on the line is
    /// what lets a stanza inside a cell stay whole — the pager has nothing else left to ask.
    pub keep_with_prev: bool,
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
    content_w: f32,
    content_h: f32,
    images: &dyn ImageSizer,
) -> Option<PlacedImage> {
    let (iw, ih) = images.size(src)?;
    if iw == 0 || ih == 0 {
        return None;
    }
    let (fw, fh) = (iw as f32, ih as f32);
    // The vertical budget is a whole page: `add_image` starts a new page when it will not fit on
    // this one, so an image capped at the content height always has somewhere to go.
    let scale = (content_w / fw).min(content_h / fh).min(1.0);
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

/// The end (exclusive) of the run of blocks starting at `i` that must stay on one page (#251).
///
/// A run grows while the block before it says `page-break-after: avoid` — which is both the
/// property itself (keeping a heading with the text it introduces) and how a container's
/// `page-break-inside: avoid` reaches the several blocks it wraps. An explicit
/// `page-break-before: always` wins over an `avoid` beside it: a forced break is a stronger
/// statement than a preference not to break.
fn keep_run_end(blocks: &[Block], i: usize) -> usize {
    let mut end = i + 1;
    while end < blocks.len()
        && blocks[end - 1].style().break_after == Some(PageBreak::Avoid)
        && blocks[end].style().break_before != Some(PageBreak::Always)
    {
        end += 1;
    }
    end
}

/// A run of laid-out lines, positioned relative to its own origin.
///
/// Named because the row machinery below nests it two deep in two different orders — cells of
/// segments going in, stages of cells coming out — and three anonymous `Vec` levels are a puzzle
/// where two named ones are not.
type Flow = Vec<LayoutLine>;

/// Cut a row's cells into the *stages* a forced break divides it into (#251).
///
/// `cells[cell][segment]` in, `stages[stage][cell]` out.
///
/// A cell that forced a break came back already cut, but a cell that did not came back whole — and
/// letting it run on while its neighbour started a new page is what loses the correspondence a
/// parallel text is entirely about: the translation would sit beside the wrong original.
///
/// So a break in *any* cell breaks the row, and the cells that did not break are cut at the same
/// vertical position. Where several cells break, the stage boundary is the tallest of their
/// segments, so a canto whose translation runs longer still keeps its opposite number level with
/// it rather than opening a near-empty page between them.
///
/// Worked example — two cells; A breaks after its first stanza at y=100, B runs straight through to
/// y=180. `heights = [100]`: stage 0 is A's stanza beside B's lines above 100, and stage 1 is A's
/// remainder beside B's lines from 100 on, each re-based to its own origin.
fn row_stages(cells: Vec<Vec<Flow>>) -> Vec<Vec<Flow>> {
    let count = cells.iter().map(Vec::len).max().unwrap_or(0);
    if count <= 1 {
        return if count == 0 {
            Vec::new()
        } else {
            vec![cells.into_iter().flatten().collect()]
        };
    }
    let heights = stage_heights(&cells, count);
    let mut stages: Vec<Vec<Flow>> = vec![Vec::new(); count];
    for cell in cells {
        let Some(last) = cell.len().checked_sub(1) else {
            continue;
        };
        for (s, segment) in cell.into_iter().enumerate() {
            if s < last {
                // Pinned by the cell's own break: it is this stage, whole.
                stages[s].push(segment);
            } else {
                for (offset, lines) in carry_across(segment, &heights, s, count) {
                    stages[offset].push(lines);
                }
            }
        }
    }
    stages
}

/// How tall each stage is: the tallest segment a cell's *own* break ends there.
///
/// A cell's last segment ends at the row's end rather than at a break, so it pins nothing — it is
/// carried across the boundaries the other cells set. That asymmetry is what makes the boundaries
/// well defined instead of circular.
fn stage_heights(cells: &[Vec<Flow>], count: usize) -> Vec<f32> {
    let mut heights = vec![0.0f32; count];
    for cell in cells {
        for (s, segment) in cell.iter().enumerate().take(cell.len().saturating_sub(1)) {
            heights[s] = heights[s].max(flow_height(segment));
        }
    }
    heights
}

/// Cut a cell's trailing flow at the stage boundaries it did not itself ask for, so a cell with no
/// break of its own still breaks where the row does. Yields `(stage, lines)`, each re-based to that
/// stage's origin; empty stages are skipped.
fn carry_across(segment: Flow, heights: &[f32], from: usize, count: usize) -> Vec<(usize, Flow)> {
    let mut carried: Vec<Flow> = vec![Vec::new(); count - from];
    let mut stage = from;
    let mut base = 0.0;
    for mut line in segment {
        // A line starting at or past the boundary belongs to the next stage. `stage` strictly
        // increases and is bounded by `count`, so this terminates even where a boundary is zero.
        while stage + 1 < count && line.top - base >= heights[stage] {
            base += heights[stage];
            stage += 1;
        }
        line.top -= base;
        carried[stage - from].push(line);
    }
    carried
        .into_iter()
        .enumerate()
        .filter(|(_, lines)| !lines.is_empty())
        .map(|(offset, lines)| (from + offset, lines))
        .collect()
}

/// The height a flow occupies: the bottom of its lowest line. Gaps still pending at the flow's
/// trailing edge are deliberately excluded — they belong to whatever comes next.
fn flow_height(lines: &Flow) -> f32 {
    lines.iter().map(|l| l.top + l.height).fold(0.0, f32::max)
}

/// Merge a table row's per-cell flows into one `top`-ordered stream, coalescing the lines that
/// share a vertical position into a single line box (#251).
///
/// Cells are laid out independently, so each flow's `top` values are relative to the same row
/// origin — sorting by `top` therefore restores reading order down the page while keeping cells
/// that agree on a position side by side. Coalescing matters because the rest of the pipeline
/// treats a line box as the paging unit: two cells' halves of one visual line must break to a new
/// page together, and a caller counting lines must see the row's height, not the sum of its cells'.
///
/// Only plain text lines coalesce. A rule or an image spans its own cell (it carries a `column_x`
/// and at most one image per line box), so those stay separate; they render at the same `top`
/// regardless, which is all that side-by-side needs.
///
/// Cells that disagree on their block structure — a heading in one and body text in the other —
/// genuinely sit at different positions, and stay in separate line boxes. That is the honest
/// outcome: there is no shared line to share. It does mean such a row is not a single paging unit
/// throughout, so a page break can fall between two lines the cells did not agree on anyway.
fn merge_cell_flows(flows: Vec<Flow>, same_line: f32) -> Flow {
    let mut lines: Flow = flows.into_iter().flatten().collect();
    // Stable, so cells stay in source order within a line box and `x` runs left to right.
    lines.sort_by(|a, b| a.top.total_cmp(&b.top));

    let mut out: Flow = Vec::with_capacity(lines.len());
    for line in lines {
        let plain = !line.rule && line.image.is_none();
        match out.last_mut() {
            Some(prev)
                if plain
                    && !prev.rule
                    && prev.image.is_none()
                    && (line.top - prev.top).abs() < same_line =>
            {
                prev.height = prev.height.max(line.height);
                // Either half of a shared line box can forbid the break above it.
                prev.keep_with_prev |= line.keep_with_prev;
                prev.runs.extend(line.runs);
            }
            _ => out.push(line),
        }
    }
    out
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
    let mut complete = true;
    // `max_pages` counts pages, but the pager is producing *columns* — a two-column page needs two
    // of them before it is whole.
    let column_budget = max_pages.saturating_mul(usize::from(opts.effective_columns()));
    // Walked in keep-runs rather than block by block: `page-break-after: avoid` binds a block to
    // the next one, so what may be split across a page is a run, not always a single block (#251).
    let mut i = 0;
    while i < blocks.len() {
        // Checked before the run, not after: once enough whole pages exist, laying out one more
        // block is work the caller has said it does not need.
        if pager.finished_page_count() >= column_budget {
            complete = false;
            break;
        }
        let end = keep_run_end(blocks, i);
        let laid = pager.add_keep_run(&blocks[i..end], |k| i + k, &mut cursor, m, images);
        i += laid.max(1);
    }
    let columns = opts.effective_columns();
    let mut laid = if complete {
        pager.finish()
    } else {
        pager.into_finished_pages()
    };
    if !complete {
        // A single keep-run or table row can emit several columns before the budget is looked at
        // again, so the pager may have overshot — and in two-column mode an odd column count would
        // make `combine_columns` build a half page the full pass never produces. Trim to whole
        // pages within the budget, so a partial pagination stays a true prefix.
        let whole = laid.len().min(column_budget) / usize::from(columns) * usize::from(columns);
        laid.truncate(whole);
    }
    (combine_columns(laid, opts, columns), complete)
}

/// Fold a run of single-column pages into multi-column ones (#194).
///
/// The pagination above lays out to the *column* measure, so what it produced is a sequence of
/// columns in reading order. A two-column page is simply the next two of them side by side: the
/// second column's content is shifted right by one column plus the gutter, and its vertical
/// positions are already correct, because every column starts at the top of the page.
///
/// That is why columns need no special handling in line breaking, justification, image fitting or
/// page breaking — by the time this runs, all of it has already happened against the right measure.
fn combine_columns(pages: Vec<Page>, opts: &LayoutOpts, columns: u8) -> Vec<Page> {
    if columns <= 1 {
        return pages;
    }
    let dx = opts.content_w() + opts.gutter();
    pages
        .chunks(usize::from(columns))
        .map(|chunk| {
            let mut lines = Vec::new();
            for (index, column) in chunk.iter().enumerate() {
                let shift = dx * index as f32;
                lines.extend(column.lines.iter().cloned().map(|mut line| {
                    if shift != 0.0 {
                        for run in &mut line.runs {
                            run.x += shift;
                        }
                        if let Some(image) = &mut line.image {
                            image.x += shift.round() as i32;
                        }
                        line.column_x += shift;
                    }
                    line
                }));
            }
            Page { lines }
        })
        .collect()
}

/// Accumulates lines into pages, breaking when the content box is full.
///
/// `measure`/`page_h` are held rather than read off `opts` so the same pager can flow a *table cell*
/// or a keep-run — a narrower measure, with no page budget of its own (#251).
struct Pager<'o> {
    opts: &'o LayoutOpts,
    hyph: &'o dyn Hyphenator,
    pages: Vec<Page>,
    current: Vec<LayoutLine>,
    cursor_y: f32,
    /// Line-breaking width for this flow.
    measure: f32,
    /// Vertical budget before a page break; infinite for a cell flow, which is paged by the row it
    /// belongs to rather than on its own.
    page_h: f32,
    /// A block gap not yet spent. Held rather than added straight to `cursor_y` so that adjacent
    /// margins **collapse to the larger** instead of summing, as CSS does (#251): a book that
    /// declares `p { margin: 1em 0 }` means one em between stanzas, not two.
    pending_gap: f32,
    /// Whether a gap at the top of this flow collapses away. True for a page (a page's own top
    /// margin must not be doubled); false for an unpaged flow, whose top edge is not a page edge.
    collapse_at_top: bool,
    /// Set while laying out a block that asked not to be split, so the lines it emits carry the
    /// request forward as [`LayoutLine::keep_with_prev`].
    keep_lines: bool,
    /// Whether such a block has emitted a line yet — the first line of a block is where a break is
    /// allowed to fall, so only the ones after it are bound to what precedes them.
    kept_one: bool,
}

impl<'o> Pager<'o> {
    fn new(opts: &'o LayoutOpts, hyph: &'o dyn Hyphenator) -> Self {
        Self {
            opts,
            hyph,
            pages: Vec::new(),
            current: Vec::new(),
            cursor_y: 0.0,
            measure: opts.content_w(),
            page_h: opts.content_h(),
            pending_gap: 0.0,
            collapse_at_top: true,
            keep_lines: false,
            kept_one: false,
        }
    }

    /// A pager with no vertical budget, `measure` wide: it flows content without ever breaking a
    /// page, so the caller can place the result as one unit. Used for a table cell and for a run of
    /// blocks a book asked to keep together.
    ///
    /// It does *not* collapse the gap at its own top edge. That edge is not a page edge — it is
    /// wherever the caller ends up placing the flow — so swallowing the first block's `margin-top`
    /// here would lose it outright, which is what made `page-break-inside: avoid` silently delete
    /// the margins of the very stanza it was keeping together.
    fn unpaged(opts: &'o LayoutOpts, hyph: &'o dyn Hyphenator, measure: f32) -> Self {
        Self {
            measure,
            page_h: f32::INFINITY,
            collapse_at_top: false,
            ..Self::new(opts, hyph)
        }
    }

    /// A pager for one table cell: unpaged and `measure` wide, but collapsing at its top edge.
    ///
    /// A cell differs from a keep-run in who owns that edge. A keep-run is placed *somewhere* by
    /// its caller, which lifts the leading margin back out so it can collapse against whatever it
    /// lands next to. A cell's top edge is the row's, and the row's is a page-edge candidate — so
    /// the margin collapses here, as it would at the top of a page. Baking it into the cell's line
    /// positions instead would leave two cells with different first-block margins sitting at
    /// different heights, and a row whose halves no longer share a line box is not a parallel text
    /// any more (#251).
    fn cell(opts: &'o LayoutOpts, hyph: &'o dyn Hyphenator, measure: f32) -> Self {
        Self {
            collapse_at_top: true,
            ..Self::unpaged(opts, hyph, measure)
        }
    }

    /// Place a line of `height`, breaking to a new page first if it would overflow a non-empty page.
    /// Run `x` is already content-relative; the line's vertical position is carried by `top`.
    fn emit(&mut self, runs: Vec<PlacedRun>, height: f32, rule: bool) {
        self.flush_gap();
        if self.cursor_y + height > self.page_h && !self.current.is_empty() {
            self.break_page();
        }
        let top = self.cursor_y;
        self.current.push(LayoutLine {
            top,
            height,
            runs,
            rule,
            image: None,
            column_x: 0.0,
            column_w: self.measure,
            keep_with_prev: self.keep_lines && self.kept_one,
        });
        self.kept_one = true;
        self.cursor_y += height;
    }

    /// Ask for a block gap. Never itself forces a page break, and never sums with the gap already
    /// waiting: two adjacent margins collapse to the larger, and the gap is spent by the next line
    /// placed (see [`Self::flush_gap`]) — so one at the top of a page, or trailing at the bottom,
    /// costs nothing. That is browser/crengine margin-collapse behaviour.
    fn gap(&mut self, dy: f32) {
        self.pending_gap = self.pending_gap.max(dy);
    }

    /// Spend the waiting gap, dropping it at the top of a page so the page's own margin is not
    /// doubled.
    fn flush_gap(&mut self) {
        if !self.current.is_empty() || !self.collapse_at_top {
            self.cursor_y += self.pending_gap;
        }
        self.pending_gap = 0.0;
    }

    /// Place a **pre-flowed** run of lines whose `top` values are relative to the flow's own origin,
    /// preserving the gaps between them (#251).
    ///
    /// [`Self::emit`] cannot do this: it derives each line's `top` from the running cursor, so a
    /// flow laid out elsewhere — a table row's cells, each measured to its own width — would lose
    /// every inter-block gap on the way in, which is exactly the "no space between the paragraphs"
    /// half of #251.
    ///
    /// The flow is kept whole when it fits on a page of its own; when it is taller than a page it
    /// splits at a line boundary, and because all of a row's cells are merged into one `top`-ordered
    /// stream before they get here, that split keeps the columns aligned.
    fn emit_flow(&mut self, lines: Flow, total_h: f32) {
        if lines.is_empty() {
            self.cursor_y += total_h;
            return;
        }
        self.flush_gap();
        if !self.current.is_empty()
            && self.cursor_y + total_h > self.page_h
            && total_h <= self.page_h
        {
            self.break_page();
        }
        // Where `top == 0` of the flow sits on the current page. Re-based, not reset, when the flow
        // spills: the lines below the break keep their spacing relative to each other.
        let mut base = self.cursor_y;
        for mut line in lines {
            if base + line.top + line.height > self.page_h && !self.current.is_empty() {
                // A line bound to the one above takes that whole group with it, so a stanza the
                // book asked to keep whole moves rather than being halved (#251). If the group
                // would empty the page it is taller than any page and the request cannot be met —
                // the break falls where it must, because losing text is worse.
                let held = if line.keep_with_prev {
                    self.take_kept_group()
                } else {
                    Vec::new()
                };
                let old_base = base;
                self.break_page();
                base = held.first().map_or(-line.top, |first| old_base - first.top);
                for mut h in held {
                    h.top = base + (h.top - old_base);
                    self.cursor_y = h.top + h.height;
                    self.current.push(h);
                }
            }
            line.top += base;
            self.cursor_y = self.cursor_y.max(line.top + line.height);
            self.current.push(line);
        }
        self.cursor_y = self.cursor_y.max(base + total_h);
    }

    /// Consume an unpaged pager, yielding its segments and the gap still pending at its trailing
    /// edge.
    ///
    /// There is more than one segment only where the book forced a break inside the flow; an
    /// unpaged pager never breaks on overflow, so ordinary content comes back as a single segment.
    /// The trailing gap is handed back rather than folded into the height because it is the *next*
    /// block's business: only there can it collapse against what actually follows, or a page edge.
    fn into_segments(self) -> (Vec<Flow>, f32) {
        let mut segments: Vec<Flow> = self.pages.into_iter().map(|p| p.lines).collect();
        segments.push(self.current);
        (segments, self.pending_gap)
    }

    /// Start a new page because the book asked to (`page-break-before/after: always`), rather than
    /// because the current one filled up (#251).
    ///
    /// A no-op at the top of a page — a forced break must not leave a blank one. An unpaged flow
    /// honours it too: it never breaks on *overflow*, having no budget to overflow, but a break the
    /// book asked for cuts it into segments its caller places with a real page between them. That
    /// is what lets a poem laid out in a table start each canto on a fresh page.
    fn force_break(&mut self) {
        if !self.current.is_empty() {
            self.break_page();
        }
    }

    /// Take back the trailing run of lines the page may not break inside, so the break can fall at
    /// its head instead. Gives back nothing when the group would empty the page: a group taller
    /// than a page cannot be kept whole, and moving it would only loop.
    fn take_kept_group(&mut self) -> Flow {
        let head = self
            .current
            .iter()
            .rposition(|l| !l.keep_with_prev)
            .unwrap_or(0);
        if head == 0 {
            return Vec::new();
        }
        self.current.split_off(head)
    }

    fn break_page(&mut self) {
        self.pages.push(Page {
            lines: std::mem::take(&mut self.current),
        });
        self.cursor_y = 0.0;
        // A margin that would have opened the next page collapses against the page edge.
        self.pending_gap = 0.0;
        // Only an unpaged flow's *first* edge is not a page edge — it is wherever the caller ends
        // up placing the flow, so a margin there is the caller's to collapse. Every edge after a
        // break is a real one, and a margin at it collapses like any other.
        self.collapse_at_top = true;
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

    /// A declared vertical margin in pixels, or inkread's own `default_px` when the book declared
    /// none (#251).
    ///
    /// `font_px` is the *block's* size, not the body's: CSS resolves an `em` against the element's
    /// own font size, so `margin-top: 1em` on an `<h1>` is one h1-em, not one body-em.
    fn resolve_margin(declared: Option<Length>, default_px: f32, font_px: f32) -> f32 {
        declared.map_or(default_px, |l| l.px(font_px))
    }

    /// inkread's own spacing around a block, in pixels, for a book that declared none.
    ///
    /// Book typography (KOReader/crengine `epub.css` model): prose is set dense — a first-line
    /// indent distinguishes a paragraph with NO blank line between (avoiding the "too many white
    /// lines" web look) — while a heading, an illustration and a rule are set apart.
    fn default_margins(&self, block: &Block) -> (f32, f32) {
        let em = self.opts.font_px;
        match block {
            Block::Heading { .. } => (em * 0.7, em * 0.5),
            Block::Paragraph { .. } => (0.0, 0.0),
            Block::ListItem { .. } => (0.0, em * 0.15),
            Block::Image { .. } => (em * 0.4, em * 0.4),
            Block::Row { .. } => (0.0, self.opts.para_gap * 0.5),
            Block::Rule { .. } => (self.opts.para_gap, self.opts.para_gap),
        }
    }

    /// Lay out a table cell's blocks, in keep-runs like a chapter's, so a cell honours the same
    /// break properties as anything else. Every block anchors to the row: a cell is one source
    /// block, and a source anchor taken inside one resolves to it (ADR-INKREAD-0012).
    fn add_cell_blocks(
        &mut self,
        cell: &[Block],
        row: usize,
        cursor: &mut usize,
        m: &dyn Metrics,
        images: &dyn ImageSizer,
    ) {
        let mut k = 0;
        while k < cell.len() {
            let end = keep_run_end(cell, k);
            let laid = self.add_keep_run(&cell[k..end], |_| row, cursor, m, images);
            k += laid.max(1);
        }
    }

    /// Lay out one keep-run: the blocks between two places a page break is allowed (#251).
    ///
    /// `index_of` maps a position in the run to the source block it anchors to — successive blocks
    /// for a chapter, the row itself for every block of a table cell.
    ///
    /// Returns how many of `blocks` it laid out. That is normally all of them, but a run is only
    /// worth holding together while it could still fit a page: past that the `avoid` cannot be
    /// honoured whatever we do, and continuing to accumulate would hold a whole chapter in memory
    /// before emitting a single page — defeating the incremental pagination [`paginate_upto`]
    /// exists for. The caller resumes at the returned offset, budget check and all.
    ///
    /// Most runs are a single block with nothing declared, and take the direct path: a chapter of
    /// prose must still break wherever it fills the page. A run that asked not to be split is
    /// flowed whole first and then placed by [`Self::emit_flow`], which moves it to the next page
    /// when it does not fit here and still fits on a page of its own. A run too tall for any page
    /// is split at a line boundary rather than dropped: honouring `avoid` is a preference, and
    /// losing text is not an acceptable way to keep it.
    fn add_keep_run(
        &mut self,
        blocks: &[Block],
        index_of: impl Fn(usize) -> usize,
        cursor: &mut usize,
        m: &dyn Metrics,
        images: &dyn ImageSizer,
    ) -> usize {
        let (Some(first), Some(last)) = (blocks.first(), blocks.last()) else {
            return 0;
        };
        if first.style().break_before == Some(PageBreak::Always) {
            self.force_break();
        }
        let indivisible = blocks.len() > 1
            || blocks
                .iter()
                .any(|b| b.style().break_inside == Some(PageBreak::Avoid));
        let laid = if indivisible {
            let mut sub = Pager::unpaged(self.opts, self.hyph, self.measure);
            let mut laid = 0;
            for (k, b) in blocks.iter().enumerate() {
                if sub.cursor_y > self.page_h {
                    break;
                }
                sub.add_block(b, index_of(k), cursor, m, images);
                laid = k + 1;
            }
            let (segments, trailing) = sub.into_segments();
            for (i, mut lines) in segments.into_iter().enumerate() {
                if i > 0 {
                    self.force_break();
                }
                // The flow kept the gap at its own top edge (see `Pager::unpaged`). Lift it back
                // out so it collapses against what precedes it on the page, or against a page edge.
                let lead = lines.first().map_or(0.0, |l| l.top);
                for line in &mut lines {
                    line.top -= lead;
                }
                let height = flow_height(&lines);
                self.gap(lead);
                self.emit_flow(lines, height);
            }
            self.gap(trailing);
            laid
        } else {
            self.add_block(first, index_of(0), cursor, m, images);
            1
        };
        if laid == blocks.len() && last.style().break_after == Some(PageBreak::Always) {
            self.force_break();
        }
        laid
    }

    fn add_block(
        &mut self,
        block: &Block,
        block_index: usize,
        cursor: &mut usize,
        m: &dyn Metrics,
        images: &dyn ImageSizer,
    ) {
        let indent = self.opts.font_px * 1.2;
        let style = block.style();
        // The book's own size if it declared one, else inkread's typography for the kind.
        let size = style.font_size.map_or_else(
            || match block {
                Block::Heading { level, .. } => self.opts.font_px * heading_scale(*level),
                _ => self.opts.font_px,
            },
            |l| l.px(self.opts.font_px),
        );
        let (default_top, default_bottom) = self.default_margins(block);
        self.gap(Self::resolve_margin(style.margin_top, default_top, size));
        // `page-break-inside: avoid`, carried down to the lines this block is about to emit.
        // `add_keep_run` covers the case where the block is paged on its own; this covers the case
        // where it is not — merged into a table row, where only the lines survive.
        let outer = (self.keep_lines, self.kept_one);
        self.keep_lines = style.break_inside == Some(PageBreak::Avoid);
        self.kept_one = false;
        match block {
            Block::Heading { content, .. } => {
                // Headings are bold by default, but that is inkread's typography, not a rule: a
                // book that says `font-weight: normal` on its title gets a normal-weight title.
                let bold = style.bold.unwrap_or(true);
                self.add_paragraph(
                    content,
                    size,
                    0.0,
                    0.0,
                    bold,
                    style.italic.unwrap_or(false),
                    effective_align(style, self.opts.align),
                    block_index,
                    cursor,
                    m,
                );
            }
            Block::Paragraph { content, style } => {
                // First line indented, the rest flush left, no trailing gap — dense and book-like.
                // The indent is the *only* thing marking where one paragraph ends and the next
                // begins, since this typography deliberately omits the blank line between them; an
                // indent applied to every line would leave prose with no paragraph breaks at all
                // (#163) and would also spend 1.2em of every line's width on nothing.
                let align = effective_align(style, self.opts.align);
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
                self.add_paragraph(
                    content,
                    self.opts.font_px,
                    first_indent,
                    0.0,
                    style.bold.unwrap_or(false),
                    style.italic.unwrap_or(false),
                    align,
                    block_index,
                    cursor,
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
                self.add_list_item(
                    &marker,
                    content,
                    self.opts.font_px,
                    style.bold.unwrap_or(false),
                    style.italic.unwrap_or(false),
                    block_index,
                    cursor,
                    m,
                );
            }
            Block::Image { src, alt, .. } => {
                if let Some(placed) =
                    fit_image(src, alt, self.measure, self.opts.content_h(), images)
                {
                    // An image occupies one character position, as `<br>` does, so the offsets of
                    // everything after it stay stable whether or not it resolves (ADR-INKREAD-0012).
                    *cursor += 1;
                    self.add_image(placed);
                } else {
                    // Unresolvable (a dangling src, an unreadable codec): fall back to naming what
                    // is missing rather than dropping it silently.
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
                    // The label is synthetic, like a list item's marker: it must not consume
                    // source-character budget, or offsets after an image would depend on whether
                    // it resolved.
                    let at = *cursor;
                    self.add_paragraph(
                        &run,
                        self.opts.font_px,
                        0.0,
                        0.0,
                        false,
                        false,
                        self.opts.align,
                        block_index,
                        cursor,
                        m,
                    );
                    *cursor = at + 1;
                }
            }
            Block::Row { cells, .. } => {
                self.add_row(cells, block_index, cursor, m, images);
            }
            Block::Rule { .. } => self.emit(Vec::new(), self.opts.para_gap.max(2.0), true),
        }
        (self.keep_lines, self.kept_one) = outer;
        self.gap(Self::resolve_margin(
            style.margin_bottom,
            default_bottom,
            size,
        ));
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
        italic_all: bool,
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
            self.measure,
            bold_all,
            italic_all,
            block,
            cursor,
            m,
            self.hyph,
        );
        let line_h = size * self.opts.line_spacing;
        let n = lines.len();
        for (i, mut runs) in lines.into_iter().enumerate() {
            align_line(&mut runs, align, self.measure, i + 1 == n, m);
            self.emit(runs, line_h, false);
        }
    }

    /// Lay out one table row: each cell flowed as a block container in its own share of the
    /// measure, then the cells' lines merged into shared line boxes.
    ///
    /// The cells are flowed independently and then merged by vertical position, so a row is as tall
    /// as its tallest cell and short cells simply leave whitespace beneath them. That is what makes
    /// a parallel text readable across: line *n* of the original and line *n* of the translation
    /// share a line box, however differently the two languages wrap.
    ///
    /// Merging on `top` rather than on line *index* is what lets a cell keep its internal structure
    /// (#251): a cell whose first block is a heading is taller at the top than its neighbour, and
    /// zipping by index would silently pull the translation up to sit beside the wrong line.
    ///
    /// The character cursor advances cell by cell in source order, so a source anchor taken inside
    /// a row still maps back to the character it came from (ADR-INKREAD-0012).
    fn add_row(
        &mut self,
        cells: &[Vec<Block>],
        block: usize,
        cursor: &mut usize,
        m: &dyn Metrics,
        images: &dyn ImageSizer,
    ) {
        if cells.is_empty() {
            return;
        }
        let n = cells.len() as f32;
        let gutter = self.opts.gutter();
        // Cells share the measure equally, the gutters coming out of it. A row of so many cells
        // that none has a usable measure is laid out as a single column instead: unreadably narrow
        // columns are worse than the interleaving this replaced.
        let cell_w = ((self.measure - gutter * (n - 1.0)) / n).max(1.0);
        if cell_w < MIN_CELL_EM * self.opts.font_px {
            for cell in cells {
                self.add_cell_blocks(cell, block, cursor, m, images);
            }
            return;
        }
        // Cells lay out block by block rather than in keep-runs: the row already places each of
        // its segments as one unit, which keeps together everything a run inside a cell could have
        // asked for.
        let mut cells_segments: Vec<Vec<Flow>> = Vec::with_capacity(cells.len());
        let mut trailing_gap = 0.0f32;
        for (index, cell) in cells.iter().enumerate() {
            let mut sub = Pager::cell(self.opts, self.hyph, cell_w);
            sub.add_cell_blocks(cell, block, cursor, m, images);
            let (mut segments, trailing) = sub.into_segments();
            let dx = index as f32 * (cell_w + gutter);
            for line in segments.iter_mut().flatten() {
                for run in &mut line.runs {
                    run.x += dx;
                }
                if let Some(image) = &mut line.image {
                    image.x += dx.round() as i32;
                }
                // A rule or an image inside a cell spans that cell, not the page.
                line.column_x += dx;
            }
            trailing_gap = trailing_gap.max(trailing);
            cells_segments.push(segments);
        }
        // A forced break inside any cell breaks the whole row there, so the cells stay level across
        // it — that correspondence is the only reason a row is laid out side by side at all.
        let stages = row_stages(cells_segments);
        let mut placed = false;
        for lines in stages {
            // Scaled to the text, not a fixed half-pixel: two cells can differ by a default block
            // margin (a list item's, say) and still be the one line the reader sees.
            let lines = merge_cell_flows(lines, self.opts.font_px * 0.25);
            if lines.is_empty() {
                continue;
            }
            if placed {
                self.force_break();
            }
            placed = true;
            let tallest = flow_height(&lines);
            self.emit_flow(lines, tallest);
        }
        self.gap(trailing_gap);
    }

    /// Lay out a list item with a hanging marker and indented body.
    #[allow(clippy::too_many_arguments)]
    fn add_list_item(
        &mut self,
        marker: &str,
        inlines: &[Inline],
        size: f32,
        bold: bool,
        italic: bool,
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
            self.measure,
            bold,
            italic,
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
                    wrap: None,
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
                wrap: None,
            }]);
        }
        let line_h = size * self.opts.line_spacing;
        for runs in lines {
            self.emit(runs, line_h, false);
        }
    }

    /// Emit an illustration as a line of its own.
    fn add_image(&mut self, image: PlacedImage) {
        self.flush_gap();
        let height = image.height as f32;
        if self.cursor_y + height > self.page_h && !self.current.is_empty() {
            self.break_page();
        }
        let top = self.cursor_y;
        self.current.push(LayoutLine {
            top,
            height,
            runs: Vec::new(),
            rule: false,
            image: Some(image),
            column_x: 0.0,
            column_w: self.measure,
            keep_with_prev: self.keep_lines && self.kept_one,
        });
        self.kept_one = true;
        self.cursor_y += height;
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
    italic_all: bool,
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
                            italic: r.italic || italic_all,
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
    italic_all: bool,
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

    for tok in tokenize(inlines, bold_all, italic_all, block, cursor) {
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
                    let push = |cur: &mut Vec<PlacedRun>, txt: String, px: f32, wrap| {
                        cur.push(PlacedRun {
                            x: indent + px,
                            text: txt,
                            size_px: size,
                            bold,
                            italic,
                            href: href.map(str::to_string),
                            anchor,
                            wrap,
                        });
                    };
                    if rest_w <= room {
                        push(&mut cur, rest.to_string(), start, None);
                        x = start + rest_w;
                        break;
                    }
                    // Two ways to split the token mid-word: an unspaced-script (CJK) break at a
                    // UAX #14 opportunity (no hyphen — for a spaceless paragraph this is the only
                    // way it wraps), else a soft-hyphenated Latin break. Same head-and-wrap tail.
                    // Each records which it was, so selection can rejoin the halves (see [`Wrap`]).
                    let split = unspaced_break_fit(rest, room, m, size, bold, italic)
                        .map(|(head, chars)| (head.to_string(), head.len(), chars, Wrap::Kept))
                        .or_else(|| {
                            hyphenate_fit(rest, room, hyph, m, size, bold, italic).map(
                                |(head, chars)| {
                                    // A compound broken at the hyphen it already has needs no
                                    // second one — "well-known" must not print as "well--".
                                    if ends_with_hyphen(head) {
                                        (head.to_string(), head.len(), chars, Wrap::Kept)
                                    } else {
                                        (format!("{head}-"), head.len(), chars, Wrap::SoftHyphen)
                                    }
                                },
                            )
                        });
                    if let Some((fragment, head_len, head_chars, wrap)) = split {
                        push(&mut cur, fragment, start, Some(wrap));
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
                    push(&mut cur, rest.to_string(), 0.0, None);
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

/// Whether `s` already ends in a hyphen, so a break after it needs no second one. ASCII and the
/// Unicode hyphen; the soft hyphen never survives into a laid-out fragment.
fn ends_with_hyphen(s: &str) -> bool {
    s.ends_with(['-', '\u{2010}'])
}

/// The longest prefix of `word` ending at a hyphenation opportunity whose text plus a trailing
/// hyphen fits within `room` px; returns `(head, head_char_count)`, or `None` if none fits. The
/// hyphen is measured even for a prefix that already ends in one (which is placed without a second)
/// — conservative by one hyphen's width, and it keeps every existing pagination unchanged.
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

#[cfg(test)]
#[path = "style_layout_tests.rs"]
mod style_tests;
