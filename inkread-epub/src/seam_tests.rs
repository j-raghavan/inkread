//! Cross-cutting invariants over the whole EPUB pipeline — CSS through to laid-out page.
//!
//! The unit tests beside each module check that a feature does what it was written to do. These
//! check the places features *meet*: a new property against every other new property, against
//! every block kind, inside a table cell as well as outside one, under two columns, at extreme page
//! geometries, and against the invariants nothing may break — no content lost, nothing overflowing
//! the content box, source anchors monotonic, and a partial pagination still a byte-identical
//! prefix of the full one (#186).
//!
//! They exist because #251's first pass shipped five defects that were each at such a meeting
//! point and invisible to a test of either feature alone.

use crate::content::parse_blocks_with;
use crate::css::Stylesheet;
use crate::layout::{
    paginate_upto, paginate_with_images, LayoutOpts, Metrics, NoHyphen, NoImages, Page,
};
struct Mono;
impl Metrics for Mono {
    fn advance(&self, t: &str, s: f32, _b: bool, _i: bool) -> f32 {
        t.chars().count() as f32 * s * 0.5
    }
}
fn opts() -> LayoutOpts {
    LayoutOpts {
        page_w: 600.0,
        page_h: 400.0,
        margin: 0.0,
        ..LayoutOpts::new(600.0, 400.0, 20.0)
    }
}
fn layout(css: &str, h: &str, opts: &LayoutOpts) -> Vec<Page> {
    let b = parse_blocks_with(
        &format!("<html><body>{h}</body></html>"),
        &Stylesheet::parse(css),
    );
    paginate_with_images(&b, opts, &Mono, &NoHyphen, &NoImages)
}
fn every_text(p: &[Page]) -> Vec<String> {
    p.iter()
        .flat_map(|g| {
            g.lines
                .iter()
                .flat_map(|l| l.runs.iter().map(|r| r.text.clone()))
        })
        .collect()
}
fn assert_sane(p: &[Page], opts: &LayoutOpts, what: &str) {
    for g in p {
        for l in &g.lines {
            assert!(l.top >= -0.01, "{what}: negative top {l:?}");
            assert!(
                l.top + l.height <= opts_content_h(opts) + 0.5,
                "{what}: overflow {} > {}",
                l.top + l.height,
                opts_content_h(opts)
            );
            assert!(
                l.column_w > 0.0 && l.column_w <= opts_content_w(opts) + 0.5,
                "{what}: bad column_w {}",
                l.column_w
            );
        }
    }
}

/// Every cell of a row starts at the row's top, whatever it opens with.
///
/// A cell's leading margin must collapse against that edge, exactly as it would at the top of a
/// page. When it does not, two cells whose first blocks declare different margins begin at
/// different heights and the row silently stops being a parallel text — which is the failure this
/// sweep exists to catch, and why the openers are deliberately mismatched.
#[test]
fn every_cell_of_a_row_starts_at_the_rows_top() {
    let openers = [
        "<p>one</p>",
        "<h3>one</h3>",
        "<ul><li>one</li></ul>",
        "<hr/><p>one</p>",
        "<blockquote><p>one</p></blockquote>",
    ];
    let css = "h3{margin:1em 0} p{margin:0.5em 0} li{margin-bottom:0.4em} blockquote{margin:2em 0}";
    for a in openers {
        for b in openers {
            let html =
                format!("<table><tr><td>{a}<p>x1</p></td><td>{b}<p>y1</p></td></tr></table>");
            let p = layout(css, &html, &opts());
            let half = opts_content_w(&opts()) * 0.5;
            let first_at = |right: bool| {
                p[0].lines
                    .iter()
                    .filter(|l| {
                        l.runs.iter().any(|r| (r.x >= half) == right)
                            || (l.rule && (l.column_x >= half) == right)
                    })
                    .map(|l| l.top)
                    .fold(f32::INFINITY, f32::min)
            };
            let (left, right) = (first_at(false), first_at(true));
            assert!(
                (left - right).abs() < 1.0,
                "`{a}` vs `{b}`: the cells start at different heights ({left} vs {right})",
            );
        }
    }
}

/// And cells that agree on their structure stay level all the way down, line for line — the
/// property a juxtalinear edition is read by.
#[test]
fn matching_cells_stay_level_line_for_line() {
    let openers = [
        "<p>one</p>",
        "<h3>one</h3>",
        "<ul><li>one</li><li>two</li></ul>",
        "<blockquote><p>one</p></blockquote>",
    ];
    let css = "h3{margin:1em 0} p{margin:0.5em 0} li{margin-bottom:0.4em} blockquote{margin:2em 0}";
    for opener in openers {
        let html = format!(
            "<table><tr><td>{opener}<p>x1</p><p>x2</p></td><td>{opener}<p>y1</p><p>y2</p></td></tr></table>"
        );
        let p = layout(css, &html, &opts());
        let half = opts_content_w(&opts()) * 0.5;
        for (i, g) in p.iter().enumerate() {
            for l in &g.lines {
                if l.rule || l.image.is_some() || l.runs.is_empty() {
                    continue;
                }
                let left = l.runs.iter().filter(|r| r.x < half).count();
                assert!(
                    left > 0 && left < l.runs.len(),
                    "`{opener}` page {i}: a line box holding only one cell: {:?}",
                    l.runs.iter().map(|r| &r.text).collect::<Vec<_>>(),
                );
            }
        }
    }
}

/// `row_stages` exists for cells that break a *different number of times*. Every other table case
/// here has cells that break alike — the shape it cannot get wrong.
#[test]
fn staggered_breaks_across_cells_lose_nothing() {
    let html = "<table><tr>\
        <td><p>a1</p><p class=\"b\">a2</p><p class=\"b\">a3</p></td>\
        <td><p>b1</p><p class=\"b\">b2</p></td>\
        <td><p>c1</p><p>c2</p><p>c3</p><p>c4</p></td>\
        </tr></table>";
    let p = layout(".b{page-break-before:always}", html, &opts());
    let mut got = every_text(&p);
    got.sort();
    assert_eq!(
        got,
        ["a1", "a2", "a3", "b1", "b2", "c1", "c2", "c3", "c4"],
        "staggered breaks across {} pages must lose nothing",
        p.len(),
    );
    assert_sane(&p, &opts(), "staggered breaks");
}

/// Margins collapse *across* the seams, not only within a flow — the behaviour `pending_gap` exists
/// for, and the thing `add_keep_run`'s lead-lift has to get right.
#[test]
fn margins_collapse_across_a_keep_run_and_a_row() {
    let tops = |css: &str, body: &str| {
        layout(css, body, &opts())
            .iter()
            .flat_map(|g| g.lines.iter().map(|l| l.top))
            .collect::<Vec<f32>>()
    };
    let loose = tops(
        "p{margin-bottom:2em} .k{margin-top:2em}",
        "<p>a</p><p class=\"k\">b</p>",
    );
    let kept = tops(
        "p{margin-bottom:2em} .k{margin-top:2em;page-break-inside:avoid}",
        "<p>a</p><p class=\"k\">b</p>",
    );
    assert_eq!(
        loose, kept,
        "keeping a block together must not move it, nor cost it its margins",
    );
}

// ── Cross-feature: every pair of the new properties on the same block ────────────────────────
#[test]
fn every_pair_of_new_properties_together() {
    let props = [
        "margin: 2em 0",
        "page-break-inside: avoid",
        "page-break-before: always",
        "page-break-after: avoid",
    ];
    for a in props {
        for b in props {
            let css = format!("p {{ {a}; {b} }}");
            let p = layout(&css, "<p>one</p><p>two</p><p>three</p>", &opts());
            assert_eq!(
                every_text(&p),
                ["one", "two", "three"],
                "lost content with `{a}` + `{b}`"
            );
            assert_sane(&p, &opts(), &css);
        }
    }
}

// ── Same, but inside a table cell (the newly-reachable path) ─────────────────────────────────
#[test]
fn every_pair_of_new_properties_inside_a_cell() {
    let props = [
        "margin: 2em 0",
        "page-break-inside: avoid",
        "page-break-before: always",
        "page-break-after: avoid",
    ];
    for a in props {
        for b in props {
            let css = format!("p {{ {a}; {b} }}");
            let p = layout(
                &css,
                "<table><tr><td><p>one</p><p>two</p></td><td><p>x</p><p>y</p></td></tr></table>",
                &opts(),
            );
            let mut t = every_text(&p);
            t.sort();
            assert_eq!(
                t,
                ["one", "two", "x", "y"],
                "lost content in a cell with `{a}` + `{b}`"
            );
            assert_sane(&p, &opts(), &css);
        }
    }
}

// ── Every block kind can now carry a style: exercise each with each new property ─────────────
#[test]
fn every_block_kind_with_every_new_property() {
    let kinds = [
        ("h2", "<p>a</p><h2 class=\"t\">H</h2><p>b</p>", "H"),
        ("p", "<p>a</p><p class=\"t\">B</p><p>b</p>", "B"),
        ("li", "<p>a</p><ul><li class=\"t\">I</li></ul><p>b</p>", "I"),
        ("hr", "<p>a</p><hr class=\"t\"/><p>b</p>", ""),
        (
            "table",
            "<p>a</p><table class=\"t\"><tr><td>C</td></tr></table><p>b</p>",
            "C",
        ),
        (
            "div",
            "<p>a</p><div class=\"t\"><p>D</p><p>E</p></div><p>b</p>",
            "D",
        ),
    ];
    let props = [
        "margin: 2em 0",
        "page-break-before: always",
        "page-break-after: always",
        "page-break-inside: avoid",
        "page-break-after: avoid",
        "margin-top: 0",
    ];
    for (name, html, own) in kinds {
        for pr in props {
            let css = format!(".t {{ {pr} }}");
            let p = layout(&css, html, &opts());
            let t = every_text(&p);
            // `own` is the styled block's own text: the block under test must survive, not merely
            // the prose around it.
            for want in ["a", "b", own] {
                assert!(
                    want.is_empty() || t.contains(&want.to_string()),
                    "{name} + {pr}: lost {want:?} -> {t:?}",
                );
            }
            assert_sane(&p, &opts(), &format!("{name} {pr}"));
        }
    }
}

// ── Extremes: the values a sentinel-bearing pager might mishandle ────────────────────────────
/// The one test that cannot call `assert_sane`: a single line taller than the whole page must be
/// placed anyway, because losing text is worse than overflowing it. A deliberate boundary of the
/// overflow invariant, not a gap in it.
#[test]
fn extreme_pages_and_measures() {
    for (w, h) in [(20.0, 20.0), (20.0, 4000.0), (4000.0, 20.0), (1.0, 1.0)] {
        let opts = LayoutOpts {
            page_w: w,
            page_h: h,
            margin: 0.0,
            ..LayoutOpts::new(w, h, 20.0)
        };
        let p = layout(
            "p { margin: 3em 0; page-break-inside: avoid }",
            "<table><tr><td><p>alpha</p><hr/></td><td><p>beta</p></td></tr></table><p>tail</p>",
            &opts,
        );
        for g in &p {
            for l in &g.lines {
                assert!(
                    l.top.is_finite() && l.height.is_finite(),
                    "non-finite at {w}x{h}: {l:?}"
                );
            }
        }
        assert!(p.len() < 10_000, "page explosion at {w}x{h}: {}", p.len());
    }
}

// ── Two columns meeting every new property ───────────────────────────────────────────────────
#[test]
fn two_column_mode_with_the_new_properties() {
    let two = LayoutOpts {
        columns: 2,
        ..opts()
    };
    for pr in [
        "margin: 2em 0",
        "page-break-before: always",
        "page-break-inside: avoid",
    ] {
        let p = layout(
            &format!("p {{ {pr} }}"),
            "<p>one</p><p>two</p><p>three</p>",
            &two,
        );
        assert_eq!(
            {
                let mut t = every_text(&p);
                t.sort();
                t
            },
            ["one", "three", "two"],
            "lost content in 2col with {pr}"
        );
    }
}

// ── Degenerate structures the walkers must not choke on ──────────────────────────────────────
#[test]
fn degenerate_markup() {
    for html in [
        "<table></table>",
        "<table><tr></tr></table>",
        "<table><tr><td></td></tr></table>",
        "<div style=\"page-break-before:always\"></div>",
        "<table><tr><td><table><tr><td>deep</td></tr></table></td></tr></table>",
        "<ul></ul>",
        "<div><div><div><p>nested</p></div></div></div>",
        "<table><tr><td><hr/></td></tr></table>",
    ] {
        let p = layout(".x{margin:1em 0}", html, &opts());
        assert_sane(&p, &opts(), html);
    }
}

// ── Incremental pagination must stay a true prefix of the full pass ──────────────────────────
#[test]
fn partial_pagination_is_a_prefix_of_the_full_one() {
    let paras: String = (0..60).map(|i| format!("<p>para{i}</p>")).collect();
    let long: String = (0..200).map(|i| format!("w{i} ")).collect();
    // The shapes that actually threaten the property: a keep-run the budget is only checked
    // around, a table row that emits a page per forced break, and one paragraph longer than
    // several pages. Both column counts, because a partial pass that stops on an odd column would
    // build a half page the full pass never produces.
    let bodies = [
        paras.clone(),
        format!(
            "{paras}<table><tr>\
               <td><p>L0</p><p class=\"b\">L1</p><p class=\"b\">L2</p></td>\
               <td><p>R0</p><p>R1</p><p>R2</p></td>\
             </tr></table><p>tail</p>"
        ),
        format!("<p>{long}</p><p>tail</p>"),
    ];
    for css in [
        "",
        ".c{page-break-inside:avoid}",
        "p{margin:1em 0}",
        ".b{page-break-before:always}",
    ] {
        for body in &bodies {
            for columns in [1u8, 2] {
                let html = format!("<div class=\"c\">{body}</div>");
                let b = parse_blocks_with(
                    &format!("<html><body>{html}</body></html>"),
                    &Stylesheet::parse(css),
                );
                let o = LayoutOpts {
                    page_h: 120.0,
                    columns,
                    ..opts()
                };
                let full = paginate_with_images(&b, &o, &Mono, &NoHyphen, &NoImages);
                for n in 1..=3usize {
                    let (part, _) = paginate_upto(&b, &o, &Mono, &NoHyphen, &NoImages, n);
                    assert!(
                        part.len() <= n,
                        "css `{css}` {columns}col: asked for {n} pages, got {}",
                        part.len(),
                    );
                    for (i, pg) in part.iter().enumerate() {
                        assert_eq!(
                            pg, &full[i],
                            "css `{css}` {columns}col: partial page {i} of {n} is not the full pass's",
                        );
                    }
                }
            }
        }
    }
}

// ── Source anchors must stay monotonic through every new path ────────────────────────────────
#[test]
fn anchors_stay_monotonic_through_cells_and_keep_runs() {
    for css in [
        "",
        "p{page-break-inside:avoid}",
        "h3{page-break-before:always}",
        "p{margin:2em 0}",
    ] {
        let p=layout(css,"<p>lead</p><table><tr><td><h3>A</h3><p>one</p></td><td><h3>B</h3><p>two</p></td></tr></table><p>tail</p>",&opts());
        let mut last = (0usize, 0usize);
        for g in &p {
            for l in &g.lines {
                for r in &l.runs {
                    let k = (r.anchor.block, r.anchor.char_offset);
                    assert!(
                        k.0 >= last.0,
                        "css `{css}`: block index went backwards {last:?} -> {k:?}"
                    );
                    last = (k.0.max(last.0), k.1);
                }
            }
        }
    }
}

fn opts_content_h(o: &LayoutOpts) -> f32 {
    (o.page_h - 2.0 * o.margin).max(1.0)
}

fn opts_content_w(o: &LayoutOpts) -> f32 {
    (o.page_w - 2.0 * o.margin).max(1.0)
}

// ── A real .epub, opened the way a reader opens one (#251) ────────────────────────────────────
//
// Everything above this point hands `parse_blocks_with` a `Stylesheet` built in memory. A book on
// a device does not: its CSS arrives as a file in a zip, named by a manifest, and has to survive
// that trip before any of it matters. This fixture is a bilingual juxtalinear poetry EPUB — the
// shape #251 was reported against — carried through the whole path: zip -> OPF -> linked
// stylesheet -> XHTML -> blocks -> pages.

mod real_epub {
    use super::*;
    use crate::content::Block;
    use crate::css::Length;
    use crate::layout::{paginate_with, Align, LayoutOpts};
    use crate::measure::CachedMetrics;
    use crate::render::{AbFont, EnHyphenator};
    use crate::tests::write_zip;
    use crate::EpubPackage;

    const STYLE_CSS: &str = r###"/* A juxtalinear edition: original left, translation right. */
table.juxta { width: 100%; }
td.orig, td.trans { vertical-align: top; }

h3 {
  text-align: center;
  font-weight: bold;
  margin: 1.5em 0 0.75em 0;
  page-break-before: always;   /* each canto opens a page */
}

p.stanza {
  margin: 1em 0;               /* stanzas separated by space, not indent */
  text-indent: 0;
  page-break-inside: avoid;    /* never halve a stanza */
}

p.note { font-style: italic; margin-top: 2em; }

ul.gloss { margin: 1em 0; }
ul.gloss li { margin-bottom: 0.5em; }

hr.sep { page-break-after: always; }
"###;
    const POEM_XHTML: &str = r###"<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Poem</title>
<link rel="stylesheet" type="text/css" href="style.css"/></head>
<body>
<p class="note">A note before the poem begins.</p>
<table class="juxta">
<tr>
  <td class="orig">
    <h3>Canto I</h3>
    <p class="stanza">Nel mezzo del cammin di nostra vita<br/>mi ritrovai per una selva oscura</p>
    <p class="stanza">che la diritta via era smarrita.<br/>Ahi quanto a dir qual era e cosa dura</p>
  </td>
  <td class="trans">
    <h3>Canto I</h3>
    <p class="stanza">Midway upon the journey of our life<br/>I found myself within a forest dark</p>
    <p class="stanza">for the straightforward pathway had been lost.<br/>Ah me how hard a thing it is to say</p>
  </td>
</tr>
<tr>
  <td class="orig">
    <h3>Canto II</h3>
    <p class="stanza">Lo giorno se n andava e l aere bruno<br/>toglieva li animai che sono in terra</p>
  </td>
  <td class="trans">
    <h3>Canto II</h3>
    <p class="stanza">Day was departing and the embrowned air<br/>released the animals that are on earth</p>
  </td>
</tr>
</table>
<hr class="sep"/>
<h3>Glossary</h3>
<ul class="gloss">
  <li>selva oscura — a dark wood</li>
  <li>diritta via — the straight way</li>
</ul>
</body></html>
"###;
    const CONTENT_OPF: &str = r###"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="id">urn:uuid:inkread-251-fixture</dc:identifier>
    <dc:title>Juxtalinear Fixture</dc:title>
    <dc:language>en</dc:language>
    <meta property="dcterms:modified">2026-08-30T00:00:00Z</meta>
  </metadata>
  <manifest>
    <item id="nav"  href="nav.xhtml"   media-type="application/xhtml+xml" properties="nav"/>
    <item id="css"  href="style.css"   media-type="text/css"/>
    <item id="ch1"  href="poem.xhtml"  media-type="application/xhtml+xml"/>
  </manifest>
  <spine><itemref idref="ch1"/></spine>
</package>
"###;
    const NAV_XHTML: &str = r###"<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><head><title>nav</title></head>
<body><nav epub:type="toc"><ol><li><a href="poem.xhtml">Poem</a></li></ol></nav></body></html>
"###;
    const CONTAINER_XML: &str = r###"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>
"###;

    fn juxta_epub() -> Vec<u8> {
        let mut buf = Vec::new();
        write_zip(
            &mut buf,
            &[
                ("mimetype", b"application/epub+zip".to_vec()),
                ("META-INF/container.xml", CONTAINER_XML.as_bytes().to_vec()),
                ("OEBPS/content.opf", CONTENT_OPF.as_bytes().to_vec()),
                ("OEBPS/nav.xhtml", NAV_XHTML.as_bytes().to_vec()),
                ("OEBPS/style.css", STYLE_CSS.as_bytes().to_vec()),
                ("OEBPS/poem.xhtml", POEM_XHTML.as_bytes().to_vec()),
            ],
        );
        buf
    }

    /// A Supernote-sized page at a normal reading size, so the fixture paginates the way the
    /// reporter's device would.
    fn device_page() -> LayoutOpts {
        LayoutOpts::new(1404.0, 1872.0, 34.0)
    }

    fn laid_out() -> (Vec<Block>, Vec<Page>) {
        let pkg = EpubPackage::open(juxta_epub()).expect("the fixture is a valid epub");
        assert!(
            !pkg.stylesheet.is_empty(),
            "the manifest's stylesheet must survive the trip out of the zip",
        );
        let blocks = parse_blocks_with(&pkg.chapters[0].html, &pkg.stylesheet);
        // The bundled reading font and hyphenator, not the test `Mono`: this fixture stands for a
        // real book on a real panel, and how the two languages wrap is the whole point of it.
        let font = AbFont::default_font();
        let pages = paginate_with(
            &blocks,
            &device_page(),
            &CachedMetrics::new(&font),
            &EnHyphenator::new(),
        );
        (blocks, pages)
    }

    fn page_text(p: &Page) -> Vec<String> {
        p.lines
            .iter()
            .flat_map(|l| l.runs.iter().map(|r| r.text.clone()))
            .collect()
    }

    fn the_row(blocks: &[Block], n: usize) -> &Vec<Vec<Block>> {
        blocks
            .iter()
            .filter_map(|b| match b {
                Block::Row { cells, .. } => Some(cells),
                _ => None,
            })
            .nth(n)
            .expect("expected a table row")
    }

    /// #251(1): a `<h3>` in a cell is a heading — bold, larger than the body, and centred *within
    /// its own cell* because the book's `style.css` said so.
    #[test]
    fn a_canto_heading_in_a_cell_is_bold_scaled_and_centred_in_its_cell() {
        let (blocks, pages) = laid_out();
        let cells = the_row(&blocks, 0);
        for cell in cells {
            let Block::Heading { level, style, .. } = &cell[0] else {
                panic!("a cell must open with its canto heading: {cell:?}");
            };
            assert_eq!(*level, 3);
            assert_eq!(
                style.bold,
                Some(true),
                "style.css declares font-weight: bold"
            );
            assert_eq!(style.align, Some(Align::Center));
        }
        // And on the page: the two headings sit on one line, each centred in its own half.
        let line = pages
            .iter()
            .flat_map(|p| &p.lines)
            .find(|l| l.runs.iter().any(|r| r.text == "Canto"))
            .expect("the canto heading is laid out");
        let body = pages
            .iter()
            .flat_map(|p| &p.lines)
            .flat_map(|l| &l.runs)
            .find(|r| r.text == "mezzo")
            .expect("body text is laid out");
        assert!(line.runs.iter().all(|r| r.bold), "{:?}", line.runs);
        assert!(
            line.runs[0].size_px > body.size_px,
            "a heading outsizes the body: {} vs {}",
            line.runs[0].size_px,
            body.size_px,
        );
        let measure = device_page().page_w - 2.0 * device_page().margin;
        assert!(
            line.runs[0].x > 0.0 && line.runs[0].x < measure * 0.5,
            "the original's heading is centred in the left cell, not at its edge: {:?}",
            line.runs.iter().map(|r| r.x).collect::<Vec<_>>(),
        );
    }

    /// #251(2): `<h3>` then two `<p>` in one cell are three blocks on three line boxes, not one
    /// flat run. Before the fix the only structure that survived was the book's own `<br/>`.
    #[test]
    fn a_cells_heading_and_stanzas_are_separate_blocks() {
        let (blocks, _) = laid_out();
        let cell = &the_row(&blocks, 0)[0];
        assert!(
            matches!(
                cell.as_slice(),
                [
                    Block::Heading { .. },
                    Block::Paragraph { .. },
                    Block::Paragraph { .. }
                ]
            ),
            "{cell:?}",
        );
    }

    /// #251(3): `p.stanza { margin: 1em 0 }` puts real space between the stanzas — the thing that
    /// distinguishes them, since this typography sets prose dense and the book zeroes the indent.
    #[test]
    fn stanzas_are_separated_by_the_margin_the_book_declared() {
        let (_, pages) = laid_out();
        let canto = pages
            .iter()
            .find(|p| page_text(p).contains(&"Nel".to_string()))
            .expect("canto I is laid out");
        // Located by text, not by index: how the two languages wrap is the font's business.
        let top_of = |word: &str| {
            canto
                .lines
                .iter()
                .find(|l| l.runs.iter().any(|r| r.text == word))
                .unwrap_or_else(|| panic!("no line carries {word:?}"))
                .top
        };
        let stanza_one_first = top_of("Nel"); // stanza 1, line 1
        let stanza_one_second = top_of("ritrovai"); // stanza 1, line 2 (after the <br/>)
        let stanza_two_first = top_of("che"); // stanza 2, line 1
        let within = stanza_one_second - stanza_one_first;
        let between = stanza_two_first - stanza_one_second;
        assert!(
            between > within,
            "the stanza gap ({between}) must exceed the line gap ({within})",
        );
        // 1em at 34px, collapsed (not summed) from the two stanzas' adjacent margins.
        assert!(
            (between - within - 34.0).abs() < 1.0,
            "one em of collapsed margin, not two: {between} - {within}",
        );
    }

    /// #251(4): `h3 { page-break-before: always }` opens a page per canto, even though the cantos
    /// live inside table cells — and `hr.sep { page-break-after: always }` ends the poem.
    #[test]
    fn every_canto_opens_its_own_page() {
        let (_, pages) = laid_out();
        let titled = |name: &str| {
            pages
                .iter()
                .position(|p| page_text(p).iter().any(|t| t == name))
        };
        assert_eq!(pages.len(), 4, "note / canto I / canto II / glossary");
        let note = titled("note").expect("the opening note");
        let glossary = titled("Glossary").expect("the glossary");
        assert_eq!(note, 0);
        assert_eq!(glossary, 3);
        let cantos: Vec<usize> = pages
            .iter()
            .enumerate()
            .filter(|(_, p)| page_text(p).iter().any(|t| t == "Canto"))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(cantos, vec![1, 2], "a canto per page, in order");
    }

    /// The reason a juxtalinear edition is a table at all: the translation must sit beside its
    /// original, line for line, on every page.
    #[test]
    fn the_translation_stays_beside_its_original_on_every_page() {
        let (_, pages) = laid_out();
        for (i, page) in pages.iter().enumerate() {
            if !page_text(page).iter().any(|t| t == "Canto") {
                continue;
            }
            for line in &page.lines {
                if line.rule || line.runs.is_empty() {
                    continue;
                }
                let measure = device_page().page_w - 2.0 * device_page().margin;
                let left = line.runs.iter().filter(|r| r.x < measure * 0.5).count();
                let right = line.runs.len() - left;
                assert!(
                    left > 0 && right > 0,
                    "page {i}: a line with only one language: {:?}",
                    line.runs.iter().map(|r| &r.text).collect::<Vec<_>>(),
                );
            }
        }
    }

    /// The reporter named lists as well as poetry. `ul.gloss { margin: 1em 0 }` reaches the run it
    /// wraps, and `li { margin-bottom: 0.5em }` separates the items.
    #[test]
    fn the_glossary_list_keeps_its_markers_and_its_declared_spacing() {
        let (blocks, pages) = laid_out();
        let items: Vec<&Block> = blocks
            .iter()
            .filter(|b| matches!(b, Block::ListItem { .. }))
            .collect();
        assert_eq!(items.len(), 2);
        assert_eq!(
            items[0].style().margin_top,
            Some(Length::Em(1.0)),
            "the <ul>'s margin transfers onto the first item",
        );
        assert_eq!(items[1].style().margin_top, None, "and only the first");
        assert_eq!(items[0].style().margin_bottom, Some(Length::Em(0.5)));
        let glossary = pages.last().expect("the glossary page");
        assert!(
            page_text(glossary)
                .iter()
                .filter(|t| *t == "\u{2022}")
                .count()
                == 2,
            "both items keep their bullet: {:?}",
            page_text(glossary),
        );
    }

    /// Nothing in the book is lost or duplicated on the way to the page.
    #[test]
    fn every_word_of_the_book_is_placed_exactly_once() {
        let (_, pages) = laid_out();
        let mut placed: Vec<String> = pages
            .iter()
            .flat_map(page_text)
            .filter(|t| t != "\u{2022}")
            .collect();
        let body = POEM_XHTML
            .split_once("<body>")
            .and_then(|(_, rest)| rest.split_once("</body>"))
            .map(|(b, _)| b)
            .expect("the fixture has a body");
        let mut expected: Vec<String> = body
            .split(['<', '>'])
            .enumerate()
            .filter(|(i, _)| i % 2 == 0)
            .flat_map(|(_, t)| t.split_whitespace().map(str::to_string))
            .collect();
        placed.sort();
        expected.sort();
        assert_eq!(
            placed, expected,
            "the page must carry exactly the book's words"
        );
    }
}
