//! RSS / Atom / JSON feed parsing (#66) via [`feed_rs`].
//!
//! `feed-rs` maps RSS 0.9x/1.0/2.0, Atom, and JSON Feed into one model, so users can add arbitrary
//! feeds and we still get consistent `(title, link, date)` extraction from a single tolerant parser.
//! This replaces an earlier hand-rolled `quick-xml` parser (which mis-handled self-closing links and
//! namespaced elements). Malformed input yields an empty list rather than an error (RR21-FR3 — never
//! panics). Pure + host-testable; the network lives in the shell.

use feed_rs::model::Link;
use serde::Serialize;

/// One entry discovered in a feed — enough for the shell to fetch + attribute the article.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FeedItem {
    /// Entry title (entities already decoded by the parser).
    pub title: String,
    /// Article URL — the entry's `alternate` link (the human page), else its first link.
    pub url: String,
    /// Published (or, failing that, updated) date, formatted "DD Mon YYYY"; `None` if the feed omits it.
    pub published: Option<String>,
}

/// Parse an RSS / Atom / JSON feed into its entries, in document order. Tolerant of malformed input.
#[must_use]
pub fn parse_feed(xml: &str) -> Vec<FeedItem> {
    let feed = match feed_rs::parser::parse(xml.as_bytes()) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    feed.entries
        .into_iter()
        .map(|e| FeedItem {
            // feed-rs does one XML entity decode; some feeds double-encode (e.g. The Verge ships
            // "&amp;#8217;"), leaving a numeric ref in the text. A second decode pass (shared with
            // extraction) finishes the job so a headline reads "Guardian's", not "Guardian&#8217;s".
            title: e
                .title
                .map(|t| crate::extract::decode_entities(t.content.trim()))
                .unwrap_or_default(),
            url: entry_url(&e.links),
            published: e
                .published
                .or(e.updated)
                .map(|d| d.format("%d %b %Y").to_string()),
        })
        .collect()
}

/// The article URL for an entry: prefer the `alternate` link (the readable page), else the first
/// link. Atom feeds carry several links (`self`, `alternate`, enclosures); picking `alternate`
/// avoids pointing at the feed itself.
fn entry_url(links: &[Link]) -> String {
    links
        .iter()
        .find(|l| l.rel.as_deref() == Some("alternate"))
        .or_else(|| links.first())
        .map(|l| l.href.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rss_title_link_and_date() {
        let xml = r#"<?xml version="1.0"?><rss version="2.0"><channel>
            <item><title>Hello World</title><link>https://x.test/a</link>
            <pubDate>Wed, 25 Jun 2026 18:33:54 +0000</pubDate></item>
            </channel></rss>"#;
        let items = parse_feed(xml);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Hello World");
        assert_eq!(items[0].url, "https://x.test/a");
        assert_eq!(items[0].published.as_deref(), Some("25 Jun 2026"));
    }

    #[test]
    fn atom_self_closing_link_uses_href_not_leaked_text() {
        // Regression for the old parser's bug: a self-closing <link href/> followed by other text
        // must take the href as the URL, never the trailing text (which put an author in the URL).
        let xml = r#"<?xml version="1.0"?><feed xmlns="http://www.w3.org/2005/Atom">
            <entry><title>Atom Title</title>
            <link href="https://x.test/b"/>
            <author><name>Sheena Vasani</name></author>
            <updated>2026-06-25T18:00:00Z</updated></entry></feed>"#;
        let items = parse_feed(xml);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].url, "https://x.test/b");
        assert!(!items[0].url.contains("Sheena"));
    }

    #[test]
    fn atom_prefers_the_alternate_link() {
        let xml = r#"<?xml version="1.0"?><feed xmlns="http://www.w3.org/2005/Atom">
            <entry><title>T</title>
            <link rel="self" href="https://x.test/feed"/>
            <link rel="alternate" href="https://x.test/article"/>
            </entry></feed>"#;
        let items = parse_feed(xml);
        assert_eq!(items[0].url, "https://x.test/article");
    }

    #[test]
    fn decodes_title_entities() {
        let xml = r#"<?xml version="1.0"?><rss version="2.0"><channel>
            <item><title>Tom &amp; Jerry&#x27;s day</title><link>https://x.test/c</link></item>
            </channel></rss>"#;
        let items = parse_feed(xml);
        assert_eq!(items[0].title, "Tom & Jerry's day");
    }

    #[test]
    fn second_pass_decodes_double_encoded_titles() {
        // The Verge ships "&amp;#8217;": feed-rs decodes once to "&#8217;", our second pass finishes
        // it to the curly apostrophe (U+2019).
        let xml = r#"<?xml version="1.0"?><rss version="2.0"><channel>
            <item><title>Guardian&amp;#8217;s phone</title><link>https://x.test/v</link></item>
            </channel></rss>"#;
        let items = parse_feed(xml);
        assert_eq!(items[0].title, "Guardian\u{2019}s phone");
    }

    #[test]
    fn parses_json_feed() {
        // New capability over the old XML-only parser: JSON Feed (jsonfeed.org).
        let json = r#"{"version":"https://jsonfeed.org/version/1","title":"Site",
            "items":[{"id":"1","url":"https://x.test/j","title":"JSON Item",
            "date_published":"2026-06-25T18:00:00Z"}]}"#;
        let items = parse_feed(json);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "JSON Item");
        assert_eq!(items[0].url, "https://x.test/j");
    }

    #[test]
    fn malformed_input_yields_empty_not_panic() {
        assert!(parse_feed("<not a feed").is_empty());
        assert!(parse_feed("").is_empty());
        assert!(parse_feed("plain text, definitely not a feed").is_empty());
    }
}
