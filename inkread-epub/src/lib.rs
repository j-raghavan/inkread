//! `inkread-epub` — EPUB parsing + (forthcoming) pure-Rust reflow for inkread (RR2-FR5 /
//! ADR-INKREAD-0007 Decision 2; reflow engine per `ADR-RUST-READER` Decision 1).
//!
//! **Phase 1 (this module): the container foundation.** Open an EPUB from bytes and expose its
//! reading-order chapters (XHTML), metadata, and table of contents as plain owned data — the input
//! the reflow/layout stage (Phase 2+) will consume. Built on [`rbook`] (Apache-2.0, AGPL-compatible;
//! the GPL `epub` crate is avoided per `ADR-RUST-READER` Decision 2). Pure logic; host-testable; no
//! vendor, no Android, no `reader-core` dependency (so it can't form a cycle with the `Document`
//! trait the adapter in `reader-core` will implement).
//!
//! Phase 2+ (not here): HTML+CSS box layout, pagination to a viewport, glyph shaping, and
//! rasterization — the forked Plato engine adapted to inkread's render target.

use std::io::Cursor;

use rbook::ebook::toc::TocEntry as RbookTocEntry;
use rbook::Epub;

/// The error surface for EPUB parsing — mirrors `inkread-dict`'s shape so the `reader-core` adapter
/// maps it uniformly. Never panics across the boundary (RR21-FR3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EpubError {
    /// Opening or parsing the container failed (bad zip, missing OPF, malformed XML, …).
    Parse(String),
}

impl std::fmt::Display for EpubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EpubError::Parse(m) => write!(f, "epub parse error: {m}"),
        }
    }
}

impl std::error::Error for EpubError {}

/// The result alias for this crate.
pub type EpubResult<T> = Result<T, EpubError>;

/// One reading-order content document (an XHTML chapter/section) from the spine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chapter {
    /// The resource href (the OPF-relative path), used to anchor the TOC and resolve links.
    pub href: String,
    /// The MIME type (e.g. `application/xhtml+xml`).
    pub mime: String,
    /// The raw XHTML markup of the document (UTF-8).
    pub html: String,
}

/// One table-of-contents navigation point; `children` form the nested outline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavPoint {
    /// The human-readable label shown in the outline.
    pub label: String,
    /// The target resource href (a chapter href, possibly with a `#fragment`), or `None` for a
    /// label-only grouping node.
    pub href: Option<String>,
    /// Nested child navigation points.
    pub children: Vec<NavPoint>,
}

/// A parsed EPUB: its metadata, reading-order [`Chapter`]s, and TOC tree — the owned, render-engine-
/// agnostic shape Phase 2 lays out. (Resource streaming for images/CSS arrives with the layout
/// stage; Phase 1 carries the text spine, which dominates a typical book's content.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpubPackage {
    /// Document title, if declared.
    pub title: Option<String>,
    /// Primary creator/author, if declared.
    pub author: Option<String>,
    /// Reading-order content documents (linear spine first; see [`Chapter`]).
    pub chapters: Vec<Chapter>,
    /// The table of contents (EPUB 3 nav, falling back to EPUB 2 NCX — rbook resolves this).
    pub toc: Vec<NavPoint>,
}

impl EpubPackage {
    /// Parse an EPUB from in-memory `bytes` (the shell hands the core file bytes over JNI, mirroring
    /// the PDF path). Reads every spine document's XHTML in reading order. Returns an
    /// [`EpubError::Parse`] on any malformed-container failure — never panics.
    pub fn open(bytes: Vec<u8>) -> EpubResult<Self> {
        let epub = Epub::read(Cursor::new(bytes)).map_err(|e| EpubError::Parse(e.to_string()))?;

        let meta = epub.metadata();
        let title = meta.title().map(|t| t.value().to_string());
        let author = meta.creators().next().map(|c| c.value().to_string());

        let mut chapters = Vec::new();
        let mut reader = epub.reader();
        while let Some(item) = reader.read_next() {
            let data = item.map_err(|e| EpubError::Parse(e.to_string()))?;
            let entry = data.manifest_entry();
            let href = entry
                .resource()
                .key()
                .value()
                .unwrap_or_default()
                .to_string();
            let mime = entry.kind().as_str().to_string();
            chapters.push(Chapter {
                href,
                mime,
                html: data.content().to_string(),
            });
        }

        let toc = epub
            .toc()
            .contents()
            .map(|root| root.iter().map(convert_nav).collect())
            .unwrap_or_default();

        Ok(Self {
            title,
            author,
            chapters,
            toc,
        })
    }

    /// Total reading-order chapter count.
    #[must_use]
    pub fn chapter_count(&self) -> usize {
        self.chapters.len()
    }
}

/// Recursively convert an rbook TOC entry into an owned [`NavPoint`].
fn convert_nav<'a>(entry: impl RbookTocEntry<'a>) -> NavPoint {
    let href = entry
        .resource()
        .and_then(|r| r.key().value().map(str::to_string));
    let children = entry.iter().map(convert_nav).collect();
    NavPoint {
        label: entry.label().to_string(),
        href,
        children,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build a minimal valid EPUB **zip** in memory: `mimetype` first, the rest stored — enough for
    /// rbook to open the fixture without an on-disk file. Two spine chapters + an EPUB-3 nav doc →
    /// exercises metadata, reading order, and TOC.
    fn sample_epub() -> Vec<u8> {
        let mut buf = Vec::new();
        write_zip(
            &mut buf,
            &[
                ("mimetype", b"application/epub+zip".to_vec()),
                ("META-INF/container.xml", CONTAINER_XML.as_bytes().to_vec()),
                ("OEBPS/content.opf", OPF.as_bytes().to_vec()),
                ("OEBPS/nav.xhtml", NAV.as_bytes().to_vec()),
                ("OEBPS/ch1.xhtml", CH1.as_bytes().to_vec()),
                ("OEBPS/ch2.xhtml", CH2.as_bytes().to_vec()),
            ],
        );
        buf
    }

    const CONTAINER_XML: &str = r#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#;

    const OPF: &str = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="id">urn:uuid:test</dc:identifier>
    <dc:title>The Test Book</dc:title>
    <dc:creator>Ada Lovelace</dc:creator>
    <dc:language>en</dc:language>
  </metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="c1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
    <item id="c2" href="ch2.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine>
    <itemref idref="c1"/>
    <itemref idref="c2"/>
  </spine>
</package>"#;

    const NAV: &str = r#"<?xml version="1.0"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
  <body><nav epub:type="toc"><ol>
    <li><a href="ch1.xhtml">Chapter One</a></li>
    <li><a href="ch2.xhtml">Chapter Two</a></li>
  </ol></nav></body>
</html>"#;

    const CH1: &str = r#"<?xml version="1.0"?>
<html xmlns="http://www.w3.org/1999/xhtml"><body><h1>One</h1><p>The first chapter.</p></body></html>"#;

    const CH2: &str = r#"<?xml version="1.0"?>
<html xmlns="http://www.w3.org/1999/xhtml"><body><h1>Two</h1><p>The second chapter.</p></body></html>"#;

    /// A minimal store-only (no compression) ZIP writer — enough for rbook to open the fixture
    /// without pulling the `zip` crate into the test. Emits local-file headers + the central
    /// directory + end-of-central-directory, with a real CRC-32 per entry.
    fn write_zip(out: &mut Vec<u8>, files: &[(&str, Vec<u8>)]) {
        struct Central {
            name: String,
            crc: u32,
            size: u32,
            offset: u32,
        }
        let mut central = Vec::new();
        for (name, data) in files {
            let offset = out.len() as u32;
            let crc = crc32(data);
            let size = data.len() as u32;
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes()); // local file header sig
            out.extend_from_slice(&20u16.to_le_bytes()); // version needed
            out.extend_from_slice(&0u16.to_le_bytes()); // flags
            out.extend_from_slice(&0u16.to_le_bytes()); // method: store
            out.extend_from_slice(&0u16.to_le_bytes()); // mod time
            out.extend_from_slice(&0u16.to_le_bytes()); // mod date
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&size.to_le_bytes()); // compressed
            out.extend_from_slice(&size.to_le_bytes()); // uncompressed
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // extra len
            out.write_all(name.as_bytes()).unwrap();
            out.write_all(data).unwrap();
            central.push(Central {
                name: (*name).to_string(),
                crc,
                size,
                offset,
            });
        }
        let cd_start = out.len() as u32;
        for c in &central {
            out.extend_from_slice(&0x0201_4b50u32.to_le_bytes()); // central dir sig
            out.extend_from_slice(&20u16.to_le_bytes()); // version made by
            out.extend_from_slice(&20u16.to_le_bytes()); // version needed
            out.extend_from_slice(&0u16.to_le_bytes()); // flags
            out.extend_from_slice(&0u16.to_le_bytes()); // method store
            out.extend_from_slice(&0u16.to_le_bytes()); // time
            out.extend_from_slice(&0u16.to_le_bytes()); // date
            out.extend_from_slice(&c.crc.to_le_bytes());
            out.extend_from_slice(&c.size.to_le_bytes());
            out.extend_from_slice(&c.size.to_le_bytes());
            out.extend_from_slice(&(c.name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // extra
            out.extend_from_slice(&0u16.to_le_bytes()); // comment
            out.extend_from_slice(&0u16.to_le_bytes()); // disk
            out.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
            out.extend_from_slice(&0u32.to_le_bytes()); // external attrs
            out.extend_from_slice(&c.offset.to_le_bytes());
            out.write_all(c.name.as_bytes()).unwrap();
        }
        let cd_size = out.len() as u32 - cd_start;
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes()); // end of central dir sig
        out.extend_from_slice(&0u16.to_le_bytes()); // disk
        out.extend_from_slice(&0u16.to_le_bytes()); // cd disk
        out.extend_from_slice(&(central.len() as u16).to_le_bytes());
        out.extend_from_slice(&(central.len() as u16).to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_start.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // comment len
    }

    /// CRC-32 (IEEE) — the ZIP checksum; table-free implementation for the test fixture.
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &b in data {
            crc ^= b as u32;
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
        !crc
    }

    #[test]
    fn opens_metadata_chapters_and_toc_in_reading_order() {
        let pkg = EpubPackage::open(sample_epub()).expect("valid epub opens");
        assert_eq!(pkg.title.as_deref(), Some("The Test Book"));
        assert_eq!(pkg.author.as_deref(), Some("Ada Lovelace"));

        assert_eq!(pkg.chapter_count(), 2, "two spine documents in order");
        assert!(pkg.chapters[0].html.contains("The first chapter."));
        assert!(pkg.chapters[1].html.contains("The second chapter."));
        assert_eq!(pkg.chapters[0].mime, "application/xhtml+xml");

        assert_eq!(pkg.toc.len(), 2, "two nav points");
        assert_eq!(pkg.toc[0].label, "Chapter One");
        assert_eq!(pkg.toc[1].label, "Chapter Two");
        assert!(pkg.toc[0]
            .href
            .as_deref()
            .unwrap_or("")
            .contains("ch1.xhtml"));
    }

    #[test]
    fn malformed_bytes_error_not_panic() {
        let err = EpubPackage::open(b"not a zip at all".to_vec());
        assert!(matches!(err, Err(EpubError::Parse(_))));
    }
}
