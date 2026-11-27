//! Assemble an [`Issue`] into a self-contained EPUB the inkread reader opens (#66).
//!
//! Emits the minimal OCF/EPUB-3 structure the reader's parser accepts: a stored `mimetype` first,
//! `META-INF/container.xml`, an OPF (manifest + spine), an EPUB-3 nav doc (TOC), a title page, and
//! one XHTML document per article. Pure + host-testable — no network, no clock (the caller stamps
//! the date). In-memory ZIP writing to a `Vec` cannot fail, so this is infallible.

use std::io::{Cursor, Write};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::extract::{MATH_IMG_CLOSE, MATH_IMG_OPEN};
use crate::model::{DailyLayoutStyle, Issue};

/// A rendered display equation: its EPUB resource name (`a{i}-eq{n}.png`) and the PNG bytes.
type MathImg = (String, Vec<u8>);

/// The Vintage nameplate, "The Inkread Daily" in blackletter, shipped as a grayscale PNG because the
/// reflow engine has no per-block font selection (see `assets/NOTICE.md`).
const VINTAGE_MASTHEAD: &[u8] = include_bytes!("../assets/vintage-masthead.png");

/// Build the issue EPUB as bytes. The result opens with the reader's EPUB backend and reflows like
/// any book (honoring the shipped font-size/spacing controls + reflow-stable resume).
#[must_use]
pub fn assemble_epub(issue: &Issue) -> Vec<u8> {
    // Render the articles first, collecting the display-equation images each one embeds, so the OPF
    // manifest can list those image resources before the package is written.
    let mut math_imgs: Vec<MathImg> = Vec::new();
    let articles: Vec<String> = (0..issue.articles.len())
        .map(|i| {
            let (doc, imgs) = article_xhtml(issue, i);
            math_imgs.extend(imgs);
            doc
        })
        .collect();

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
    add(
        &mut zw,
        "OEBPS/content.opf",
        opf(issue, &math_imgs).as_bytes(),
        deflate,
    );
    add(&mut zw, "OEBPS/nav.xhtml", nav(issue).as_bytes(), deflate);
    add(
        &mut zw,
        "OEBPS/title.xhtml",
        title_page(issue).as_bytes(),
        deflate,
    );
    // Vintage draws a blackletter nameplate at the head of each article (ADR-INKREAD-0018 D1); it is
    // one shared image, referenced from every article. PNG is already compressed, so store it.
    if issue.style == DailyLayoutStyle::Vintage {
        add(
            &mut zw,
            "OEBPS/vintage-masthead.png",
            VINTAGE_MASTHEAD,
            stored,
        );
    }
    for (i, doc) in articles.iter().enumerate() {
        add(&mut zw, &article_path(i), doc.as_bytes(), deflate);
    }
    // Each display equation renders to its own PNG (already compressed, so store it).
    for (name, png) in &math_imgs {
        add(&mut zw, &format!("OEBPS/{name}"), png, stored);
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
fn opf(issue: &Issue, math_imgs: &[MathImg]) -> String {
    let mut manifest = String::from(
        r#"    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="title" href="title.xhtml" media-type="application/xhtml+xml"/>
"#,
    );
    if issue.style == DailyLayoutStyle::Vintage {
        manifest.push_str(
            "    <item id=\"masthead\" href=\"vintage-masthead.png\" media-type=\"image/png\"/>\n",
        );
    }
    let mut spine = String::from("    <itemref idref=\"title\"/>\n");
    for i in 0..issue.articles.len() {
        manifest.push_str(&format!(
            "    <item id=\"a{i}\" href=\"a{i:04}.xhtml\" media-type=\"application/xhtml+xml\"/>\n"
        ));
        spine.push_str(&format!("    <itemref idref=\"a{i}\"/>\n"));
    }
    // Each display-equation image is a manifest resource (not in the spine — images are referenced,
    // not read in order). The name (`a{i}-eq{n}.png`) is a valid id and cannot collide with an
    // article id (`a{i}`), the nav, the title or the masthead.
    for (name, _) in math_imgs {
        let id = name.trim_end_matches(".png");
        manifest.push_str(&format!(
            "    <item id=\"{id}\" href=\"{name}\" media-type=\"image/png\"/>\n"
        ));
    }
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="id">urn:inkread-daily:{style}:{date}</dc:identifier>
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
        style = issue.style.as_str(),
    )
}

/// The issue's articles grouped by source (#269): `(source, article indices)` in the reader's source
/// order ([`Issue::sources`]), then any source not listed there in first-seen order. Within a section
/// articles keep their reading order. Only sources with articles appear.
///
/// The spine stays round-robin (#107) — it is the page-turn order, and interleaving is what keeps
/// every source near the front. Only the contents views are grouped, which is how a reader finds the
/// topics worth reading in a past issue. The front page orders its sections by the same rule
/// (`DailyController.inSourceOrder`).
fn sections(issue: &Issue) -> Vec<(&str, Vec<usize>)> {
    let mut out: Vec<(&str, Vec<usize>)> = issue
        .sources
        .iter()
        .map(|s| (s.as_str(), Vec::new()))
        .collect();
    for (i, art) in issue.articles.iter().enumerate() {
        match out.iter_mut().find(|(s, _)| *s == art.source) {
            Some((_, indices)) => indices.push(i),
            None => out.push((art.source.as_str(), vec![i])),
        }
    }
    out.retain(|(_, indices)| !indices.is_empty());
    out
}

/// An article's href relative to OEBPS/ (where the OPF, nav and title page live).
fn article_href(i: usize) -> String {
    article_path(i).trim_start_matches("OEBPS/").to_string()
}

/// The EPUB-3 nav doc: the cover, then one entry per source with its articles nested beneath (#269).
/// A section entry links to its first article, so choosing a source in the TOC opens it.
fn nav(issue: &Issue) -> String {
    let mut items = String::from("    <li><a href=\"title.xhtml\">Cover</a></li>\n");
    for (source, indices) in sections(issue) {
        items.push_str(&format!(
            "    <li><a href=\"{}\">{}</a><ol>\n",
            article_href(indices[0]),
            esc(source)
        ));
        for &i in &indices {
            items.push_str(&format!(
                "      <li><a href=\"{}\">{}</a></li>\n",
                article_href(i),
                esc(&issue.articles[i].title)
            ));
        }
        items.push_str("    </ol></li>\n");
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

/// The cover: issue title, date, and the **In This Issue** contents (#198), grouped by source (#269).
///
/// A newspaper opens with a contents page so you can decide what is worth reading before committing
/// to a page turn — which matters more on e-ink, where browsing costs a refresh each way. Each source
/// is a section, as on the Daily front page; every entry is a link into the article and shows a short
/// excerpt where one can be had.
fn title_page(issue: &Issue) -> String {
    let mut toc = String::new();
    for (source, indices) in sections(issue) {
        toc.push_str(&format!(
            "<h3 class=\"section\">{}</h3>\n<ul>\n",
            esc(source)
        ));
        for &i in &indices {
            let art = &issue.articles[i];
            toc.push_str(&format!(
                "    <li><a href=\"{}\">{}</a>",
                article_href(i),
                esc(&art.title)
            ));
            // Title-only when there is nothing worth showing — an empty or near-empty line under
            // every headline reads worse than no line at all.
            if let Some(text) = art.excerpt(EXCERPT_CHARS) {
                toc.push_str(&format!(
                    "<br/><span class=\"excerpt\">{}</span>",
                    esc(&text)
                ));
            }
            toc.push_str("</li>\n");
        }
        toc.push_str("</ul>\n");
    }
    // The quotation sits between the masthead and the contents (#195) — where a paper puts it, and
    // read before deciding what to read.
    let quote = crate::quote::quote_for(&issue.date).map_or(String::new(), |q| {
        format!(
            "<blockquote class=\"quote\"><p>{}</p>\n<p class=\"attrib\">{} · {}</p></blockquote>\n",
            esc(q.text),
            esc(q.author),
            esc(q.work)
        )
    });
    xhtml(
        &issue.title,
        &format!(
            "<h1>{}</h1>\n<p class=\"date\">{}</p>\n{quote}<h2>In This Issue</h2>\n{toc}",
            esc(&issue.title),
            esc(&issue.date)
        ),
        issue.style,
    )
}

/// How much of an article to show under its headline. Long enough to say what the piece is about,
/// short enough that a dozen of them still fit a contents page.
const EXCERPT_CHARS: usize = 160;

/// One article document: headline, a source · date byline, then the clean article body. Returns the
/// document plus the `(resource name, PNG bytes)` of every display equation the body embeds, for the
/// caller to add to the package and the manifest.
fn article_xhtml(issue: &Issue, i: usize) -> (String, Vec<MathImg>) {
    let art = &issue.articles[i];
    // Vintage sets the byline in capitals (ADR-INKREAD-0018 D1). Uppercase the raw text *before*
    // escaping, so an entity like `&amp;` is not mangled into `&AMP;`.
    let caps = |s: &str| {
        if issue.style == DailyLayoutStyle::Vintage {
            s.to_uppercase()
        } else {
            s.to_string()
        }
    };
    // Vintage sets the source line as a dateline ("REUTERS · REPORTED 23 Nov 2026"); the other styles
    // keep the plain "source · date".
    let byline = {
        let date = art
            .published
            .as_deref()
            .map(clean_date)
            .filter(|d| !d.is_empty())
            .map(|d| {
                let label = if issue.style == DailyLayoutStyle::Vintage {
                    "REPORTED "
                } else {
                    ""
                };
                format!(" · {label}{}", esc(&caps(&d)))
            })
            .unwrap_or_default();
        format!("{}{date}", esc(&caps(&art.source)))
    };
    // Vintage opens each article with a blackletter nameplate and a rule, then a folio line (date at
    // the left, edition at the right), then the headline and dateline, and a closing rule above the
    // body columns, all drawn full-width in the band (ADR-INKREAD-0018 D1).
    let (lead, body_rule) = if issue.style == DailyLayoutStyle::Vintage {
        (
            format!(
                "<p class=\"masthead\"><img src=\"vintage-masthead.png\" alt=\"The Daily\"/></p>\n\
                 <hr/>\n\
                 <table class=\"folio\"><tr><td>{}</td>\
                 <td class=\"edition\">EVENING EDITION</td></tr></table>\n",
                esc(&issue.date.to_uppercase()),
            ),
            "<hr/>\n",
        )
    } else {
        (String::new(), "")
    };
    // Render and embed the body's display equations, rewriting each math <img> to its resource.
    let (body_html, math_imgs) = embed_math(&art.body_html, i);
    let body = format!(
        "{lead}<h1>{}</h1>\n<p class=\"byline\">{byline}</p>\n{body_rule}{body_html}",
        esc(&art.title),
    );
    (xhtml(&art.title, &body, issue.style), math_imgs)
}

/// Render the display-equation images the extractor left in `body` (each `<img class="latex"
/// alt="LATEX"/>`, LaTeX in the `alt`), returning the body with every equation pointed at a unique
/// per-article image resource, plus the `(resource name, PNG bytes)` to embed.
///
/// A formula that does not render keeps its LaTeX as the visible text (the image is dropped), so one
/// unsupported equation degrades gracefully instead of breaking the issue. The extractor emits the
/// marker in an exact shape (quotes and `>` are escaped inside the `alt`, so `"/>` first appears at
/// the tag's close), which lets this scan it without a full HTML parse.
fn embed_math(body: &str, article_index: usize) -> (String, Vec<MathImg>) {
    if !body.contains(MATH_IMG_OPEN) {
        return (body.to_string(), Vec::new());
    }
    // Load the bundled math font once for the whole article (not per equation). If it fails to load,
    // every equation below degrades to its LaTeX text, which is the same graceful path as a formula
    // that does not render.
    let font = crate::math::load_font();
    let mut out = String::with_capacity(body.len());
    let mut imgs: Vec<MathImg> = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.find(MATH_IMG_OPEN) {
        out.push_str(&rest[..i]);
        let after = &rest[i + MATH_IMG_OPEN.len()..];
        let Some(j) = after.find(MATH_IMG_CLOSE) else {
            // Not the extractor's shape (cannot happen for emitted markers) — keep it verbatim.
            out.push_str(&rest[i..]);
            return (out, imgs);
        };
        let escaped_latex = &after[..j];
        rest = &after[j + MATH_IMG_CLOSE.len()..];
        match font
            .as_ref()
            .and_then(|f| crate::math::render_display(f, &unescape(escaped_latex)))
        {
            Some(png) => {
                let name = format!("a{article_index}-eq{}.png", imgs.len());
                out.push_str(&format!(
                    "<img class=\"latex\" src=\"{name}\" alt=\"{escaped_latex}\"/>"
                ));
                imgs.push((name, png));
            }
            // Keep the formula readable as its LaTeX source when it does not render.
            None => out.push_str(escaped_latex),
        }
    }
    out.push_str(rest);
    (out, imgs)
}

/// Reverse [`esc`] for the five XML entities, to recover the raw LaTeX from an escaped `alt` value.
/// `&amp;` is decoded last so an input like `&amp;lt;` becomes `&lt;`, not `<`.
fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Turn a raw feed date into a short display date — drop the weekday prefix and the time/zone, so
/// "Wed, 25 Jun 2026 18:33:54 +0000" → "25 Jun 2026" and "2026-06-25T18:33:54-04:00" → "2026-06-25".
fn clean_date(s: &str) -> String {
    let mut s = s.trim();
    if let Some(idx) = s.find(", ") {
        if idx <= 4 {
            s = s[idx + 2..].trim(); // strip a leading weekday like "Wed, "
        }
    }
    if let Some(t) = s.find('T') {
        return s[..t].to_string(); // ISO-8601 → keep the date
    }
    s.split_whitespace().take(3).collect::<Vec<_>>().join(" ") // RFC-822 → "25 Jun 2026"
}

/// Wrap `body` (raw XHTML markup) in a minimal, well-formed XHTML document titled `title`, with the
/// style's head CSS (ADR-INKREAD-0018).
fn xhtml(title: &str, body: &str, style: DailyLayoutStyle) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml">
  <head><title>{}</title>{head}</head>
  <body>
{body}
  </body>
</html>"#,
        esc(title),
        head = head_css(style),
    )
}

/// Extra per-style CSS for a document head (ADR-INKREAD-0018 D1). Vintage centres the headline and
/// its nameplate, dateline and byline for a classic masthead feel (centre alignment and class
/// selectors are honoured by the reflow engine, #188); the other styles add nothing, so their
/// documents stay byte-identical to before styles existed.
fn head_css(style: DailyLayoutStyle) -> &'static str {
    match style {
        DailyLayoutStyle::Vintage => {
            "<style>\
             .masthead { text-align: center; margin: 0 0 0.1em; } \
             hr { margin: 0.12em 0; } \
             .folio { margin: 0.1em 0; } \
             .edition { text-align: right; } \
             h1 { text-align: center; margin: 0.3em 0 0.05em; } \
             .byline { text-align: center; margin: 0 0 0.15em; }\
             </style>"
        }
        _ => "",
    }
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
