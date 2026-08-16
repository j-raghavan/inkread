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

/// A **Calibre-Web** feed, shaped from `cps/templates/feed.xml` rather than from calibre desktop.
/// The two servers differ in ways that matter, and this is the one the issue reporter runs:
/// navigation uses `rel="subsection"`, covers use `image`/`image/thumbnail`, there are *two*
/// `search` links, and the acquisition media type comes from the host's mime database — so an EPUB
/// arrives as `application/octet-stream` wherever that database has no entry for it.
const CALIBRE_WEB_FEED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:dcterms="http://purl.org/dc/terms/">
  <id>urn:uuid:2853dacf</id>
  <updated>2026-08-16T10:00:00+00:00</updated>
  <link rel="start" href="/opds" type="application/atom+xml;profile=opds-catalog;type=feed;kind=navigation"/>
  <link rel="next" title="Next" href="/opds/new?offset=60" type="application/atom+xml;profile=opds-catalog;type=feed;kind=navigation"/>
  <link rel="search" href="/opds/osd" type="application/opensearchdescription+xml"/>
  <link type="application/atom+xml" rel="search" title="Search" href="/opds/search/{searchTerms}"/>
  <title>Calibre-Web</title>
  <entry>
    <title>A Wizard of Earthsea</title>
    <id>urn:uuid:9c1f</id>
    <updated>2026-08-01T09:00:00+00:00</updated>
    <author><name>Ursula K. Le Guin</name></author>
    <published>1968-11-01T00:00:00+00:00</published>
    <content type="xhtml"><div xmlns="http://www.w3.org/1999/xhtml">TAGS: fantasy</div></content>
    <link type="image/jpeg" href="/opds/cover/7" rel="http://opds-spec.org/image"/>
    <link type="image/jpeg" href="/opds/cover/7" rel="http://opds-spec.org/image/thumbnail"/>
    <link rel="http://opds-spec.org/acquisition" href="/opds/download/7/epub/"
          length="402118" title="EPUB" type="application/octet-stream"/>
    <link rel="http://opds-spec.org/acquisition" href="/opds/download/7/pdf/"
          length="9100000" title="PDF" type="application/pdf"/>
  </entry>
  <entry>
    <title>Fantasy</title>
    <id>/opds/category/3</id>
    <updated>2026-08-16T10:00:00+00:00</updated>
    <link rel="subsection" type="application/atom+xml;profile=opds-catalog" href="/opds/category/3"/>
  </entry>
</feed>"#;

#[test]
fn a_calibre_web_feed_classifies_and_stays_downloadable() {
    let catalog = parse_catalog(CALIBRE_WEB_FEED);
    assert_eq!(catalog.entries.len(), 2);

    let book = &catalog.entries[0];
    assert_eq!(book.kind, EntryKind::Acquisition);
    assert_eq!(book.author, "Ursula K. Le Guin");
    // The EPUB is served as application/octet-stream because the host mime database has no entry
    // for it. Refusing it on that basis would make the reader useless against a real Calibre-Web,
    // so the link's title and href are consulted before giving up.
    let exts: Vec<&str> = book.formats.iter().map(|f| f.ext.as_str()).collect();
    assert_eq!(
        exts,
        ["epub", "pdf"],
        "octet-stream EPUB still resolves, and still sorts first"
    );
    assert_eq!(book.formats[0].href, "/opds/download/7/epub/");
    assert_eq!(book.formats[0].bytes, Some(402_118));

    // `rel="subsection"` with an opds-catalog profile is a navigation row.
    let nav = &catalog.entries[1];
    assert_eq!(nav.kind, EntryKind::Navigation);
    assert_eq!(nav.href, "/opds/category/3");

    assert_eq!(
        book.cover, "/opds/cover/7",
        "image/thumbnail is offered and preferred"
    );
}

/// Calibre-Web's **root** `/opds` feed (`cps/templates/index.xml`) — the first screen a reader
/// sees. Its navigation links carry a catalog `type` and **no `rel` at all**, which is the shape
/// most likely to be mishandled: a classifier that keyed on `rel` would render this as an empty
/// library, and the reader would conclude the server was unreachable.
const CALIBRE_WEB_ROOT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <id>urn:uuid:2853dacf</id>
  <updated>2026-08-16T10:00:00+00:00</updated>
  <link rel="self" href="/opds" type="application/atom+xml;profile=opds-catalog;kind=navigation"/>
  <link rel="start" title="Start" href="/opds" type="application/atom+xml;profile=opds-catalog;kind=navigation"/>
  <link rel="search" href="/opds/osd" type="application/opensearchdescription+xml"/>
  <link type="application/atom+xml" rel="search" title="Search" href="/opds/search/{searchTerms}"/>
  <title>Calibre-Web</title>
  <entry>
    <title>Alphabetical Books</title>
    <link href="/opds/books" type="application/atom+xml;profile=opds-catalog"/>
    <id>/opds/books</id>
    <updated>2026-08-16T10:00:00+00:00</updated>
    <content type="text">Books sorted alphabetically</content>
  </entry>
  <entry>
    <title>Recently added Books</title>
    <link href="/opds/new" type="application/atom+xml;profile=opds-catalog"/>
    <id>/opds/new</id>
    <updated>2026-08-16T10:00:00+00:00</updated>
    <content type="text">The latest books</content>
  </entry>
</feed>"#;

#[test]
fn the_calibre_web_root_feed_is_navigable() {
    let catalog = parse_catalog(CALIBRE_WEB_ROOT);
    assert_eq!(catalog.title, "Calibre-Web");
    assert_eq!(
        catalog.entries.len(),
        2,
        "the root feed's shelves are listed"
    );

    let first = &catalog.entries[0];
    assert_eq!(first.kind, EntryKind::Navigation);
    assert_eq!(first.title, "Alphabetical Books");
    // The destination is what makes the row tappable; without it the library is a dead end.
    assert_eq!(
        first.href, "/opds/books",
        "a rel-less catalog link is still followed"
    );
    assert_eq!(first.summary, "Books sorted alphabetically");
    assert_eq!(catalog.entries[1].href, "/opds/new");
}

#[test]
fn the_search_link_chosen_is_the_one_that_can_be_searched() {
    // Calibre-Web advertises the OpenSearch *description document* first and the query template
    // second. Picking the first would send every search to a metadata document and return nothing.
    let catalog = parse_catalog(CALIBRE_WEB_FEED);
    assert_eq!(catalog.search_template, "/opds/search/{searchTerms}");
}

#[test]
fn a_description_only_search_link_yields_no_template() {
    // With nothing searchable advertised, the shell must get "" and hide the search box rather than
    // offer a control that silently does nothing.
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>t</title><id>i</id><updated>2026-08-14T10:00:00+00:00</updated>
  <link rel="search" href="/opds/osd" type="application/opensearchdescription+xml"/>
</feed>"#;
    assert_eq!(parse_catalog(xml).search_template, "");
}

#[test]
fn a_format_is_never_guessed_into_something_unopenable() {
    // A MOBI served as octet-stream must stay unopenable: neither its title nor its href names a
    // format this reader handles, so the fallbacks must not manufacture one.
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>t</title><id>i</id><updated>2026-08-14T10:00:00+00:00</updated>
  <entry>
    <title>Only MOBI</title><id>e</id><updated>2026-08-14T10:00:00+00:00</updated>
    <link rel="http://opds-spec.org/acquisition" href="/opds/download/9/mobi/"
          title="MOBI" type="application/octet-stream"/>
  </entry>
</feed>"#;
    let entry = &parse_catalog(xml).entries[0];
    assert!(entry.formats[0].ext.is_empty(), "no format was invented");
}

#[test]
fn a_query_string_cannot_smuggle_a_format_into_the_href() {
    // Only path segments are scanned: a title in the query must not make an unopenable download
    // look openable.
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>t</title><id>i</id><updated>2026-08-14T10:00:00+00:00</updated>
  <entry>
    <title>Trap</title><id>e</id><updated>2026-08-14T10:00:00+00:00</updated>
    <link rel="http://opds-spec.org/acquisition" href="/dl/9?name=epub"
          type="application/octet-stream"/>
  </entry>
</feed>"#;
    assert!(parse_catalog(xml).entries[0].formats[0].ext.is_empty());
}

/// The JSON contract the Kotlin shell reads, key by key.
///
/// This is the seam with no compiler and no runtime error behind it: a key the core renames, or
/// spells in a different convention, reaches the shell as a value that is simply *absent*. Nothing
/// throws, nothing logs — the feature just quietly does nothing. That is exactly how the search
/// template was lost (emitted `search_template`, read `searchTemplate`), so every key the shell
/// looks up is pinned here.
#[test]
fn the_json_keys_are_exactly_what_the_shell_reads() {
    let value: serde_json::Value =
        serde_json::from_str(&parse_catalog_json(CALIBRE_WEB_FEED)).expect("valid JSON");

    // Feed level — mirrors OpdsController.parseCatalog.
    for key in ["isCatalog", "title", "entries", "next", "searchTemplate"] {
        assert!(value.get(key).is_some(), "feed key `{key}` is missing");
    }
    assert_eq!(value["searchTemplate"], "/opds/search/{searchTerms}");

    // Entry level.
    let book = &value["entries"][0];
    for key in ["kind", "title", "author", "published", "cover", "formats"] {
        assert!(book.get(key).is_some(), "entry key `{key}` is missing");
    }

    // Format level.
    let format = &book["formats"][0];
    for key in ["mime", "href", "bytes", "ext"] {
        assert!(format.get(key).is_some(), "format key `{key}` is missing");
    }

    // A navigation entry's destination.
    assert!(
        value["entries"][1].get("href").is_some(),
        "navigation `href` is missing"
    );
}

#[test]
fn an_empty_shelf_and_a_non_catalog_are_told_apart() {
    // Both produce zero entries, and only one of them is the reader's mistake to fix. Pointing at a
    // server's HTML UI instead of its catalog must not report "your library is empty".
    let empty_feed = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Calibre-Web</title><id>i</id><updated>2026-08-16T10:00:00+00:00</updated>
</feed>"#;
    let empty = parse_catalog(empty_feed);
    assert!(
        empty.is_catalog,
        "a real feed with no books is still a catalog"
    );
    assert!(empty.entries.is_empty());

    for not_a_catalog in [
        "<html><body><h1>Calibre-Web</h1></body></html>",
        "404 Not Found",
        "",
    ] {
        assert!(
            !parse_catalog(not_a_catalog).is_catalog,
            "{not_a_catalog:?} is not a catalog",
        );
    }
}

#[test]
fn empty_catalog_json_constant_matches_a_real_empty_catalog() {
    // The defensive fallback in parse_catalog_json must not drift from the derived serialization.
    assert_eq!(
        serde_json::to_string(&Catalog::default()).unwrap(),
        Catalog::EMPTY_JSON
    );
}
