//! A deliberately tiny **CSS subset** for EPUB block styling (#188).
//!
//! `ADR-INKREAD-0007` / `ADR-RUST-READER` Decision 1 chose a simplified block/inline content model
//! over an arbitrary CSS box tree, and that stays true here: this module does **not** implement the
//! cascade, inheritance, the box model, or layout. It answers one narrow question — *"did the book
//! ask for this block to be centred, unindented, or unbolded?"* — because dropping those three
//! declarations is what makes a normal trade EPUB's title page render as left-aligned bold
//! fragments instead of a centred title block.
//!
//! Scope, deliberately: **type and class selectors only** (`h1`, `.c`, `h1.c`), three properties
//! ([`BlockStyle`]), and specificity/order tie-breaking. Anything else — descendant combinators,
//! ids, attribute and pseudo selectors, `@media` blocks, inheritance, lengths — is *ignored rather
//! than approximated*, because a mis-applied rule is worse than an unstyled one.

use crate::layout::Align;

/// The block-level properties a book may declare that inkread honours.
///
/// Every field is `Option` so "the book said nothing" stays distinguishable from "the book asked
/// for the default" — the layout stage needs that difference to decide whether a reader's global
/// alignment preference applies (see `layout::declared_align`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BlockStyle {
    /// `text-align`, if declared.
    pub align: Option<Align>,
    /// `text-indent`: `Some(false)` when the book explicitly zeroes it, `Some(true)` for any
    /// non-zero value. The declared *length* is not honoured — inkread applies its own book-
    /// typography indent — so this is a three-state opt-out, not a measurement.
    pub indent: Option<bool>,
    /// `font-weight`: `Some(true)` for bold-ish (`bold`/`bolder`/`600`+), `Some(false)` for
    /// normal-ish (`normal`/`lighter`/`500`-).
    pub bold: Option<bool>,
}

impl BlockStyle {
    /// True when the book declared none of the three properties.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.align.is_none() && self.indent.is_none() && self.bold.is_none()
    }

    /// Return `self` with every property `higher` declares overridden — the inheritance step, used
    /// to fold a container's declared style into the block nested inside it.
    #[must_use]
    pub fn overlaid_with(mut self, higher: &BlockStyle) -> BlockStyle {
        self.overlay(higher);
        self
    }

    /// Overlay `higher` onto `self`; every property `higher` declares wins.
    fn overlay(&mut self, higher: &BlockStyle) {
        if higher.align.is_some() {
            self.align = higher.align;
        }
        if higher.indent.is_some() {
            self.indent = higher.indent;
        }
        if higher.bold.is_some() {
            self.bold = higher.bold;
        }
    }
}

/// Parse a `style="…"` attribute body — the declarations alone, with no selector.
#[must_use]
pub fn parse_inline(style_attr: &str) -> BlockStyle {
    parse_declarations(style_attr)
}

/// One parsed rule: a simple selector plus the declarations it carries.
#[derive(Debug, Clone)]
struct Rule {
    /// Element name to match (`h1`), or `None` for "any element" (`.c`, `*`).
    tag: Option<String>,
    /// Class to match (`c`), or `None` for "any class" (`h1`, `*`).
    class: Option<String>,
    /// CSS specificity, coarsely: `*` = 0, tag = 1, class = 10, tag.class = 11.
    specificity: u8,
    /// Source order, so equal-specificity rules resolve last-wins.
    order: usize,
    style: BlockStyle,
}

/// A parsed stylesheet — the book's declarations, queryable by element name + class attribute.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stylesheet {
    rules: Vec<Rule>,
}

// `Rule` holds no float/interior mutability, but `BlockStyle` is `PartialEq` only, so derive by hand
// to keep `Stylesheet` comparable in tests without leaking the requirement onto `Rule`'s fields.
impl PartialEq for Rule {
    fn eq(&self, other: &Self) -> bool {
        self.tag == other.tag
            && self.class == other.class
            && self.specificity == other.specificity
            && self.order == other.order
            && self.style == other.style
    }
}
impl Eq for Rule {}

impl Stylesheet {
    /// Parse a stylesheet. Malformed input yields fewer rules, never an error and never a panic —
    /// a book with broken CSS must still open (RR21-FR3).
    #[must_use]
    pub fn parse(css: &str) -> Self {
        let mut sheet = Self::default();
        sheet.add(css);
        sheet
    }

    /// Append another stylesheet's rules after this one's, so later sources win ties. Used to layer
    /// a chapter's in-document `<style>` over the book-wide linked stylesheets.
    pub fn add(&mut self, css: &str) {
        let stripped = strip_comments(css);
        let mut rest = stripped.as_str();
        while let Some(brace) = rest.find('{') {
            let head = rest[..brace].trim();
            let (body, tail) = take_block(&rest[brace + 1..]);
            rest = tail;
            // `@charset "x"; p` / `@import url(y); p` leave a statement before the real selector;
            // keep only the part after the last `;` so those don't swallow the following rule.
            let selectors = head.rsplit(';').next().unwrap_or(head).trim();
            // An at-rule's own block (`@media`, `@font-face`, `@page`) is skipped whole: honouring
            // its nested rules would mean modelling the conditions they are nested under.
            if selectors.starts_with('@') || selectors.is_empty() {
                continue;
            }
            let style = parse_declarations(body);
            if style.is_empty() {
                continue;
            }
            for sel in selectors.split(',') {
                if let Some((tag, class, specificity)) = parse_selector(sel.trim()) {
                    let order = self.rules.len();
                    self.rules.push(Rule {
                        tag,
                        class,
                        specificity,
                        order,
                        style,
                    });
                }
            }
        }
    }

    /// True when no rule was understood (an absent, empty, or entirely unsupported stylesheet).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Resolve the declared style for an element: every matching rule folded in specificity order,
    /// then the element's own `style="…"` attribute on top.
    #[must_use]
    pub fn resolve(
        &self,
        tag: &str,
        class_attr: Option<&str>,
        style_attr: Option<&str>,
    ) -> BlockStyle {
        let mut out = BlockStyle::default();
        if !self.rules.is_empty() {
            let mut matched: Vec<&Rule> = self
                .rules
                .iter()
                .filter(|r| r.matches(tag, class_attr))
                .collect();
            matched.sort_by_key(|r| (r.specificity, r.order));
            for rule in matched {
                out.overlay(&rule.style);
            }
        }
        if let Some(inline) = style_attr {
            out.overlay(&parse_inline(inline));
        }
        out
    }
}

impl Rule {
    /// True when this rule's tag and class constraints both admit the element.
    fn matches(&self, tag: &str, class_attr: Option<&str>) -> bool {
        if let Some(want) = &self.tag {
            if !want.eq_ignore_ascii_case(tag) {
                return false;
            }
        }
        if let Some(want) = &self.class {
            let has = class_attr
                .unwrap_or_default()
                .split_whitespace()
                .any(|c| c == want);
            if !has {
                return false;
            }
        }
        true
    }
}

/// Parse one simple selector into `(tag, class, specificity)`. Returns `None` for anything beyond
/// `tag`, `.class`, `tag.class`, and `*` — combinators, ids, attributes and pseudo-classes are
/// skipped rather than approximated.
fn parse_selector(sel: &str) -> Option<(Option<String>, Option<String>, u8)> {
    if sel.is_empty() || sel.contains([' ', '>', '+', '~', '#', '[', ':', '(', '*']) {
        // `*` alone would be legal, but a universal rule carries no information we act on beyond
        // what a tag rule gives us, and `*` inside a longer selector is unsupported anyway.
        return None;
    }
    let (tag, class) = match sel.split_once('.') {
        Some((t, c)) => {
            if c.is_empty() || c.contains('.') {
                return None; // `.` alone, or a multi-class selector (`.a.b`) we don't model
            }
            let tag = (!t.is_empty()).then(|| t.to_ascii_lowercase());
            (tag, Some(c.to_string()))
        }
        None => (Some(sel.to_ascii_lowercase()), None),
    };
    let specificity = match (&tag, &class) {
        (Some(_), Some(_)) => 11,
        (None, Some(_)) => 10,
        (Some(_), None) => 1,
        (None, None) => 0,
    };
    Some((tag, class, specificity))
}

/// Parse a declaration block body (`prop: value; …`) into the properties we honour.
fn parse_declarations(body: &str) -> BlockStyle {
    let mut style = BlockStyle::default();
    for decl in body.split(';') {
        let Some((prop, value)) = decl.split_once(':') else {
            continue;
        };
        let prop = prop.trim().to_ascii_lowercase();
        // `!important` is stripped rather than modelled: within the tiny subset here, honouring it
        // would only reorder rules that already agree in practice.
        let value = value
            .trim()
            .trim_end_matches("!important")
            .trim()
            .to_ascii_lowercase();
        match prop.as_str() {
            "text-align" => style.align = parse_align(&value),
            "text-indent" => style.indent = Some(!is_zero_length(&value)),
            "font-weight" => style.bold = parse_font_weight(&value),
            _ => {}
        }
    }
    style
}

/// `text-align` → [`Align`]. `start`/`end` are the EPUB 3 logical forms; inkread is horizontal
/// left-to-right, so they map onto left/right.
fn parse_align(value: &str) -> Option<Align> {
    match value {
        "left" | "start" => Some(Align::Left),
        "right" | "end" => Some(Align::Right),
        "center" | "centre" => Some(Align::Center),
        "justify" => Some(Align::Justify),
        _ => None,
    }
}

/// True for `0` in any unit (`0`, `0em`, `0%`, `0.0px`) — the only `text-indent` value that changes
/// what we do. A malformed or unparseable value reads as non-zero, preserving the default indent.
fn is_zero_length(value: &str) -> bool {
    let num: String = value
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '+')
        .collect();
    num.parse::<f32>().is_ok_and(|n| n == 0.0)
}

/// `font-weight` → bold-ish or normal-ish. Numeric weights split at 600, as CSS does.
fn parse_font_weight(value: &str) -> Option<bool> {
    match value {
        "normal" | "lighter" => Some(false),
        "bold" | "bolder" => Some(true),
        _ => value.parse::<u16>().ok().map(|w| w >= 600),
    }
}

/// Remove `/* … */` comments. An unterminated comment swallows the rest, as a browser does.
fn strip_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        match rest[start + 2..].find("*/") {
            Some(end) => rest = &rest[start + 2 + end + 2..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Split off a brace-balanced block body, returning `(body, rest_after_closing_brace)`. The opening
/// brace is already consumed. An unclosed block runs to end of input.
fn take_block(after_open: &str) -> (&str, &str) {
    let mut depth = 1usize;
    for (i, ch) in after_open.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return (&after_open[..i], &after_open[i + 1..]);
                }
            }
            _ => {}
        }
    }
    (after_open, "")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stylesheet from #188 — Project Gutenberg's *Pride and Prejudice* (#1342), the book whose
    /// title page rendered as left-aligned bold fragments.
    const PG_CSS: &str = r#"
        h1        { margin-top: 5%; text-align: center; clear: both; font-weight: normal }
        .c        { text-align: center; text-indent: 0% }
        .cbig250  { text-align: center; text-indent: 0%; font-weight: normal; font-size: 200% }
    "#;

    #[test]
    fn the_pride_and_prejudice_title_resolves_centred_unbolded_and_unindented() {
        let sheet = Stylesheet::parse(PG_CSS);
        // The reported case: a bare <h1> with no class.
        let h1 = sheet.resolve("h1", None, None);
        assert_eq!(h1.align, Some(Align::Center), "text-align was dropped");
        assert_eq!(h1.bold, Some(false), "font-weight: normal was dropped");

        // A classed decorative block opts out of the paragraph indent.
        let c = sheet.resolve("p", Some("c"), None);
        assert_eq!(c.align, Some(Align::Center));
        assert_eq!(c.indent, Some(false), "text-indent: 0% was dropped");
    }

    #[test]
    fn class_beats_element_and_later_beats_earlier_at_equal_specificity() {
        let sheet = Stylesheet::parse(
            "p { text-align: left } .mid { text-align: center } p { font-weight: bold }",
        );
        assert_eq!(
            sheet.resolve("p", Some("mid"), None).align,
            Some(Align::Center)
        );
        assert_eq!(sheet.resolve("p", None, None).align, Some(Align::Left));
        assert_eq!(sheet.resolve("p", None, None).bold, Some(true));

        let last_wins = Stylesheet::parse("p { text-align: left } p { text-align: right }");
        assert_eq!(last_wins.resolve("p", None, None).align, Some(Align::Right));
    }

    #[test]
    fn a_tag_qualified_class_outranks_a_bare_class() {
        let sheet = Stylesheet::parse("h1.t { text-align: right } .t { text-align: center }");
        assert_eq!(
            sheet.resolve("h1", Some("t"), None).align,
            Some(Align::Right)
        );
        assert_eq!(
            sheet.resolve("p", Some("t"), None).align,
            Some(Align::Center)
        );
    }

    #[test]
    fn an_inline_style_attribute_outranks_every_rule() {
        let sheet = Stylesheet::parse("p { text-align: left }");
        let s = sheet.resolve("p", None, Some("text-align: center; text-indent: 0"));
        assert_eq!(s.align, Some(Align::Center));
        assert_eq!(s.indent, Some(false));
    }

    #[test]
    fn a_class_only_matches_its_own_element_and_one_of_several_classes() {
        let sheet = Stylesheet::parse(".c { text-align: center }");
        assert_eq!(
            sheet.resolve("p", Some("first c last"), None).align,
            Some(Align::Center)
        );
        assert_eq!(
            sheet.resolve("p", Some("cc"), None).align,
            None,
            "no prefix match"
        );
        assert_eq!(sheet.resolve("p", None, None).align, None);
    }

    #[test]
    fn unsupported_selectors_are_ignored_not_approximated() {
        // Each of these would change the wrong blocks if we pretended to understand it.
        for sel in [
            "div p",
            "div > p",
            "p + p",
            "p ~ p",
            "#id",
            "p[lang]",
            "p:first-child",
            "*",
        ] {
            let sheet = Stylesheet::parse(&format!("{sel} {{ text-align: center }}"));
            assert!(sheet.is_empty(), "{sel} should be skipped, got {sheet:?}");
        }
    }

    #[test]
    fn at_rules_are_skipped_whole_without_losing_the_rules_around_them() {
        let sheet = Stylesheet::parse(
            r#"@charset "utf-8";
               @import url(other.css);
               h1 { text-align: center }
               @media print { h1 { text-align: right } p { text-align: right } }
               @font-face { font-family: x; src: url(x.ttf) }
               p { text-indent: 0 }"#,
        );
        assert_eq!(
            sheet.resolve("h1", None, None).align,
            Some(Align::Center),
            "@media leaked"
        );
        assert_eq!(
            sheet.resolve("p", None, None).indent,
            Some(false),
            "rule after @font-face lost"
        );
        assert_eq!(sheet.resolve("p", None, None).align, None);
    }

    #[test]
    fn comments_are_stripped_including_an_unterminated_one() {
        let sheet = Stylesheet::parse("/* c */ h1 { text-align: /* mid */ center }");
        assert_eq!(sheet.resolve("h1", None, None).align, Some(Align::Center));
        // An unterminated comment swallows the rest, as a browser does.
        assert!(!Stylesheet::parse("h1 { text-align: center } /* oops").is_empty());
        assert!(Stylesheet::parse("/* oops h1 { text-align: center }").is_empty());
    }

    #[test]
    fn text_indent_is_a_zero_opt_out_not_a_measurement() {
        for zero in ["0", "0em", "0%", "0.0px", "+0em", "-0em"] {
            let s = Stylesheet::parse(&format!("p {{ text-indent: {zero} }}"));
            assert_eq!(s.resolve("p", None, None).indent, Some(false), "{zero}");
        }
        for nonzero in ["1.5em", "5%", "12px", "inherit"] {
            let s = Stylesheet::parse(&format!("p {{ text-indent: {nonzero} }}"));
            assert_eq!(s.resolve("p", None, None).indent, Some(true), "{nonzero}");
        }
    }

    #[test]
    fn font_weight_splits_at_600_like_css() {
        for normal in ["normal", "lighter", "100", "400", "500"] {
            let s = Stylesheet::parse(&format!("h1 {{ font-weight: {normal} }}"));
            assert_eq!(s.resolve("h1", None, None).bold, Some(false), "{normal}");
        }
        for bold in ["bold", "bolder", "600", "700", "900"] {
            let s = Stylesheet::parse(&format!("h1 {{ font-weight: {bold} }}"));
            assert_eq!(s.resolve("h1", None, None).bold, Some(true), "{bold}");
        }
        assert_eq!(
            Stylesheet::parse("h1 { font-weight: wat }")
                .resolve("h1", None, None)
                .bold,
            None,
            "an unparseable weight declares nothing"
        );
    }

    #[test]
    fn important_is_stripped_and_alignment_keywords_map() {
        let s = Stylesheet::parse("p { text-align: center !important }");
        assert_eq!(s.resolve("p", None, None).align, Some(Align::Center));
        for (css, want) in [
            ("left", Align::Left),
            ("start", Align::Left),
            ("right", Align::Right),
            ("end", Align::Right),
            ("justify", Align::Justify),
        ] {
            let s = Stylesheet::parse(&format!("p {{ text-align: {css} }}"));
            assert_eq!(s.resolve("p", None, None).align, Some(want), "{css}");
        }
    }

    #[test]
    fn selectors_and_properties_are_case_insensitive() {
        let sheet = Stylesheet::parse("H1 { TEXT-ALIGN: CENTER }");
        assert_eq!(sheet.resolve("h1", None, None).align, Some(Align::Center));
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
            let _ = Stylesheet::parse(junk); // must not panic
        }
        assert!(
            Stylesheet::parse("p { color: red }").is_empty(),
            "no honoured property"
        );
        // An unclosed block still contributes its declarations rather than being lost.
        assert_eq!(
            Stylesheet::parse("p { text-align: center")
                .resolve("p", None, None)
                .align,
            Some(Align::Center)
        );
    }

    #[test]
    fn multiple_sources_layer_with_later_winning() {
        let mut sheet = Stylesheet::parse("p { text-align: left }");
        sheet.add("p { text-align: center }");
        assert_eq!(sheet.resolve("p", None, None).align, Some(Align::Center));
    }

    #[test]
    fn a_comma_separated_selector_list_applies_to_every_branch() {
        let sheet = Stylesheet::parse("h1, h2, .t { text-align: center }");
        for (tag, class) in [("h1", None), ("h2", None), ("p", Some("t"))] {
            assert_eq!(
                sheet.resolve(tag, class, None).align,
                Some(Align::Center),
                "{tag}"
            );
        }
        assert_eq!(sheet.resolve("h3", None, None).align, None);
    }
}
