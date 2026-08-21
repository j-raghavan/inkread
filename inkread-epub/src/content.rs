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

use crate::css::{BlockStyle, Stylesheet};

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

    let start = find_body(root).unwrap_or(root);
    let mut out = Vec::new();
    let mut pending: Vec<Inline> = Vec::new();
    walk_blocks(start, &mut out, &mut pending, sheet, BlockStyle::default());
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
fn declared(el: &scraper::node::Element, sheet: &Stylesheet, inherited: BlockStyle) -> BlockStyle {
    let style_attr = el.attr("style");
    // Fast path for the overwhelmingly common block: no book CSS and no inline style to apply.
    if sheet.is_empty() && style_attr.is_none() {
        return inherited;
    }
    let own = sheet.resolve(el.name(), el.attr("class"), style_attr);
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
    sheet: &Stylesheet,
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
                                style: declared(el, sheet, inherited),
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
                                style: declared(el, sheet, inherited),
                            });
                        }
                    }
                    "br" => pending.push(Inline::Break),
                    "hr" => {
                        flush_paragraph(out, pending, inherited);
                        out.push(Block::Rule);
                    }
                    "ul" | "ol" => {
                        flush_paragraph(out, pending, inherited);
                        walk_list(
                            child,
                            name == "ol",
                            out,
                            sheet,
                            declared(el, sheet, inherited),
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
                        let inner = declared(el, sheet, inherited);
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

/// Emit each `<li>` of a list as a flattened [`Block::ListItem`]; `inherited` is the list's own
/// declared style, which its items inherit.
fn walk_list(
    node: NodeRef<Node>,
    ordered: bool,
    out: &mut Vec<Block>,
    sheet: &Stylesheet,
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
                        style: declared(el, sheet, inherited),
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
