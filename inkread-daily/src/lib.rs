//! `inkread-daily` (#66): turn followed feed/article sources into a self-contained **daily-issue
//! EPUB** that the inkread reader opens like any book — calm, offline, e-ink-first reading.
//!
//! The crate owns the **core** of the pipeline (per the project decision): [`parse_feed`] (RSS/Atom →
//! article links), [`extract_readable`] (article HTML → clean, well-formed XHTML), and
//! [`assemble_epub`] / [`assemble_issue_from_json`] (compose an [`Issue`] into a valid EPUB). It is
//! **pure and host-testable** — no network, no clock. The Android shell does the HTTP fetch and hands
//! the core the fetched bytes (as JSON over JNI), keeping this crate vendor- and IO-free (IR-7).
//! Delivery as EPUB reuses the whole existing reader (reflow, font controls, reflow-stable resume).

mod epub;
mod extract;
mod feed;
mod model;

pub use epub::assemble_epub;
pub use extract::extract_readable;
pub use feed::{parse_feed, FeedItem};
pub use model::{Article, Issue, Source};

use serde::Deserialize;

/// Parse a feed and return its entries as a JSON array (the JNI-friendly form of [`parse_feed`]).
#[must_use]
pub fn parse_feed_json(xml: &str) -> String {
    serde_json::to_string(&parse_feed(xml)).unwrap_or_else(|_| "[]".to_string())
}

/// The shell's fetched issue, handed to the core as JSON: per article the raw fetched `html`, which
/// the core extracts into clean XHTML before assembling.
#[derive(Deserialize)]
struct RawIssue {
    title: String,
    date: String,
    articles: Vec<RawArticle>,
}

#[derive(Deserialize)]
struct RawArticle {
    title: String,
    source: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    published: Option<String>,
    /// Raw fetched article HTML — the core runs readability extraction over it.
    html: String,
}

/// Assemble an issue EPUB from the shell's fetched JSON: each article's raw `html` is run through
/// [`extract_readable`] (clean, well-formed XHTML), then the whole [`Issue`] is assembled. Returns
/// the EPUB bytes, or a typed error string when the JSON is malformed (the boundary validation).
pub fn assemble_issue_from_json(json: &str) -> Result<Vec<u8>, String> {
    let raw: RawIssue = serde_json::from_str(json).map_err(|e| format!("daily issue json: {e}"))?;
    let articles = raw
        .articles
        .into_iter()
        .map(|a| {
            // The page's own <h1> usually repeats the article title; drop that leading paragraph so
            // the title isn't shown twice (the issue already renders the title above the body).
            let body_html = without_leading_title(&extract_readable(&a.html), &a.title);
            Article {
                title: a.title,
                source: a.source,
                url: a.url,
                published: a.published,
                body_html,
            }
        })
        .collect();
    let issue = Issue {
        title: raw.title,
        date: raw.date,
        articles,
    };
    Ok(assemble_epub(&issue))
}

/// Drop a leading `<p>…</p>` from `body` when it just repeats the article `title` (the page's own
/// heading). Compares on alphanumerics only, so punctuation/escaping differences don't matter.
fn without_leading_title(body: &str, title: &str) -> String {
    let norm = |s: &str| {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    let tnorm = norm(title);
    if tnorm.len() < 6 {
        return body.to_string(); // too short to match confidently
    }
    if let Some(end) = body.find("</p>") {
        let first = body[..end].trim_start_matches("<p>");
        let decoded = extract_readable_decode(first);
        let fnorm = norm(&decoded);
        if fnorm == tnorm || fnorm.starts_with(&tnorm) || tnorm.starts_with(&fnorm) {
            return body[end + 4..].trim_start().to_string();
        }
    }
    body.to_string()
}

/// Reverse the XHTML escaping of a single extracted block for comparison (mirror of the assembler's
/// escape — `&amp;`/`&lt;`/… back to text).
fn extract_readable_decode(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

#[cfg(test)]
mod json_tests {
    use super::*;
    use inkread_epub::EpubPackage;

    #[test]
    fn assembles_an_issue_from_fetched_json_with_extraction() {
        let json = r#"{
            "title": "inkread daily",
            "date": "25 Jun 2026",
            "articles": [
                {"title":"Hello","source":"Blog","url":"https://x.test/1",
                 "html":"<html><body><script>evil()</script><article><p>This is a real paragraph of article prose, long enough to survive extraction &amp; reading.</p></article></body></html>"}
            ]
        }"#;
        let bytes = assemble_issue_from_json(json).expect("assembles");
        let pkg = EpubPackage::open(bytes).expect("valid EPUB");
        assert_eq!(pkg.chapter_count(), 2, "title page + one article");
        let html: String = pkg.chapters.iter().map(|c| c.html.clone()).collect();
        assert!(
            html.contains("real paragraph of article prose"),
            "extracted body present: {html}"
        );
        assert!(!html.contains("evil()"), "script dropped by extraction");
    }

    #[test]
    fn malformed_json_is_a_typed_error_not_a_panic() {
        assert!(assemble_issue_from_json("{not json").is_err());
    }

    #[test]
    fn parse_feed_json_round_trips() {
        let json = parse_feed_json(
            r#"<rss><channel><item><title>T</title><link>https://x.test/1</link></item></channel></rss>"#,
        );
        assert!(
            json.contains("\"title\":\"T\"") && json.contains("https://x.test/1"),
            "{json}"
        );
    }
}
