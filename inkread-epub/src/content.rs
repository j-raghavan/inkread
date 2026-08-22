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

use crate::css::{self, BlockStyle, StyledNode, Stylesheet};

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
    Row {
        cells: Vec<Vec<Inline>>,
        style: BlockStyle,
    },
    /// A standalone (block-level) image.
    Image { src: String, alt: String },
    /// A horizontal rule (`<hr/>`) — a section divider.
    Rule,
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
/// containers. All three properties in [`BlockStyle`] are inherited ones in CSS, and books routinely
/// declare them on a wrapping `<div>` rather than on each block inside it.
///
/// Note this is *narrower* than what the selector engine can express: a rule matching a container
/// styles the blocks nested inside it, which is inheritance — the engine resolves descendant
/// selectors such as `div.titlepage p` on its own.
fn declared(
    node: NodeRef<Node>,
    el: &scraper::node::Element,
    sheet: &simplecss::StyleSheet<'_>,
    inherited: BlockStyle,
) -> BlockStyle {
    let style_attr = el.attr("style");
    // Fast path for the overwhelmingly common block: no book CSS and no inline style to apply.
    if sheet.rules.is_empty() && style_attr.is_none() {
        return inherited;
    }
    let own = css::resolve(sheet, &StyledNode(node), style_attr);
    inherited.overlaid_with(&own)
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
                        out.push(Block::Rule);
                    }
                    "table" => {
                        flush_paragraph(out, pending, inherited);
                        walk_table(child, out, sheet, declared(child, el, sheet, inherited));
                    }
                    "ul" | "ol" => {
                        flush_paragraph(out, pending, inherited);
                        walk_list(
                            child,
                            name == "ol",
                            out,
                            sheet,
                            declared(child, el, sheet, inherited),
                        );
                    }
                    "img" => {
                        // Standalone (block-level) image.
                        flush_paragraph(out, pending, inherited);
                        if let Some(src) = el.attr("src") {
                            out.push(Block::Image {
                                src: src.to_string(),
                                alt: el.attr("alt").unwrap_or_default().to_string(),
                            });
                        }
                    }
                    _ if NON_CONTENT_TAGS.contains(&name) => {}
                    _ if INLINE_TAGS.contains(&name) => {
                        // Inline emphasis at block level → fold into the anonymous paragraph.
                        collect_inlines_into(child, Style::default(), None, pending);
                    }
                    // Any other element (div/section/blockquote/figure/article/… or unknown) is a
                    // transparent block container: flush, then descend so its content forms its own
                    // blocks rather than merging across the boundary. Its declared style descends
                    // with it — a `<div class="titlepage">` styles the blocks it wraps.
                    _ => {
                        let inner = declared(child, el, sheet, inherited);
                        flush_paragraph(out, pending, inherited);
                        walk_blocks(child, out, pending, sheet, inner);
                        flush_paragraph(out, pending, inner);
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
                let cells: Vec<Vec<Inline>> = child
                    .children()
                    .filter_map(|c| match c.value() {
                        Node::Element(ce) if matches!(ce.name(), "td" | "th") => {
                            Some(collect_inlines(c))
                        }
                        _ => None,
                    })
                    .collect();
                // A row of nothing but spacing cells is layout scaffolding, not content.
                if !cells.is_empty() && !cells.iter().all(|c| is_blank(c)) {
                    out.push(Block::Row {
                        cells,
                        style: declared(child, el, sheet, inherited),
                    });
                }
            }
            _ => {}
        }
    }
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
            Node::Element(el) => {
                let name = el.name();
                match name {
                    "b" | "strong" => collect_inlines_into(
                        child,
                        Style {
                            bold: true,
                            ..style
                        },
                        href,
                        out,
                    ),
                    "i" | "em" | "cite" => collect_inlines_into(
                        child,
                        Style {
                            italic: true,
                            ..style
                        },
                        href,
                        out,
                    ),
                    "a" => {
                        let nested = el.attr("href").or(href);
                        collect_inlines_into(child, style, nested, out);
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
                    // span/code/sub/sup/… and any unknown inline wrapper: descend, keep style.
                    _ => collect_inlines_into(child, style, href, out),
                }
            }
            _ => {}
        }
    }
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
mod tests {
    use super::*;
    use crate::layout::Align;

    fn body(inner: &str) -> String {
        format!("<html><body>{inner}</body></html>")
    }

    fn run(inlines: &[Inline]) -> String {
        inlines
            .iter()
            .map(|i| match i {
                Inline::Run(r) => r.text.clone(),
                Inline::Break => "\\n".into(),
                Inline::Image { alt, .. } => format!("[img:{alt}]"),
            })
            .collect()
    }

    #[test]
    fn headings_and_paragraphs() {
        let b = parse_blocks(&body("<h2>Title</h2><p>Hello world.</p>"));
        assert_eq!(b.len(), 2);
        assert!(matches!(b[0], Block::Heading { level: 2, .. }));
        match &b[1] {
            Block::Paragraph { content, .. } => assert_eq!(run(content), "Hello world."),
            _ => panic!("expected paragraph"),
        }
    }

    #[test]
    fn emphasis_and_links_carry_style() {
        let b = parse_blocks(&body(
            r#"<p>plain <b>bold</b> <i>it</i> <a href="x.html">link</a></p>"#,
        ));
        let Block::Paragraph { content, .. } = &b[0] else {
            panic!("paragraph")
        };
        // runs: "plain ", "bold", " ", "it", " ", "link"
        let bold = content
            .iter()
            .any(|i| matches!(i, Inline::Run(r) if r.text == "bold" && r.bold && !r.italic));
        let ital = content
            .iter()
            .any(|i| matches!(i, Inline::Run(r) if r.text == "it" && r.italic));
        let link = content.iter().any(
            |i| matches!(i, Inline::Run(r) if r.text == "link" && r.href.as_deref() == Some("x.html")),
        );
        assert!(bold && ital && link, "{content:?}");
    }

    #[test]
    fn whitespace_is_collapsed_and_edges_trimmed() {
        let b = parse_blocks(&body("<p>  lots\n   of   space  </p>"));
        let Block::Paragraph { content, .. } = &b[0] else {
            panic!()
        };
        assert_eq!(run(content), "lots of space");
    }

    #[test]
    fn lists_flatten_to_indexed_items() {
        let b = parse_blocks(&body("<ol><li>one</li><li>two</li></ol>"));
        assert_eq!(b.len(), 2);
        assert!(matches!(
            b[0],
            Block::ListItem {
                ordered: true,
                index: 1,
                ..
            }
        ));
        assert!(matches!(b[1], Block::ListItem { index: 2, .. }));
    }

    #[test]
    fn br_becomes_break_and_hr_becomes_rule() {
        let b = parse_blocks(&body("<p>a<br/>b</p><hr/>"));
        let Block::Paragraph { content, .. } = &b[0] else {
            panic!()
        };
        assert!(content.iter().any(|i| matches!(i, Inline::Break)));
        assert!(matches!(b[1], Block::Rule));
    }

    #[test]
    fn loose_inline_text_becomes_anonymous_paragraph() {
        let b = parse_blocks(&body("Just loose text with <em>emphasis</em>."));
        assert_eq!(b.len(), 1);
        let Block::Paragraph { content, .. } = &b[0] else {
            panic!("expected anonymous paragraph")
        };
        assert_eq!(run(content), "Just loose text with emphasis.");
    }

    #[test]
    fn entities_are_decoded_and_block_image_extracted() {
        let b = parse_blocks(&body(
            r#"<p>Tom &amp; Jerry</p><img src="a.png" alt="pic"/>"#,
        ));
        let Block::Paragraph { content, .. } = &b[0] else {
            panic!()
        };
        assert_eq!(run(content), "Tom & Jerry");
        assert!(matches!(&b[1], Block::Image { src, alt } if src == "a.png" && alt == "pic"));
    }

    #[test]
    fn divs_do_not_merge_across_block_boundaries() {
        let b = parse_blocks(&body("<div>first</div><div>second</div>"));
        assert_eq!(b.len(), 2, "{b:?}");
        assert!(matches!(&b[0], Block::Paragraph { content, .. } if run(content) == "first"));
        assert!(matches!(&b[1], Block::Paragraph { content, .. } if run(content) == "second"));
    }

    #[test]
    fn style_and_script_contents_never_become_visible_text() {
        // html5ever keeps <style>/<script> in the body tree, so a transparent-container walk would
        // emit their source as prose — CSS printed mid-chapter.
        let b = parse_blocks(&body(
            "<style>p { color: red }</style>\
             <script>var x = 1;</script>\
             <p>Real text.</p>",
        ));
        assert!(
            matches!(&b[..], [Block::Paragraph { content, .. }] if run(content) == "Real text."),
            "{b:?}"
        );
        // Nested inside a paragraph, too.
        let n = parse_blocks(&body("<p>Before<script>var y = 2;</script> after.</p>"));
        let Block::Paragraph { content, .. } = &n[0] else {
            panic!()
        };
        assert_eq!(run(content), "Before after.");
    }

    /// #188: the reported failure — Project Gutenberg's *Pride and Prejudice* title page rendered
    /// as left-aligned bold fragments because the book's stylesheet was never consulted.
    #[test]
    fn a_declared_style_reaches_the_block_it_applies_to() {
        let sheet = Stylesheet::parse(
            "h1 { text-align: center; font-weight: normal } .c { text-align: center; text-indent: 0% }",
        );
        let b = parse_blocks_with(
            &body(
                r#"<h1><i>PRIDE.<br/>and<br/>PREJUDICE</i></h1><p class="c">A decorative line.</p>"#,
            ),
            &sheet,
        );
        let Block::Heading { style, .. } = &b[0] else {
            panic!("expected heading, got {b:?}")
        };
        assert_eq!(style.align, Some(Align::Center));
        assert_eq!(style.bold, Some(false));

        let Block::Paragraph { style, .. } = &b[1] else {
            panic!()
        };
        assert_eq!(style.align, Some(Align::Center));
        assert_eq!(style.indent, Some(false));
    }

    /// #201: `div.titlepage p` is ordinary Sigil/InDesign/calibre output. The previous hand-rolled
    /// subset dropped every selector that reached past one element, so books written that way still
    /// showed #188's symptom after it was "fixed".
    #[test]
    fn a_descendant_selector_reaches_the_block_it_matches() {
        let sheet = Stylesheet::parse("div.titlepage p { text-align: center; text-indent: 0 }");
        let b = parse_blocks_with(
            &body(r#"<div class="titlepage"><p>Title</p></div><p>Ordinary prose.</p>"#),
            &sheet,
        );
        let Block::Paragraph { style, .. } = &b[0] else {
            panic!("expected the title paragraph, got {b:?}")
        };
        assert_eq!(style.align, Some(Align::Center));
        assert_eq!(style.indent, Some(false));

        // …and the prose outside the container is untouched by it.
        let Block::Paragraph { style, .. } = &b[1] else {
            panic!()
        };
        assert!(style.is_empty(), "{style:?}");
    }

    #[test]
    fn a_container_declaration_is_inherited_by_the_blocks_it_wraps() {
        // All three properties inherit in CSS, and books routinely style a wrapping <div> rather
        // than each block inside it.
        let sheet = Stylesheet::parse(".titlepage { text-align: center; text-indent: 0 }");
        let b = parse_blocks_with(
            &body(r#"<div class="titlepage"><p>Title</p>loose text<ul><li>item</li></ul></div>"#),
            &sheet,
        );
        for block in &b {
            let style = match block {
                Block::Paragraph { style, .. }
                | Block::Heading { style, .. }
                | Block::ListItem { style, .. } => style,
                other => panic!("unexpected {other:?}"),
            };
            assert_eq!(style.align, Some(Align::Center), "{block:?}");
            assert_eq!(style.indent, Some(false), "{block:?}");
        }
        assert_eq!(b.len(), 3, "paragraph, anonymous block, list item: {b:?}");
    }

    #[test]
    fn a_blocks_own_declaration_overrides_what_it_inherits() {
        let sheet = Stylesheet::parse(".mid { text-align: center } p { text-align: right }");
        let b = parse_blocks_with(&body(r#"<div class="mid"><p>x</p></div>"#), &sheet);
        let Block::Paragraph { style, .. } = &b[0] else {
            panic!()
        };
        assert_eq!(
            style.align,
            Some(Align::Right),
            "own rule beats the inherited one"
        );
    }

    #[test]
    fn an_in_document_style_block_layers_over_the_book_stylesheet() {
        let book = Stylesheet::parse("p { text-align: left }");
        let b = parse_blocks_with(
            "<html><head><style>p { text-align: center }</style></head><body><p>x</p></body></html>",
            &book,
        );
        let Block::Paragraph { style, .. } = &b[0] else {
            panic!()
        };
        assert_eq!(
            style.align,
            Some(Align::Center),
            "the chapter's own <style> wins"
        );
    }

    #[test]
    fn without_a_stylesheet_every_block_declares_nothing() {
        // parse_blocks stays the unstyled path; a book with no CSS must be byte-for-byte as before.
        let b = parse_blocks(&body(
            r#"<h1 class="c">T</h1><p style="text-align: center">x</p>"#,
        ));
        let Block::Heading { style, .. } = &b[0] else {
            panic!()
        };
        assert!(style.is_empty(), "no sheet, no class lookup: {style:?}");
        // …but an inline style= attribute is carried by the element itself, so it still applies.
        let Block::Paragraph { style, .. } = &b[1] else {
            panic!()
        };
        assert_eq!(style.align, Some(Align::Center));
    }

    #[test]
    fn empty_body_is_empty_and_bare_text_does_not_panic() {
        assert!(parse_blocks("<html><body></body></html>").is_empty());
        // Bare text gets wrapped in an implicit body by the parser → one anonymous paragraph.
        let b = parse_blocks("just words");
        assert!(
            matches!(&b[..], [Block::Paragraph { content, .. }] if run(content) == "just words")
        );
    }
}

#[cfg(test)]
mod table_tests {
    use super::*;

    fn text_of(inlines: &[Inline]) -> String {
        inlines
            .iter()
            .map(|i| match i {
                Inline::Run(r) => r.text.as_str(),
                _ => "",
            })
            .collect()
    }

    fn rows(html: &str) -> Vec<Vec<String>> {
        parse_blocks(html)
            .into_iter()
            .filter_map(|b| match b {
                Block::Row { cells, .. } => Some(cells.iter().map(|c| text_of(c)).collect()),
                _ => None,
            })
            .collect()
    }

    /// The case this exists for: a bilingual juxtalinear text, read across rather than down.
    #[test]
    fn a_parallel_text_keeps_its_pairs_together() {
        let html = "<table>\
             <tr><td>Au commencement</td><td>In the beginning</td></tr>\
             <tr><td>la terre etait vide</td><td>the earth was empty</td></tr>\
             </table>";
        assert_eq!(
            rows(html),
            vec![
                vec![
                    "Au commencement".to_string(),
                    "In the beginning".to_string()
                ],
                vec![
                    "la terre etait vide".to_string(),
                    "the earth was empty".to_string()
                ],
            ],
        );
    }

    /// `<thead>`/`<tbody>`/`<tfoot>` group rows without changing them.
    #[test]
    fn section_wrappers_are_transparent() {
        let html = "<table>\
             <thead><tr><th>Source</th><th>Gloss</th></tr></thead>\
             <tbody><tr><td>lupus</td><td>wolf</td></tr></tbody>\
             <tfoot><tr><td>fin</td><td>end</td></tr></tfoot>\
             </table>";
        assert_eq!(
            rows(html),
            vec![
                vec!["Source".to_string(), "Gloss".to_string()],
                vec!["lupus".to_string(), "wolf".to_string()],
                vec!["fin".to_string(), "end".to_string()],
            ],
        );
    }

    /// A caption is prose about the table; dropping it would lose text the author wrote.
    #[test]
    fn a_caption_survives_as_a_paragraph() {
        let blocks =
            parse_blocks("<table><caption>Genesis 1:1</caption><tr><td>a</td></tr></table>");
        let captions: Vec<String> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph { content, .. } => Some(text_of(content)),
                _ => None,
            })
            .collect();
        assert_eq!(captions, vec!["Genesis 1:1".to_string()]);
    }

    /// Spacer rows are layout scaffolding, and an empty row would take a line of the page.
    #[test]
    fn rows_with_no_content_are_dropped() {
        assert!(rows("<table><tr><td> </td><td></td></tr></table>").is_empty());
    }

    /// A single-cell table is how a lot of EPUB2 does plain layout; it must behave like prose.
    #[test]
    fn a_one_cell_row_is_still_a_row_of_one() {
        assert_eq!(
            rows("<table><tr><td>just text</td></tr></table>"),
            vec![vec!["just text".to_string()]]
        );
    }

    /// Inline emphasis inside a cell is preserved rather than flattened away.
    #[test]
    fn cells_keep_their_inline_emphasis() {
        let blocks = parse_blocks("<table><tr><td>plain <em>stressed</em></td></tr></table>");
        let Some(Block::Row { cells, .. }) = blocks.into_iter().next() else {
            panic!("expected a row");
        };
        let italics: Vec<bool> = cells[0]
            .iter()
            .filter_map(|i| match i {
                Inline::Run(r) => Some(r.italic),
                _ => None,
            })
            .collect();
        assert!(
            italics.contains(&true),
            "the <em> run should still be italic"
        );
    }
}
