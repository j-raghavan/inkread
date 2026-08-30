//! Tests for the EPUB content model (#188/#200/#251), split out to keep `content.rs` nearer the
//! size guideline — the pattern `layout_tests.rs`, `css_tests.rs` and `img_tests.rs` already follow.
//! Included via `#[path]` so `super::*` resolves to the content module.

use super::*;
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
    assert!(matches!(b[1], Block::Rule { .. }));
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
    assert!(matches!(&b[1], Block::Image { src, alt, .. } if src == "a.png" && alt == "pic"));
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
        &body(r#"<h1><i>PRIDE.<br/>and<br/>PREJUDICE</i></h1><p class="c">A decorative line.</p>"#),
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
    assert!(matches!(&b[..], [Block::Paragraph { content, .. }] if run(content) == "just words"));
}

use crate::layout::Align;

fn body(inner: &str) -> String {
    format!("<html><body>{inner}</body></html>")
}

fn text_of(inlines: &[Inline]) -> String {
    inlines
        .iter()
        .map(|i| match i {
            Inline::Run(r) => r.text.as_str(),
            _ => "",
        })
        .collect()
}

/// All the text a cell's blocks carry, joined — the cell as the reader sees it, whatever
/// structure it is built from.
fn blocks_text(blocks: &[Block]) -> String {
    blocks
        .iter()
        .map(|b| match b {
            Block::Heading { content, .. }
            | Block::Paragraph { content, .. }
            | Block::ListItem { content, .. } => text_of(content),
            Block::Row { cells, .. } => cells.iter().map(|c| blocks_text(c)).collect(),
            Block::Image { .. } | Block::Rule { .. } => String::new(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn rows(html: &str) -> Vec<Vec<String>> {
    parse_blocks(html)
        .into_iter()
        .filter_map(|b| match b {
            Block::Row { cells, .. } => Some(cells.iter().map(|c| blocks_text(c)).collect()),
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
    let blocks = parse_blocks("<table><caption>Genesis 1:1</caption><tr><td>a</td></tr></table>");
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

/// #251(1): a heading in a cell was flattened to body text — no level, no bold, no centring.
#[test]
fn a_heading_in_a_cell_stays_a_heading() {
    let b = parse_blocks("<table><tr><td><h3>Canto I</h3><p>line</p></td></tr></table>");
    let Some(Block::Row { cells, .. }) = b.into_iter().next() else {
        panic!("expected a row");
    };
    assert!(
        matches!(cells[0][0], Block::Heading { level: 3, .. }),
        "{:?}",
        cells[0]
    );
    assert!(
        matches!(cells[0][1], Block::Paragraph { .. }),
        "{:?}",
        cells[0]
    );
}

/// #251(2): consecutive blocks in a cell ran together, because a cell held one flat inline run.
#[test]
fn a_cell_keeps_its_blocks_apart() {
    let b = parse_blocks("<table><tr><td><p>one</p><p>two</p></td></tr></table>");
    let Some(Block::Row { cells, .. }) = b.into_iter().next() else {
        panic!("expected a row");
    };
    assert_eq!(
        blocks_text(&cells[0]),
        "one two",
        "two paragraphs, not one run: {:?}",
        cells[0]
    );
    assert_eq!(cells[0].len(), 2, "{:?}", cells[0]);
}

/// Every block kind a cell may contain goes through the ordinary walker, so a list in a cell is
/// a list rather than its items run together.
#[test]
fn a_cell_lowers_lists_and_rules_like_any_container() {
    let b = parse_blocks("<table><tr><td><ul><li>a</li><li>b</li></ul><hr/></td></tr></table>");
    let Some(Block::Row { cells, .. }) = b.into_iter().next() else {
        panic!("expected a row");
    };
    assert!(
        matches!(
            cells[0].as_slice(),
            [
                Block::ListItem { index: 1, .. },
                Block::ListItem { index: 2, .. },
                Block::Rule { .. }
            ]
        ),
        "{:?}",
        cells[0]
    );
}

/// A cell's own declaration was never resolved at all: only the `<tr>` was.
#[test]
fn a_cells_own_declaration_reaches_the_blocks_inside_it() {
    let sheet = Stylesheet::parse("td.verse { text-align: center }");
    let b = parse_blocks_with(
        &body(r#"<table><tr><td class="verse"><p>x</p></td><td><p>y</p></td></tr></table>"#),
        &sheet,
    );
    let Some(Block::Row { cells, .. }) = b.into_iter().next() else {
        panic!("expected a row");
    };
    let align = |c: &Vec<Block>| match &c[0] {
        Block::Paragraph { style, .. } => style.align,
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(align(&cells[0]), Some(Align::Center));
    assert_eq!(align(&cells[1]), None, "the other cell is untouched");
}

/// A table cell is a layout context, so inkread's invented prose indent does not apply inside
/// one — but the book may still ask for it.
#[test]
fn a_cell_drops_the_prose_indent_unless_the_book_asks_for_it() {
    let plain = parse_blocks("<table><tr><td><p>x</p></td></tr></table>");
    let Some(Block::Row { cells, .. }) = plain.into_iter().next() else {
        panic!("expected a row");
    };
    let Block::Paragraph { style, .. } = &cells[0][0] else {
        panic!("expected a paragraph");
    };
    assert_eq!(style.indent, Some(false));

    let sheet = Stylesheet::parse("td p { text-indent: 1.5em }");
    let asked = parse_blocks_with(&body("<table><tr><td><p>x</p></td></tr></table>"), &sheet);
    let Some(Block::Row { cells, .. }) = asked.into_iter().next() else {
        panic!("expected a row");
    };
    let Block::Paragraph { style, .. } = &cells[0][0] else {
        panic!("expected a paragraph");
    };
    assert_eq!(style.indent, Some(true), "the book's declaration wins");
}

/// A container's forced break lands on the edge of the run it wraps, not on every block in it:
/// a `<div class="poem" style="page-break-before: always">` is one break, not one per line.
#[test]
fn a_containers_forced_break_lands_on_the_runs_edges() {
    let sheet = Stylesheet::parse(".poem { page-break-before: always; page-break-after: always }");
    let b = parse_blocks_with(
        &body(r#"<div class="poem"><p>a</p><p>b</p><p>c</p></div>"#),
        &sheet,
    );
    assert_eq!(b.len(), 3);
    let br = |i: usize| {
        let s = b[i].style();
        (s.break_before, s.break_after)
    };
    assert_eq!(br(0).0, Some(PageBreak::Always), "before the first only");
    assert_eq!(br(1).0, None);
    assert_eq!(br(2).0, None);
    assert_eq!(br(2).1, Some(PageBreak::Always), "after the last only");
    assert_eq!(br(0).1, None);
}

/// `page-break-inside: avoid` on a container means "do not break *between* these", which is
/// `page-break-after: avoid` on every block but the last.
#[test]
fn a_containers_avoid_inside_binds_its_blocks_together() {
    let sheet = Stylesheet::parse(".stanza { page-break-inside: avoid }");
    let b = parse_blocks_with(
        &body(r#"<div class="stanza"><p>a</p><p>b</p><p>c</p></div>"#),
        &sheet,
    );
    let after: Vec<_> = b.iter().map(|x| x.style().break_after).collect();
    assert_eq!(
        after,
        vec![Some(PageBreak::Avoid), Some(PageBreak::Avoid), None],
        "the last block is free to be followed by a break",
    );
    assert!(
        b.iter()
            .all(|x| x.style().break_inside == Some(PageBreak::Avoid)),
        "each block also asks not to be halved on its own",
    );
}

/// A block that declared its own break keeps it: a container's request is the weaker one.
#[test]
fn a_blocks_own_break_beats_its_containers() {
    let sheet = Stylesheet::parse(
        ".poem { page-break-before: always } .run-on { page-break-before: auto }",
    );
    let b = parse_blocks_with(
        &body(r#"<div class="poem"><p class="run-on">a</p><p>b</p></div>"#),
        &sheet,
    );
    assert_eq!(
        b[0].style().break_before,
        Some(PageBreak::Auto),
        "the paragraph's own declaration stands",
    );
}

/// Inline emphasis inside a cell is preserved rather than flattened away.
#[test]
fn cells_keep_their_inline_emphasis() {
    let blocks = parse_blocks("<table><tr><td>plain <em>stressed</em></td></tr></table>");
    let Some(Block::Row { cells, .. }) = blocks.into_iter().next() else {
        panic!("expected a row");
    };
    let Some(Block::Paragraph { content, .. }) = cells[0].first() else {
        panic!("expected the cell's loose text to become a paragraph");
    };
    let italics: Vec<bool> = content
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
