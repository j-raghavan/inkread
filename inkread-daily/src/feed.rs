//! RSS / Atom feed parsing (#66): feed XML → a list of article links the shell then fetches.
//!
//! A small pull parser over [`quick_xml`] that handles both RSS (`<item>` · `<title>` · `<link>` text
//! · `<pubDate>`) and Atom (`<entry>` · `<title>` · `<link href>` · `<published>`/`<updated>`).
//! Tolerant: unknown elements are ignored and a malformed feed yields the items parsed so far rather
//! than an error (RR21-FR3 — never panics). Pure + host-testable; the network lives in the shell.

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Serialize;

/// One entry discovered in a feed — enough for the shell to fetch + attribute the article.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FeedItem {
    /// Entry title.
    pub title: String,
    /// Article URL (RSS `<link>` text or Atom `<link href>`).
    pub url: String,
    /// Published/updated timestamp as the feed states it, if present (raw; the shell formats it).
    pub published: Option<String>,
}

#[derive(Clone, Copy)]
enum Field {
    Title,
    Link,
    Published,
}

/// Parse an RSS or Atom feed into its entries, in document order. Tolerant of malformed input.
#[must_use]
pub fn parse_feed(xml: &str) -> Vec<FeedItem> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut items: Vec<FeedItem> = Vec::new();
    let mut cur: Option<FeedItem> = None;
    let mut field: Option<Field> = None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                match local_name(e.name().as_ref()).as_str() {
                    "item" | "entry" => cur = Some(FeedItem::default()),
                    "title" => field = Some(Field::Title),
                    "link" => {
                        // Atom carries the URL in the href attribute (RSS uses the element text).
                        if let Some(c) = cur.as_mut() {
                            if let Some(href) = attr(&e, b"href") {
                                if c.url.is_empty() {
                                    c.url = href;
                                }
                            }
                        }
                        field = Some(Field::Link);
                    }
                    "pubdate" | "published" | "updated" => field = Some(Field::Published),
                    _ => {}
                }
            }
            // Text and CData are distinct event types; funnel both through one applier.
            Ok(Event::Text(t)) => apply_text(&mut cur, field, &to_string(t.into_inner())),
            Ok(Event::CData(t)) => apply_text(&mut cur, field, &to_string(t.into_inner())),
            Ok(Event::End(e)) => match local_name(e.name().as_ref()).as_str() {
                "item" | "entry" => {
                    if let Some(c) = cur.take() {
                        if !c.title.is_empty() || !c.url.is_empty() {
                            items.push(c);
                        }
                    }
                }
                "title" | "link" | "pubdate" | "published" | "updated" => field = None,
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break, // tolerant: stop on EOF or a malformed event
            _ => {}
        }
    }
    items
}

/// Decode raw element bytes to a (lossy) String.
fn to_string(bytes: std::borrow::Cow<'_, [u8]>) -> String {
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Apply a text/CData run to the current entry's active field.
fn apply_text(cur: &mut Option<FeedItem>, field: Option<Field>, text: &str) {
    if let (Some(c), Some(f)) = (cur.as_mut(), field) {
        let t = text.trim();
        match f {
            // Decode entities so a headline reads "children's", not "children&#x27;s".
            Field::Title => c.title.push_str(&crate::extract::decode_entities(t)),
            Field::Link => {
                if c.url.is_empty() {
                    c.url.push_str(t);
                }
            }
            Field::Published => {
                if c.published.is_none() && !t.is_empty() {
                    c.published = Some(t.to_string());
                }
            }
        }
    }
}

/// The lowercased local element name (namespace prefix stripped).
fn local_name(qname: &[u8]) -> String {
    let name = match qname.iter().rposition(|&b| b == b':') {
        Some(i) => &qname[i + 1..],
        None => qname,
    };
    String::from_utf8_lossy(name).to_ascii_lowercase()
}

/// Read a tag attribute value by (lowercased) name.
fn attr(e: &quick_xml::events::BytesStart<'_>, name: &[u8]) -> Option<String> {
    e.attributes().flatten().find_map(|a| {
        if local_name(a.key.as_ref()) == String::from_utf8_lossy(name) {
            Some(String::from_utf8_lossy(&a.value).into_owned())
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rss_items() {
        let xml = r#"<rss><channel>
            <title>Feed</title>
            <item><title>First post</title><link>https://x.test/1</link><pubDate>Mon, 24 Jun 2026</pubDate></item>
            <item><title>Second</title><link>https://x.test/2</link></item>
        </channel></rss>"#;
        let items = parse_feed(xml);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "First post");
        assert_eq!(items[0].url, "https://x.test/1");
        assert_eq!(items[0].published.as_deref(), Some("Mon, 24 Jun 2026"));
        assert_eq!(items[1].url, "https://x.test/2");
        assert_eq!(items[1].published, None);
    }

    #[test]
    fn parses_atom_entries_with_href_links() {
        let xml = r#"<feed xmlns="http://www.w3.org/2005/Atom">
            <title>Site</title>
            <entry><title>Atom one</title><link href="https://a.test/1"/><published>2026-06-24</published></entry>
        </feed>"#;
        let items = parse_feed(xml);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Atom one");
        assert_eq!(items[0].url, "https://a.test/1");
        assert_eq!(items[0].published.as_deref(), Some("2026-06-24"));
    }

    #[test]
    fn cdata_titles_and_malformed_tail_are_tolerated() {
        let xml = "<rss><channel><item><title><![CDATA[Title & <b>markup</b>]]></title>\
            <link>https://x.test/c</link></item><item><title>truncated";
        let items = parse_feed(xml);
        assert!(!items.is_empty());
        assert_eq!(items[0].title, "Title & <b>markup</b>");
        assert_eq!(items[0].url, "https://x.test/c");
    }

    #[test]
    fn empty_or_junk_input_yields_no_items_without_panicking() {
        assert!(parse_feed("").is_empty());
        assert!(parse_feed("not xml at all <<<").is_empty());
    }
}
