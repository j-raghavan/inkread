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
        let text = crate::extract::collapse_ws(&source);
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
pub(crate) fn truncate_on_word(text: &str, max_chars: usize) -> String {
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

/// The daily layout style the reader chose (ADR-INKREAD-0018): a preset bundle of column count,
/// headline band, and issue markup. `Papernews` is the default — a two-column paper with a
/// full-width headline band.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DailyLayoutStyle {
    /// Today's plain reader look: one column, no band.
    Standard,
    /// The default: two columns with a full-width headline band.
    #[default]
    Papernews,
    /// Papernews plus a centred headline and an uppercased byline (ADR-INKREAD-0018 D1).
    Vintage,
}

impl DailyLayoutStyle {
    /// The column count the daily opens in for this style (1 or 2).
    #[must_use]
    pub fn columns(self) -> u8 {
        match self {
            DailyLayoutStyle::Standard => 1,
            DailyLayoutStyle::Papernews | DailyLayoutStyle::Vintage => 2,
        }
    }

    /// Whether the full-width headline band is on for this style (ADR-INKREAD-0017).
    #[must_use]
    pub fn has_band(self) -> bool {
        matches!(
            self,
            DailyLayoutStyle::Papernews | DailyLayoutStyle::Vintage
        )
    }

    /// The stable lowercase token used in the OPF identifier and over JNI.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            DailyLayoutStyle::Standard => "standard",
            DailyLayoutStyle::Papernews => "papernews",
            DailyLayoutStyle::Vintage => "vintage",
        }
    }

    /// Parse the token ([`Self::as_str`]); unknown or legacy → the default (Papernews), so a daily
    /// issue stamped before styles existed keeps its band.
    #[must_use]
    pub fn from_token(s: &str) -> Self {
        match s {
            "standard" => DailyLayoutStyle::Standard,
            "vintage" => DailyLayoutStyle::Vintage,
            _ => DailyLayoutStyle::Papernews,
        }
    }
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
    /// The followed sources' names in the reader's order (#269): the contents page and the TOC group
    /// articles under these, in this order. A source with no article gets no section; an article
    /// whose source is not listed is grouped after the listed ones.
    #[serde(default)]
    pub sources: Vec<String>,
    /// The reader's chosen layout style (ADR-INKREAD-0018). Defaults to Papernews, so a legacy issue
    /// JSON without the field assembles as the two-column paper.
    #[serde(default)]
    pub style: DailyLayoutStyle,
}

impl Issue {
    /// Whether the issue has no articles (the assembler still produces a valid, if empty, EPUB).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.articles.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_style_bundles_columns_and_band() {
        // ADR-INKREAD-0018 D1: Papernews is the default and carries the band in two columns.
        assert_eq!(DailyLayoutStyle::default(), DailyLayoutStyle::Papernews);
        assert_eq!(DailyLayoutStyle::Papernews.columns(), 2);
        assert!(DailyLayoutStyle::Papernews.has_band());
        assert_eq!(DailyLayoutStyle::Vintage.columns(), 2);
        assert!(DailyLayoutStyle::Vintage.has_band());
        assert_eq!(DailyLayoutStyle::Standard.columns(), 1);
        assert!(!DailyLayoutStyle::Standard.has_band());
    }

    #[test]
    fn layout_style_token_round_trips_and_legacy_defaults_to_papernews() {
        for s in [
            DailyLayoutStyle::Standard,
            DailyLayoutStyle::Papernews,
            DailyLayoutStyle::Vintage,
        ] {
            assert_eq!(DailyLayoutStyle::from_token(s.as_str()), s);
        }
        // A pre-style identifier segment (a bare date, or anything unknown) is Papernews, so a legacy
        // issue keeps its band (ADR-INKREAD-0018 D2).
        assert_eq!(
            DailyLayoutStyle::from_token("2026-11-25"),
            DailyLayoutStyle::Papernews
        );
    }

    #[test]
    fn issue_json_without_style_defaults_to_papernews() {
        let issue: Issue =
            serde_json::from_str(r#"{"title":"t","date":"d","articles":[]}"#).unwrap();
        assert_eq!(issue.style, DailyLayoutStyle::Papernews);
    }

    #[test]
    fn wire_style_tokens_agree_across_serde_from_token_and_as_str() {
        // The style token crosses the FFI three ways that must agree on one literal: the Kotlin shell
        // writes it into the issue JSON, serde parses that JSON (rename_all = "lowercase"), and
        // `as_str`/`from_token` carry it through the OPF identifier and back on the reader. Pin the
        // three wire literals (the same ones `DailyController.LayoutStyle` uses) to every mechanism, so
        // a rename on any side that silently drifts the token fails here rather than on a device.
        for (token, expected) in [
            ("standard", DailyLayoutStyle::Standard),
            ("papernews", DailyLayoutStyle::Papernews),
            ("vintage", DailyLayoutStyle::Vintage),
        ] {
            let issue: Issue = serde_json::from_str(&format!(
                r#"{{"title":"t","date":"d","articles":[],"style":"{token}"}}"#
            ))
            .unwrap();
            assert_eq!(issue.style, expected, "serde parses {token:?}");
            assert_eq!(
                DailyLayoutStyle::from_token(token),
                expected,
                "from_token {token:?}"
            );
            assert_eq!(expected.as_str(), token, "as_str {token:?}");
        }
    }
}
