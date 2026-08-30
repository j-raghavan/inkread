//! A deliberately tiny **CSS subset** for EPUB block styling (#188, widened in #201 and #251).
//!
//! `ADR-INKREAD-0007` / `ADR-RUST-READER` Decision 1 chose a simplified block/inline content model
//! over an arbitrary CSS box tree, and that stays true here: this module implements no cascade and
//! no layout. It answers one narrow question — *"what did the book ask for this block?"* — because
//! dropping those declarations is what makes a normal trade EPUB's title page render as
//! left-aligned bold fragments instead of a centred title block, and its poetry as prose.
//!
//! Of the box model it honours exactly one thing: **vertical margins**. That is not a slippery
//! slope towards a box tree but the narrowest answer to a real defect — vertical space is how a
//! book marks off a stanza, and there is no other way for it to say so. Horizontal margins,
//! padding, borders and percentage lengths stay out, because each needs a containing block's width
//! to mean anything and there is no box here to measure (#251).
//!
//! Selector matching is [`simplecss`]'s, not ours: it handles descendant/child/sibling combinators,
//! ids, attribute selectors and specificity ordering. Pseudo-classes it delegates back to us, and
//! of those it can express we answer `:first-child` and `:lang()`; the rest are interaction states
//! with no meaning in a paginated reader. `:last-child` and `:nth-child()` it cannot express at
//! all, so a rule using one is dropped — a real gap, and the reason a book's last-stanza spacing
//! may not arrive. What stays inkread's is the *policy* — which properties are honoured, which of
//! them inherit, and what their values mean.

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
#[derive(Debug, Clone, Copy)]
pub enum Length {
    /// A multiple of the **block's own** font size, as CSS resolves `em`, so a margin on a heading
    /// scales with the heading.
    ///
    /// `rem` should resolve against the root size and is resolved here like `em`. In a reflowed
    /// book almost nothing sets a font size away from the body's, so the two coincide for all but
    /// headings — where being wrong by the heading's own scale is the smaller error against
    /// carrying a second size through layout.
    Em(f32),
    /// An absolute CSS pixel length.
    Px(f32),
}

/// Total equality, so that [`BlockStyle`] and [`crate::content::Block`] can derive `Eq` — which
/// they need to be compared for a repagination check. Written by hand rather than derived because
/// `f32`'s `PartialEq` says `NaN != NaN`, which would break `Eq`'s reflexivity: [`parse_length`]
/// rejects non-finite values, but the variants are public and a caller could still build one.
impl PartialEq for Length {
    fn eq(&self, other: &Self) -> bool {
        let same = |a: f32, b: f32| a == b || (a.is_nan() && b.is_nan());
        match (self, other) {
            (Length::Em(a), Length::Em(b)) | (Length::Px(a), Length::Px(b)) => same(*a, *b),
            _ => false,
        }
    }
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

/// What a book asked to happen at a block's edge, or inside it (#251).
///
/// One type for `page-break-before`/`-after`/`-inside` and their CSS3 `break-*` spellings, because
/// they share a value grammar. `Auto` is kept distinct from "not declared" so a later rule can
/// cancel an earlier one — `h3 { page-break-before: always }` then `.run-on h3 { ...: auto }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageBreak {
    /// No constraint, explicitly declared.
    Auto,
    /// Force a break here (`always`, and the page-side keywords, which inkread has no notion of).
    Always,
    /// Do not break here (`avoid`).
    Avoid,
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
    /// `page-break-before` / `break-before` (#251): start this block on a new page.
    ///
    /// Like `margin`, and unlike the four properties above, the break properties do not inherit —
    /// but a book routinely declares one on the container rather than the block, so [`crate::content`]
    /// transfers a container's onto the blocks it wraps.
    pub break_before: Option<PageBreak>,
    /// `page-break-after` / `break-after` (#251). `Avoid` is what keeps a heading on the same page
    /// as the text it introduces, and is also how a container's `page-break-inside: avoid` is
    /// expressed over the several blocks it wraps.
    pub break_after: Option<PageBreak>,
    /// `page-break-inside` / `break-inside` (#251): `Avoid` asks for the block not to be split
    /// across a page boundary, so a stanza moves whole to the next page rather than being halved.
    pub break_inside: Option<PageBreak>,
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
            && self.break_before.is_none()
            && self.break_after.is_none()
            && self.break_inside.is_none()
    }

    /// The half of this style that a container passes down to the blocks nested inside it.
    ///
    /// `margin` and the `page-break-*` properties are not inherited in CSS, and threading them into
    /// a container's descendants is not a near-miss but the opposite of what the book asked: a
    /// `<div style="margin: 2em">` around a poem would put two ems between every line of it, and a
    /// `page-break-before` on it would start every line on a new page. What a container declares
    /// for itself reaches the blocks it wraps through [`crate::content`]'s transfer step instead.
    #[must_use]
    pub(crate) fn inherited_only(self) -> BlockStyle {
        BlockStyle {
            align: self.align,
            indent: self.indent,
            bold: self.bold,
            italic: self.italic,
            ..BlockStyle::default()
        }
    }

    /// Return `self` with every property `higher` declares overridden — the inheritance step, used
    /// to fold a container's declared style into the block nested inside it.
    ///
    /// Only the four text properties actually overlay. The rest are *replaced* by `higher`'s,
    /// because they do not inherit; see [`BlockStyle::inherited_only`].
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
        // Nor do the break properties. A container's are transferred onto the blocks it wraps by
        // `content::apply_container_style`, which is a different thing from inheriting them: a
        // `page-break-before` on a `<div>` means one break, not one before every block inside.
        self.break_before = higher.break_before;
        self.break_after = higher.break_after;
        self.break_inside = higher.break_inside;
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
            // Every arm reads `x = parse(..).or(x)`: a value we cannot parse declares nothing and
            // leaves the previous declaration standing, rather than cancelling it.
            "text-align" => self.align = parse_align(&value).or(self.align),
            "text-indent" => self.indent = Some(!is_zero_length(&value)),
            "font-weight" => self.bold = parse_font_weight(&value).or(self.bold),
            "font-style" => self.italic = parse_font_style(&value).or(self.italic),
            "margin-top" => self.margin_top = parse_length(&value).or(self.margin_top),
            "margin-bottom" => self.margin_bottom = parse_length(&value).or(self.margin_bottom),
            "page-break-before" | "break-before" => {
                self.break_before = parse_page_break(&value).or(self.break_before);
            }
            "page-break-after" | "break-after" => {
                self.break_after = parse_page_break(&value).or(self.break_after);
            }
            "page-break-inside" | "break-inside" => {
                self.break_inside = parse_page_break(&value).or(self.break_inside);
            }
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
        let css = flatten_media(css);
        if css.trim().is_empty() {
            return;
        }
        if !self.css.is_empty() {
            self.css.push('\n');
        }
        self.css.push_str(&css);
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

/// Lift the rules inside a screen-applicable `@media` block out to the top level, and drop the ones
/// meant for another medium (#251).
///
/// The selector engine skips at-rules wholesale, so a book that wraps its body styling in
/// `@media screen { … }` — which a great deal of Calibre and KindleGen output does — declared
/// nothing at all. Flattening beats teaching the engine media queries: inkread renders to exactly
/// one medium, so a query is not a condition to evaluate but a question of whether the block is
/// addressed to us.
///
/// A query naming `print`, `speech`, or a vendor medium (`amzn-kf8`, `amzn-mobi`) is not; anything
/// else — `screen`, `all`, a bare feature query, no query at all — is. Other at-rules
/// (`@font-face`, `@page`, `@import`) pass through untouched for the engine to skip as before.
fn flatten_media(css: &str) -> String {
    if !css.contains("@media") {
        return css.to_string();
    }
    // Comments are stripped first so a commented-out `@media` cannot be flattened into live rules.
    let source = strip_comments(css);
    let mut out = String::with_capacity(source.len());
    let mut rest = source.as_str();
    while let Some(at) = rest.find("@media") {
        out.push_str(&rest[..at]);
        let after = &rest[at + "@media".len()..];
        let Some(open) = after.find('{') else {
            break; // Truncated at-rule: nothing left to lift.
        };
        let query = &after[..open];
        let body = &after[open + 1..];
        let Some(end) = matching_brace(body) else {
            break;
        };
        if media_applies(query) {
            out.push_str(&body[..end]);
        }
        rest = &body[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Offset of the `}` closing a block whose opening `{` has already been consumed.
fn matching_brace(body: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in body.char_indices() {
        match c {
            '{' => depth += 1,
            '}' if depth == 0 => return Some(i),
            '}' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// True when a media query addresses a reflowing screen reader.
fn media_applies(query: &str) -> bool {
    const NOT_OURS: &[&str] = &["print", "speech", "aural", "braille", "embossed", "amzn-"];
    let q = query.to_ascii_lowercase();
    !NOT_OURS.iter().any(|m| q.contains(m))
}

/// Remove `/* … */` comments. Unterminated comments run to the end, as CSS says.
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

/// Split a CSS length into its numeric prefix and its unit, as borrowed slices.
///
/// The prefix characters are all ASCII, so the split is always on a char boundary however the value
/// is spelled — a leading multi-byte character simply yields an empty number.
fn split_number(value: &str) -> (&str, &str) {
    let end = value
        .find(|c: char| !c.is_ascii_digit() && !matches!(c, '.' | '-' | '+'))
        .unwrap_or(value.len());
    (&value[..end], value[end..].trim())
}

/// True for `0` in any unit (`0`, `0em`, `0%`, `0.0px`) — the only `text-indent` value that changes
/// what we do. A malformed or unparseable value reads as non-zero, preserving the default indent.
fn is_zero_length(value: &str) -> bool {
    split_number(value).0.parse::<f32>().is_ok_and(|n| n == 0.0)
}

/// A CSS length → [`Length`], for the vertical margins inkread honours (#251).
///
/// `None` for anything that cannot be resolved without a box model — `auto`, a percentage, an
/// unrecognised keyword, a unitless non-zero (invalid CSS) — so the book declares nothing and
/// inkread's own typography stands rather than a guessed length replacing it. A bare `0` is valid
/// in any unit and means zero.
fn parse_length(value: &str) -> Option<Length> {
    let (number, unit) = split_number(value.trim());
    let n: f32 = number.parse().ok()?;
    if !n.is_finite() {
        return None;
    }
    // Negative margins pull content into its neighbour; without a box model there is nothing for
    // them to overlap, and honouring them would only eat the gap around them.
    let n = n.max(0.0);
    match unit {
        "em" | "rem" => Some(Length::Em(n)),
        // CSS puts `ex` at the x-height and `ch` at the width of a "0"; both are about half an em
        // in a text face, which is nearer than treating them as a whole one.
        "ex" | "ch" => Some(Length::Em(n * 0.5)),
        "px" => Some(Length::Px(n)),
        // A CSS absolute unit: convert at the reference 96 dpi rather than dropping the intent.
        "pt" => Some(Length::Px(n * 96.0 / 72.0)),
        "" if n == 0.0 => Some(Length::Px(0.0)),
        _ => None,
    }
}

/// A `page-break-*` / `break-*` value → [`PageBreak`].
///
/// inkread paginates a single stream with no notion of a left- or right-hand page, so `left` and
/// `right` (and their CSS3 `recto`/`verso` spellings) are honoured as the break they request and
/// not as the parity they request. Column- and region-scoped values ask for a break in a flow
/// inkread does not have, so they declare no constraint rather than being promoted to a page break
/// the book did not ask for.
fn parse_page_break(value: &str) -> Option<PageBreak> {
    match value {
        "always" | "page" | "left" | "right" | "recto" | "verso" => Some(PageBreak::Always),
        "avoid" | "avoid-page" => Some(PageBreak::Avoid),
        "auto" | "column" | "region" | "avoid-column" | "avoid-region" => Some(PageBreak::Auto),
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
