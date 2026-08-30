//! Phase 2 — the EPUB **content model** (ADR-INKREAD-0007 / RR2-FR5).
//!
//! Lowers a chapter's XHTML into a linear sequence of owned [`Block`]s carrying [`Inline`] runs —
//! the render-engine-agnostic shape the Phase 3 layout/pagination stage consumes. We keep the
//! *semantic* structure (headings, paragraphs, lists, emphasis, links, images, breaks, rules) and
//! drop presentational noise; a full CSS cascade is a Phase 3 concern (most EPUB body styling is
//! carried by these semantic tags, which suffices for a first reflow).
//!
//! Blocks additionally carry the narrow set of *declared* styles the book asked for (#188) —
//! see [`crate::css`]. That is a lookup, not a cascade: the simplified content model stands.
//!
//! The HTML is parsed with `scraper` (html5ever) into a transient tree that never escapes this
//! function — only the owned `Vec<Block>` (all `String`/`Vec`, so `Send + Sync`) is returned.

use ego_tree::NodeRef;
use scraper::node::Node;
use scraper::{Html, Selector};

use crate::css::{self, BlockStyle, PageBreak, StyledNode, Stylesheet};

/// Inline-level content within a [`Block`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inline {
    /// A run of text carrying accumulated emphasis and an optional hyperlink target.
    Run(TextRun),
    /// An explicit line break (`<br/>`).
    Break,
    /// An inline image (`<img>` inside flowing text).
    Image { src: String, alt: String },
}

/// A run of text with its accumulated emphasis + optional link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextRun {
    /// The visible text (whitespace-collapsed, HTML entities already decoded by the parser).
    pub text: String,
    /// Bold (`<b>`/`<strong>`).
    pub bold: bool,
    /// Italic (`<i>`/`<em>`/`<cite>`).
    pub italic: bool,
    /// Hyperlink target if this run is inside an `<a href>`.
    pub href: Option<String>,
}

/// Block-level content — the linear sequence the layout engine paginates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// A heading (`<h1>`–`<h6>`); `level` is 1–6.
    Heading {
        level: u8,
        content: Vec<Inline>,
        /// What the book's stylesheet declared for this block (#188).
        style: BlockStyle,
    },
    /// A paragraph (`<p>`, or an anonymous block of loose inline content).
    Paragraph {
        content: Vec<Inline>,
        /// What the book's stylesheet declared for this block (#188).
        style: BlockStyle,
    },
    /// A list item flattened out of its `<ul>`/`<ol>`; `ordered` + 1-based `index` drive the marker.
    ListItem {
        ordered: bool,
        index: usize,
        content: Vec<Inline>,
        /// What the book's stylesheet declared for this block (#188).
        style: BlockStyle,
    },
    /// One row of a `<table>`, its cells laid side by side (#200).
    ///
    /// Tables are how parallel texts are built — a bilingual juxtalinear edition puts the original
    /// in one cell and the translation in the next, and reading it means reading *across*. Treated
    /// as an unknown container the grid flattens into document order, which is row-by-row then
    /// cell-by-cell, so the two languages interleave down the page and the correspondence the book
    /// is entirely about is lost.
    ///
    /// Each cell holds *blocks*, not a flat inline run (#251): a cell is a block container like any
    /// other, so a heading inside one is a heading, two paragraphs inside one are two paragraphs,
    /// and every block carries the style the book declared for it.
    Row {
        cells: Vec<Vec<Block>>,
        /// The row's own declared style. Only its non-inherited half is read — a row is spaced and
        /// broken like any block; what it declares for its *text* is inherited by the cells.
        style: BlockStyle,
    },
    /// A standalone (block-level) image.
    Image {
        src: String,
        alt: String,
        style: BlockStyle,
    },
    /// A horizontal rule (`<hr/>`) — a section divider.
    ///
    /// It carries a style for the same reason the others do: `<hr class="pagebreak"/>` styled
    /// `page-break-after: always` is how a great many EPUB2 books start a chapter, and a rule that
    /// could declare nothing could not say it.
    Rule { style: BlockStyle },
}

impl Block {
    /// What the book declared for this block.
    ///
    /// Every block kind carries one. Spacing and page breaks are not typographic niceties that only
    /// text can want: `<hr class="pagebreak"/>`, `table { margin: 1em 0 }` and a plate on its own
    /// page are all ordinary things for a book to ask for, and a block that could declare nothing
    /// could not ask.
    #[must_use]
    pub fn style(&self) -> &BlockStyle {
        match self {
            Block::Heading { style, .. }
            | Block::Paragraph { style, .. }
            | Block::ListItem { style, .. }
            | Block::Row { style, .. }
            | Block::Image { style, .. }
            | Block::Rule { style } => style,
        }
    }

    /// As [`Block::style`], for amending what a container declared onto the blocks it wraps.
    fn style_mut(&mut self) -> &mut BlockStyle {
        match self {
            Block::Heading { style, .. }
            | Block::Paragraph { style, .. }
            | Block::ListItem { style, .. }
            | Block::Row { style, .. }
            | Block::Image { style, .. }
            | Block::Rule { style } => style,
        }
    }
}

/// Accumulated inline emphasis as the walker descends styled spans.
#[derive(Debug, Clone, Copy, Default)]
struct Style {
    bold: bool,
    italic: bool,
}

/// Tags whose *contents* are code/data, never prose. Their text nodes must never reach a [`Block`]:
/// html5ever keeps them in the tree, so without this a `<style>` inside `<body>` (legal in HTML5,
/// and used by a fair number of EPUBs) renders its CSS source as a visible paragraph. `head`/`title`
/// are reachable only when [`find_body`] finds no `<body>` and the walk starts at the document root.
const NON_CONTENT_TAGS: &[&str] = &["style", "script", "template", "head", "title"];

/// Tags that start a new line wherever they are met inside an inline run.
///
/// A `<li>` collects its content as inlines, so `<li><p>one</p><p>two</p></li>` would otherwise
/// render as the single word `onetwo` — the paragraph boundary vanishing along with the block
/// (#251). Emitting a break where a block was keeps the structure the reader can see, without a
/// second block model inside every inline context.
const BLOCK_TAGS: &[&str] = &[
    "p",
    "div",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "li",
    "ul",
    "ol",
    "dl",
    "dt",
    "dd",
    "blockquote",
    "section",
    "article",
    "aside",
    "header",
    "footer",
    "figure",
    "figcaption",
    "pre",
    "address",
    "tr",
    "td",
    "th",
    "table",
];

/// Tags treated as inline emphasis/markup when encountered at block level (folded into the current
/// anonymous paragraph rather than breaking it).
const INLINE_TAGS: &[&str] = &[
    "a", "b", "strong", "i", "em", "cite", "span", "code", "sub", "sup", "small", "u", "mark", "q",
    "abbr", "time", "kbd", "samp", "var", "s", "del", "ins",
];

/// Parse a chapter's XHTML into a linear [`Block`] sequence, with no stylesheet — every block's
/// declared style is empty. See [`parse_blocks_with`].
#[must_use]
pub fn parse_blocks(html: &str) -> Vec<Block> {
    parse_blocks_with(html, &Stylesheet::default())
}

/// Parse a chapter's XHTML against the book's `sheet`. Resolves `<body>` and walks it; loose inline
/// content between block elements becomes anonymous paragraphs. The chapter's own `<style>` blocks
/// are layered over `sheet` (later source wins). Never panics.
#[must_use]
pub fn parse_blocks_with(html: &str, sheet: &Stylesheet) -> Vec<Block> {
    let doc = Html::parse_document(html);
    let root = doc.tree.root();

    // A chapter may carry its own <style>; layer it over the book-wide sheet for this chapter only.
    let in_document = style_text(&doc);
    let merged;
    let sheet = if in_document.trim().is_empty() {
        sheet
    } else {
        let mut s = sheet.clone();
        s.add(&in_document);
        merged = s;
        &merged
    };
    // Parsed once per chapter, not once per block: the selector engine borrows this source, which
    // is why `Stylesheet` holds owned text.
    let parsed = simplecss::StyleSheet::parse(sheet.source());

    let start = find_body(root).unwrap_or(root);
    let mut out = Vec::new();
    let mut pending: Vec<Inline> = Vec::new();
    walk_blocks(
        start,
        &mut out,
        &mut pending,
        &parsed,
        BlockStyle::default(),
    );
    flush_paragraph(&mut out, &mut pending, BlockStyle::default());
    out
}

/// Concatenate the text of every `<style>` element in the document, head or body. Separated by
/// newlines so two blocks cannot run together into one malformed rule.
fn style_text(doc: &Html) -> String {
    // A literal selector that always parses; `unwrap_or_default` keeps the no-panic guarantee
    // without asserting that (RR21-FR3).
    Selector::parse("style")
        .map(|sel| {
            doc.select(&sel)
                .map(|el| el.text().collect::<String>())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Resolve an element's declared style against `sheet`, folding it over what it inherits from its
/// containers. Books routinely declare the *inherited* properties on a wrapping `<div>` rather than
/// on each block inside it.
///
/// Note this is *narrower* than what the selector engine can express: a rule matching a container
/// styles the blocks nested inside it, which is inheritance — the engine resolves descendant
/// selectors such as `div.titlepage p` on its own.
///
/// Whatever `inherited` holds, the result carries only *this* element's `margin` and `page-break-*`
/// — never a container's. The rule is enforced here rather than trusted to every caller, because
/// the fast path below returns without ever reaching [`BlockStyle::overlaid_with`], which is where
/// it would otherwise live.
fn declared(
    node: NodeRef<Node>,
    el: &scraper::node::Element,
    sheet: &simplecss::StyleSheet<'_>,
    inherited: BlockStyle,
) -> BlockStyle {
    let style_attr = el.attr("style");
    // Fast path for the overwhelmingly common block: no book CSS and no inline style to apply.
    if sheet.rules.is_empty() && style_attr.is_none() {
        return inherited.inherited_only();
    }
    let own = css::resolve(sheet, &StyledNode(node), style_attr);
    inherited.inherited_only().overlaid_with(&own)
}

/// Depth-first search for the `<body>` element.
fn find_body<'a>(node: NodeRef<'a, Node>) -> Option<NodeRef<'a, Node>> {
    for child in node.children() {
        if let Node::Element(el) = child.value() {
            if el.name() == "body" {
                return Some(child);
            }
        }
        if let Some(found) = find_body(child) {
            return Some(found);
        }
    }
    None
}

/// Walk block-level structure under `node`, emitting [`Block`]s into `out`. Loose inline content
/// accumulates in `pending` and is flushed as a paragraph when a block boundary is hit. `inherited`
/// carries the declared style of the enclosing containers down to the blocks nested inside them.
fn walk_blocks(
    node: NodeRef<Node>,
    out: &mut Vec<Block>,
    pending: &mut Vec<Inline>,
    sheet: &simplecss::StyleSheet<'_>,
    inherited: BlockStyle,
) {
    for child in node.children() {
        match child.value() {
            Node::Text(t) => push_text(pending, t, Style::default(), None),
            Node::Element(el) => {
                let name = el.name();
                match name {
                    "p" => {
                        flush_paragraph(out, pending, inherited);
                        let content = collect_inlines(child);
                        if !is_blank(&content) {
                            out.push(Block::Paragraph {
                                content,
                                style: declared(child, el, sheet, inherited),
                            });
                        }
                    }
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                        flush_paragraph(out, pending, inherited);
                        let level = name.as_bytes()[1] - b'0';
                        let content = collect_inlines(child);
                        if !is_blank(&content) {
                            out.push(Block::Heading {
                                level,
                                content,
                                style: declared(child, el, sheet, inherited),
                            });
                        }
                    }
                    "br" => pending.push(Inline::Break),
                    "hr" => {
                        flush_paragraph(out, pending, inherited);
                        out.push(Block::Rule {
                            style: declared(child, el, sheet, inherited),
                        });
                    }
                    "table" => {
                        flush_paragraph(out, pending, inherited);
                        let own = declared(child, el, sheet, inherited);
                        let start = out.len();
                        walk_table(child, out, sheet, own.inherited_only());
                        apply_container_style(&mut out[start..], &own);
                    }
                    "ul" | "ol" => {
                        flush_paragraph(out, pending, inherited);
                        let own = declared(child, el, sheet, inherited);
                        let start = out.len();
                        walk_list(child, name == "ol", out, sheet, own.inherited_only());
                        apply_container_style(&mut out[start..], &own);
                    }
                    "img" => {
                        // Standalone (block-level) image.
                        flush_paragraph(out, pending, inherited);
                        if let Some(src) = el.attr("src") {
                            out.push(Block::Image {
                                src: src.to_string(),
                                alt: el.attr("alt").unwrap_or_default().to_string(),
                                style: declared(child, el, sheet, inherited),
                            });
                        }
                    }
                    _ if NON_CONTENT_TAGS.contains(&name) => {}
                    _ if INLINE_TAGS.contains(&name) => {
                        // Inline emphasis at block level → fold into the anonymous paragraph, the
                        // tag's *own* emphasis included: `<em>x</em>` between two paragraphs is
                        // italic, and descending straight into its children would drop that.
                        collect_element(child, el, Style::default(), None, pending);
                    }
                    // Any other element (div/section/blockquote/figure/article/… or unknown) is a
                    // transparent block container: flush, then descend so its content forms its own
                    // blocks rather than merging across the boundary. Its declared style descends
                    // with it — a `<div class="titlepage">` styles the blocks it wraps.
                    _ => {
                        let own = declared(child, el, sheet, inherited);
                        let descends = own.inherited_only();
                        flush_paragraph(out, pending, inherited);
                        let start = out.len();
                        walk_blocks(child, out, pending, sheet, descends);
                        flush_paragraph(out, pending, descends);
                        apply_container_style(&mut out[start..], &own);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Emit each `<tr>` of a table as a [`Block::Row`]; `inherited` is the table's own declared style.
///
/// Section wrappers (`<thead>`/`<tbody>`/`<tfoot>`) are transparent — they group rows without
/// changing them. A `<caption>` is prose about the table and becomes an ordinary paragraph rather
/// than being dropped. Anything else inside a table is ignored: it has no meaning outside a row.
fn walk_table(
    node: NodeRef<Node>,
    out: &mut Vec<Block>,
    sheet: &simplecss::StyleSheet<'_>,
    inherited: BlockStyle,
) {
    for child in node.children() {
        let Node::Element(el) = child.value() else {
            continue;
        };
        match el.name() {
            "thead" | "tbody" | "tfoot" => walk_table(child, out, sheet, inherited),
            "caption" => {
                let content = collect_inlines(child);
                if !is_blank(&content) {
                    out.push(Block::Paragraph {
                        content,
                        style: declared(child, el, sheet, inherited),
                    });
                }
            }
            "tr" => {
                let style = declared(child, el, sheet, inherited);
                let descends = style.inherited_only();
                let cells: Vec<Vec<Block>> = child
                    .children()
                    .filter_map(|c| match c.value() {
                        Node::Element(ce) if matches!(ce.name(), "td" | "th") => {
                            Some(walk_cell(c, sheet, declared(c, ce, sheet, descends)))
                        }
                        _ => None,
                    })
                    .collect();
                // A row of nothing but spacing cells is layout scaffolding, not content.
                if !cells.is_empty() && !cells.iter().all(Vec::is_empty) {
                    out.push(Block::Row {
                        style: hoist_edge_breaks(&cells, style),
                        cells,
                    });
                }
            }
            _ => {}
        }
    }
}

/// Transfer what a container declared for *itself* onto the blocks it wraps (#251).
///
/// `margin` and `page-break-*` do not inherit, but books declare them on the wrapper far more often
/// than on the block — `<div class="poem" style="page-break-before: always">`, `blockquote { margin:
/// 1em 0 }`. Inheriting them would be wrong (one break before the poem, not one before every line
/// of it), so they are *transferred* instead: the request lands on the edge of the run the
/// container produced, which is where a container with no border or padding puts it in CSS too —
/// its margin collapses through to its first and last child.
///
/// `page-break-inside: avoid` becomes `page-break-after: avoid` between the blocks, which is what
/// it means over a run: do not break *between* these. Each block also keeps the request itself,
/// which is what makes a single-block container's `avoid` indivisible — with several blocks the run
/// is already indivisible, and there the per-block copy is inert.
///
/// A block that declared its own value keeps it: a container's request is the weaker one. Every
/// block kind can carry one, so a container whose run begins with an `<hr/>` or a plate is served
/// like any other.
fn apply_container_style(blocks: &mut [Block], container: &BlockStyle) {
    if blocks.is_empty() || container.is_empty() {
        return;
    }
    let last = blocks.len() - 1;
    let inside_avoid = container.break_inside == Some(PageBreak::Avoid);
    for (i, block) in blocks.iter_mut().enumerate() {
        let style = block.style_mut();
        if i == 0 {
            style.break_before = style.break_before.or(container.break_before);
            style.margin_top = style.margin_top.or(container.margin_top);
        }
        if i == last {
            style.break_after = style.break_after.or(container.break_after);
            style.margin_bottom = style.margin_bottom.or(container.margin_bottom);
        }
        if inside_avoid {
            style.break_inside = style.break_inside.or(Some(PageBreak::Avoid));
            if i != last {
                style.break_after = style.break_after.or(Some(PageBreak::Avoid));
            }
        }
    }
}

/// Lower one `<td>`/`<th>` into the blocks it contains (#251).
///
/// A cell is an ordinary block container, so this is [`walk_blocks`] over the cell's children — the
/// same walker that gives headings, paragraphs, lists, rules and images their meaning everywhere
/// else.
///
/// `own` is the cell's own declared style; like any container it passes down the inherited half and
/// transfers the rest onto the run it produced.
///
/// One thing does not carry in: inkread's first-line prose indent. That indent is inkread's
/// typography, not the book's (CSS defaults `text-indent` to zero), and a table cell is a layout
/// context — often only half the measure wide, where an indent on a two-word cell reads as a
/// mistake. A book that genuinely wants indented paragraphs inside a cell still gets them, because
/// its own declaration overrides this default like any other.
fn walk_cell(
    node: NodeRef<Node>,
    sheet: &simplecss::StyleSheet<'_>,
    own: BlockStyle,
) -> Vec<Block> {
    let descends = BlockStyle {
        indent: Some(false),
        ..BlockStyle::default()
    }
    .overlaid_with(&own.inherited_only());
    let mut out = Vec::new();
    let mut pending = Vec::new();
    walk_blocks(node, &mut out, &mut pending, sheet, descends);
    flush_paragraph(&mut out, &mut pending, descends);
    apply_container_style(&mut out, &own);
    out
}

/// Lift a forced break declared at a cell's outer edge onto the row itself (#251).
///
/// A `page-break-before: always` on the first block *inside* a cell has nowhere to go: the cell's
/// own flow has not started, so the break falls at its top edge and collapses, exactly as it would
/// at the top of a page. The break the book asked for is against the row, and only the row can
/// take it. `h3 { page-break-before: always }` on a poem laid out as a table — a canto per page —
/// is the case that needs this, and it is the one the reporter of #251 named.
///
/// Only the outer edges lift. A break *between* two blocks of a cell is the row's business too, but
/// a positional one: the layout cuts the row there, keeping the cells level (see `row_stages`).
fn hoist_edge_breaks(cells: &[Vec<Block>], mut style: BlockStyle) -> BlockStyle {
    let asked = |b: Option<&Block>, pick: fn(&BlockStyle) -> Option<PageBreak>| {
        b.map(Block::style).and_then(pick) == Some(PageBreak::Always)
    };
    if style.break_before.is_none() && cells.iter().any(|c| asked(c.first(), |s| s.break_before)) {
        style.break_before = Some(PageBreak::Always);
    }
    if style.break_after.is_none() && cells.iter().any(|c| asked(c.last(), |s| s.break_after)) {
        style.break_after = Some(PageBreak::Always);
    }
    style
}

/// Emit each `<li>` of a list as a flattened [`Block::ListItem`]; `inherited` is the list's own
/// declared style, which its items inherit.
fn walk_list(
    node: NodeRef<Node>,
    ordered: bool,
    out: &mut Vec<Block>,
    sheet: &simplecss::StyleSheet<'_>,
    inherited: BlockStyle,
) {
    let mut index = 0usize;
    for child in node.children() {
        if let Node::Element(el) = child.value() {
            if el.name() == "li" {
                index += 1;
                let content = collect_inlines(child);
                if !is_blank(&content) {
                    out.push(Block::ListItem {
                        ordered,
                        index,
                        content,
                        style: declared(child, el, sheet, inherited),
                    });
                }
            }
        }
    }
}

/// Collect the inline content of a block element into a fresh `Vec`.
fn collect_inlines(node: NodeRef<Node>) -> Vec<Inline> {
    let mut out = Vec::new();
    collect_inlines_into(node, Style::default(), None, &mut out);
    trim_edges(&mut out);
    out
}

/// Recursively collect inline content under `node`, accumulating emphasis/link state.
fn collect_inlines_into(
    node: NodeRef<Node>,
    style: Style,
    href: Option<&str>,
    out: &mut Vec<Inline>,
) {
    for child in node.children() {
        match child.value() {
            Node::Text(t) => push_text(out, t, style, href),
            Node::Element(el) => collect_element(child, el, style, href, out),
            _ => {}
        }
    }
}

/// Collect one inline element — the emphasis/link/replacement it introduces, then its children.
///
/// Split out from [`collect_inlines_into`] so a caller that is *already looking at* the element can
/// reuse the dispatch. [`walk_blocks`] is that caller: it meets `<em>` at block level and needs the
/// tag's own italic, which descending straight into the tag's children would drop.
fn collect_element(
    node: NodeRef<Node>,
    el: &scraper::node::Element,
    style: Style,
    href: Option<&str>,
    out: &mut Vec<Inline>,
) {
    let name = el.name();
    match name {
        "b" | "strong" => collect_inlines_into(
            node,
            Style {
                bold: true,
                ..style
            },
            href,
            out,
        ),
        "i" | "em" | "cite" => collect_inlines_into(
            node,
            Style {
                italic: true,
                ..style
            },
            href,
            out,
        ),
        "a" => {
            let nested = el.attr("href").or(href);
            collect_inlines_into(node, style, nested, out);
        }
        "br" => out.push(Inline::Break),
        "img" => {
            if let Some(src) = el.attr("src") {
                out.push(Inline::Image {
                    src: src.to_string(),
                    alt: el.attr("alt").unwrap_or_default().to_string(),
                });
            }
        }
        _ if NON_CONTENT_TAGS.contains(&name) => {}
        // A block met inside an inline run: keep the line break it stands for, so two paragraphs
        // in a list item do not fuse into one word.
        _ if BLOCK_TAGS.contains(&name) => {
            push_block_break(out);
            collect_inlines_into(node, style, href, out);
            push_block_break(out);
        }
        // span/code/sub/sup/… and any unknown inline wrapper: descend, keep style.
        _ => collect_inlines_into(node, style, href, out),
    }
}

/// A line break standing for a block boundary — never leading, never doubled, so an empty wrapper
/// or a run of nested blocks costs no blank lines.
fn push_block_break(out: &mut Vec<Inline>) {
    if out.is_empty() || matches!(out.last(), Some(Inline::Break)) {
        return;
    }
    if let Some(Inline::Run(r)) = out.last() {
        if r.text.trim().is_empty() && out.len() == 1 {
            return;
        }
    }
    out.push(Inline::Break);
}

/// Append a whitespace-collapsed text run, merging into the previous run when its style/link match.
fn push_text(out: &mut Vec<Inline>, raw: &str, style: Style, href: Option<&str>) {
    let text = collapse_ws(raw);
    if text.is_empty() {
        return;
    }
    // Merge adjacent same-styled runs so emphasis spans don't fragment the text needlessly.
    if let Some(Inline::Run(prev)) = out.last_mut() {
        if prev.bold == style.bold && prev.italic == style.italic && prev.href.as_deref() == href {
            prev.text.push_str(&text);
            return;
        }
    }
    out.push(Inline::Run(TextRun {
        text,
        bold: style.bold,
        italic: style.italic,
        href: href.map(str::to_string),
    }));
}

/// Flush the pending anonymous-block inlines as a paragraph (if non-blank), clearing the buffer.
/// An anonymous block has no element of its own, so it takes the enclosing container's `style`.
fn flush_paragraph(out: &mut Vec<Block>, pending: &mut Vec<Inline>, style: BlockStyle) {
    if pending.is_empty() {
        return;
    }
    trim_edges(pending);
    if !is_blank(pending) {
        out.push(Block::Paragraph {
            content: std::mem::take(pending),
            style,
        });
    } else {
        pending.clear();
    }
}

/// Collapse any run of ASCII/Unicode whitespace (incl. newlines) into a single space — HTML's
/// normal whitespace handling. Leading/trailing edges are trimmed per-block in [`trim_edges`].
fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out
}

/// Trim the leading space of the first run and the trailing space of the last run of a block.
fn trim_edges(inlines: &mut [Inline]) {
    if let Some(Inline::Run(first)) = inlines.first_mut() {
        let trimmed = first.text.trim_start().to_string();
        first.text = trimmed;
    }
    if let Some(Inline::Run(last)) = inlines.last_mut() {
        let trimmed = last.text.trim_end().to_string();
        last.text = trimmed;
    }
}

/// True when a block carries no visible text and no image/break (pure whitespace).
fn is_blank(inlines: &[Inline]) -> bool {
    inlines.iter().all(|i| match i {
        Inline::Run(r) => r.text.trim().is_empty(),
        Inline::Break => true,
        Inline::Image { .. } => false,
    })
}

#[cfg(test)]
#[path = "content_tests.rs"]
mod tests;
