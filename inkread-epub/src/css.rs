//! A deliberately tiny **CSS subset** for EPUB block styling (#188, widened in #201).
//!
//! `ADR-INKREAD-0007` / `ADR-RUST-READER` Decision 1 chose a simplified block/inline content model
//! over an arbitrary CSS box tree, and that stays true here: this module does **not** implement the
//! cascade, inheritance, the box model, or layout. It answers one narrow question — *"did the book
//! ask for this block to be centred, unindented, or unbolded?"* — because dropping those three
//! declarations is what makes a normal trade EPUB's title page render as left-aligned bold
//! fragments instead of a centred title block.
//!
//! Selector matching is [`simplecss`]'s, not ours: it handles descendant/child/sibling combinators,
//! ids, attribute and pseudo-class selectors, and specificity ordering. What stays inkread's is the
//! *policy* — which three properties are honoured, and what their values mean here.

use ego_tree::NodeRef;
use scraper::node::Node;
use simplecss::{AttributeOperator, DeclarationTokenizer, Element as CssElement, PseudoClass};

use crate::layout::Align;

/// A declared vertical length, kept in its own unit until layout knows the font size (#251).
///
/// Only the units a vertical margin can be resolved from without a box model are represented.
/// `%` is deliberately absent: a percentage margin resolves against the *containing block's width*,
/// which is a box-model measurement inkread does not have (ADR-INKREAD-0007), and guessing it
/// against the height or the font size would be a different length than the book asked for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Length {
    /// A multiple of the font size (`em`, and `rem` — the root size is the body size here).
    Em(f32),
    /// An absolute CSS pixel length.
    Px(f32),
}

impl Eq for Length {}

impl Length {
    /// Resolve to pixels against a font size.
    #[must_use]
    pub fn px(self, font_px: f32) -> f32 {
        match self {
            Length::Em(v) => v * font_px,
            Length::Px(v) => v,
        }
    }
}

/// The block-level properties a book may declare that inkread honours.
///
/// Every field is `Option` so "the book said nothing" stays distinguishable from "the book asked
/// for the default" — the layout stage needs that difference to decide whether a reader's global
/// alignment preference applies (see `effective_align` in `layout`).
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
    /// `font-style`: `Some(true)` for `italic`/`oblique`, `Some(false)` for `normal` (#170).
    ///
    /// Italic already rendered from `<i>`, `<em>` and `<cite>`; what was missing was the CSS. A book
    /// that italicises through its stylesheet rather than a tag rendered upright.
    pub italic: Option<bool>,
    /// `margin-top`, from the longhand or the `margin` shorthand (#251).
    ///
    /// Unlike the four above, margins do **not** inherit — see [`BlockStyle::overlaid_with`]. When
    /// declared it replaces inkread's own gap before the block; when absent inkread's typography
    /// stands. This is the only part of the box model honoured: vertical separation is what a book
    /// uses to mark a stanza or set a heading apart, and dropping it renders poetry as prose.
    pub margin_top: Option<Length>,
    /// `margin-bottom`, from the longhand or the `margin` shorthand (#251).
    pub margin_bottom: Option<Length>,
}

impl BlockStyle {
    /// True when the book declared none of the properties.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.align.is_none()
            && self.indent.is_none()
            && self.bold.is_none()
            && self.italic.is_none()
            && self.margin_top.is_none()
            && self.margin_bottom.is_none()
    }

    /// Return `self` with every property `higher` declares overridden — the inheritance step, used
    /// to fold a container's declared style into the block nested inside it.
    #[must_use]
    pub(crate) fn overlaid_with(mut self, higher: &BlockStyle) -> BlockStyle {
        if higher.align.is_some() {
            self.align = higher.align;
        }
        if higher.indent.is_some() {
            self.indent = higher.indent;
        }
        if higher.bold.is_some() {
            self.bold = higher.bold;
        }
        if higher.italic.is_some() {
            self.italic = higher.italic;
        }
        // `margin` is not an inherited property. Taking the container's would give every block it
        // wraps the container's own spacing — a `<div style="margin: 2em">` around a poem would put
        // two ems between every line of it.
        self.margin_top = higher.margin_top;
        self.margin_bottom = higher.margin_bottom;
        self
    }

    /// Fold one declaration in, ignoring the properties inkread does not honour.
    fn apply(&mut self, name: &str, value: &str) {
        // `!important` is stripped rather than modelled: within the tiny subset here, honouring it
        // would only reorder rules that already agree in practice.
        let value = value
            .trim()
            .trim_end_matches("!important")
            .trim()
            .to_ascii_lowercase();
        match name.trim().to_ascii_lowercase().as_str() {
            "text-align" => {
                if let Some(a) = parse_align(&value) {
                    self.align = Some(a);
                }
            }
            "text-indent" => self.indent = Some(!is_zero_length(&value)),
            "font-weight" => {
                if let Some(b) = parse_font_weight(&value) {
                    self.bold = Some(b);
                }
            }
            "font-style" => {
                if let Some(i) = parse_font_style(&value) {
                    self.italic = Some(i);
                }
            }
            "margin-top" => self.margin_top = parse_length(&value).or(self.margin_top),
            "margin-bottom" => self.margin_bottom = parse_length(&value).or(self.margin_bottom),
            "margin" => {
                let (top, bottom) = parse_margin_shorthand(&value);
                self.margin_top = top.or(self.margin_top);
                self.margin_bottom = bottom.or(self.margin_bottom);
            }
            _ => {}
        }
    }

    /// Fold in a `style="…"` attribute body — declarations alone, no selector.
    fn apply_inline(&mut self, style_attr: &str) {
        for d in DeclarationTokenizer::from(style_attr) {
            self.apply(d.name, d.value);
        }
    }
}

/// The book's CSS, kept as owned source.
///
/// [`simplecss::StyleSheet`] borrows the text it parses, so it cannot live in [`crate::EpubPackage`]
/// (which is owned, `Clone` and `Eq`). Holding the source instead lets a chapter parse build the
/// borrowed sheet once, for the duration of that parse.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stylesheet {
    css: String,
}

impl Stylesheet {
    /// A stylesheet over `css`.
    #[must_use]
    pub fn parse(css: &str) -> Self {
        let mut sheet = Self::default();
        sheet.add(css);
        sheet
    }

    /// Append another source after this one, so later sources win ties at equal specificity.
    pub fn add(&mut self, css: &str) {
        if css.trim().is_empty() {
            return;
        }
        if !self.css.is_empty() {
            self.css.push('\n');
        }
        self.css.push_str(css);
    }

    /// True when the book declared no CSS at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.css.trim().is_empty()
    }

    /// The accumulated source, for [`simplecss::StyleSheet::parse`].
    #[must_use]
    pub(crate) fn source(&self) -> &str {
        &self.css
    }
}

/// Resolve the declared style for `el`: every matching rule folded in specificity order, then the
/// element's own `style="…"` attribute on top.
pub(crate) fn resolve<E: CssElement>(
    sheet: &simplecss::StyleSheet<'_>,
    el: &E,
    style_attr: Option<&str>,
) -> BlockStyle {
    let mut out = BlockStyle::default();
    // simplecss sorts `rules` by specificity at parse time, and a stable sort keeps source order
    // within a specificity — which is exactly the CSS tie-break.
    for rule in &sheet.rules {
        if rule.selector.matches(el) {
            for d in &rule.declarations {
                out.apply(d.name, d.value);
            }
        }
    }
    if let Some(inline) = style_attr {
        out.apply_inline(inline);
    }
    out
}

/// Bridges the transient html5ever tree the content walker holds to simplecss's matching, so
/// selectors that reach beyond a single element (`div.titlepage p`, `p:first-child`) resolve
/// against the real document rather than being dropped.
#[derive(Clone, Copy)]
pub(crate) struct StyledNode<'a>(pub NodeRef<'a, Node>);

impl<'a> StyledNode<'a> {
    fn element(&self) -> Option<&'a scraper::node::Element> {
        self.0.value().as_element()
    }
}

impl CssElement for StyledNode<'_> {
    fn parent_element(&self) -> Option<Self> {
        let mut node = self.0.parent()?;
        loop {
            if node.value().is_element() {
                return Some(StyledNode(node));
            }
            node = node.parent()?;
        }
    }

    fn prev_sibling_element(&self) -> Option<Self> {
        self.0
            .prev_siblings()
            .find(|n| n.value().is_element())
            .map(StyledNode)
    }

    fn has_local_name(&self, name: &str) -> bool {
        // Element names are case-insensitive in HTML; html5ever already lowercases them, but a
        // stylesheet may still write `H1`.
        self.element()
            .is_some_and(|el| el.name().eq_ignore_ascii_case(name))
    }

    fn attribute_matches(&self, local_name: &str, operator: AttributeOperator<'_>) -> bool {
        self.element()
            .and_then(|el| el.attr(local_name))
            .is_some_and(|value| {
                // `Contains` (`[class~=x]`, which is what `.x` compiles to) splits on a single
                // space, so a prettified `class="a\n  b"` would not match `.b`. Collapse runs of
                // whitespace first, which is how a browser reads a class list.
                let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
                operator.matches(&normalized)
            })
    }

    fn pseudo_class_matches(&self, class: PseudoClass<'_>) -> bool {
        match class {
            PseudoClass::FirstChild => self.prev_sibling_element().is_none(),
            // :lang() is the only other one a book might reasonably use on a block; link/visited/
            // hover/active/focus have no meaning in a paginated reader.
            PseudoClass::Lang(want) => self
                .element()
                .and_then(|el| el.attr("lang"))
                .is_some_and(|got| got.eq_ignore_ascii_case(want)),
            _ => false,
        }
    }
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

/// A CSS length → [`Length`], for the vertical margins inkread honours (#251).
///
/// `None` for anything that cannot be resolved without a box model — `auto`, a percentage, an
/// unrecognised keyword, a unitless non-zero (invalid CSS) — so the book declares nothing and
/// inkread's own typography stands rather than a guessed length replacing it. A bare `0` is valid
/// in any unit and means zero.
fn parse_length(value: &str) -> Option<Length> {
    let value = value.trim();
    let digits: String = value
        .chars()
        .take_while(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+'))
        .collect();
    let n: f32 = digits.parse().ok()?;
    if !n.is_finite() {
        return None;
    }
    // Negative margins pull content into its neighbour; without a box model there is nothing for
    // them to overlap, and honouring them would only eat the gap around them.
    let n = n.max(0.0);
    match value[digits.len()..].trim() {
        "em" | "rem" | "ex" | "ch" => Some(Length::Em(n)),
        "px" => Some(Length::Px(n)),
        // A CSS absolute unit: convert at the reference 96 dpi rather than dropping the intent.
        "pt" => Some(Length::Px(n * 96.0 / 72.0)),
        "" if n == 0.0 => Some(Length::Px(0.0)),
        _ => None,
    }
}

/// The `margin` shorthand → its top and bottom components.
///
/// CSS box order: one value is all four sides, two are `vertical horizontal`, three are
/// `top horizontal bottom`, four are `top right bottom left`.
fn parse_margin_shorthand(value: &str) -> (Option<Length>, Option<Length>) {
    let parts: Vec<&str> = value.split_whitespace().collect();
    match parts.as_slice() {
        [all] => {
            let l = parse_length(all);
            (l, l)
        }
        [v, _] => {
            let l = parse_length(v);
            (l, l)
        }
        [t, _, b] | [t, _, b, _] => (parse_length(t), parse_length(b)),
        _ => (None, None),
    }
}

/// `font-style` → italic or upright (#170).
///
/// `oblique` counts as italic: it is a slant request, and the renderer already falls back to
/// shearing a regular face for a family that bundles no italic, which is exactly what oblique asks
/// for. An unrecognised value declares nothing rather than guessing upright, so a reader's own
/// setting is not overridden by a keyword we failed to understand.
fn parse_font_style(value: &str) -> Option<bool> {
    match value {
        "normal" => Some(false),
        "italic" | "oblique" => Some(true),
        _ => None,
    }
}

/// `font-weight` → bold-ish or normal-ish. Numeric weights split at 600, as CSS does.
fn parse_font_weight(value: &str) -> Option<bool> {
    match value {
        "normal" | "lighter" => Some(false),
        "bold" | "bolder" => Some(true),
        _ => value.parse::<u16>().ok().map(|w| w >= 600),
    }
}

#[cfg(test)]
#[path = "css_tests.rs"]
mod tests;
