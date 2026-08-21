//! Tests for the CSS subset (#188/#201), split out to keep `css.rs` nearer the size guideline.
//! Included via `#[path]` so `super::*` resolves to the css module.

use super::*;
use scraper::Html;

/// Resolve the declared style of the element carrying `data-t` in `html`, against `css`.
/// Going through a real parsed document is the point: matching now sees the whole tree.
fn styled(css: &str, html: &str) -> BlockStyle {
    let owned = Stylesheet::parse(css);
    let sheet = simplecss::StyleSheet::parse(owned.source());
    let doc = Html::parse_document(html);
    let node = target(doc.tree.root()).expect("no element carries data-t");
    let style_attr = node.value().as_element().and_then(|el| el.attr("style"));
    resolve(&sheet, &StyledNode(node), style_attr)
}

fn target(node: NodeRef<'_, Node>) -> Option<NodeRef<'_, Node>> {
    for child in node.children() {
        if child
            .value()
            .as_element()
            .is_some_and(|el| el.attr("data-t").is_some())
        {
            return Some(child);
        }
        if let Some(found) = target(child) {
            return Some(found);
        }
    }
    None
}

/// The stylesheet from #188 — Project Gutenberg's *Pride and Prejudice* (#1342), the book whose
/// title page rendered as left-aligned bold fragments.
const PG_CSS: &str = r"
    h1        { margin-top: 5%; text-align: center; clear: both; font-weight: normal }
    .c        { text-align: center; text-indent: 0% }
    .cbig250  { text-align: center; text-indent: 0%; font-weight: normal; font-size: 200% }
";

#[test]
fn the_pride_and_prejudice_title_resolves_centred_unbolded_and_unindented() {
    let h1 = styled(PG_CSS, "<body><h1 data-t>PRIDE</h1></body>");
    assert_eq!(h1.align, Some(Align::Center), "text-align was dropped");
    assert_eq!(h1.bold, Some(false), "font-weight: normal was dropped");

    let c = styled(PG_CSS, r#"<body><p class="c" data-t>x</p></body>"#);
    assert_eq!(c.align, Some(Align::Center));
    assert_eq!(c.indent, Some(false), "text-indent: 0% was dropped");
}

/// #201: the selectors the hand-rolled subset dropped. `div.titlepage p` is ordinary Sigil,
/// InDesign and calibre output, so these are not an exotic tail.
#[test]
fn selectors_beyond_a_single_element_now_resolve() {
    let cases: [(&str, &str); 5] = [
        (
            "div.titlepage p { text-align: center }",
            r#"<body><div class="titlepage"><p data-t>x</p></div></body>"#,
        ),
        (
            "div > p { text-align: center }",
            "<body><div><p data-t>x</p></div></body>",
        ),
        (
            "#title { text-align: center }",
            r#"<body><p id="title" data-t>x</p></body>"#,
        ),
        (
            "p[lang] { text-align: center }",
            r#"<body><p lang="fr" data-t>x</p></body>"#,
        ),
        (
            "p:first-child { text-align: center }",
            "<body><div><p data-t>x</p><p>y</p></div></body>",
        ),
    ];
    for (css, html) in cases {
        assert_eq!(
            styled(css, html).align,
            Some(Align::Center),
            "selector dropped: {css}"
        );
    }
    // …and they still discriminate: the same rules must not match the wrong element.
    assert_eq!(
        styled(
            "div.titlepage p { text-align: center }",
            r#"<body><div class="other"><p data-t>x</p></div></body>"#
        )
        .align,
        None
    );
    assert_eq!(
        styled(
            "p:first-child { text-align: center }",
            "<body><div><p>y</p><p data-t>x</p></div></body>"
        )
        .align,
        None
    );
}

#[test]
fn class_beats_element_and_later_beats_earlier_at_equal_specificity() {
    let css = "p { text-align: left } .mid { text-align: center } p { font-weight: bold }";
    assert_eq!(
        styled(css, r#"<body><p class="mid" data-t>x</p></body>"#).align,
        Some(Align::Center)
    );
    let plain = styled(css, "<body><p data-t>x</p></body>");
    assert_eq!(plain.align, Some(Align::Left));
    assert_eq!(plain.bold, Some(true));

    assert_eq!(
        styled(
            "p { text-align: left } p { text-align: right }",
            "<body><p data-t>x</p></body>"
        )
        .align,
        Some(Align::Right)
    );
}

#[test]
fn a_tag_qualified_class_outranks_a_bare_class() {
    let css = ".t { text-align: center } h1.t { text-align: right }";
    assert_eq!(
        styled(css, r#"<body><h1 class="t" data-t>x</h1></body>"#).align,
        Some(Align::Right)
    );
    assert_eq!(
        styled(css, r#"<body><p class="t" data-t>x</p></body>"#).align,
        Some(Align::Center)
    );
}

#[test]
fn an_inline_style_attribute_outranks_every_rule() {
    let s = styled(
        "p { text-align: left }",
        r#"<body><p style="text-align: center; text-indent: 0" data-t>x</p></body>"#,
    );
    assert_eq!(s.align, Some(Align::Center));
    assert_eq!(s.indent, Some(false));
}

#[test]
fn a_class_matches_one_of_several_including_across_newlines() {
    let css = ".c { text-align: center }";
    assert_eq!(
        styled(css, r#"<body><p class="first c last" data-t>x</p></body>"#).align,
        Some(Align::Center)
    );
    // A prettified class list is whitespace, not necessarily single spaces.
    assert_eq!(
        styled(css, "<body><p class=\"first\n  c\" data-t>x</p></body>").align,
        Some(Align::Center)
    );
    assert_eq!(
        styled(css, r#"<body><p class="cc" data-t>x</p></body>"#).align,
        None,
        "no prefix match"
    );
}

#[test]
fn at_rules_do_not_lose_the_rules_around_them() {
    let sheet = r#"@charset "utf-8";
                   @import url(other.css);
                   h1 { text-align: center }
                   @font-face { font-family: x; src: url(x.ttf) }
                   p { text-indent: 0 }"#;
    assert_eq!(
        styled(sheet, "<body><h1 data-t>x</h1></body>").align,
        Some(Align::Center)
    );
    assert_eq!(
        styled(sheet, "<body><p data-t>x</p></body>").indent,
        Some(false),
        "rule after @font-face lost"
    );
}

#[test]
fn comments_are_stripped() {
    assert_eq!(
        styled(
            "/* c */ h1 { text-align: /* mid */ center }",
            "<body><h1 data-t>x</h1></body>"
        )
        .align,
        Some(Align::Center)
    );
}

#[test]
fn text_indent_is_a_zero_opt_out_not_a_measurement() {
    for zero in ["0", "0em", "0%", "0.0px", "+0em", "-0em"] {
        let css = format!("p {{ text-indent: {zero} }}");
        assert_eq!(
            styled(&css, "<body><p data-t>x</p></body>").indent,
            Some(false),
            "{zero}"
        );
    }
    for nonzero in ["1.5em", "5%", "12px", "inherit"] {
        let css = format!("p {{ text-indent: {nonzero} }}");
        assert_eq!(
            styled(&css, "<body><p data-t>x</p></body>").indent,
            Some(true),
            "{nonzero}"
        );
    }
}

#[test]
fn font_weight_splits_at_600_like_css() {
    for normal in ["normal", "lighter", "100", "400", "500"] {
        let css = format!("h1 {{ font-weight: {normal} }}");
        assert_eq!(
            styled(&css, "<body><h1 data-t>x</h1></body>").bold,
            Some(false),
            "{normal}"
        );
    }
    for bold in ["bold", "bolder", "600", "700", "900"] {
        let css = format!("h1 {{ font-weight: {bold} }}");
        assert_eq!(
            styled(&css, "<body><h1 data-t>x</h1></body>").bold,
            Some(true),
            "{bold}"
        );
    }
    assert_eq!(
        styled("h1 { font-weight: wat }", "<body><h1 data-t>x</h1></body>").bold,
        None,
        "an unparseable weight declares nothing"
    );
}

#[test]
fn important_is_stripped_and_alignment_keywords_map() {
    assert_eq!(
        styled(
            "p { text-align: center !important }",
            "<body><p data-t>x</p></body>"
        )
        .align,
        Some(Align::Center)
    );
    for (css, want) in [
        ("left", Align::Left),
        ("start", Align::Left),
        ("right", Align::Right),
        ("end", Align::Right),
        ("justify", Align::Justify),
    ] {
        let rule = format!("p {{ text-align: {css} }}");
        assert_eq!(
            styled(&rule, "<body><p data-t>x</p></body>").align,
            Some(want),
            "{css}"
        );
    }
}

#[test]
fn selectors_and_properties_are_case_insensitive() {
    assert_eq!(
        styled(
            "H1 { TEXT-ALIGN: CENTER }",
            "<body><h1 data-t>x</h1></body>"
        )
        .align,
        Some(Align::Center)
    );
}

#[test]
fn malformed_css_yields_no_rules_and_never_panics() {
    for junk in [
        "",
        "   ",
        "}}}{{{",
        "p {",
        "p { text-align",
        "{ }",
        ";;;",
        "p { : center }",
        "p { text-align: }",
        "@media {",
        "\u{0}\u{1}",
    ] {
        let _ = styled(junk, "<body><p data-t>x</p></body>"); // must not panic
    }
    assert!(
        styled("p { color: red }", "<body><p data-t>x</p></body>").is_empty(),
        "no honoured property"
    );
}

/// A book's CSS is arbitrary bytes; nothing here may panic on non-ASCII, and a rule beside the
/// non-ASCII must still be understood (RR21-FR3).
#[test]
fn multibyte_css_never_panics_and_still_parses() {
    for css in [
        "/* коммент */ h1 { text-align: center }",
        "/* незакрытый h1 { text-align: center }",
        ".Ünicöde { text-align: center }",
        "h1 { text-align: cëntre }",
        "h1 { font-family: \"日本語\"; text-align: center }",
        "日本語 { text-align: center }",
        "h1 { text-indent: 0日 }",
        "h1 { text-align: center /* 日",
        "🙂 { text-align: center } h1 { text-align: right }",
    ] {
        let _ = styled(css, r#"<body><h1 class="Ünicöde" data-t>x</h1></body>"#);
    }
    assert_eq!(
        styled(
            "/* коммент */ h1 { text-align: center }",
            "<body><h1 data-t>x</h1></body>"
        )
        .align,
        Some(Align::Center)
    );
    // Non-ASCII *values* are fine, and a rule beside them still resolves.
    assert_eq!(
        styled(
            "h1 { font-family: \"日本語\"; text-align: right }",
            "<body><h1 data-t>x</h1></body>"
        )
        .align,
        Some(Align::Right)
    );
}

/// A known limitation of the selector engine, pinned so it is a decision rather than a surprise.
///
/// simplecss 0.2.2 treats a character as identifier-legal only when `c as u32 > 237`
/// (`stream.rs::is_non_ascii`), where CSS says `>= 0x80`. Identifiers holding U+0080–U+00ED —
/// which is most Western European accented letters, `Ü` (U+00DC) among them — therefore fail to
/// parse, and a rule keyed on such a class is dropped rather than mis-applied.
///
/// Accepted deliberately: ASCII class names are what calibre, Sigil and InDesign emit, and the
/// combinator/id/attribute/pseudo selectors gained in exchange are far commoner in real books.
/// Above the threshold (`ö`, U+00F6) it already works, which is why this pins both sides.
#[test]
fn a_latin1_class_name_is_a_known_selector_engine_limitation() {
    assert_eq!(
        styled(
            ".Ünicöde { text-align: right }",
            r#"<body><p class="Ünicöde" data-t>x</p></body>"#
        )
        .align,
        None,
        "if this now matches, simplecss fixed is_non_ascii — drop this test and the caveat"
    );
    // Dropped, never mis-applied: the surrounding rules still resolve.
    assert_eq!(
        styled(
            ".Ünicöde { text-align: right } p { text-align: center }",
            r#"<body><p class="Ünicöde" data-t>x</p></body>"#
        )
        .align,
        Some(Align::Center)
    );
    // A class name above the threshold parses and matches normally.
    assert_eq!(
        styled(
            ".öbig { text-align: right }",
            r#"<body><p class="öbig" data-t>x</p></body>"#
        )
        .align,
        Some(Align::Right)
    );
}

#[test]
fn multiple_sources_layer_with_later_winning() {
    let mut sheet = Stylesheet::parse("p { text-align: left }");
    sheet.add("p { text-align: center }");
    let parsed = simplecss::StyleSheet::parse(sheet.source());
    let doc = Html::parse_document("<body><p data-t>x</p></body>");
    let node = target(doc.tree.root()).unwrap();
    assert_eq!(
        resolve(&parsed, &StyledNode(node), None).align,
        Some(Align::Center)
    );
}

#[test]
fn a_comma_separated_selector_list_applies_to_every_branch() {
    let css = "h1, h2, .t { text-align: center }";
    for html in [
        "<body><h1 data-t>x</h1></body>",
        "<body><h2 data-t>x</h2></body>",
        r#"<body><p class="t" data-t>x</p></body>"#,
    ] {
        assert_eq!(styled(css, html).align, Some(Align::Center), "{html}");
    }
    assert_eq!(styled(css, "<body><h3 data-t>x</h3></body>").align, None);
}

#[test]
fn an_empty_stylesheet_is_empty_and_adding_blank_sources_keeps_it_so() {
    let mut sheet = Stylesheet::default();
    assert!(sheet.is_empty());
    sheet.add("   \n  ");
    assert!(
        sheet.is_empty(),
        "blank source should not make it non-empty"
    );
    sheet.add("p { text-align: center }");
    assert!(!sheet.is_empty());
}
