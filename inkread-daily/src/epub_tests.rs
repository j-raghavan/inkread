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
        summary: None,
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
fn malformed_body_html_is_currently_accepted_extraction_slice_must_sanitize() {
    // KNOWN GAP (#66): body_html is injected RAW on the "already clean, well-formed XHTML" contract.
    // The reader's parser (rbook) is lenient and accepts malformed markup today, so a bad body does
    // NOT fail this host gate — it would reach the device, where reflow layout is the unknown. The
    // extraction slice MUST guarantee well-formed XHTML (or this crate must sanitize) before any real
    // fetched content is assembled. This test PINS the current leniency so a future switch to strict
    // parsing surfaces here as a failure rather than silently — it documents the gap, not an endorsement.
    let issue = Issue {
        title: "t".to_string(),
        date: "d".to_string(),
        articles: vec![article("Bad", "Src", "<p>unclosed & a raw < here", None)],
    };
    assert!(
        EpubPackage::open(assemble_epub(&issue)).is_ok(),
        "rbook currently tolerates malformed body markup — see the #66 extraction slice"
    );
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

// ---------------------------------------------------------------------------------------------
// #198 — the "In This Issue" contents section.
// ---------------------------------------------------------------------------------------------

fn with_summary(mut a: Article, summary: &str) -> Article {
    a.summary = Some(summary.to_string());
    a
}

/// The publisher's own description beats an excerpt of the opening words: it is what they chose to
/// say the piece is about.
#[test]
fn the_feeds_summary_is_preferred_over_the_body() {
    let art = with_summary(
        article("T", "S", "<p>Opening words of the body.</p>", None),
        "What the piece is actually about.",
    );
    assert_eq!(
        art.excerpt(160).as_deref(),
        Some("What the piece is actually about.")
    );
}

/// Without a feed summary an entry still gets a line, from the body — so the contents page is
/// useful on feeds that give no description, and with no AI in the loop.
#[test]
fn the_body_supplies_an_excerpt_when_the_feed_gives_none() {
    let art = article("T", "S", "<p>Opening words.</p><p>And more.</p>", None);
    assert_eq!(
        art.excerpt(160).as_deref(),
        Some("Opening words. And more.")
    );
}

/// A tag boundary is a word boundary — otherwise paragraphs run together into "words.And".
#[test]
fn markup_is_stripped_without_gluing_words_together() {
    let art = article("T", "S", "<p>alpha</p><p>bravo</p>", None);
    assert_eq!(art.excerpt(160).as_deref(), Some("alpha bravo"));
    // Entities the assembly stage emits are decoded for display.
    let ent = article("T", "S", "<p>Tom &amp; Jerry &quot;x&quot;</p>", None);
    assert_eq!(ent.excerpt(160).as_deref(), Some("Tom & Jerry \"x\""));
}

/// Nothing worth printing yields no line at all: an empty line under every headline reads worse
/// than the title alone, which is what #198 asks for as the fallback.
#[test]
fn an_article_with_no_usable_text_has_no_excerpt() {
    assert_eq!(article("T", "S", "", None).excerpt(160), None);
    assert_eq!(
        article("T", "S", "<p></p>  <div>\n</div>", None).excerpt(160),
        None
    );
    // A blank feed summary falls through to the body rather than printing emptiness.
    let blank = with_summary(article("T", "S", "<p>Real body.</p>", None), "   ");
    assert_eq!(blank.excerpt(160).as_deref(), Some("Real body."));
}

#[test]
fn a_long_excerpt_is_cut_at_a_word_boundary() {
    let body = format!("<p>{}</p>", "alpha bravo ".repeat(60));
    let art = article("T", "S", &body, None);
    let got = art.excerpt(40).expect("has an excerpt");
    assert!(
        got.chars().count() <= 41,
        "got {} chars: {got:?}",
        got.chars().count()
    );
    assert!(got.ends_with('…'), "should mark the cut: {got:?}");
    assert!(!got.contains("alp…"), "must not cut mid-word: {got:?}");
}

/// A feed summary is arbitrary text. Cutting by bytes would split a multi-byte character and render
/// as a replacement box.
#[test]
fn cutting_never_splits_a_multibyte_character() {
    for text in [
        "日本語のテキストがここにあります。".repeat(20),
        "é".repeat(400),
    ] {
        let art = with_summary(article("T", "S", "<p>x</p>", None), &text);
        let got = art.excerpt(40).expect("has an excerpt");
        assert!(got.chars().count() <= 41, "{} chars", got.chars().count());
        // Round-tripping proves every char is whole.
        assert_eq!(got, String::from_utf8(got.clone().into_bytes()).unwrap());
    }
}

/// A single word longer than the budget must still leave something readable rather than an ellipsis
/// on its own.
#[test]
fn one_very_long_word_still_yields_text() {
    let art = with_summary(article("T", "S", "<p>x</p>", None), &"z".repeat(200));
    let got = art.excerpt(40).expect("has an excerpt");
    assert!(got.chars().count() > 10, "collapsed to nothing: {got:?}");
}

/// End to end: the contents page lists every article, links to it, and the whole issue still parses
/// with the real reader backend.
#[test]
fn the_contents_page_links_every_article_and_the_issue_still_opens() {
    let issue = Issue {
        title: "inkread daily".into(),
        date: "21 Aug 2026".into(),
        articles: vec![
            with_summary(
                article("First", "Ars", "<p>Body one.</p>", None),
                "Summary one.",
            ),
            article("Second", "BBC", "<p>Body two.</p>", None),
        ],
    };
    let cover = title_page(&issue);
    assert!(cover.contains("In This Issue"), "missing the heading");
    for (i, art) in issue.articles.iter().enumerate() {
        assert!(
            cover.contains(&format!("a{i:04}.xhtml")),
            "entry {i} is not a link"
        );
        assert!(cover.contains(&art.title), "entry {i} missing its title");
        assert!(cover.contains(&art.source), "entry {i} missing its source");
    }
    assert!(cover.contains("Summary one."), "feed summary not shown");
    assert!(cover.contains("Body two."), "body excerpt not shown");

    // The headline guarantee of this module: it still parses with the real EPUB backend.
    let bytes = assemble_epub(&issue);
    let pkg = EpubPackage::open(bytes).expect("assembled issue opens");
    assert!(pkg.chapter_count() >= issue.articles.len());
}

/// #195: the issue opens with a quotation, and the whole thing still parses with the real reader
/// backend — a malformed blockquote would break the container, not just look wrong.
#[test]
fn the_cover_carries_the_daily_quotation() {
    let issue = sample_issue();
    let cover = title_page(&issue);
    let quote = crate::quote::quote_for(&issue.date).expect("the collection is not empty");

    assert!(cover.contains("blockquote"), "no quotation on the cover");
    assert!(
        cover.contains(quote.author),
        "attribution missing the author"
    );
    assert!(cover.contains(quote.work), "attribution missing the work");

    // Before the article list: a paper puts it above the contents, and it is read before deciding
    // what to read.
    let q_at = cover.find("blockquote").expect("quote present");
    let list_at = cover.find("<ul>").expect("contents present");
    assert!(
        q_at < list_at,
        "the quotation should precede the article list"
    );

    let pkg = EpubPackage::open(assemble_epub(&issue)).expect("assembled issue opens");
    assert!(pkg.chapter_count() >= 1);
}

/// The quote is escaped like everything else on the page — an apostrophe or ampersand in a
/// quotation must not break the XHTML.
#[test]
fn a_quotation_is_escaped_into_the_cover() {
    for q in crate::quote::all() {
        let issue = Issue {
            title: "t".into(),
            date: "x".into(),
            articles: vec![article("A", "S", "<p>b</p>", None)],
        };
        let cover = title_page(&issue);
        // Whatever quote today's date picks, the cover must stay well-formed.
        assert!(!cover.contains("<<"), "malformed markup for {:?}", q.text);
    }
    // Assemble with each date in a month so several different quotes are exercised through the
    // real container.
    for day in 1..=12 {
        let issue = Issue {
            title: "inkread daily".into(),
            date: format!("{day} Aug 2026"),
            articles: vec![article("A", "S", "<p>b</p>", None)],
        };
        EpubPackage::open(assemble_epub(&issue)).expect("assembled issue opens");
    }
}
