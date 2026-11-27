//! Readability extraction (#66): raw article HTML → clean, well-formed XHTML of the **main content**.
//!
//! A heuristic (not a full readability port), but enough to read calmly: it isolates the
//! `<article>`/`<main>` region when present, drops page chrome (nav/header/footer/aside/form/figure,
//! plus script/style/svg), skips short non-content blocks (share/subscribe/counts/nav links), decodes
//! HTML entities (named + numeric), and emits the remaining block text as escaped `<p>` paragraphs.
//! Output is always well-formed XHTML so the assembler injects it safely. Pure + host-testable.
//!
//! The HTML is parsed with `scraper` (html5ever) — the same parser `inkread-epub` uses — rather than
//! a hand-rolled tag scanner: comments, CDATA, conditional comments, and malformed nesting are
//! handled by the parser, so comment bodies (e.g. `->` arrows) can never leak into the prose.

use ego_tree::NodeRef;
use scraper::node::Node;
use scraper::{ElementRef, Html};

/// Tags whose closing (or self-closing) ends a paragraph — block-level boundaries.
const BLOCK_TAGS: &[&str] = &[
    "p",
    "br",
    "div",
    "li",
    "tr",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "blockquote",
    "section",
    "article",
    "header",
    "footer",
    "ul",
    "ol",
    "table",
    "pre",
];

/// Elements whose entire body is non-content chrome — dropped wholesale (like script/style).
const SKIP_TAGS: &[&str] = &[
    "script",
    "style",
    "nav",
    "header",
    "footer",
    "aside",
    "form",
    "button",
    "figure",
    "figcaption",
    "noscript",
    "svg",
    "select",
    "label",
];

/// Blocks shorter than this are almost always chrome (Share · Subscribe · "3" · "Jun 21" · nav
/// links), not article prose — dropped. Real paragraphs comfortably exceed it.
const MIN_BLOCK_CHARS: usize = 40;

/// The exact markup the extractor emits for a display equation, and that the assembler scans back
/// for ([`crate::epub::embed_math`]): `MATH_IMG_OPEN` + the escaped LaTeX + `MATH_IMG_CLOSE`, wrapped
/// in a `<p class="eq">`. Shared so the two ends cannot drift. Because `>` and `"` are escaped inside
/// the `alt`, `MATH_IMG_CLOSE` first appears at the tag's close, which lets the scan skip a full parse.
pub(crate) const MATH_IMG_OPEN: &str = "<img class=\"latex\" alt=\"";
pub(crate) const MATH_IMG_CLOSE: &str = "\"/>";

/// Extract the main readable body of `html` as well-formed XHTML (one `<p>` per real text block).
///
/// Returns an **empty** string when the page carries no extractable prose (an interactive page, a
/// link-only feed entry). The caller decides what to show instead; see [`summary_fallback`].
#[must_use]
pub fn extract_readable(html: &str) -> String {
    let mut out = String::new();
    let mut last = String::new();
    for blk in to_blocks(html) {
        let block = match blk {
            Blk::Code(raw) => {
                // A code/command block: keep every line. HTML ignores one newline after <pre>; drop it
                // and the trailing whitespace, then emit verbatim (escaped, newlines kept) so the reflow
                // engine sets it monospace. The prose filters below do not apply — short code is valid
                // and a listing never ends the article.
                let code = raw.strip_prefix('\n').unwrap_or(&raw).trim_end();
                if !code.is_empty() {
                    out.push_str("<pre>");
                    out.push_str(&escape(code));
                    out.push_str("</pre>\n");
                }
                continue;
            }
            Blk::Math(latex) => {
                // A display equation: keep it as a math image the assembler renders to a PNG and
                // embeds (the reflow engine has no math typesetting). The LaTeX rides in the `alt`
                // — it is the assembler's render source, and the visible fallback if a formula does
                // not render. Like code, it bypasses the prose filters: an equation is short and
                // never ends the article.
                out.push_str("<p class=\"eq\">");
                out.push_str(MATH_IMG_OPEN);
                out.push_str(&escape(&latex));
                out.push_str(MATH_IMG_CLOSE);
                out.push_str("</p>\n");
                continue;
            }
            Blk::Prose(block) => block,
        };
        let t = collapse_ws(&strip_zero_width(&block));
        // Newsletter / related-stories boilerplate and an email signature mark the end of the article
        // — drop them and everything after (the "Follow topics…" CTA, a "More from …" link list, or a
        // mailing-list footer under a signature). Checked before the length filter so a short marker
        // (a bare "-- " signature line) still ends the article rather than being skipped as too short.
        if is_tail_boilerplate(&t) {
            break;
        }
        if t.chars().count() < MIN_BLOCK_CHARS || is_diagram_source(&t) || is_author_bio(&t) {
            continue;
        }
        // Mailing-list archive chrome (openwall/oss-security and the like): the email-header block,
        // the bracketed prev/next nav, and the "Follow us on …" call to action all pass the length
        // filter but are not article prose.
        if is_email_headers(&t) || is_bracket_nav(&t) || is_social_cta(&t) {
            continue;
        }
        if t == last {
            continue; // drop a duplicated consecutive paragraph (e.g. a caption repeated)
        }
        last.clone_from(&t);
        out.push_str("<p>");
        out.push_str(&escape(&t));
        out.push_str("</p>\n");
    }
    out
}

/// The body to show when [`extract_readable`] found no prose: the feed's own `description`/`summary`
/// rendered as one clean paragraph (#198), or a plain note when the feed gave no summary either.
///
/// The summary is the publisher's own line about the piece, so it is a better fallback than a dead
/// placeholder. It may carry markup and entities, so it is reduced to plain text and re-escaped; the
/// extractor's length and boilerplate filters are deliberately *not* applied, since a short summary
/// is exactly what we want to keep here.
#[must_use]
pub(crate) fn summary_fallback(summary: Option<&str>) -> String {
    if let Some(text) = summary
        .map(|s| collapse_ws(&crate::model::strip_markup(s)))
        .filter(|s| !s.is_empty())
    {
        return format!("<p>{}</p>\n", escape(&text));
    }
    "<p>(No readable text could be extracted. Open the original to read it.)</p>".to_string()
}

/// One extracted block: ordinary prose (whitespace-collapsed, emitted as a `<p>`), a preformatted
/// code/command block (lines kept, emitted as a `<pre>` the reflow engine sets monospace), or a
/// display equation (its LaTeX, emitted as a math `<img>` the assembler renders and embeds).
enum Blk {
    Prose(String),
    Code(String),
    Math(String),
}

/// A `<pre>` is code — not a plain-text mailing-list post — when it wraps a `<code>` element or
/// carries a syntax-highlighting class. Code keeps its lines; a bare `<pre>` of wrapped prose is split
/// into paragraphs by [`split_pre_paragraphs`] as before.
fn is_code_pre(pre: NodeRef<Node>) -> bool {
    fn has_code(node: NodeRef<Node>) -> bool {
        node.children().any(|c| match c.value() {
            Node::Element(el) => el.name() == "code" || has_code(c),
            _ => false,
        })
    }
    if let Node::Element(el) = pre.value() {
        if el.attr("class").is_some_and(|c| {
            let c = c.to_ascii_lowercase();
            c.contains("language-")
                || c.contains("highlight")
                || c.contains("sourcecode")
                || c.contains("hljs")
                || c.contains("codehilite")
        }) {
            return true;
        }
    }
    has_code(pre)
}

/// The LaTeX of a WordPress-style math image, or `None` for an ordinary illustration. WordPress
/// (and the common `latex.php` renderers) ship an equation as `<img class="latex" alt="…LaTeX…">`,
/// so the formula is in the `alt` attribute; a `latex.php`/`/latex/` src is the same thing served
/// by other engines. An ordinary `<img>` returns `None` and is dropped as before.
fn math_latex(el: &scraper::node::Element) -> Option<String> {
    let is_math = el.attr("class").is_some_and(|c| {
        c.to_ascii_lowercase()
            .split_whitespace()
            .any(|w| w == "latex")
    }) || el
        .attr("src")
        .is_some_and(|s| s.contains("latex.php") || s.contains("/latex/"));
    if !is_math {
        return None;
    }
    el.attr("alt")
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(str::to_string)
}

/// A display (block) equation is marked `\displaystyle` by WordPress; an inline formula is not. Only
/// display equations become their own math image; inline formulas are rendered to a readable Unicode
/// approximation in the running text, since the plain-text body cannot carry an image mid-sentence.
fn is_display_math(latex: &str) -> bool {
    latex.contains("displaystyle")
}

/// Render an inline LaTeX formula to a readable Unicode approximation for the plain-text body.
///
/// The body is plain text with no place for an inline image, so an inline formula is approximated:
/// Greek letters, super/subscripts, `\sqrt`, `\frac`, blackboard sets and the common operators map
/// to Unicode. Crucially, anything unrecognised has its LaTeX commands and braces **stripped**, so a
/// formula never leaks a backslash or a brace onto the page (which reads as a rendering bug). Imperfect
/// for complex maths by design: display equations, where the maths is load-bearing, render as images.
fn latex_to_unicode(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    collapse_ws(&convert_latex(&chars))
}

/// Convert a LaTeX char slice to Unicode text (recursive for group arguments).
fn convert_latex(chars: &[char]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' => {
                let mut j = i + 1;
                while j < chars.len() && chars[j].is_ascii_alphabetic() {
                    j += 1;
                }
                if j > i + 1 {
                    i = apply_latex_command(
                        &chars[i + 1..j].iter().collect::<String>(),
                        chars,
                        j,
                        &mut out,
                    );
                } else if j < chars.len() {
                    match chars[j] {
                        '{' | '}' | '%' | '$' | '#' | '&' | '_' => out.push(chars[j]),
                        ',' | ';' | ' ' | '!' | ':' => out.push(' '),
                        _ => {}
                    }
                    i = j + 1;
                } else {
                    i = j;
                }
            }
            '^' => {
                let (g, next) = latex_group(chars, i + 1);
                out.push_str(&to_script(&convert_latex(&g), true));
                i = next;
            }
            '_' => {
                let (g, next) = latex_group(chars, i + 1);
                out.push_str(&to_script(&convert_latex(&g), false));
                i = next;
            }
            '{' | '}' | '$' => i += 1,
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Handle a `\command` starting after its name (at `after`), writing to `out`; returns the new index.
fn apply_latex_command(name: &str, chars: &[char], after: usize, out: &mut String) -> usize {
    match name {
        "frac" | "tfrac" | "dfrac" => {
            let (num, a) = latex_group(chars, after);
            let (den, b) = latex_group(chars, a);
            out.push_str(&paren_if_compound(&convert_latex(&num)));
            out.push('/');
            out.push_str(&paren_if_compound(&convert_latex(&den)));
            b
        }
        "sqrt" => {
            let (g, a) = latex_group(chars, after);
            out.push('√');
            out.push_str(&paren_if_compound(&convert_latex(&g)));
            a
        }
        "text" | "mathrm" | "mathsf" | "mathbf" | "mathit" | "boldsymbol" | "operatorname"
        | "mbox" => {
            let (g, a) = latex_group(chars, after);
            out.push_str(&convert_latex(&g));
            a
        }
        "mathbb" | "mathcal" | "mathscr" | "mathfrak" => {
            let (g, a) = latex_group(chars, after);
            out.push_str(&blackboard(&convert_latex(&g)));
            a
        }
        other => {
            out.push_str(latex_symbol(other));
            after
        }
    }
}

/// Read a group argument at `start`: a `{...}` (returning its inner chars) or a single token
/// (one char, or a `\command`), skipping any leading spaces. Returns the chars and the next index.
fn latex_group(chars: &[char], mut i: usize) -> (Vec<char>, usize) {
    while i < chars.len() && chars[i] == ' ' {
        i += 1;
    }
    if i >= chars.len() {
        return (Vec::new(), i);
    }
    if chars[i] == '{' {
        let (start, mut depth, mut j) = (i + 1, 1i32, i + 1);
        while j < chars.len() && depth > 0 {
            match chars[j] {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            j += 1;
        }
        (chars[start..j].to_vec(), (j + 1).min(chars.len()))
    } else if chars[i] == '\\' {
        let mut j = i + 1;
        while j < chars.len() && chars[j].is_ascii_alphabetic() {
            j += 1;
        }
        let end = j.max(i + 2).min(chars.len());
        (chars[i..end].to_vec(), end)
    } else {
        (vec![chars[i]], i + 1)
    }
}

/// Wrap `s` in parentheses when it is a compound expression (has a space or an operator), so a
/// fraction or root reads unambiguously; a single symbol is left bare.
fn paren_if_compound(s: &str) -> String {
    let compound = s.chars().count() > 1
        && s.chars()
            .any(|c| matches!(c, ' ' | '+' | '-' | '/' | '×' | '·' | '±' | '=' | '<' | '>'));
    if compound {
        format!("({s})")
    } else {
        s.to_string()
    }
}

/// Map a string to super- or subscript Unicode where each character has one; characters without a
/// script form are kept as-is (so `x^{2}` → `x²`, `a_{ij}` → `aᵢⱼ`, `n^{k+1}` → `nᵏ⁺¹` best-effort).
fn to_script(s: &str, sup: bool) -> String {
    s.chars()
        .map(|c| script_char(c, sup).unwrap_or(c))
        .collect()
}

fn script_char(c: char, sup: bool) -> Option<char> {
    if sup {
        Some(match c {
            '0' => '⁰',
            '1' => '¹',
            '2' => '²',
            '3' => '³',
            '4' => '⁴',
            '5' => '⁵',
            '6' => '⁶',
            '7' => '⁷',
            '8' => '⁸',
            '9' => '⁹',
            '+' => '⁺',
            '-' => '⁻',
            '=' => '⁼',
            '(' => '⁽',
            ')' => '⁾',
            'n' => 'ⁿ',
            'i' => 'ⁱ',
            'a' => 'ᵃ',
            'b' => 'ᵇ',
            'c' => 'ᶜ',
            'k' => 'ᵏ',
            'm' => 'ᵐ',
            'x' => 'ˣ',
            'T' => 'ᵀ',
            _ => return None,
        })
    } else {
        Some(match c {
            '0' => '₀',
            '1' => '₁',
            '2' => '₂',
            '3' => '₃',
            '4' => '₄',
            '5' => '₅',
            '6' => '₆',
            '7' => '₇',
            '8' => '₈',
            '9' => '₉',
            '+' => '₊',
            '-' => '₋',
            '=' => '₌',
            '(' => '₍',
            ')' => '₎',
            'a' => 'ₐ',
            'e' => 'ₑ',
            'i' => 'ᵢ',
            'j' => 'ⱼ',
            'n' => 'ₙ',
            'o' => 'ₒ',
            'x' => 'ₓ',
            'k' => 'ₖ',
            'm' => 'ₘ',
            _ => return None,
        })
    }
}

/// Map a single blackboard/caligraphic letter to its Unicode double-struck form (R → ℝ, …); other
/// text is kept as-is.
fn blackboard(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'R' => 'ℝ',
            'N' => 'ℕ',
            'Z' => 'ℤ',
            'Q' => 'ℚ',
            'C' => 'ℂ',
            'P' => 'ℙ',
            'H' => 'ℍ',
            // 𝕋/𝔼/𝔽 live only in the Mathematical Alphanumeric block, which the devices' system
            // fonts do not cover (no Noto Math face), so leave the plain letter, not a tofu box.
            _ => c,
        })
        .collect()
}

/// The Unicode for a LaTeX control word, or `""` to drop a layout-only command (`\displaystyle`,
/// `\left`, …) and any command we do not model (so nothing leaks to the page).
fn latex_symbol(name: &str) -> &'static str {
    match name {
        // Lowercase Greek.
        "alpha" => "α",
        "beta" => "β",
        "gamma" => "γ",
        "delta" => "δ",
        "epsilon" => "ε",
        "varepsilon" => "ε",
        "zeta" => "ζ",
        "eta" => "η",
        "theta" => "θ",
        "vartheta" => "ϑ",
        "iota" => "ι",
        "kappa" => "κ",
        "lambda" => "λ",
        "mu" => "μ",
        "nu" => "ν",
        "xi" => "ξ",
        "pi" => "π",
        "varpi" => "ϖ",
        "rho" => "ρ",
        "varrho" => "ϱ",
        "sigma" => "σ",
        "tau" => "τ",
        "upsilon" => "υ",
        "phi" => "φ",
        "varphi" => "φ",
        "chi" => "χ",
        "psi" => "ψ",
        "omega" => "ω",
        // Uppercase Greek.
        "Gamma" => "Γ",
        "Delta" => "Δ",
        "Theta" => "Θ",
        "Lambda" => "Λ",
        "Xi" => "Ξ",
        "Pi" => "Π",
        "Sigma" => "Σ",
        "Upsilon" => "Υ",
        "Phi" => "Φ",
        "Psi" => "Ψ",
        "Omega" => "Ω",
        // Relations and operators.
        "times" => "×",
        "cdot" => "·",
        "div" => "÷",
        "pm" => "±",
        "mp" => "∓",
        "ast" => "∗",
        "star" => "⋆",
        "circ" => "∘",
        "bullet" => "•",
        "oplus" => "⊕",
        "otimes" => "⊗",
        "leq" | "le" => "≤",
        "geq" | "ge" => "≥",
        "neq" | "ne" => "≠",
        "approx" => "≈",
        "equiv" => "≡",
        "cong" => "≅",
        "sim" => "∼",
        "simeq" => "≃",
        "propto" => "∝",
        "ll" => "≪",
        "gg" => "≫",
        "subset" => "⊂",
        "subseteq" => "⊆",
        "supset" => "⊃",
        "supseteq" => "⊇",
        "in" => "∈",
        "notin" => "∉",
        "ni" => "∋",
        "cup" => "∪",
        "cap" => "∩",
        "setminus" => "∖",
        "emptyset" | "varnothing" => "∅",
        "forall" => "∀",
        "exists" => "∃",
        "nabla" => "∇",
        "partial" => "∂",
        "infty" => "∞",
        "sum" => "∑",
        "prod" => "∏",
        "int" => "∫",
        "oint" => "∮",
        "sqrt" => "√",
        "angle" => "∠",
        "perp" => "⊥",
        "parallel" => "∥",
        // Arrows.
        "to" | "rightarrow" => "→",
        "leftarrow" => "←",
        "leftrightarrow" => "↔",
        "Rightarrow" => "⇒",
        "Leftarrow" => "⇐",
        "Leftrightarrow" => "⇔",
        "mapsto" => "↦",
        "implies" => "⇒",
        "iff" => "⇔",
        "uparrow" => "↑",
        "downarrow" => "↓",
        // Dots and spacing-ish.
        "ldots" | "dots" => "…",
        "cdots" => "⋯",
        "vdots" => "⋮",
        "ddots" => "⋱",
        "quad" => " ",
        "qquad" => "  ",
        "prime" => "′",
        // Common functions kept as words.
        "sin" => "sin",
        "cos" => "cos",
        "tan" => "tan",
        "log" => "log",
        "ln" => "ln",
        "exp" => "exp",
        "lim" => "lim",
        "max" => "max",
        "min" => "min",
        "det" => "det",
        "deg" => "deg",
        "gcd" => "gcd",
        "dim" => "dim",
        // Layout-only / unmodelled: drop so nothing leaks.
        _ => "",
    }
}

/// Split `html` into blocks. Parses with html5ever, narrows to the article body, then walks the tree:
/// text accumulates into the current block, [`BLOCK_TAGS`] elements end it, [`SKIP_TAGS`] subtrees are
/// dropped, and a code `<pre>` becomes a [`Blk::Code`]. Comments/CDATA/doctype nodes are never text, so
/// they cannot leak. The parser already decodes entities in text nodes, so no manual pass is needed.
fn to_blocks(html: &str) -> Vec<Blk> {
    let doc = Html::parse_document(html);
    let start = main_node(doc.tree.root());
    let mut blocks = Vec::new();
    let mut cur = String::new();
    walk(start, &mut cur, &mut blocks);
    if !cur.trim().is_empty() {
        blocks.push(Blk::Prose(cur));
    }
    blocks
}

/// Narrow to the article body when the page marks it up (`<article>` then `<main>`), else `<body>`
/// (or the root). Only trusts the region if it actually carries prose — some pages ship an empty
/// `<main>` shell. Avoids pulling in the surrounding nav/sidebar/footer.
fn main_node(root: NodeRef<Node>) -> NodeRef<Node> {
    for tag in ["article", "main"] {
        if let Some(n) = find_element(root, tag) {
            if text_len(n) > 200 {
                return n;
            }
        }
    }
    find_element(root, "body").unwrap_or(root)
}

/// Depth-first search for the first element named `name`.
fn find_element<'a>(node: NodeRef<'a, Node>, name: &str) -> Option<NodeRef<'a, Node>> {
    for child in node.children() {
        if let Node::Element(el) = child.value() {
            if el.name() == name {
                return Some(child);
            }
        }
        if let Some(found) = find_element(child, name) {
            return Some(found);
        }
    }
    None
}

/// Total length of all descendant text under `node` (used to tell a prose region from an empty shell).
fn text_len(node: NodeRef<Node>) -> usize {
    ElementRef::wrap(node)
        .map(|e| e.text().map(str::len).sum())
        .unwrap_or(0)
}

/// Walk block-level structure under `node`, emitting block text into `blocks`. Loose inline text
/// accumulates in `cur` and flushes at each block boundary; [`SKIP_TAGS`] subtrees are skipped whole.
fn walk(node: NodeRef<Node>, cur: &mut String, blocks: &mut Vec<Blk>) {
    for child in node.children() {
        match child.value() {
            Node::Text(t) => cur.push_str(t),
            Node::Element(el) => {
                let name = el.name();
                if SKIP_TAGS.contains(&name) {
                    continue; // chrome (script/style/nav/…) — drop the whole subtree
                }
                if name == "img" {
                    if let Some(latex) = math_latex(el) {
                        if is_display_math(&latex) {
                            // A block equation: end the running paragraph and keep it as its own
                            // math block for the assembler to render and embed.
                            if !cur.trim().is_empty() {
                                blocks.push(Blk::Prose(std::mem::take(cur)));
                            }
                            blocks.push(Blk::Math(latex));
                        } else {
                            // Inline math: the plain-text body has no place for an image mid-line,
                            // so render the formula to a readable Unicode approximation (never raw
                            // LaTeX, which would leak backslashes and braces onto the page).
                            let text = latex_to_unicode(&latex);
                            if !text.is_empty() {
                                cur.push(' ');
                                cur.push_str(&text);
                                cur.push(' ');
                            }
                        }
                    }
                    continue; // a non-math image carries no readable text — drop it as before
                }
                if name == "pre" {
                    if !cur.trim().is_empty() {
                        blocks.push(Blk::Prose(std::mem::take(cur)));
                    }
                    let text: String = ElementRef::wrap(child)
                        .map(|e| e.text().collect())
                        .unwrap_or_default();
                    if is_code_pre(child) {
                        // Genuine code/commands: every line is significant, so keep the block whole and
                        // let the reflow engine set it monospace line-for-line.
                        blocks.push(Blk::Code(text));
                    } else {
                        // A plain-text <pre> (mailing-list post) carries its structure as newlines, not
                        // markup. Split it on blank lines before the whitespace collapse, so the body
                        // keeps its paragraphs instead of flattening into one run-on block with the email
                        // headers merged in.
                        blocks.extend(split_pre_paragraphs(&text).into_iter().map(Blk::Prose));
                    }
                    continue;
                }
                let is_block = BLOCK_TAGS.contains(&name);
                if is_block && !cur.trim().is_empty() {
                    blocks.push(Blk::Prose(std::mem::take(cur)));
                }
                walk(child, cur, blocks);
                if is_block && !cur.trim().is_empty() {
                    blocks.push(Blk::Prose(std::mem::take(cur)));
                }
            }
            _ => {} // Comment / Doctype / ProcessingInstruction — never content
        }
    }
}

/// Decode named + numeric HTML entities to text (so `&amp;`, `&#x27;`, `&#8217;` become `& ' '`).
pub(crate) fn decode_entities(s: &str) -> String {
    let named = s
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
        .replace("&rsquo;", "\u{2019}")
        .replace("&lsquo;", "\u{2018}")
        .replace("&ldquo;", "\u{201C}")
        .replace("&rdquo;", "\u{201D}")
        .replace("&hellip;", "…");
    decode_numeric(&named)
}

/// Decode numeric entities `&#NN;` and `&#xHH;`.
fn decode_numeric(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    loop {
        match rest.find("&#") {
            None => {
                out.push_str(rest);
                break;
            }
            Some(i) => {
                out.push_str(&rest[..i]);
                let tail = &rest[i + 2..];
                match tail.find(';') {
                    Some(semi) => {
                        let code = &tail[..semi];
                        let parsed = code
                            .strip_prefix(['x', 'X'])
                            .and_then(|h| u32::from_str_radix(h, 16).ok())
                            .or_else(|| code.parse::<u32>().ok());
                        match parsed.and_then(char::from_u32) {
                            Some(c) => {
                                out.push(c);
                                rest = &tail[semi + 1..];
                            }
                            None => {
                                out.push_str("&#");
                                rest = tail;
                            }
                        }
                    }
                    None => {
                        out.push_str("&#");
                        rest = tail;
                    }
                }
            }
        }
    }
    out
}

/// Strip zero-width / BOM characters that render as stray boxes ("8") on e-ink.
fn strip_zero_width(s: &str) -> String {
    s.chars()
        .filter(|&c| {
            !matches!(
                c,
                '\u{feff}' | '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{2060}'
            )
        })
        .collect()
}

/// Whether a block begins the page's end-matter (newsletter CTA / related-stories list) — once hit,
/// the rest of the page is dropped. Matched on a lowercased prefix to avoid mid-article false hits.
fn is_tail_boilerplate(t: &str) -> bool {
    let lower = t.to_ascii_lowercase();
    const MARKERS: &[&str] = &[
        "follow topics and authors",
        "more from",
        "most popular",
        "related stories",
        "related reading",
        "sign up for the",
        "subscribe to our",
        "read more:",
        "share this", // WordPress/Jetpack end-of-post widget ("Share this:"); also "share this story"
        "comments (",
    ];
    // "-- " opening a block is the email signature delimiter (RFC 3676): the signature, and any
    // mailing-list footer appended after it, are not article prose.
    MARKERS.iter().any(|m| lower.starts_with(m)) || t.starts_with("-- ")
}

/// Split preformatted text into paragraphs on blank lines, joining the soft-wrapped lines within
/// each. Without this a `<pre>` collapses into one run-on paragraph and its email headers merge into
/// the body (mailing-list archives). The resulting blocks still pass [`extract_readable`]'s length
/// and chrome filters, so a short header or a signature line is dropped there.
fn split_pre_paragraphs(text: &str) -> Vec<String> {
    let mut paras = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            if !cur.is_empty() {
                paras.push(cur.join(" "));
                cur.clear();
            }
        } else {
            cur.push(line.trim());
        }
    }
    if !cur.is_empty() {
        paras.push(cur.join(" "));
    }
    paras
}

/// Email header keys that mark a mailing-list archive's header block rather than article prose.
const EMAIL_HEADERS: &[&str] = &[
    "message-id:",
    "date:",
    "from:",
    "to:",
    "cc:",
    "subject:",
    "in-reply-to:",
    "references:",
    "reply-to:",
    "sender:",
    "list-id:",
    "precedence:",
    "mime-version:",
    "content-type:",
];

/// Whether a block is a mailing-list archive's email-header block: it opens with a header key and
/// carries at least two. A real paragraph that merely mentions one ("Date: to be confirmed") is not
/// matched, which keeps prose safe.
fn is_email_headers(t: &str) -> bool {
    let l = t.to_ascii_lowercase();
    EMAIL_HEADERS.iter().any(|h| l.starts_with(h))
        && EMAIL_HEADERS.iter().filter(|h| l.contains(*h)).count() >= 2
}

/// Whether a block is a row of bracketed nav links ("[<prev] [next>] [day] [month] [year] [list]"),
/// the archive chrome around a post.
fn is_bracket_nav(t: &str) -> bool {
    t.trim_start().starts_with('[') && t.matches('[').count() >= 3
}

/// Whether a block is a "follow us on <platform>" call to action (site chrome, not prose).
fn is_social_cta(t: &str) -> bool {
    let l = t.to_ascii_lowercase();
    l.starts_with("follow ")
        && [
            "twitter",
            " on x",
            "facebook",
            "instagram",
            "mastodon",
            "bluesky",
        ]
        .iter()
        .any(|p| l.contains(p))
}

/// Whether a short block reads as an author byline/bio ("X is a senior reporter … joined …") rather
/// than article prose. Conservative: needs the "is a", a journalism role, and a bio cue, and stays
/// short — so real sentences ("Linus is a software engineer who created Linux") don't match.
fn is_author_bio(t: &str) -> bool {
    if t.len() >= 400 {
        return false;
    }
    let l = t.to_ascii_lowercase();
    let role = [
        "reporter",
        "writer",
        "editor",
        "journalist",
        "correspondent",
        "columnist",
    ]
    .iter()
    .any(|r| l.contains(r));
    let cue = [
        "joined",
        "covers",
        "covering",
        "previously",
        "is a senior",
        "based in",
    ]
    .iter()
    .any(|c| l.contains(c));
    l.contains(" is a ") && role && cue
}

/// Whether a block is Mermaid/diagram source (which flattens to unreadable gibberish in prose).
fn is_diagram_source(t: &str) -> bool {
    let head = t.split_whitespace().next().unwrap_or("");
    matches!(
        head,
        "flowchart"
            | "sequenceDiagram"
            | "gantt"
            | "classDiagram"
            | "stateDiagram"
            | "stateDiagram-v2"
            | "erDiagram"
            | "journey"
            | "mindmap"
            | "gitGraph"
            | "pie"
    ) || ["graph TD", "graph LR", "graph RL", "graph BT"]
        .iter()
        .any(|g| t.starts_with(g))
}

/// Collapse runs of whitespace to single spaces and trim.
pub(crate) fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Escape the five XML metacharacters so extracted text is well-formed inside the XHTML body.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolates_article_and_drops_chrome_and_short_blocks() {
        let html = "<html><body>\
            <nav><a href='/'>Home</a><a href='/about'>About</a></nav>\
            <header>Subscribe Sign in</header>\
            <article><h1>The Headline</h1>\
              <p>This is a genuine paragraph of article prose that is plainly long enough to read.</p>\
              <div>Share</div>\
              <p>A second substantial paragraph follows, also comfortably past the minimum length.</p>\
            </article>\
            <footer>Copyright junk and more links here</footer></body></html>";
        let out = extract_readable(html);
        assert!(out.contains("genuine paragraph of article prose"), "{out}");
        assert!(out.contains("second substantial paragraph"), "{out}");
        assert!(!out.contains("Subscribe"), "header chrome dropped: {out}");
        assert!(
            !out.contains(">Share<") && !out.contains("Share</p>"),
            "short block dropped: {out}"
        );
        assert!(
            !out.contains("Home") && !out.contains("Copyright"),
            "nav/footer dropped: {out}"
        );
    }

    #[test]
    fn mailing_list_archive_drops_chrome_headers_and_keeps_paragraphs() {
        // An openwall/oss-security-style archive page: a follow CTA, a bracketed nav row, then the
        // email in a <pre> whose structure is newlines (headers, then blank-line-separated body).
        let html = "<html><body>\
            <p>Follow @Example on Twitter for new release announcements and other news here</p>\
            <p>[&lt;prev] [next&gt;] [day] [month] [year] [list]</p>\
            <pre>\nMessage-ID: &lt;abc@host&gt;\nDate: Wed, 25 Nov 2026 17:26:38 +0100\n\
From: A Person &lt;a@host&gt;\nTo: list@host\nSubject: The Real Subject\n\nHello,\n\n\
This is the first real paragraph of the message, long enough to read comfortably.\n\
It is soft-wrapped across two source lines.\n\n\
And this is a second real paragraph that also comfortably exceeds the minimum length.\n\n\
-- A Person, somewhere\n\n\
Please check out the Open Source Software Security Wiki, a mailing-list footer to drop.\n</pre>\
            </body></html>";
        let out = extract_readable(html);
        assert!(out.contains("first real paragraph of the message"), "{out}");
        assert!(out.contains("second real paragraph"), "{out}");
        // The signature delimiter and the footer after it are dropped.
        assert!(
            !out.contains("A Person, somewhere"),
            "signature dropped: {out}"
        );
        assert!(
            !out.contains("mailing-list footer"),
            "footer after signature dropped: {out}"
        );
        // Soft-wrapped lines within a paragraph are joined.
        assert!(
            out.contains("read comfortably. It is soft-wrapped"),
            "soft wrap joined: {out}"
        );
        // The two real paragraphs stay separate blocks (blank-line split, not one run-on).
        assert!(out.matches("<p>").count() >= 2, "paragraphs split: {out}");
        // Chrome is gone: headers, nav, CTA, and the bare greeting.
        assert!(
            !out.contains("Message-ID") && !out.contains("Subject"),
            "headers dropped: {out}"
        );
        assert!(
            !out.to_lowercase().contains("follow @example"),
            "CTA dropped: {out}"
        );
        assert!(!out.contains("[next"), "bracket nav dropped: {out}");
        assert!(!out.contains("Hello,"), "bare greeting dropped: {out}");
    }

    #[test]
    fn chrome_filters_spare_real_prose() {
        // The chrome detectors must not fire on ordinary sentences.
        assert!(!is_email_headers(
            "From the beginning the project aimed to date the artifacts precisely."
        ));
        assert!(!is_bracket_nav(
            "The array was initialised as [1, 2, 3] in the example we studied."
        ));
        assert!(!is_social_cta(
            "Follow the money and you will usually find the real motive behind it."
        ));
        // But the actual chrome is caught.
        assert!(is_email_headers("Message-ID: <x> Date: y From: z"));
        assert!(is_bracket_nav("[<prev] [next>] [day] [month]"));
        assert!(is_social_cta("Follow @Example on Twitter for updates"));
    }

    #[test]
    fn html_comments_with_inner_gt_are_stripped_not_leaked() {
        // An HTML comment whose body contains `>` (arrows / conditional comments) must not leak —
        // the regression behind the "->->-> Top Sources: None -->" artifact in the reader.
        let html = "<article>\
            <!-- ->->->->->->-> Top Sources: None -->\
            <p>Zuckerberg's increasingly bizarre war on whistleblowers begins here, plainly.</p>\
            <!--[if gt IE 8]><p>conditional-comment junk</p><![endif]-->\
            <p>And a second real paragraph of prose, comfortably past the minimum length.</p>\
            </article>";
        let out = extract_readable(html);
        assert!(out.contains("bizarre war on whistleblowers"), "{out}");
        assert!(out.contains("second real paragraph"), "{out}");
        assert!(!out.contains("Top Sources"), "comment text leaked: {out}");
        assert!(
            !out.contains("-->") && !out.contains("->->"),
            "arrows leaked: {out}"
        );
        assert!(
            !out.contains("conditional-comment"),
            "conditional comment leaked: {out}"
        );
    }

    #[test]
    fn decodes_named_and_numeric_entities() {
        assert_eq!(
            decode_entities("children&#x27;s &amp; co &#8212; done"),
            "children's & co — done"
        );
        // And in extracted prose, entities are decoded then re-escaped to well-formed XHTML.
        let out = extract_readable("<article><p>children&#x27;s books are a body-horror genre apparently here</p></article>");
        assert!(
            out.contains("children&apos;s") || out.contains("children's"),
            "{out}"
        );
    }

    #[test]
    fn drops_bom_dups_bio_and_tail_boilerplate() {
        let html = "<article>\
            <p>\u{feff}To make it easier to play games, one side shows a virtual gamepad with controls.</p>\
            <p>To make it easier to play games, one side shows a virtual gamepad with controls.</p>\
            <p>Jay Peters is a senior reporter covering technology. He joined The Verge in 2019.</p>\
            <p>Android 17 is getting a dedicated gaming mode for foldables, a genuinely useful feature.</p>\
            <p>Follow topics and authors from this story to see more like this in your feed.</p>\
            <p>I drove the Slate Truck — there's more to it than EV minimalism</p>\
            </article>";
        let out = extract_readable(html);
        assert!(!out.contains('\u{feff}'), "BOM stripped");
        assert_eq!(
            out.matches("one side shows a virtual gamepad").count(),
            1,
            "dedup: {out}"
        );
        assert!(
            !out.contains("senior reporter"),
            "author bio dropped: {out}"
        );
        assert!(
            out.contains("Android 17 is getting"),
            "real prose kept: {out}"
        );
        assert!(
            !out.contains("Follow topics"),
            "newsletter CTA dropped: {out}"
        );
        assert!(
            !out.contains("Slate Truck"),
            "trailing related link dropped: {out}"
        );
    }

    #[test]
    fn a_wordpress_share_widget_ends_the_article() {
        // A WordPress/Jetpack post with no <article>/<main> wrapper falls back to <body>, so the
        // trailing share widget ("Share this:"), reader comments, next/prev nav and footer would
        // otherwise flow in as prose. The "Share this:" heading marks the end of the post: drop it
        // and everything after it, keeping only the real body.
        let html = "<body><div class=\"post-content\">\
            <p>The department invites applications for a tenure-track faculty position in mathematics.</p>\
            <div class=\"sharedaddy sd-sharing-enabled\"><h3 class=\"sd-title\">Share this:</h3>\
              <a href=\"#\">Share on WhatsApp (Opens in new window) WhatsApp</a>\
              <a href=\"#\">Share on Bluesky (Opens in new window) Bluesky</a></div>\
            <div class=\"comments\"><p>One consequence of the Riemann hypothesis is polynomial-time testing.</p></div>\
            <p>Blog at WordPress.com. Ben Eastaugh and Chris Sternal-Johnson.</p>\
            </div></body>";
        let out = extract_readable(html);
        assert!(
            out.contains("tenure-track faculty position"),
            "real body kept: {out}"
        );
        assert!(
            !out.contains("Opens in new window"),
            "share links dropped: {out}"
        );
        assert!(
            !out.contains("Riemann hypothesis"),
            "reader comments dropped: {out}"
        );
        assert!(!out.contains("WordPress.com"), "footer dropped: {out}");
    }

    #[test]
    fn chrome_only_extracts_empty_then_falls_back_to_the_summary() {
        // A page with only chrome yields no prose: extraction is empty, and the caller falls back.
        let out =
            extract_readable("<html><body><nav>Home About</nav><script>x</script></body></html>");
        assert!(out.is_empty(), "no prose extracts to empty: {out:?}");

        // With a feed summary, the fallback shows it as a clean paragraph: tags stripped and a bare
        // `&` (feed summaries are entity-decoded upstream) escaped exactly once, not doubled.
        let with = summary_fallback(Some("<b>A short</b> feed & blurb."));
        assert!(with.contains("A short feed &amp; blurb."), "{with}");
        assert!(
            !with.contains("&amp;amp;"),
            "escaped once, not twice: {with}"
        );
        assert!(!with.contains("<b>"), "tags stripped: {with}");
        // With no summary, a plain note, and no em dash in either (house style).
        let without = summary_fallback(None);
        assert!(
            without.contains("No readable text could be extracted"),
            "{without}"
        );
        assert!(
            !with.contains('\u{2014}') && !without.contains('\u{2014}'),
            "no em dash in the fallbacks"
        );
    }

    #[test]
    fn a_code_pre_is_kept_whole_with_its_lines() {
        // A <pre><code> block is emitted as a <pre> with its newlines preserved, not collapsed into a
        // run-on paragraph, so the reflow engine can set it monospace line-for-line.
        let html = "<article><p>Here is the smallest program that greets you politely:</p>\
            <pre><code>fn main() {\n    println!(\"hi\");\n}</code></pre>\
            <p>That prints a greeting to standard output on your terminal screen.</p></article>";
        let out = extract_readable(html);
        assert!(
            out.contains("<pre>"),
            "a code block must emit a <pre>: {out}"
        );
        assert!(
            out.contains("fn main() {\n    println!"),
            "the code's newlines and indentation must be preserved: {out:?}",
        );
        assert!(
            out.contains("<p>Here is the smallest program that greets you politely:</p>"),
            "prose around it still emits <p>: {out}",
        );
    }

    #[test]
    fn a_language_classed_pre_without_code_is_still_code() {
        // Syntax-highlight markup often puts the hint on the <pre> class with no <code> child.
        let html = "<article><pre class=\"language-sh highlight\">sudo apt update\nsudo apt upgrade</pre>\
            <p>Those two commands refresh the package index and install the pending upgrades.</p></article>";
        let out = extract_readable(html);
        assert!(
            out.contains("<pre>sudo apt update\nsudo apt upgrade</pre>"),
            "got: {out:?}"
        );
    }

    #[test]
    fn a_display_equation_becomes_a_math_image_marker() {
        // WordPress (Terry Tao) ships a display equation as <img class="latex" alt="\displaystyle …">
        // inside a paragraph. It must survive extraction as a math <img> the assembler can render,
        // carrying its LaTeX in the alt — not vanish the way an ordinary image does.
        let html = "<article>\
            <p>The partial sums of the first N positive integers satisfy the identity</p>\
            <p><img class=\"latex\" alt=\"\\displaystyle \\sum_{n=1}^N n = \\frac{N(N+1)}{2}\" \
               src=\"https://s0.wp.com/latex.php?latex=x\"/></p>\
            <p>which is proved by induction on the number of terms involved here.</p></article>";
        let out = extract_readable(html);
        assert!(
            out.contains("<p class=\"eq\"><img class=\"latex\" alt=\""),
            "a display equation emits a math image: {out}"
        );
        assert!(
            out.contains("\\displaystyle \\sum_{n=1}^N n = \\frac{N(N+1)}{2}"),
            "the LaTeX rides in the alt: {out}"
        );
        assert!(
            out.contains("partial sums of the first") && out.contains("proved by induction"),
            "the prose around it is kept: {out}"
        );
    }

    #[test]
    fn an_inline_equation_renders_to_readable_unicode() {
        // An inline formula (no \displaystyle) cannot be placed mid-line in the plain-text body, so
        // it is rendered to a readable Unicode approximation in the running text, never raw LaTeX.
        let html = "<article><p>We write the golden ratio \
            <img class=\"latex\" alt=\"\\phi = (1+\\sqrt 5)/2\" src=\"/latex/x.png\"/> \
            throughout the remainder of this discussion for brevity.</p></article>";
        let out = extract_readable(html);
        assert!(
            out.contains("golden ratio φ = (1+√5)/2 throughout"),
            "inline math renders to Unicode in the running line: {out}"
        );
        assert!(
            !out.contains('\\') && !out.contains('{') && !out.contains('}'),
            "no LaTeX backslashes or braces leak onto the page: {out}"
        );
        assert!(
            !out.contains("<img"),
            "inline math does not emit an image: {out}"
        );
    }

    #[test]
    fn latex_to_unicode_handles_common_inline_math() {
        assert_eq!(latex_to_unicode("x^2 + y^2 = z^2"), "x² + y² = z²");
        assert_eq!(latex_to_unicode("a_i"), "aᵢ");
        assert_eq!(latex_to_unicode("\\frac{N(N+1)}{2}"), "(N(N+1))/2");
        assert_eq!(latex_to_unicode("\\sqrt{2}"), "√2");
        assert_eq!(latex_to_unicode("O(n \\log n)"), "O(n log n)");
        assert_eq!(latex_to_unicode("\\mathbb{R}^n"), "ℝⁿ");
        assert_eq!(latex_to_unicode("\\alpha \\le \\beta"), "α ≤ β");
        assert_eq!(latex_to_unicode("x \\in \\mathbb{Z}"), "x ∈ ℤ");
    }

    #[test]
    fn latex_to_unicode_never_leaks_backslashes_or_braces() {
        // An unmodelled command and its braces are stripped, not leaked, so nothing reads as a bug.
        for src in [
            "\\displaystyle \\int_0^\\infty e^{-x} dx",
            "\\operatorname{Hom}(A, B)",
            "\\widehat{\\mathcal{F}}_{\\xi}",
            "\\unknowncmd{stuff}",
        ] {
            let out = latex_to_unicode(src);
            assert!(
                !out.contains('\\') && !out.contains('{') && !out.contains('}'),
                "no backslash/brace leaks for {src:?}: got {out:?}"
            );
        }
    }

    #[test]
    fn an_ordinary_image_is_still_dropped() {
        // A plain illustration is not math and carries no prose — extraction drops it as before.
        let html = "<article><p>A chart follows that summarises the quarterly revenue figures here.</p>\
            <p><img class=\"wp-image-123\" alt=\"Revenue by quarter\" src=\"/chart.png\"/></p></article>";
        let out = extract_readable(html);
        assert!(!out.contains("<img"), "a non-math image is dropped: {out}");
        assert!(
            !out.contains("class=\"eq\""),
            "no math block emitted: {out}"
        );
        assert!(
            out.contains("quarterly revenue figures"),
            "prose kept: {out}"
        );
    }

    #[test]
    fn a_bare_text_pre_still_splits_into_paragraphs() {
        // A mailing-list <pre> (no <code>, no language class) keeps the old behaviour: blank-line
        // separated paragraphs, not a monospace block.
        let html = "<article><pre>First paragraph of a plain-text post that is long enough to keep.\n\n\
            Second paragraph of the same plain-text post, also plenty long to survive.</pre></article>";
        let out = extract_readable(html);
        assert!(
            !out.contains("<pre>"),
            "a bare text <pre> must not become a code block: {out}"
        );
        assert_eq!(
            out.matches("<p>").count(),
            2,
            "it splits into two paragraphs: {out}"
        );
    }
}
