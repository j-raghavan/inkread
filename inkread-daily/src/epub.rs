//! Assemble an [`Issue`] into a self-contained EPUB the inkread reader opens (#66).
//!
//! Emits the minimal OCF/EPUB-3 structure the reader's parser accepts: a stored `mimetype` first,
//! `META-INF/container.xml`, an OPF (manifest + spine), an EPUB-3 nav doc (TOC), a title page, and
//! one XHTML document per article. Pure + host-testable — no network, no clock (the caller stamps
//! the date). In-memory ZIP writing to a `Vec` cannot fail, so this is infallible.

use std::io::{Cursor, Write};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::model::Issue;

/// Build the issue EPUB as bytes. The result opens with the reader's EPUB backend and reflows like
/// any book (honoring the shipped font-size/spacing controls + reflow-stable resume).
#[must_use]
pub fn assemble_epub(issue: &Issue) -> Vec<u8> {
    let mut zw = ZipWriter::new(Cursor::new(Vec::new()));

    // OCF requires the mimetype entry FIRST and STORED (uncompressed).
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    add(&mut zw, "mimetype", b"application/epub+zip", stored);

    let deflate = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    add(
        &mut zw,
        "META-INF/container.xml",
        CONTAINER_XML.as_bytes(),
        deflate,
    );
    add(&mut zw, "OEBPS/content.opf", opf(issue).as_bytes(), deflate);
    add(&mut zw, "OEBPS/nav.xhtml", nav(issue).as_bytes(), deflate);
    add(
        &mut zw,
        "OEBPS/title.xhtml",
        title_page(issue).as_bytes(),
        deflate,
    );
    for i in 0..issue.articles.len() {
        add(
            &mut zw,
            &article_path(i),
            article_xhtml(issue, i).as_bytes(),
            deflate,
        );
    }

    zw.finish()
        .expect("in-memory zip finish is infallible")
        .into_inner()
}

/// Write one ZIP entry (in-memory writes are infallible — entry names are crate-controlled).
fn add(zw: &mut ZipWriter<Cursor<Vec<u8>>>, name: &str, data: &[u8], opts: SimpleFileOptions) {
    zw.start_file(name, opts).expect("valid zip entry name");
    zw.write_all(data)
        .expect("in-memory zip write is infallible");
}

/// The XHTML path for article `i` (zero-padded so reading order is stable in any tooling).
fn article_path(i: usize) -> String {
    format!("OEBPS/a{i:04}.xhtml")
}

const CONTAINER_XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#;

/// The OPF package: metadata + a manifest of (nav, title, every article) and a spine that opens on
/// the title page then reads the articles in order.
fn opf(issue: &Issue) -> String {
    let mut manifest = String::from(
        r#"    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="title" href="title.xhtml" media-type="application/xhtml+xml"/>
"#,
    );
    let mut spine = String::from("    <itemref idref=\"title\"/>\n");
    for i in 0..issue.articles.len() {
        manifest.push_str(&format!(
            "    <item id=\"a{i}\" href=\"a{i:04}.xhtml\" media-type=\"application/xhtml+xml\"/>\n"
        ));
        spine.push_str(&format!("    <itemref idref=\"a{i}\"/>\n"));
    }
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="id">urn:inkread-daily:{date}</dc:identifier>
    <dc:title>{title} — {date}</dc:title>
    <dc:language>en</dc:language>
  </metadata>
  <manifest>
{manifest}  </manifest>
  <spine>
{spine}  </spine>
</package>"#,
        title = esc(&issue.title),
        date = esc(&issue.date),
    )
}

/// The EPUB-3 nav doc: a TOC linking the title page and each article (by its headline).
fn nav(issue: &Issue) -> String {
    let mut items = String::from("    <li><a href=\"title.xhtml\">Cover</a></li>\n");
    for (i, art) in issue.articles.iter().enumerate() {
        items.push_str(&format!(
            "    <li><a href=\"a{i:04}.xhtml\">{}</a></li>\n",
            esc(&art.title)
        ));
    }
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
  <head><title>Contents</title></head>
  <body><nav epub:type="toc"><ol>
{items}  </ol></nav></body>
</html>"#
    )
}

/// The cover/title page: issue title, date, and a contents list.
fn title_page(issue: &Issue) -> String {
    let mut toc = String::new();
    for art in &issue.articles {
        toc.push_str(&format!(
            "    <li>{}<br/><span class=\"src\">{}</span></li>\n",
            esc(&art.title),
            esc(&art.source)
        ));
    }
    xhtml(
        &issue.title,
        &format!(
            "<h1>{}</h1>\n<p class=\"date\">{}</p>\n<ul>\n{toc}</ul>",
            esc(&issue.title),
            esc(&issue.date)
        ),
    )
}

/// One article document: headline, a source · date byline, then the clean article body.
fn article_xhtml(issue: &Issue, i: usize) -> String {
    let art = &issue.articles[i];
    let byline = match &art.published {
        Some(d) => format!("{} · {}", esc(&art.source), esc(d)),
        None => esc(&art.source),
    };
    let body = format!(
        "<h1>{}</h1>\n<p class=\"byline\">{byline}</p>\n{}",
        esc(&art.title),
        art.body_html // trusted: caller's contract is already-clean, well-formed XHTML (not escaped)
    );
    xhtml(&art.title, &body)
}

/// Wrap `body` (raw XHTML markup) in a minimal, well-formed XHTML document titled `title`.
fn xhtml(title: &str, body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml">
  <head><title>{}</title></head>
  <body>
{body}
  </body>
</html>"#,
        esc(title)
    )
}

/// Escape the five XML metacharacters so user-supplied titles/sources/dates can't break the markup.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
#[path = "epub_tests.rs"]
mod tests;
