//! Tests for the issue→EPUB assembler (#66). The headline guarantee: an assembled issue parses
//! with the *real* reader EPUB backend (`inkread_epub::EpubPackage`), so a malformed container is
//! caught on the host, not the device.

use super::*;
use crate::model::{Article, Issue};
use inkread_epub::EpubPackage;

fn article(title: &str, source: &str, body: &str, published: Option<&str>) -> Article {
    Article {
        title: title.to_string(),
        source: source.to_string(),
        url: format!("https://example.test/{title}"),
        published: published.map(str::to_string),
        body_html: body.to_string(),
    }
}

fn sample_issue() -> Issue {
    Issue {
        title: "inkread daily".to_string(),
        date: "24 Jun 2026".to_string(),
        articles: vec![
            article(
                "Calm Computing",
                "Hacker News",
                "<p>Long-form reading, away from the feed.</p>",
                Some("24 Jun 2026"),
            ),
            article(
                "E-ink & You",
                "A Blog",
                "<p>Why grayscale is restful.</p><p>And paper-like.</p>",
                None,
            ),
        ],
    }
}

#[test]
fn assembled_issue_parses_with_the_reader_epub_backend() {
    let bytes = assemble_epub(&sample_issue());
    let pkg = EpubPackage::open(bytes).expect("assembled issue is a valid EPUB");
    // Title page + 2 articles in the spine (the reader sees each as a chapter).
    assert_eq!(pkg.chapter_count(), 3, "title page + two articles");
}

#[test]
fn issue_metadata_and_article_text_survive_the_round_trip() {
    let bytes = assemble_epub(&sample_issue());
    let pkg = EpubPackage::open(bytes).unwrap();
    let title = pkg.title.clone().unwrap_or_default();
    assert!(
        title.contains("inkread daily") && title.contains("24 Jun 2026"),
        "issue title carries the name + date: {title:?}"
    );
    // The article bodies reach the parsed chapters (search the concatenated chapter HTML).
    let html: String = pkg.chapters.iter().map(|c| c.html.clone()).collect();
    assert!(html.contains("Long-form reading"), "article 1 body present");
    assert!(html.contains("paper-like"), "article 2 body present");
    assert!(html.contains("Hacker News"), "byline/source present");
}

#[test]
fn an_empty_issue_still_assembles_a_valid_epub() {
    let issue = Issue {
        title: "inkread daily".to_string(),
        date: "24 Jun 2026".to_string(),
        articles: vec![],
    };
    assert!(issue.is_empty());
    let pkg = EpubPackage::open(assemble_epub(&issue)).expect("empty issue is still a valid EPUB");
    assert_eq!(pkg.chapter_count(), 1, "just the title page");
}

#[test]
fn xml_metacharacters_in_titles_do_not_break_the_container() {
    // A hostile headline with &, <, >, quotes must not produce malformed XHTML/OPF.
    let issue = Issue {
        title: "Tom & Jerry <b>\"news\"</b>".to_string(),
        date: "24 Jun 2026".to_string(),
        articles: vec![article(
            "5 < 10 & \"quotes\" > here",
            "A & B News",
            "<p>Body with &amp; an entity.</p>",
            None,
        )],
    };
    let pkg = EpubPackage::open(assemble_epub(&issue))
        .expect("escaped metacharacters keep the EPUB well-formed");
    assert_eq!(pkg.chapter_count(), 2);
    assert!(
        pkg.title
            .clone()
            .unwrap_or_default()
            .contains("Tom & Jerry"),
        "title decodes back: {:?}",
        pkg.title
    );
}
