//! `inkread-opds` (ADR-INKREAD-0016): the **classification** half of the OPDS library client.
//!
//! Readers keep their books in a catalog server and want them on the device without a cable. The
//! Android shell fetches an OPDS document and hands the XML to this crate, which turns it into the
//! browse model the shelf renders: which entries lead to another feed, which entries are books,
//! which of a book's formats the reader can actually open, and where the paging/search links go.
//!
//! Everything here is **pure**: no network, no clock, no host string, no product name (IR-7). The
//! shell owns the server address, resolves relative hrefs against it, and does every byte of I/O.
//!
//! OPDS 1.2 is Atom with extra link relations, so [`feed_rs`] does the parsing (the same dependency
//! `inkread-daily` uses) and this crate supplies the OPDS meaning on top. The public surface is the
//! string-in/string-out [`parse_catalog_json`], matching the `parse_feed_json` / `decide` contract
//! the other crates expose over the same JNI bridge.

use feed_rs::model::{Entry, Link};
use serde::Serialize;

/// Parse an OPDS document into the catalog JSON the shell renders (the JNI-friendly form of
/// [`parse_catalog`]).
///
/// Any unusable input — malformed XML, a non-feed document, an empty body — yields a catalog with
/// no entries rather than an error (RR21-FR3: junk in → benign out).
#[must_use]
pub fn parse_catalog_json(xml: &str) -> String {
    serde_json::to_string(&parse_catalog(xml)).unwrap_or_else(|_| Catalog::EMPTY_JSON.to_string())
}

/// Parse an OPDS document into a [`Catalog`]. Tolerant of malformed input (see
/// [`parse_catalog_json`]).
#[must_use]
pub fn parse_catalog(xml: &str) -> Catalog {
    let feed = match feed_rs::parser::parse(xml.as_bytes()) {
        Ok(f) => f,
        Err(_) => return Catalog::default(),
    };
    Catalog {
        is_catalog: true,
        title: feed
            .title
            .map(|t| t.content.trim().to_string())
            .unwrap_or_default(),
        next: feed_href(&feed.links, &["next"]),
        prev: feed_href(&feed.links, &["previous", "prev"]),
        start: feed_href(&feed.links, &["start"]),
        search_template: search_template(&feed.links),
        entries: feed.entries.into_iter().map(catalog_entry).collect(),
    }
}

/// One OPDS feed, flattened into what a browse screen needs.
///
/// Serialized **camelCase**: this JSON is read by the Kotlin shell, and a Rust-side `snake_case`
/// field silently reaches it as a key it never looks up — the value is simply absent, with no error
/// anywhere. [`tests::the_json_keys_are_exactly_what_the_shell_reads`] pins the whole contract.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    /// Whether the document parsed as a feed at all.
    ///
    /// An empty catalog is ambiguous without this: a genuinely empty shelf and a URL pointing at
    /// something that is not OPDS — a server's HTML UI, an error page — both yield no entries. The
    /// shell needs to tell a reader "your library is empty" from "that address is not a catalog",
    /// because only one of them is their mistake to fix.
    pub is_catalog: bool,
    /// Feed title, for the screen's heading.
    pub title: String,
    pub entries: Vec<CatalogEntry>,
    /// Paging + navigation links, verbatim as the server wrote them (usually relative — the shell
    /// resolves them against the feed URL). Empty when the feed omits the relation.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub next: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub prev: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub start: String,
    /// The OpenSearch template, with its `{searchTerms}` placeholder left in place for the shell to
    /// substitute (it owns the escaping, since it owns the URL).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub search_template: String,
}

impl Catalog {
    /// The serialization of an empty catalog — the defensive fallback in [`parse_catalog_json`].
    pub(crate) const EMPTY_JSON: &'static str = r#"{"isCatalog":false,"title":"","entries":[]}"#;
}

/// What tapping an entry does: load another feed, or download a book.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    /// Leads to another OPDS feed (a shelf, an author, a category).
    Navigation,
    /// A book, with one or more downloadable formats.
    Acquisition,
}

/// One entry in a catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatalogEntry {
    pub kind: EntryKind,
    pub title: String,
    /// Authors joined with ", "; empty when the entry names none.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub author: String,
    /// Summary (or, failing that, content) as the server supplied it.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub summary: String,
    /// Published (or, failing that, updated) date as "DD Mon YYYY" — the format the daily companion
    /// already renders, so dates read the same everywhere in the app.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub published: String,
    /// Where a `Navigation` entry leads. Empty for an acquisition entry.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub href: String,
    /// Cover art, preferring the thumbnail (an e-ink list wants the small one).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub cover: String,
    /// Downloadable formats, **best first**: the ones this reader can open, in preference order,
    /// then anything else. Empty for a navigation entry.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub formats: Vec<Format>,
}

/// One downloadable rendition of a book.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Format {
    /// The media type the server advertised.
    pub mime: String,
    /// The acquisition href, verbatim (the shell resolves and fetches it).
    pub href: String,
    /// Size in bytes when the server declared one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// The file extension this reader would save it as (`epub`/`pdf`/`cbz`/`txt`), or empty when
    /// inkread cannot open the format. An acquisition href generally carries no extension of its
    /// own, so this is what the shell names the downloaded file — and how the UI knows what to
    /// offer versus grey out.
    pub ext: String,
}

/// Formats the reader opens, in the order it prefers them. EPUB first: it reflows, so it is the
/// format that actually reads well on a small panel. Kept in step with the shell's supported set.
const EXT_ORDER: [&str; 4] = ["epub", "pdf", "cbz", "txt"];

/// Media types that identify those formats. Several servers get this wrong (see
/// [`extension_for`]), so it is the first source consulted, not the only one.
const PREFERRED: [(&str, &str); 5] = [
    ("application/epub+zip", "epub"),
    ("application/pdf", "pdf"),
    ("application/x-cbz", "cbz"),
    ("application/vnd.comicbook+zip", "cbz"),
    ("text/plain", "txt"),
];

/// The OPDS relation prefix marking a link as a download. OPDS defines sub-relations
/// (`/open-access`, `/borrow`, `/buy`, `/sample`), so this matches on the prefix rather than an
/// exact string — a lending catalog's `…/acquisition/borrow` is still a way to get the book.
const REL_ACQUISITION: &str = "http://opds-spec.org/acquisition";

/// Cover-art relations, most wanted first: a thumbnail is the right size for a list on a panel that
/// repaints slowly, so prefer it over the full cover.
const REL_IMAGES: [&str; 4] = [
    "http://opds-spec.org/image/thumbnail",
    "http://opds-spec.org/thumbnail",
    "http://opds-spec.org/image",
    "http://opds-spec.org/cover",
];

/// The media-type marker OPDS uses for "this link is another catalog feed" — the navigation/
/// acquisition discriminator when an entry carries no acquisition link.
const CATALOG_PROFILE: &str = "profile=opds-catalog";

/// Classify one Atom entry as navigation or acquisition and flatten it.
fn catalog_entry(entry: Entry) -> CatalogEntry {
    let formats = acquisition_formats(&entry.links);
    let kind = if formats.is_empty() {
        EntryKind::Navigation
    } else {
        EntryKind::Acquisition
    };
    CatalogEntry {
        kind,
        title: entry
            .title
            .map(|t| t.content.trim().to_string())
            .unwrap_or_default(),
        author: entry
            .authors
            .iter()
            .map(|p| p.name.trim())
            .filter(|n| !n.is_empty())
            .collect::<Vec<_>>()
            .join(", "),
        summary: entry
            .summary
            .map(|t| t.content)
            .or_else(|| entry.content.and_then(|c| c.body))
            .map(|s| s.trim().to_string())
            .unwrap_or_default(),
        published: entry
            .published
            .or(entry.updated)
            .map(|d| d.format("%d %b %Y").to_string())
            .unwrap_or_default(),
        // A navigation entry's destination: the catalog link if it is marked as one, else the
        // entry's first link — some servers omit the profile marker on a plain alternate link.
        href: match kind {
            EntryKind::Navigation => navigation_href(&entry.links),
            EntryKind::Acquisition => String::new(),
        },
        cover: image_href(&entry.links),
        formats,
    }
}

/// The download links on an entry, best-supported first. Unsupported formats are kept (so the UI can
/// show that a MOBI exists and cannot be opened) but sorted last.
fn acquisition_formats(links: &[Link]) -> Vec<Format> {
    let mut formats: Vec<Format> = links
        .iter()
        .filter(|l| {
            l.rel
                .as_deref()
                .is_some_and(|r| r.starts_with(REL_ACQUISITION))
                && !l.href.trim().is_empty()
        })
        .map(|l| {
            let mime = l.media_type.clone().unwrap_or_default();
            let href = l.href.trim().to_string();
            Format {
                ext: extension_for(&mime, l.title.as_deref().unwrap_or_default(), &href)
                    .unwrap_or_default()
                    .to_string(),
                mime,
                href,
                bytes: l.length,
            }
        })
        .collect();
    // Stable sort by preference rank, so equally-ranked formats keep the server's order.
    formats.sort_by_key(|f| rank(&f.ext));
    formats
}

/// Preference rank of a resolved extension: its index in [`EXT_ORDER`], or past the end when the
/// reader cannot open it. Ranking on the *resolved* extension rather than the media type matters —
/// a server that labels its EPUBs `application/octet-stream` must still have them sort first.
fn rank(ext: &str) -> usize {
    EXT_ORDER
        .iter()
        .position(|e| *e == ext)
        .unwrap_or(EXT_ORDER.len())
}

/// The file extension this reader would save a download as, or `None` when it cannot open it.
///
/// Three sources, in descending order of authority, because **the media type alone is not reliable
/// in the field**. Servers commonly derive it from the host's mime database, and where that database
/// has no entry for a format the link goes out as `application/octet-stream` — a correctly-served
/// EPUB that a mime-only reader would refuse to download.
///
/// 1. The declared media type — right when present, and the only standards-blessed source.
/// 2. The link's `title`, which acquisition links conventionally set to the format name ("EPUB").
/// 3. A format token in the href, since download URLs usually name the format they serve.
///
/// Every source must match a format this reader actually opens, so a wrong guess degrades to
/// "cannot open" rather than to a download that fails on the way in.
fn extension_for(mime: &str, title: &str, href: &str) -> Option<&'static str> {
    if let Some(ext) = PREFERRED
        .iter()
        .find(|(m, _)| mime.eq_ignore_ascii_case(m))
        .map(|(_, ext)| *ext)
    {
        return Some(ext);
    }
    if let Some(ext) = format_token(title) {
        return Some(ext);
    }
    // Path segments only — a query string may carry anything, including a title.
    href.split(['?', '#'])
        .next()
        .unwrap_or_default()
        .split('/')
        .find_map(format_token)
}

/// `token` as one of the formats this reader opens, ignoring case and any leading dot.
fn format_token(token: &str) -> Option<&'static str> {
    let cleaned = token.trim().trim_start_matches('.');
    EXT_ORDER
        .iter()
        .find(|e| cleaned.eq_ignore_ascii_case(e))
        .copied()
}

/// Where a navigation entry leads: a link explicitly marked as a catalog feed, else the first link
/// with an href (a server may omit the profile marker).
fn navigation_href(links: &[Link]) -> String {
    links
        .iter()
        .find(|l| {
            l.media_type
                .as_deref()
                .is_some_and(|t| t.contains(CATALOG_PROFILE))
        })
        .or_else(|| links.iter().find(|l| !l.href.trim().is_empty()))
        .map(|l| l.href.trim().to_string())
        .unwrap_or_default()
}

/// The entry's cover art, preferring the smallest useful rendition (see [`REL_IMAGES`]).
fn image_href(links: &[Link]) -> String {
    REL_IMAGES
        .iter()
        .find_map(|want| {
            links
                .iter()
                .find(|l| l.rel.as_deref() == Some(*want) && !l.href.trim().is_empty())
        })
        .map(|l| l.href.trim().to_string())
        .unwrap_or_default()
}

/// The searchable URL template — a `search` link whose href actually carries `{searchTerms}`.
///
/// A feed may advertise **several** `search` links: one pointing at an OpenSearch *description
/// document* (which describes how to search, and is not itself a query) and one carrying the query
/// template. Taking whichever came first would send the search to the description document, which
/// answers with metadata rather than results — a search that quietly returns nothing rather than
/// failing. Requiring the placeholder picks the usable link regardless of order, and yields nothing
/// when only a description document is offered, which correctly hides the search box.
fn search_template(links: &[Link]) -> String {
    links
        .iter()
        .find(|l| {
            l.rel
                .as_deref()
                .is_some_and(|r| r.eq_ignore_ascii_case("search"))
                && l.href.contains(SEARCH_TERMS)
        })
        .map(|l| l.href.trim().to_string())
        .unwrap_or_default()
}

/// The OpenSearch placeholder the shell substitutes the reader's query into.
const SEARCH_TERMS: &str = "{searchTerms}";

/// The feed-level href for the first of `rels` the feed carries (paging / start / search).
fn feed_href(links: &[Link], rels: &[&str]) -> String {
    rels.iter()
        .find_map(|want| {
            links.iter().find(|l| {
                l.rel
                    .as_deref()
                    .is_some_and(|r| r.eq_ignore_ascii_case(want))
                    && !l.href.trim().is_empty()
            })
        })
        .map(|l| l.href.trim().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
