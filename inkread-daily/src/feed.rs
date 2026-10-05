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
    /// The entry's own `description`/`summary`, when the feed carries one (#198) — the publisher's
    /// line about the piece, used for the contents page in preference to an excerpt of its opening.
    pub summary: Option<String>,
}

/// A parsed feed: its own title and its entries.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ParsedFeed {
    /// The feed's own name (RSS `<channel><title>`, Atom `<feed><title>`, JSON Feed `title`), cleaned
    /// for use as a byline (#268); `None` when the feed gives none or only whitespace.
    pub title: Option<String>,
    /// Entries in document order.
    pub items: Vec<FeedItem>,
}

/// Parse an RSS / Atom / JSON feed into its title and entries (in document order). Malformed input
/// yields an empty [`ParsedFeed`] rather than an error (RR21-FR3).
#[must_use]
pub fn parse_feed(xml: &str) -> ParsedFeed {
    let feed = match feed_rs::parser::parse(xml.as_bytes()) {
        Ok(f) => f,
        Err(_) => return ParsedFeed::default(),
    };
    let title = feed
        .title
        .map(|t| feed_title(&t.content))
        .filter(|t| !t.is_empty());
    let items = feed
        .entries
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
            // Same double-decode as the title: a summary goes on the contents page, so a stray
            // "&#8217;" would be read by a person. Markup is left alone here and stripped where the
            // excerpt is built — many feeds put a whole HTML paragraph in `description`.
            summary: e
                .summary
                .map(|t| crate::extract::decode_entities(t.content.trim()))
                .filter(|t| !t.is_empty()),
        })
        .collect();
    ParsedFeed { title, items }
}

/// The longest feed title kept as a byline. It is shown on a Sources row, the front page and every
/// article's byline, so a publisher's paragraph-length `<title>` must not become one.
const FEED_TITLE_CHARS: usize = 60;

/// Separators publishers put between their name and a tagline or section: "EL PAÍS: el periódico
/// global", "Ars Technica - All content", "BBC News | Home". The name is the part before.
const TAGLINE_SEPARATORS: [&str; 5] = [": ", " - ", " – ", " — ", " | "];

/// Clean a feed's title for use as a byline: decode the entities feed-rs leaves behind (the same
/// double-encoding as entry titles), collapse whitespace, drop a trailing tagline, and cap the length.
fn feed_title(raw: &str) -> String {
    let title = crate::extract::collapse_ws(&crate::extract::decode_entities(raw));
    let name = TAGLINE_SEPARATORS
        .iter()
        .filter_map(|sep| title.find(sep))
        .min()
        .map_or(title.as_str(), |at| title[..at].trim());
    // A title that *starts* with a separator has no name before it; keep the whole thing.
    let name = if name.is_empty() {
        title.as_str()
    } else {
        name
    };
    crate::model::truncate_on_word(name, FEED_TITLE_CHARS)
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
        let items = parse_feed(xml).items;
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
        let items = parse_feed(xml).items;
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
        let items = parse_feed(xml).items;
        assert_eq!(items[0].url, "https://x.test/article");
    }

    #[test]
    fn decodes_title_entities() {
        let xml = r#"<?xml version="1.0"?><rss version="2.0"><channel>
            <item><title>Tom &amp; Jerry&#x27;s day</title><link>https://x.test/c</link></item>
            </channel></rss>"#;
        let items = parse_feed(xml).items;
        assert_eq!(items[0].title, "Tom & Jerry's day");
    }

    #[test]
    fn second_pass_decodes_double_encoded_titles() {
        // The Verge ships "&amp;#8217;": feed-rs decodes once to "&#8217;", our second pass finishes
        // it to the curly apostrophe (U+2019).
        let xml = r#"<?xml version="1.0"?><rss version="2.0"><channel>
            <item><title>Guardian&amp;#8217;s phone</title><link>https://x.test/v</link></item>
            </channel></rss>"#;
        let items = parse_feed(xml).items;
        assert_eq!(items[0].title, "Guardian\u{2019}s phone");
    }

    #[test]
    fn parses_json_feed() {
        // New capability over the old XML-only parser: JSON Feed (jsonfeed.org).
        let json = r#"{"version":"https://jsonfeed.org/version/1","title":"Site",
            "items":[{"id":"1","url":"https://x.test/j","title":"JSON Item",
            "date_published":"2026-06-25T18:00:00Z"}]}"#;
        let items = parse_feed(json).items;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "JSON Item");
        assert_eq!(items[0].url, "https://x.test/j");
    }

    #[test]
    fn malformed_input_yields_empty_not_panic() {
        assert!(parse_feed("<not a feed").items.is_empty());
        assert!(parse_feed("").items.is_empty());
        assert!(parse_feed("plain text, definitely not a feed")
            .items
            .is_empty());
        assert_eq!(parse_feed("<not a feed"), ParsedFeed::default());
    }

    /// The reported case: a feed added as `rss.elpais.com` names itself by its channel title, with
    /// the publisher's tagline dropped (the live feed's title is "EL PAÍS: el periódico global").
    #[test]
    fn rss_channel_title_is_the_feed_title() {
        let xml = r#"<?xml version="1.0"?><rss version="2.0"><channel><title>EL PA&#205;S: el peri&#243;dico global</title>
            <item><title>A</title><link>https://x.test/a</link></item></channel></rss>"#;
        let feed = parse_feed(xml);
        assert_eq!(feed.title.as_deref(), Some("EL PAÍS"));
        assert_eq!(
            feed.items.len(),
            1,
            "entries still parse alongside the title"
        );
    }

    /// The feed's title, not the first entry's: Atom puts both in `<title>` elements.
    #[test]
    fn atom_feed_title_is_not_an_entry_title() {
        let xml = r#"<?xml version="1.0"?><feed xmlns="http://www.w3.org/2005/Atom">
            <title>Atom Site</title>
            <entry><title>Entry One</title><link href="https://x.test/1"/></entry></feed>"#;
        assert_eq!(parse_feed(xml).title.as_deref(), Some("Atom Site"));
    }

    #[test]
    fn json_feed_title_is_read() {
        let json = r#"{"version":"https://jsonfeed.org/version/1","title":"Site","items":[]}"#;
        assert_eq!(parse_feed(json).title.as_deref(), Some("Site"));
    }

    /// No title, or only whitespace, is `None` — the shell keeps its host-derived byline rather
    /// than renaming a source to nothing.
    #[test]
    fn a_missing_or_blank_title_is_none() {
        let none = r#"<rss version="2.0"><channel>
            <item><title>A</title><link>https://x.test/a</link></item></channel></rss>"#;
        let blank = r#"<rss version="2.0"><channel><title>
              </title><item><title>A</title><link>https://x.test/a</link></item></channel></rss>"#;
        assert_eq!(parse_feed(none).title, None);
        assert_eq!(parse_feed(blank).title, None);
    }

    /// Line breaks and indentation inside `<title>` collapse; a double-encoded entity decodes.
    #[test]
    fn a_title_is_cleaned_for_use_as_a_byline() {
        let xml = r#"<rss version="2.0"><channel><title>
              The  Guardian&amp;#8217;s
              Feed </title></channel></rss>"#;
        assert_eq!(
            parse_feed(xml).title.as_deref(),
            Some("The Guardian\u{2019}s Feed")
        );
    }

    /// A paragraph-length title is capped on a character boundary — multi-byte text included.
    #[test]
    fn a_long_title_is_capped_without_splitting_a_character() {
        let long = "é".repeat(200);
        let xml = format!(r#"<rss version="2.0"><channel><title>{long}</title></channel></rss>"#);
        let title = parse_feed(&xml).title.unwrap();
        assert!(title.chars().count() <= FEED_TITLE_CHARS + 1, "{title}");
        assert!(title.ends_with('…'), "{title}");
        assert!(title.trim_end_matches('…').chars().all(|c| c == 'é'));
    }

    #[test]
    fn a_tagline_after_the_name_is_dropped() {
        for (raw, want) in [
            ("Ars Technica - All content", "Ars Technica"),
            ("BBC News | Home", "BBC News"),
            ("The Verge – All Posts", "The Verge"),
            ("Quanta Magazine", "Quanta Magazine"),
            // Hyphens without spaces are part of a name, not a separator.
            ("Hacker-News Digest", "Hacker-News Digest"),
            // The earliest separator wins: the name is what comes before any of them.
            ("Site: News - World", "Site"),
        ] {
            assert_eq!(feed_title(raw), want, "{raw}");
        }
    }

    /// No name before the separator: keep the title rather than byline the source with nothing.
    #[test]
    fn a_title_that_starts_with_a_separator_is_kept_whole() {
        assert_eq!(feed_title("- Weekly"), "- Weekly");
    }
}
