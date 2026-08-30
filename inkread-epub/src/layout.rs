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
use crate::css::{BlockStyle, Length};

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
fn merge_cell_flows(flows: Vec<Vec<LayoutLine>>) -> Vec<LayoutLine> {
    /// Positions closer than this are the same line box. Cells are measured independently, so two
    /// halves of one line agree to within float noise rather than exactly.
    const SAME_LINE: f32 = 0.5;

    let mut lines: Vec<LayoutLine> = flows.into_iter().flatten().collect();
    // Stable, so cells stay in source order within a line box and `x` runs left to right.
    lines.sort_by(|a, b| a.top.total_cmp(&b.top));

    let mut out: Vec<LayoutLine> = Vec::with_capacity(lines.len());
    for line in lines {
        let plain = !line.rule && line.image.is_none();
        match out.last_mut() {
            Some(prev)
                if plain
                    && !prev.rule
                    && prev.image.is_none()
                    && (line.top - prev.top).abs() < SAME_LINE =>
            {
                prev.height = prev.height.max(line.height);
                prev.column_x = prev.column_x.min(line.column_x);
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
    for (block_index, block) in blocks.iter().enumerate() {
        // Checked before the block, not after: once enough whole pages exist, laying out one more
        // block is work the caller has said it does not need.
        if pager.finished_page_count() >= column_budget {
            complete = false;
            break;
        }
        pager.add_block(block, block_index, &mut cursor, m, images);
    }
    let columns = opts.effective_columns();
    let laid = if complete {
        pager.finish()
    } else {
        pager.into_finished_pages()
    };
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
/// — a narrower measure with no page budget of its own (#251). That is what lets a cell's blocks go
/// through the very same [`Pager::add_block`] dispatch as the chapter's, instead of a second,
/// thinner implementation that would drift from it.
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
        }
    }

    /// A pager for one table cell: `measure` wide, unpaged.
    fn cell(opts: &'o LayoutOpts, hyph: &'o dyn Hyphenator, measure: f32) -> Self {
        Self {
            measure,
            page_h: f32::INFINITY,
            ..Self::new(opts, hyph)
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
        });
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
        if !self.current.is_empty() {
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
    fn emit_flow(&mut self, lines: Vec<LayoutLine>, total_h: f32) {
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
                self.break_page();
                base = -line.top;
            }
            line.top += base;
            self.cursor_y = self.cursor_y.max(line.top + line.height);
            self.current.push(line);
        }
        self.cursor_y = self.cursor_y.max(base + total_h);
    }

    /// Consume a cell pager, yielding its lines and the height they occupy.
    ///
    /// A cell pager has no vertical budget, so nothing was ever broken off into `pages` — the whole
    /// flow is still in `current`, and `cursor_y` is its height including any trailing block gap.
    fn into_flow(self) -> (Vec<LayoutLine>, f32) {
        debug_assert!(self.pages.is_empty(), "a cell flow must not paginate");
        (self.current, self.cursor_y)
    }

    fn break_page(&mut self) {
        self.pages.push(Page {
            lines: std::mem::take(&mut self.current),
        });
        self.cursor_y = 0.0;
        // A margin that would have opened the next page collapses against the page edge.
        self.pending_gap = 0.0;
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

    /// A declared vertical margin in pixels, or `default_em` of inkread's own typography when the
    /// book declared none (#251).
    fn margin(&self, declared: Option<Length>, default_em: f32) -> f32 {
        declared.map_or(self.opts.font_px * default_em, |l| l.px(self.opts.font_px))
    }

    /// Lay out one block. The single place a [`Block`] becomes lines — a chapter's blocks and a
    /// table cell's blocks both come through here, so a heading in a cell is set exactly as a
    /// heading in a chapter is (#251).
    ///
    /// Book typography (KOReader/crengine `epub.css` model): paragraphs are set dense — a first-line
    /// indent of ~1.2em distinguishes them with NO blank line between (avoids the "too many white
    /// lines" web look). Headings are bold + scaled with a margin before (0.7em) and after (0.5em);
    /// the before-margin collapses at the top of a page.
    fn add_block(
        &mut self,
        block: &Block,
        block_index: usize,
        cursor: &mut usize,
        m: &dyn Metrics,
        images: &dyn ImageSizer,
    ) {
        let indent = self.opts.font_px * 1.2;
        match block {
            Block::Heading {
                level,
                content,
                style,
            } => {
                self.gap(self.margin(style.margin_top, 0.7));
                let size = self.opts.font_px * heading_scale(*level);
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
                self.gap(self.margin(style.margin_bottom, 0.5));
            }
            Block::Paragraph { content, style } => {
                // Prose is set dense by default — no gap either side — so a declared margin is the
                // only thing that separates one paragraph from the next by space rather than by
                // indent. That is how a book marks off a stanza (#251).
                self.gap(self.margin(style.margin_top, 0.0));
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
                self.gap(self.margin(style.margin_bottom, 0.0));
            }
            Block::ListItem {
                ordered,
                index,
                content,
                style,
            } => {
                self.gap(self.margin(style.margin_top, 0.0));
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
                self.gap(self.margin(style.margin_bottom, 0.15));
            }
            Block::Image { src, alt } => {
                if let Some(placed) = fit_image(src, alt, self.measure, self.page_h, images) {
                    // An image occupies one character position, as `<br>` does, so the offsets of
                    // everything after it stay stable whether or not it resolves (ADR-INKREAD-0012).
                    *cursor += 1;
                    self.gap(self.opts.font_px * 0.4);
                    self.add_image(placed);
                    self.gap(self.opts.font_px * 0.4);
                    return;
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
                self.gap(self.opts.font_px * 0.4);
                // The label is synthetic, like a list item's marker: it must not consume source-
                // character budget, or offsets after an image would depend on whether it resolved.
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
                self.gap(self.opts.font_px * 0.4);
            }
            Block::Row { cells } => {
                self.add_row(cells, block_index, cursor, m, images);
                self.gap(self.opts.para_gap * 0.5);
            }
            Block::Rule => self.add_rule(self.opts.para_gap),
        }
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
                for b in cell {
                    self.add_block(b, block, cursor, m, images);
                }
            }
            return;
        }
        let mut flows: Vec<Vec<LayoutLine>> = Vec::with_capacity(cells.len());
        let mut tallest = 0.0f32;
        for (index, cell) in cells.iter().enumerate() {
            let mut sub = Pager::cell(self.opts, self.hyph, cell_w);
            for b in cell {
                sub.add_block(b, block, cursor, m, images);
            }
            let (mut lines, height) = sub.into_flow();
            let dx = index as f32 * (cell_w + gutter);
            for line in &mut lines {
                for run in &mut line.runs {
                    run.x += dx;
                }
                if let Some(image) = &mut line.image {
                    image.x += dx.round() as i32;
                }
                // A rule or an image inside a cell spans that cell, not the page.
                line.column_x += dx;
            }
            tallest = tallest.max(height);
            flows.push(lines);
        }
        self.emit_flow(merge_cell_flows(flows), tallest);
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
