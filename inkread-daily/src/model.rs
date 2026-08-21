//! The `inkread-daily` domain model (#66): the data a daily issue is built from.
//!
//! Deliberately tiny. A [`Source`] is a feed/site the user follows; an [`Article`] is one extracted,
//! ready-to-read piece; an [`Issue`] is the compiled set for a day. Fetching feeds happens in the
//! Android shell; this crate parses feeds, extracts readable text, and assembles the EPUB. The types
//! are serde-(de)serializable so the shell can hand the core a fetched issue as JSON over JNI.

use serde::{Deserialize, Serialize};

/// A content source the user follows (RSS/Atom feed or a site). The fetch layer owns the network;
/// this is the persisted identity the shell stores. **Ahead of its consumer:** defined here as the
/// stable model type the fetch/persistence slice will read — assembly attributes articles by the
/// flat [`Article::source`] byline today and does not reference this struct yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    /// Human-facing name shown as the article byline (e.g. "Hacker News").
    pub name: String,
    /// The feed/page URL the fetch layer polls.
    pub url: String,
}

/// One ready-to-read article in an issue: a title, the source it came from, the original URL, an
/// optional published date (already formatted for display), and the **clean** body as simple,
/// well-formed XHTML-compatible markup (paragraphs/headings) — the output of readability extraction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Article {
    /// Article headline.
    pub title: String,
    /// Display name of the originating source (the byline).
    pub source: String,
    /// Canonical article URL (kept for attribution / a future "open original").
    pub url: String,
    /// Pre-formatted published date for display, if known (e.g. "24 Jun 2026").
    pub published: Option<String>,
    /// Clean article body as XHTML-compatible markup (well-formed; readability output).
    pub body_html: String,
    /// The feed's own `description`/`summary` for this entry, when it gave one (#198). Preferred
    /// over a body excerpt: it is what the publisher chose to say the piece is about, where an
    /// excerpt is just its opening words.
    pub summary: Option<String>,
}

impl Article {
    /// A short line describing the article for the contents page (#198), at most `max_chars`.
    ///
    /// Prefers the feed's own summary and falls back to the opening of the body, so an entry
    /// usually has one without a feed providing it and without any AI in the loop. `None` when
    /// neither yields anything worth printing — the caller then shows the title alone, which reads
    /// better than an empty line under every headline.
    #[must_use]
    pub fn excerpt(&self, max_chars: usize) -> Option<String> {
        let source = self
            .summary
            .as_deref()
            .map(strip_markup)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| strip_markup(&self.body_html));
        let text = source.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty() {
            return None;
        }
        Some(truncate_on_word(&text, max_chars))
    }
}

/// Drop tags and decode the handful of entities the assembly stage emits, leaving plain text.
///
/// Deliberately not a parser: this runs over already-clean output from the readability stage, and a
/// contents line does not justify building a DOM per article. Anything between `<` and `>` goes,
/// which is exactly right for well-formed input and fails safe on anything else — worst case a
/// stray fragment is dropped from a preview line.
fn strip_markup(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' '); // a tag boundary is a word boundary: "<p>a</p><p>b</p>" is "a b"
            }
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
}

/// Cut `text` to at most `max_chars` **characters**, breaking at a word boundary and marking the
/// cut with an ellipsis. Counting characters rather than bytes is what keeps this from splitting a
/// multi-byte character — a feed summary is arbitrary text, and half a character renders as a box.
fn truncate_on_word(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let cut: String = text.chars().take(max_chars).collect();
    let trimmed = match cut.rsplit_once(char::is_whitespace) {
        // Keep the word boundary only if it leaves most of the budget; a very long first word would
        // otherwise cut the line to almost nothing.
        Some((head, _)) if head.chars().count() >= max_chars / 2 => head,
        _ => cut.trim_end(),
    };
    format!("{}…", trimmed.trim_end_matches([',', ';', ':', '.', ' ']))
}

/// A compiled daily issue: a dated, titled set of articles in reading order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    /// Issue title (e.g. "inkread daily").
    pub title: String,
    /// The issue date, pre-formatted for display (the caller stamps it — this crate reads no clock).
    pub date: String,
    /// Articles in reading order.
    pub articles: Vec<Article>,
}

impl Issue {
    /// Whether the issue has no articles (the assembler still produces a valid, if empty, EPUB).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.articles.is_empty()
    }
}
