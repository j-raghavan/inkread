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

/// Extract the main readable body of `html` as well-formed XHTML (one `<p>` per real text block).
///
/// Returns an **empty** string when the page carries no extractable prose (an interactive page, a
/// link-only feed entry). The caller decides what to show instead; see [`summary_fallback`].
#[must_use]
pub fn extract_readable(html: &str) -> String {
    let mut out = String::new();
    let mut last = String::new();
    for block in to_blocks(html) {
        let t = collapse_ws(&strip_zero_width(&block));
        if t.chars().count() < MIN_BLOCK_CHARS || is_diagram_source(&t) || is_author_bio(&t) {
            continue;
        }
        // Mailing-list archive chrome (openwall/oss-security and the like): the email-header block,
        // the bracketed prev/next nav, and the "Follow us on …" call to action all pass the length
        // filter but are not article prose.
        if is_email_headers(&t) || is_bracket_nav(&t) || is_social_cta(&t) {
            continue;
        }
        // Newsletter / related-stories boilerplate marks the end of the article — drop it + the rest
        // (the "Follow topics…" CTA and the trailing "More from …" link list).
        if is_tail_boilerplate(&t) {
            break;
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

/// Split `html` into text blocks. Parses with html5ever, narrows to the article body, then walks the
/// tree: text accumulates into the current block, [`BLOCK_TAGS`] elements end it, and [`SKIP_TAGS`]
/// subtrees are dropped. Comments/CDATA/doctype nodes are never text, so they cannot leak. The parser
/// already decodes entities in text nodes, so no manual entity pass is needed here.
fn to_blocks(html: &str) -> Vec<String> {
    let doc = Html::parse_document(html);
    let start = main_node(doc.tree.root());
    let mut blocks = Vec::new();
    let mut cur = String::new();
    walk(start, &mut cur, &mut blocks);
    if !cur.trim().is_empty() {
        blocks.push(cur);
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
fn walk(node: NodeRef<Node>, cur: &mut String, blocks: &mut Vec<String>) {
    for child in node.children() {
        match child.value() {
            Node::Text(t) => cur.push_str(t),
            Node::Element(el) => {
                let name = el.name();
                if SKIP_TAGS.contains(&name) {
                    continue; // chrome (script/style/nav/…) — drop the whole subtree
                }
                if name == "pre" {
                    // Preformatted text (mailing-list posts, plain-text articles) carries its
                    // structure as newlines, not markup. Split it on blank lines here, before the
                    // whitespace collapse, so the body keeps its paragraphs instead of flattening into
                    // one run-on block with the email headers merged in.
                    if !cur.trim().is_empty() {
                        blocks.push(std::mem::take(cur));
                    }
                    let text: String = ElementRef::wrap(child)
                        .map(|e| e.text().collect())
                        .unwrap_or_default();
                    blocks.extend(split_pre_paragraphs(&text));
                    continue;
                }
                let is_block = BLOCK_TAGS.contains(&name);
                if is_block && !cur.trim().is_empty() {
                    blocks.push(std::mem::take(cur));
                }
                walk(child, cur, blocks);
                if is_block && !cur.trim().is_empty() {
                    blocks.push(std::mem::take(cur));
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
        "share this story",
        "comments (",
    ];
    MARKERS.iter().any(|m| lower.starts_with(m))
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
And this is a second real paragraph that also comfortably exceeds the minimum length.\n</pre>\
            </body></html>";
        let out = extract_readable(html);
        assert!(out.contains("first real paragraph of the message"), "{out}");
        assert!(out.contains("second real paragraph"), "{out}");
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
}
