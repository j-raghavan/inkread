//! Host tests for the OPDS classification (ADR-INKREAD-0016).
//!
//! The fixtures mirror what the two servers #175 names actually emit — the shapes were taken from
//! `src/calibre/srv/opds.py` (navigation entries carry a `profile=opds-catalog;kind=navigation`
//! link; books carry `rel="http://opds-spec.org/acquisition"` links plus cover/thumbnail relations)
//! and Calibre-Web's `cps/opds.py`. Parsing a realistic document is the point: a synthetic feed
//! would not exercise the rel/media-type discrimination that does all the work here.

use super::*;

/// A calibre-shaped acquisition feed: one book in three formats, with cover art and paging.
const ACQUISITION_FEED: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>calibre library</title>
  <id>urn:uuid:1c5c1a4e</id>
  <updated>2026-08-14T10:00:00+00:00</updated>
  <link rel="start" type="application/atom+xml;type=feed;profile=opds-catalog" href="/opds"/>
  <link rel="next" type="application/atom+xml;type=feed;profile=opds-catalog" href="/opds/navcatalog/4e6577?offset=25"/>
  <link rel="search" type="application/atom+xml" href="/opds/search/{searchTerms}"/>
  <entry>
    <title>The Left Hand of Darkness</title>
    <id>urn:uuid:ac2f9b1e</id>
    <author><name>Ursula K. Le Guin</name></author>
    <updated>2026-08-01T09:00:00+00:00</updated>
    <published>1969-03-01T00:00:00+00:00</published>
    <summary>Winter is a planet of perpetual ice.</summary>
    <link type="application/pdf" href="/get/PDF/42/lib" rel="http://opds-spec.org/acquisition" length="9100000"/>
    <link type="application/epub+zip" href="/get/EPUB/42/lib" rel="http://opds-spec.org/acquisition" length="410000"/>
    <link type="application/x-mobipocket-ebook" href="/get/MOBI/42/lib" rel="http://opds-spec.org/acquisition"/>
    <link type="image/jpeg" href="/get/cover/42/lib" rel="http://opds-spec.org/cover"/>
    <link type="image/jpeg" href="/get/thumb/42/lib" rel="http://opds-spec.org/thumbnail"/>
  </entry>
</feed>"#;

/// A calibre-shaped navigation feed: entries that lead to further feeds, no downloads.
const NAVIGATION_FEED: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>calibre library</title>
  <id>urn:uuid:9f0b</id>
  <updated>2026-08-14T10:00:00+00:00</updated>
  <entry>
    <title>By Author</title>
    <id>calibre-navcatalog:8b1f</id>
    <updated>2026-08-14T10:00:00+00:00</updated>
    <content type="text">Books sorted by author</content>
    <link type="application/atom+xml;type=feed;profile=opds-catalog;kind=navigation" href="/opds/navcatalog/617574686f7273"/>
  </entry>
</feed>"#;

#[test]
fn acquisition_entry_is_classified_and_ranked() {
    let catalog = parse_catalog(ACQUISITION_FEED);
    assert_eq!(catalog.title, "calibre library");
    assert_eq!(catalog.entries.len(), 1);

    let book = &catalog.entries[0];
    assert_eq!(book.kind, EntryKind::Acquisition);
    assert_eq!(book.title, "The Left Hand of Darkness");
    assert_eq!(book.author, "Ursula K. Le Guin");
    assert_eq!(book.summary, "Winter is a planet of perpetual ice.");
    assert!(
        book.href.is_empty(),
        "an acquisition entry has no navigation destination"
    );

    // EPUB is listed after PDF in the feed but must come first: it reflows, so it is what the
    // reader should download by default.
    let exts: Vec<&str> = book.formats.iter().map(|f| f.ext.as_str()).collect();
    assert_eq!(
        exts,
        ["epub", "pdf", ""],
        "supported formats first, in preference order"
    );
    assert_eq!(book.formats[0].href, "/get/EPUB/42/lib");
    assert_eq!(book.formats[0].bytes, Some(410_000));
    assert_eq!(book.formats[2].mime, "application/x-mobipocket-ebook");
}

#[test]
fn thumbnail_is_preferred_over_the_full_cover() {
    let book = &parse_catalog(ACQUISITION_FEED).entries[0];
    assert_eq!(
        book.cover, "/get/thumb/42/lib",
        "an e-ink list wants the small rendition"
    );
}

#[test]
fn feed_level_paging_and_search_links_are_surfaced() {
    let catalog = parse_catalog(ACQUISITION_FEED);
    assert_eq!(catalog.start, "/opds");
    assert_eq!(catalog.next, "/opds/navcatalog/4e6577?offset=25");
    assert!(
        catalog.prev.is_empty(),
        "this feed declares no previous page"
    );
    // The placeholder survives: the shell owns substitution because it owns the URL escaping.
    assert_eq!(catalog.search_template, "/opds/search/{searchTerms}");
}

#[test]
fn navigation_entry_carries_its_destination_and_no_formats() {
    let catalog = parse_catalog(NAVIGATION_FEED);
    assert_eq!(catalog.entries.len(), 1);

    let nav = &catalog.entries[0];
    assert_eq!(nav.kind, EntryKind::Navigation);
    assert_eq!(nav.title, "By Author");
    assert_eq!(nav.href, "/opds/navcatalog/617574686f7273");
    assert_eq!(
        nav.summary, "Books sorted by author",
        "content stands in for a missing summary"
    );
    assert!(nav.formats.is_empty());
}

#[test]
fn acquisition_sub_relations_still_count_as_downloads() {
    // OPDS defines /open-access, /borrow, /buy, /sample under the acquisition relation. A lending
    // catalog's entry is still a way to get the book, so the prefix must match, not the exact rel.
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>t</title><id>i</id><updated>2026-08-14T10:00:00+00:00</updated>
  <entry>
    <title>Borrowable</title><id>e</id><updated>2026-08-14T10:00:00+00:00</updated>
    <link type="application/epub+zip" href="/borrow/7" rel="http://opds-spec.org/acquisition/borrow"/>
  </entry>
</feed>"#;
    let entry = &parse_catalog(xml).entries[0];
    assert_eq!(entry.kind, EntryKind::Acquisition);
    assert_eq!(entry.formats.len(), 1);
    assert_eq!(entry.formats[0].ext, "epub");
}

#[test]
fn an_entry_whose_formats_are_all_unopenable_is_still_a_book() {
    // Classification is about what the entry *is*, not what we can read. The UI needs to show the
    // book and explain that nothing on it is openable — silently hiding it would look like data loss.
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>t</title><id>i</id><updated>2026-08-14T10:00:00+00:00</updated>
  <entry>
    <title>Only MOBI</title><id>e</id><updated>2026-08-14T10:00:00+00:00</updated>
    <link type="application/x-mobipocket-ebook" href="/get/MOBI/9/lib" rel="http://opds-spec.org/acquisition"/>
  </entry>
</feed>"#;
    let entry = &parse_catalog(xml).entries[0];
    assert_eq!(entry.kind, EntryKind::Acquisition);
    assert_eq!(entry.formats.len(), 1);
    assert!(
        entry.formats[0].ext.is_empty(),
        "no extension ⇒ the shell must not offer it"
    );
}

#[test]
fn media_types_match_case_insensitively() {
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>t</title><id>i</id><updated>2026-08-14T10:00:00+00:00</updated>
  <entry>
    <title>Shouty</title><id>e</id><updated>2026-08-14T10:00:00+00:00</updated>
    <link type="APPLICATION/EPUB+ZIP" href="/get/EPUB/1/lib" rel="http://opds-spec.org/acquisition"/>
  </entry>
</feed>"#;
    assert_eq!(parse_catalog(xml).entries[0].formats[0].ext, "epub");
}

#[test]
fn malformed_input_yields_an_empty_catalog_and_never_panics() {
    // RR21-FR3: junk in → benign out. Each of these reaches the parser from a hostile or broken
    // server, and none may take the reader down.
    for junk in [
        "",
        "   ",
        "not xml at all",
        "<feed",
        "<html><body>404</body></html>",
        "\u{feff}<feed>",
    ] {
        let catalog = parse_catalog(junk);
        assert!(
            catalog.entries.is_empty(),
            "unexpected entries from {junk:?}"
        );
        assert!(catalog.next.is_empty());
    }
}

#[test]
fn json_form_round_trips_and_omits_absent_fields() {
    let json = parse_catalog_json(ACQUISITION_FEED);
    let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");

    assert_eq!(value["title"], "calibre library");
    assert_eq!(value["entries"][0]["kind"], "acquisition");
    assert_eq!(value["entries"][0]["formats"][0]["ext"], "epub");
    // `prev` is absent from the feed, so it must be absent from the JSON — the shell tests for the
    // key's presence to decide whether to show a "previous page" control.
    assert!(
        value.get("prev").is_none(),
        "empty links are omitted, not sent as \"\""
    );
    assert!(value["entries"][0].get("href").is_none());
}

#[test]
fn empty_catalog_json_constant_matches_a_real_empty_catalog() {
    // The defensive fallback in parse_catalog_json must not drift from the derived serialization.
    assert_eq!(
        serde_json::to_string(&Catalog::default()).unwrap(),
        Catalog::EMPTY_JSON
    );
}
