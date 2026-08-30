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

#![allow(clippy::float_cmp)]
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
fn o() -> LayoutOpts {
    LayoutOpts {
        page_w: 600.0,
        page_h: 400.0,
        margin: 0.0,
        ..LayoutOpts::new(600.0, 400.0, 20.0)
    }
}
fn go(css: &str, h: &str, opts: &LayoutOpts) -> Vec<Page> {
    let b = parse_blocks_with(
        &format!("<html><body>{h}</body></html>"),
        &Stylesheet::parse(css),
    );
    paginate_with_images(&b, opts, &Mono, &NoHyphen, &NoImages)
}
fn all(p: &[Page]) -> Vec<String> {
    p.iter()
        .flat_map(|g| {
            g.lines
                .iter()
                .flat_map(|l| l.runs.iter().map(|r| r.text.clone()))
        })
        .collect()
}
fn sane(p: &[Page], opts: &LayoutOpts, what: &str) {
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
            let p = go(&css, "<p>one</p><p>two</p><p>three</p>", &o());
            assert_eq!(
                all(&p),
                ["one", "two", "three"],
                "lost content with `{a}` + `{b}`"
            );
            sane(&p, &o(), &css);
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
            let p = go(
                &css,
                "<table><tr><td><p>one</p><p>two</p></td><td><p>x</p><p>y</p></td></tr></table>",
                &o(),
            );
            let mut t = all(&p);
            t.sort();
            assert_eq!(
                t,
                ["one", "two", "x", "y"],
                "lost content in a cell with `{a}` + `{b}`"
            );
            sane(&p, &o(), &css);
        }
    }
}

// ── Every block kind can now carry a style: exercise each with each new property ─────────────
#[test]
fn every_block_kind_with_every_new_property() {
    let kinds = [
        ("h2", "<p>a</p><h2 class=\"t\">H</h2><p>b</p>"),
        ("p", "<p>a</p><p class=\"t\">B</p><p>b</p>"),
        ("li", "<p>a</p><ul><li class=\"t\">I</li></ul><p>b</p>"),
        ("hr", "<p>a</p><hr class=\"t\"/><p>b</p>"),
        (
            "table",
            "<p>a</p><table class=\"t\"><tr><td>C</td></tr></table><p>b</p>",
        ),
        (
            "div",
            "<p>a</p><div class=\"t\"><p>D</p><p>E</p></div><p>b</p>",
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
    for (name, html) in kinds {
        for pr in props {
            let css = format!(".t {{ {pr} }}");
            let p = go(&css, html, &o());
            assert!(!all(&p).is_empty(), "{name} + {pr}: everything vanished");
            assert!(
                all(&p).contains(&"a".to_string()) && all(&p).contains(&"b".to_string()),
                "{name} + {pr}: surrounding prose lost -> {:?}",
                all(&p)
            );
            sane(&p, &o(), &format!("{name} {pr}"));
        }
    }
}

// ── Extremes: the values a sentinel-bearing pager might mishandle ────────────────────────────
#[test]
fn extreme_pages_and_measures() {
    for (w, h) in [(20.0, 20.0), (20.0, 4000.0), (4000.0, 20.0), (1.0, 1.0)] {
        let opts = LayoutOpts {
            page_w: w,
            page_h: h,
            margin: 0.0,
            ..LayoutOpts::new(w, h, 20.0)
        };
        let p = go(
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
    let two = LayoutOpts { columns: 2, ..o() };
    for pr in [
        "margin: 2em 0",
        "page-break-before: always",
        "page-break-inside: avoid",
    ] {
        let p = go(
            &format!("p {{ {pr} }}"),
            "<p>one</p><p>two</p><p>three</p>",
            &two,
        );
        assert_eq!(
            {
                let mut t = all(&p);
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
        let p = go(".x{margin:1em 0}", html, &o());
        sane(&p, &o(), html);
    }
}

// ── Incremental pagination must stay a true prefix of the full pass ──────────────────────────
#[test]
fn partial_pagination_is_a_prefix_of_the_full_one() {
    let body: String = (0..60).map(|i| format!("<p>para{i}</p>")).collect();
    for css in [
        "",
        ".c{page-break-inside:avoid}",
        "p{margin:1em 0}",
        "p{page-break-inside:avoid;margin:1em 0}",
    ] {
        let html = format!("<div class=\"c\">{body}</div>");
        let b = parse_blocks_with(
            &format!("<html><body>{html}</body></html>"),
            &Stylesheet::parse(css),
        );
        let opts = LayoutOpts {
            page_h: 120.0,
            ..o()
        };
        let full = paginate_with_images(&b, &opts, &Mono, &NoHyphen, &NoImages);
        for n in 1..=3usize {
            let (part, _) = paginate_upto(&b, &opts, &Mono, &NoHyphen, &NoImages, n);
            for (i, pg) in part.iter().enumerate() {
                assert_eq!(
                    pg, &full[i],
                    "css `{css}`: partial page {i} of {n} differs from the full pass"
                );
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
        let p=go(css,"<p>lead</p><table><tr><td><h3>A</h3><p>one</p></td><td><h3>B</h3><p>two</p></td></tr></table><p>tail</p>",&o());
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
