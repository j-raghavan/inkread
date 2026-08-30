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

pub mod content;
pub mod css;
pub mod img;
pub mod layout;
pub mod measure;
pub mod render;
mod transcode;
pub use content::{parse_blocks, parse_blocks_with, Block, Inline, TextRun};
pub use css::{BlockStyle, Length, PageBreak, Stylesheet};
pub use img::ImageError;
pub use layout::{
    paginate, paginate_upto, paginate_with, paginate_with_images, Align, Hyphenator, ImageSizer,
    LayoutLine, LayoutOpts, Metrics, NoHyphen, NoImages, Page, PlacedImage, PlacedRun,
};
pub use measure::{CachedHyphenator, CachedMetrics};
pub use render::{
    clear_reading_fonts, page_glyphs, reading_font_names, register_fallback_font,
    register_reading_font, render_page, render_page_with_images, AbFont, EnHyphenator, GrayCanvas,
    ImageSource, NoImageBytes, PlacedGlyph,
};

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

/// A parsed EPUB: its metadata, reading-order [`Chapter`]s, TOC tree, and the narrow slice of its
/// styling inkread honours — the owned, render-engine-agnostic shape Phase 2 lays out. (Resource
/// streaming for images arrives with the layout stage; Phase 1 carries the text spine, which
/// dominates a typical book's content.)
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
    /// The book's image resources (#187), read on demand.
    pub images: ImageStore,
    /// The book's declared block styling, merged from every `text/css` manifest resource in
    /// manifest order (#188). Pass it to [`parse_blocks_with`] when parsing a chapter.
    ///
    /// Merging the book's stylesheets into one sheet rather than resolving each chapter's `<link>`
    /// elements is deliberate: virtually every EPUB ships one or two book-wide stylesheets whose
    /// rules are authored to apply throughout, and one merged sheet costs a fraction of the memory
    /// of a per-chapter copy on a large omnibus. A chapter's own `<style>` block still layers on
    /// top, per-chapter, inside [`parse_blocks_with`].
    pub stylesheet: Stylesheet,
}

/// The book's images, resolved from the retained container when one is actually drawn.
///
/// Holding every image decoded would cost several times the file itself (RGBA8 is 4 bytes a pixel,
/// usually larger than the compressed original), and an illustrated book would pay all of it at
/// open — the cost profile #186 is about. The CBZ backend keeps its archive for the same reason.
///
/// Separated from [`EpubPackage`] so a reader can take ownership of the images without also
/// holding a second copy of every chapter's markup.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageStore {
    /// Hrefs of the manifest's image resources, in manifest order.
    hrefs: Vec<String>,
    container: Vec<u8>,
}

impl ImageStore {
    /// Hrefs of the manifest's image resources, in manifest order.
    #[must_use]
    pub fn hrefs(&self) -> &[String] {
        &self.hrefs
    }

    /// True when the book declares no images.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hrefs.is_empty()
    }

    /// The bytes of the image resource `src` refers to, or `None` when the book has no such
    /// resource (a dangling `<img src>`, common enough in the wild to be routine).
    ///
    /// `src` is whatever the chapter's markup said, so it is resolved leniently: an exact archive
    /// entry first, then by file name, which is how chapter hrefs are already matched to TOC
    /// targets. Never panics on a malformed container (RR21-FR3).
    #[must_use]
    pub fn bytes(&self, src: &str) -> Option<Vec<u8>> {
        let wanted = strip_fragment(src);
        if wanted.is_empty() {
            return None;
        }
        let mut zip = zip::ZipArchive::new(Cursor::new(&self.container)).ok()?;
        let name = zip
            .file_names()
            .find(|n| *n == wanted)
            .or_else(|| {
                zip.file_names()
                    .find(|n| basename(n) == basename(wanted) && !basename(wanted).is_empty())
            })?
            .to_string();
        let mut entry = zip.by_name(&name).ok()?;
        let mut out = Vec::with_capacity(usize::try_from(entry.size()).unwrap_or_default());
        std::io::Read::read_to_end(&mut entry, &mut out).ok()?;
        Some(out)
    }

    /// The intrinsic pixel size of the image `src` refers to, read from its header.
    #[must_use]
    pub fn size(&self, src: &str) -> Option<(u32, u32)> {
        img::dimensions(&self.bytes(src)?)
    }
}

impl EpubPackage {
    /// Parse an EPUB from in-memory `bytes` (the shell hands the core file bytes over JNI, mirroring
    /// the PDF path). Reads every spine document's XHTML in reading order. Returns an
    /// [`EpubError::Parse`] on any malformed-container failure — never panics.
    pub fn open(bytes: Vec<u8>) -> EpubResult<Self> {
        // Repair legacy text encodings first (#159). rbook refuses any non-UTF-8 resource outright,
        // so without this a windows-1251 Russian EPUB does not open at all. A conforming book comes
        // back from this untouched.
        let bytes = transcode::to_utf8(bytes);
        // Kept for on-demand image extraction; rbook borrows its own copy for the parse.
        let container = bytes.clone();
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

        // A stylesheet that will not read is skipped, never fatal: a book with broken CSS must
        // still open, just unstyled (RR21-FR3).
        let mut stylesheet = Stylesheet::default();
        for entry in epub.manifest().styles() {
            if let Ok(css) = entry.read_str() {
                stylesheet.add(&css);
            }
        }

        // Names only — reading every image here would pay an illustrated book's whole decode cost
        // at open, for pages the reader may never reach.
        let images = ImageStore {
            hrefs: epub
                .manifest()
                .images()
                .filter_map(|e| e.resource().key().value().map(str::to_string))
                .collect(),
            container,
        };

        Ok(Self {
            title,
            author,
            chapters,
            toc,
            images,
            stylesheet,
        })
    }

    /// Total reading-order chapter count.
    #[must_use]
    pub fn chapter_count(&self) -> usize {
        self.chapters.len()
    }
}

/// The part of a path after the last `/` — archive entries and `<img src>` rarely agree on the
/// directory prefix, and matching by file name is what the TOC path already does.
fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Drop a `#fragment` / `?query` suffix from a resource reference.
fn strip_fragment(src: &str) -> &str {
    let end = src.find(['#', '?']).unwrap_or(src.len());
    &src[..end]
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
    pub(crate) fn sample_epub() -> Vec<u8> {
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

    /// A book whose manifest declares a stylesheet, styled the way #188's *Pride and Prejudice*
    /// title page is.
    fn styled_epub() -> Vec<u8> {
        const STYLED_OPF: &str = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="id">urn:uuid:test</dc:identifier>
    <dc:title>The Test Book</dc:title>
    <dc:language>en</dc:language>
  </metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="css" href="style.css" media-type="text/css"/>
    <item id="c1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
    <item id="c2" href="ch2.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine>
    <itemref idref="c1"/>
    <itemref idref="c2"/>
  </spine>
</package>"#;
        const CSS: &str = "h1 { text-align: center; font-weight: normal }\n.c { text-indent: 0 }";
        let mut buf = Vec::new();
        write_zip(
            &mut buf,
            &[
                ("mimetype", b"application/epub+zip".to_vec()),
                ("META-INF/container.xml", CONTAINER_XML.as_bytes().to_vec()),
                ("OEBPS/content.opf", STYLED_OPF.as_bytes().to_vec()),
                ("OEBPS/nav.xhtml", NAV.as_bytes().to_vec()),
                ("OEBPS/style.css", CSS.as_bytes().to_vec()),
                ("OEBPS/ch1.xhtml", CH1.as_bytes().to_vec()),
                ("OEBPS/ch2.xhtml", CH2.as_bytes().to_vec()),
            ],
        );
        buf
    }

    /// #188: the book's stylesheet has to survive the trip out of the zip, or nothing downstream
    /// can honour it.
    #[test]
    fn the_manifests_stylesheet_is_read_and_applies_to_a_parsed_chapter() {
        let pkg = EpubPackage::open(styled_epub()).expect("valid epub opens");
        assert!(
            !pkg.stylesheet.is_empty(),
            "manifest stylesheet was not read"
        );

        let blocks = parse_blocks_with(&pkg.chapters[0].html, &pkg.stylesheet);
        let Some(Block::Heading { style, .. }) = blocks.first() else {
            panic!("expected a heading, got {blocks:?}")
        };
        assert_eq!(style.align, Some(layout::Align::Center));
        assert_eq!(style.bold, Some(false));
    }

    /// A book with a real PNG in its manifest, referenced from the chapter (#187).
    fn illustrated_epub() -> Vec<u8> {
        const ILLUS_OPF: &str = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="id">urn:uuid:illus</dc:identifier>
    <dc:title>An Illustrated Book</dc:title>
    <dc:language>en</dc:language>
  </metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="pic" href="images/plate.png" media-type="image/png"/>
    <item id="c1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
    <item id="c2" href="ch2.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine><itemref idref="c1"/><itemref idref="c2"/></spine>
</package>"#;
        let mut buf = Vec::new();
        write_zip(
            &mut buf,
            &[
                ("mimetype", b"application/epub+zip".to_vec()),
                ("META-INF/container.xml", CONTAINER_XML.as_bytes().to_vec()),
                ("OEBPS/content.opf", ILLUS_OPF.as_bytes().to_vec()),
                ("OEBPS/nav.xhtml", NAV.as_bytes().to_vec()),
                ("OEBPS/images/plate.png", test_png(4, 2)),
                ("OEBPS/ch1.xhtml", CH1.as_bytes().to_vec()),
                ("OEBPS/ch2.xhtml", CH2.as_bytes().to_vec()),
            ],
        );
        buf
    }

    /// A real PNG of the given size, via the encoder rather than hand-written bytes.
    pub(crate) fn test_png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().expect("header");
            writer
                .write_image_data(&vec![128u8; (w * h) as usize])
                .expect("data");
        }
        out
    }

    /// #187: nothing downstream can draw an illustration the container never surfaces.
    #[test]
    fn a_manifest_image_is_listed_and_readable_by_href_or_file_name() {
        let pkg = EpubPackage::open(illustrated_epub()).expect("valid epub opens");
        assert_eq!(pkg.images.hrefs().len(), 1, "{:?}", pkg.images.hrefs());

        // Whatever prefix the manifest recorded, the chapter's own `src` still resolves: exactly,
        // by archive path, and by bare file name.
        for src in [
            pkg.images.hrefs()[0].as_str(),
            "OEBPS/images/plate.png",
            "images/plate.png",
            "plate.png",
            "../images/plate.png",
            "plate.png#anchor",
        ] {
            assert_eq!(pkg.images.size(src), Some((4, 2)), "src = {src}");
        }
    }

    #[test]
    fn a_dangling_image_reference_is_none_not_a_failure() {
        let pkg = EpubPackage::open(illustrated_epub()).expect("valid epub opens");
        for missing in ["nope.png", "", "images/", "#frag"] {
            assert_eq!(pkg.images.bytes(missing), None, "src = {missing}");
            assert_eq!(pkg.images.size(missing), None, "src = {missing}");
        }
        // A book with no images at all lists none and resolves nothing.
        let plain = EpubPackage::open(sample_epub()).expect("valid epub opens");
        assert!(plain.images.is_empty());
        assert_eq!(plain.images.bytes("plate.png"), None);
    }

    #[test]
    fn a_book_without_any_css_has_an_empty_stylesheet() {
        let pkg = EpubPackage::open(sample_epub()).expect("valid epub opens");
        assert!(pkg.stylesheet.is_empty());
    }

    #[test]
    fn malformed_bytes_error_not_panic() {
        let err = EpubPackage::open(b"not a zip at all".to_vec());
        assert!(matches!(err, Err(EpubError::Parse(_))));
    }

    /// Encode Cyrillic + ASCII as **windows-1251** — the encoding a great many Russian EPUBs still
    /// declare, especially the FB2-converted ones. А–Я and а–я are contiguous at 0xC0 and 0xE0.
    pub(crate) fn to_cp1251(s: &str) -> Vec<u8> {
        s.chars()
            .map(|c| match c {
                'А'..='Я' => 0xC0 + (c as u32 - 'А' as u32) as u8,
                'а'..='я' => 0xE0 + (c as u32 - 'а' as u32) as u8,
                'Ё' => 0xA8,
                'ё' => 0xB8,
                c if c.is_ascii() => c as u8,
                _ => b'?',
            })
            .collect()
    }

    /// A book whose chapters are windows-1251, declared in the XML prolog as EPUB permits.
    fn cp1251_epub() -> Vec<u8> {
        let chapter = "<?xml version=\"1.0\" encoding=\"windows-1251\"?>\n\
             <html xmlns=\"http://www.w3.org/1999/xhtml\"><body>\
             <h1>Глава</h1><p>Первая глава книги.</p></body></html>";
        let mut buf = Vec::new();
        write_zip(
            &mut buf,
            &[
                ("mimetype", b"application/epub+zip".to_vec()),
                ("META-INF/container.xml", CONTAINER_XML.as_bytes().to_vec()),
                ("OEBPS/content.opf", OPF.as_bytes().to_vec()),
                ("OEBPS/nav.xhtml", NAV.as_bytes().to_vec()),
                ("OEBPS/ch1.xhtml", to_cp1251(chapter)),
                ("OEBPS/ch2.xhtml", CH2.as_bytes().to_vec()),
            ],
        );
        buf
    }

    /// #159: a Russian EPUB opened but rendered nothing. Glyph coverage was ruled out (every
    /// bundled face has Cyrillic), which leaves decoding: a non-UTF-8 chapter must still reach the
    /// layout stage as text, not as emptiness. Blank pages are the worst failure mode here because
    /// the book *opens* — nothing tells the reader the text was dropped rather than absent.
    #[test]
    fn a_windows_1251_chapter_still_yields_text() {
        let pkg = EpubPackage::open(cp1251_epub()).expect("a cp1251 epub still opens");
        let html = &pkg.chapters[0].html;
        assert!(
            html.contains("Первая глава книги."),
            "cp1251 chapter decoded to {html:?}",
        );
    }
}

#[cfg(test)]
#[path = "seam_tests.rs"]
mod seam_tests;
