//! Minimal readability extraction (#66): raw article HTML → clean, well-formed XHTML paragraphs.
//!
//! Not a full readability port — it drops `<script>`/`<style>` content and all markup, then re-emits
//! the visible block text as **escaped `<p>` elements**. The guarantee that matters: the output is
//! always **well-formed XHTML**, so the assembler can inject it raw without the malformed-body risk
//! the EPUB review flagged. Pure + host-testable. A richer extractor (main-content heuristics) is a
//! later refinement; this gives a calm, readable single column today.

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

/// Extract readable body text from `html` as well-formed XHTML (one `<p>` per text block). Never
/// produces malformed markup; empty input yields a single placeholder paragraph.
#[must_use]
pub fn extract_readable(html: &str) -> String {
    let mut out = String::new();
    for block in to_blocks(html) {
        let t = collapse_ws(&block);
        if !t.is_empty() {
            out.push_str("<p>");
            out.push_str(&escape(&t));
            out.push_str("</p>\n");
        }
    }
    if out.is_empty() {
        out.push_str("<p>(No readable text could be extracted.)</p>");
    }
    out
}

/// Split `html` into text blocks: strip `<script>`/`<style>` bodies and all tags, breaking a block at
/// every block-level tag boundary. Decodes the common HTML entities.
fn to_blocks(html: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut cur = String::new();
    let mut rest = html;
    loop {
        // Text up to the next tag.
        let lt = match rest.find('<') {
            Some(p) => p,
            None => {
                cur.push_str(rest);
                break;
            }
        };
        cur.push_str(&rest[..lt]);
        let after = &rest[lt + 1..];
        let gt = match after.find('>') {
            Some(p) => p,
            None => break, // unterminated tag — stop
        };
        let tag = &after[..gt];
        let mut next = &after[gt + 1..];
        let is_close = tag.starts_with('/');
        let name = tag_name(tag);
        if !is_close && (name == "script" || name == "style") {
            // Skip the element's whole body, up to and including its closing tag.
            let close = format!("</{name}");
            next = match next.to_ascii_lowercase().find(&close) {
                Some(rel) => match next[rel..].find('>') {
                    Some(g) => &next[rel + g + 1..],
                    None => "",
                },
                None => "",
            };
        } else if BLOCK_TAGS.contains(&name.as_str()) && !cur.trim().is_empty() {
            blocks.push(std::mem::take(&mut cur));
        }
        if next.is_empty() {
            break;
        }
        rest = next;
    }
    if !cur.trim().is_empty() {
        blocks.push(cur);
    }
    blocks.into_iter().map(|b| decode_entities(&b)).collect()
}

/// The lowercased element name from a tag's inner text (handles `</p>`, `<p class=…>`, `<br/>`).
fn tag_name(tag: &str) -> String {
    let t = tag.trim_start_matches('/').trim_start();
    t.split(|c: char| c.is_whitespace() || c == '/' || c == '>')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Decode the handful of entities that appear in article text.
fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
        .replace("&hellip;", "…")
}

/// Collapse runs of whitespace to single spaces and trim.
fn collapse_ws(s: &str) -> String {
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
    fn strips_tags_and_scripts_to_clean_paragraphs() {
        let html = "<html><head><style>p{color:red}</style></head><body>\
            <script>evil()</script><h1>Title</h1><p>First &amp; foremost.</p>\
            <div>Second block</div></body></html>";
        let out = extract_readable(html);
        assert!(out.contains("<p>Title</p>"), "{out}");
        assert!(out.contains("<p>First &amp; foremost.</p>"), "{out}");
        assert!(out.contains("<p>Second block</p>"), "{out}");
        assert!(!out.contains("evil"), "script body dropped: {out}");
        assert!(!out.contains("color:red"), "style body dropped: {out}");
    }

    #[test]
    fn output_is_well_formed_and_escaped() {
        // Entity-encoded metacharacters (valid HTML) decode then re-escape, so the body stays
        // well-formed XHTML (no raw markup leaks).
        let out = extract_readable("<p>a &lt; b &amp;&amp; c &gt; d</p>");
        assert!(
            out.contains("&lt;") && out.contains("&amp;") && out.contains("&gt;"),
            "{out}"
        );
        assert!(!out.contains("< b"), "no raw markup leaks: {out}");
    }

    #[test]
    fn empty_input_yields_a_placeholder_paragraph() {
        let out = extract_readable("<html><body><script>x</script></body></html>");
        assert!(out.starts_with("<p>") && out.ends_with("</p>"), "{out}");
    }
}
