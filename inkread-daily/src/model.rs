//! The `inkread-daily` domain model (#66): the data a daily issue is built from.
//!
//! Deliberately tiny and dependency-free. A [`Source`] is a feed/site the user follows; an
//! [`Article`] is one extracted, ready-to-read piece; an [`Issue`] is the compiled set for a day.
//! Fetching feeds and extracting readable text happen at the edges (the Android shell / a later
//! extraction slice) and produce these values — this crate only *assembles* them into an EPUB.

/// A content source the user follows (RSS/Atom feed or a site). The fetch layer owns the network;
/// this is the persisted identity the shell stores. **Ahead of its consumer:** defined here as the
/// stable model type the fetch/persistence slice will read — assembly attributes articles by the
/// flat [`Article::source`] byline today and does not reference this struct yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// Human-facing name shown as the article byline (e.g. "Hacker News").
    pub name: String,
    /// The feed/page URL the fetch layer polls.
    pub url: String,
}

/// One ready-to-read article in an issue: a title, the source it came from, the original URL, an
/// optional published date (already formatted for display), and the **clean** body as simple,
/// well-formed XHTML-compatible markup (paragraphs/headings) — the output of readability extraction.
#[derive(Debug, Clone, PartialEq, Eq)]
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
}

/// A compiled daily issue: a dated, titled set of articles in reading order.
#[derive(Debug, Clone, PartialEq, Eq)]
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
