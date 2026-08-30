//! Tests for the CSS-declared block properties #251 added — vertical margins and page breaks —
//! from a hand-built `Block` through to a laid-out page, and from a book's stylesheet through the
//! same path. Split out of `layout_tests.rs`, which had grown past twice the size guideline;
//! included via `#[path]` so `super::*` resolves to the layout module.

use super::*;
use crate::content::TextRun;
use crate::css::{BlockStyle, Length, PageBreak};

/// A monospace metric — every character half the font size wide — so a fixture can state exactly
/// how many characters fit a line and where it must wrap. A copy of `layout_tests`' own, which is
/// private to that module; four lines is cheaper than a shared test-support seam.
struct Mono;
impl Metrics for Mono {
    fn advance(&self, text: &str, size_px: f32, _b: bool, _i: bool) -> f32 {
        text.chars().count() as f32 * size_px * 0.5
    }
}

/// A page whose content box holds exactly `lines` lines of `chars` characters, so a fixture can
/// straddle a boundary deliberately. `Mono` advances half the font size per character.
fn page_opts(lines: f32, chars: f32) -> LayoutOpts {
    const FONT: f32 = 20.0;
    LayoutOpts {
        page_w: chars * FONT * 0.5,
        page_h: lines * FONT * 1.4,
        margin: 0.0,
        ..LayoutOpts::new(100.0, 100.0, FONT)
    }
}

/// One unstyled paragraph — the block nearly every fixture below is built from.
fn para(text: &str, style: BlockStyle) -> Block {
    Block::Paragraph {
        content: vec![Inline::Run(TextRun {
            text: text.to_string(),
            bold: false,
            italic: false,
            href: None,
        })],
        style,
    }
}

/// The `top` of every line the blocks lay out to, in page order.
fn line_tops(blocks: &[Block], opts: &LayoutOpts) -> Vec<f32> {
    paginate(blocks, opts, &Mono)
        .into_iter()
        .flat_map(|pg| pg.lines)
        .map(|l| l.top)
        .collect()
}

// ── Declared vertical margins (#251) ──────────────────────────────────────────────────────────

mod margins {
    use super::*;

    /// Line tops on a page roomy enough that nothing wraps or breaks unless a fixture means it to.
    fn tops(blocks: &[Block]) -> Vec<f32> {
        line_tops(blocks, &LayoutOpts::new(1200.0, 1600.0, 20.0))
    }

    /// #251(3): prose is set dense, so without this a book's stanza spacing vanished entirely.
    #[test]
    fn a_declared_margin_separates_paragraphs_that_would_otherwise_be_dense() {
        let dense = tops(&[
            para("a", BlockStyle::default()),
            para("b", BlockStyle::default()),
        ]);
        let spaced = tops(&[
            para("a", BlockStyle::default()),
            para(
                "b",
                BlockStyle {
                    margin_top: Some(Length::Em(2.0)),
                    ..BlockStyle::default()
                },
            ),
        ]);
        assert!(
            spaced[1] - spaced[0] > dense[1] - dense[0] + 30.0,
            "a 2em margin should open a real gap ({:?} vs {:?})",
            spaced,
            dense,
        );
    }

    /// Adjacent margins collapse to the larger, as CSS does — otherwise a book that declares both
    /// `margin-bottom` and `margin-top` on its paragraphs gets twice the space it asked for.
    #[test]
    fn adjacent_margins_collapse_to_the_larger() {
        let both = |a: f32, b: f32| {
            tops(&[
                para(
                    "a",
                    BlockStyle {
                        margin_bottom: Some(Length::Em(a)),
                        ..BlockStyle::default()
                    },
                ),
                para(
                    "b",
                    BlockStyle {
                        margin_top: Some(Length::Em(b)),
                        ..BlockStyle::default()
                    },
                ),
            ])
        };
        let collapsed = both(2.0, 1.0);
        let alone = both(2.0, 0.0);
        assert_eq!(
            collapsed[1] - collapsed[0],
            alone[1] - alone[0],
            "1em against 2em must collapse to 2em, not sum to 3em",
        );
    }

    /// A margin at the top of a page collapses against the page edge; otherwise the page's own top
    /// margin is silently doubled whenever a break lands before a spaced block.
    #[test]
    fn a_margin_at_the_top_of_a_page_is_dropped() {
        // Two lines fit; the third starts a page, and it is the one carrying the margin.
        let short = LayoutOpts::new(1200.0, 100.0, 20.0);
        let blocks = [
            para("a", BlockStyle::default()),
            para("b", BlockStyle::default()),
            para(
                "c",
                BlockStyle {
                    margin_top: Some(Length::Em(3.0)),
                    ..BlockStyle::default()
                },
            ),
        ];
        let pages = paginate(&blocks, &short, &Mono);
        assert!(pages.len() > 1, "the fixture must actually break");
        assert_eq!(
            pages.last().unwrap().lines[0].top,
            0.0,
            "the first line of a page sits at the content origin",
        );
    }

    /// A book that zeroes a heading's margin gets a heading with no space around it, rather than
    /// inkread's default reasserting itself.
    #[test]
    fn a_zeroed_margin_overrides_inkreads_own_gap() {
        let heading = |style: BlockStyle| {
            tops(&[
                para("a", BlockStyle::default()),
                Block::Heading {
                    level: 2,
                    content: vec![Inline::Run(TextRun {
                        text: "h".to_string(),
                        bold: false,
                        italic: false,
                        href: None,
                    })],
                    style,
                },
            ])
        };
        let default = heading(BlockStyle::default());
        let zeroed = heading(BlockStyle {
            margin_top: Some(Length::Px(0.0)),
            ..BlockStyle::default()
        });
        assert!(
            zeroed[1] - zeroed[0] < default[1] - default[0],
            "a declared zero must beat the 0.7em default ({:?} vs {:?})",
            zeroed,
            default,
        );
    }
}

// ── Forced and avoided page breaks (#251) ─────────────────────────────────────────────────────

mod page_breaks {
    use super::*;

    /// Four lines at a comfortable measure: nothing wraps unless the fixture means it to.
    fn four_line_page() -> LayoutOpts {
        page_opts(4.0, 40.0)
    }

    fn text_of(page: &Page) -> Vec<String> {
        page.lines
            .iter()
            .flat_map(|l| l.runs.iter().map(|r| r.text.clone()))
            .collect()
    }

    fn always_before() -> BlockStyle {
        BlockStyle {
            break_before: Some(PageBreak::Always),
            ..BlockStyle::default()
        }
    }

    /// #251(4): the property the reporter wants for starting each poem on a fresh page.
    #[test]
    fn a_forced_break_starts_the_block_on_a_new_page() {
        let blocks = [para("a", BlockStyle::default()), para("b", always_before())];
        let pages = paginate(&blocks, &four_line_page(), &Mono);
        assert_eq!(pages.len(), 2, "both would otherwise share a page");
        assert_eq!(text_of(&pages[0]), ["a"]);
        assert_eq!(text_of(&pages[1]), ["b"]);
    }

    /// A forced break at the top of a page must not leave a blank one.
    #[test]
    fn a_forced_break_at_the_top_of_a_page_does_not_blank_it() {
        let pages = paginate(&[para("a", always_before())], &four_line_page(), &Mono);
        assert_eq!(pages.len(), 1);
        assert_eq!(text_of(&pages[0]), ["a"]);
    }

    /// `page-break-after: always` breaks on the far side of the block.
    #[test]
    fn a_forced_break_after_ends_the_page() {
        let blocks = [
            para(
                "a",
                BlockStyle {
                    break_after: Some(PageBreak::Always),
                    ..BlockStyle::default()
                },
            ),
            para("b", BlockStyle::default()),
        ];
        let pages = paginate(&blocks, &four_line_page(), &Mono);
        assert_eq!(pages.len(), 2);
        assert_eq!(text_of(&pages[0]), ["a"]);
    }

    /// #251(4): the stanza that must not be halved. Three lines will not fit in the one left on
    /// the page, so the whole stanza moves rather than two lines going over.
    #[test]
    fn an_avoided_break_moves_the_whole_block_to_the_next_page() {
        // One line of a three-line page spent, then a stanza three lines long: it cannot fit in
        // what is left, so unbound it straddles the boundary.
        let o = page_opts(3.0, 12.0);
        let stanza = "one two three four five six";
        let long = |style| {
            paginate(
                &[para("x", BlockStyle::default()), para(stanza, style)],
                &o,
                &Mono,
            )
        };
        let split = long(BlockStyle::default());
        assert!(
            split[0].lines.len() > 1,
            "the fixture must actually straddle the boundary: {split:?}",
        );
        let kept = long(BlockStyle {
            break_inside: Some(PageBreak::Avoid),
            ..BlockStyle::default()
        });
        assert_eq!(
            text_of(&kept[0]),
            ["x"],
            "the stanza should move whole to the next page",
        );
        let total: usize = kept.iter().map(|pg| pg.lines.len()).sum();
        assert_eq!(
            total,
            split.iter().map(|pg| pg.lines.len()).sum::<usize>(),
            "no line is lost or duplicated by moving the block",
        );
    }

    /// Honouring `avoid` is a preference; losing text is not an acceptable way to keep it. A block
    /// taller than any page still splits.
    #[test]
    fn a_block_taller_than_a_page_still_splits() {
        let narrow = page_opts(4.0, 12.0);
        let huge = (0..40)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let pages = paginate(
            &[para(
                &huge,
                BlockStyle {
                    break_inside: Some(PageBreak::Avoid),
                    ..BlockStyle::default()
                },
            )],
            &narrow,
            &Mono,
        );
        assert!(pages.len() > 1, "it cannot fit; it must split");
        let placed: Vec<String> = pages.iter().flat_map(text_of).collect();
        assert!(
            placed.iter().any(|t| t.contains("w0")) && placed.iter().any(|t| t.contains("w39")),
            "every word must still be placed: {placed:?}",
        );
    }

    /// `page-break-after: avoid` binds a heading to the text it introduces, so the pair moves
    /// together rather than leaving the heading stranded at the foot of a page.
    #[test]
    fn avoid_after_keeps_a_block_with_the_next_one() {
        let o = four_line_page();
        let blocks = |style| {
            [
                para("a", BlockStyle::default()),
                para("b", BlockStyle::default()),
                para("c", BlockStyle::default()),
                para("heading", style),
                para("body", BlockStyle::default()),
            ]
        };
        let loose = paginate(&blocks(BlockStyle::default()), &o, &Mono);
        assert_eq!(
            text_of(&loose[0]),
            ["a", "b", "c", "heading"],
            "unbound, the heading fills the page and its body goes over",
        );
        let bound = paginate(
            &blocks(BlockStyle {
                break_after: Some(PageBreak::Avoid),
                ..BlockStyle::default()
            }),
            &o,
            &Mono,
        );
        assert_eq!(
            text_of(&bound[0]),
            ["a", "b", "c"],
            "bound, the heading goes over with its body",
        );
        assert_eq!(text_of(&bound[1]), ["heading", "body"]);
    }

    /// A forced break is a stronger statement than a preference not to break, so it ends a keep-run
    /// rather than being swallowed by it.
    #[test]
    fn a_forced_break_wins_over_an_avoid_beside_it() {
        let blocks = [
            para(
                "a",
                BlockStyle {
                    break_after: Some(PageBreak::Avoid),
                    ..BlockStyle::default()
                },
            ),
            para("b", always_before()),
        ];
        let pages = paginate(&blocks, &four_line_page(), &Mono);
        assert_eq!(pages.len(), 2, "the forced break still happens");
        assert_eq!(text_of(&pages[0]), ["a"]);
    }
}

// ── Stylesheet through to laid-out page (#251) ────────────────────────────────────────────────
//
// The unit tests above build `Block`s by hand. These drive the whole path the reporter's book
// takes — book CSS, XHTML, lowering, pagination — because most of what #251 reported was not any
// one stage being wrong but two stages disagreeing about who owned a property.

mod css_to_page {
    use super::*;
    use crate::content::parse_blocks_with;
    use crate::css::Stylesheet;
    use crate::layout::{paginate_upto, paginate_with_images, ImageSizer, NoImages};

    fn opts() -> LayoutOpts {
        LayoutOpts {
            page_w: 600.0,
            page_h: 400.0,
            margin: 0.0,
            ..LayoutOpts::new(600.0, 400.0, 20.0)
        }
    }

    fn blocks(css: &str, body: &str) -> Vec<Block> {
        parse_blocks_with(
            &format!("<html><body>{body}</body></html>"),
            &Stylesheet::parse(css),
        )
    }

    fn pages(css: &str, body: &str) -> Vec<Page> {
        paginate_with_images(&blocks(css, body), &opts(), &Mono, &NoHyphen, &NoImages)
    }

    fn tops(css: &str, body: &str) -> Vec<f32> {
        pages(css, body)
            .into_iter()
            .flat_map(|p| p.lines)
            .map(|l| l.top)
            .collect()
    }

    /// The two halves of #251 must not cancel: a stanza asks to be spaced *and* to be kept whole,
    /// and flowing it through a sub-pager used to swallow both of its margins.
    #[test]
    fn keeping_a_block_together_does_not_delete_its_margins() {
        let html = "<p>a</p><p>b</p><p>c</p>";
        assert_eq!(
            tops("p { margin: 2em 0; page-break-inside: avoid }", html),
            tops("p { margin: 2em 0 }", html),
            "`avoid` must not cost the block its spacing",
        );
    }

    /// A container's margin reaches the run it wraps, the way its page-break request does.
    /// Blockquote-wrapped verse is one of the commonest shapes in an EPUB.
    #[test]
    fn a_container_margin_separates_the_run_it_wraps() {
        let t = tops(
            "blockquote { margin: 2em 0 }",
            "<p>a</p><blockquote><p>b</p></blockquote><p>c</p>",
        );
        assert!(t[1] - t[0] > 40.0, "the blockquote's margin is lost: {t:?}");
        assert!(t[2] - t[1] > 40.0, "…on both edges: {t:?}");
    }

    /// A container's margin and break must reach the run, NOT every block in it. An anonymous
    /// paragraph is stamped with the style it inherits, so this is where a leak shows up first.
    #[test]
    fn a_container_break_does_not_leak_onto_every_anonymous_paragraph() {
        assert_eq!(
            pages(
                ".x { page-break-before: always }",
                r#"<div class="x">one<hr/>two<hr/>three</div>"#
            )
            .len(),
            1,
            "one break before the div, not one before every paragraph in it",
        );
    }

    /// The same leak by the other route: with no book CSS, `declared` takes a fast path that never
    /// reaches the inheritance step, so the rule has to be enforced in `declared` itself.
    #[test]
    fn a_container_break_does_not_leak_through_the_no_stylesheet_fast_path() {
        let b = parse_blocks_with(
            r#"<html><body><div style="page-break-before: always"><p>a</p><p>b</p><p>c</p></div></body></html>"#,
            &Stylesheet::default(),
        );
        let breaks: Vec<_> = b.iter().map(|x| x.style().break_before).collect();
        assert_eq!(
            breaks,
            vec![Some(PageBreak::Always), None, None],
            "an inline style on a wrapper must not descend",
        );
    }

    /// `<hr class="pagebreak"/>` is how a great many EPUB2 books start a chapter. A rule that
    /// cannot carry a style cannot say it.
    #[test]
    fn a_rule_can_force_a_page_break() {
        assert_eq!(
            pages(
                ".pb { page-break-after: always }",
                r#"<p>a</p><hr class="pb"/><p>b</p>"#
            )
            .len(),
            2,
        );
    }

    /// A break declared around a table reaches it. The reporter's whole document is a table, so a
    /// break that only worked on paragraphs would not close #251 for them.
    #[test]
    fn a_break_around_a_table_reaches_the_row() {
        assert_eq!(
            pages(
                ".poem { page-break-before: always }",
                r#"<p>a</p><div class="poem"><table><tr><td>x</td><td>y</td></tr></table></div>"#,
            )
            .len(),
            2,
        );
    }

    /// CSS resolves `em` against the element's own font size, so a margin on an `<h1>` is one
    /// h1-em. Resolving against the body size made every heading margin come out short.
    #[test]
    fn an_em_margin_resolves_against_the_blocks_own_font_size() {
        let t = tops("h1 { margin-top: 1em }", "<p>a</p><h1>H</h1>");
        let gap = t[1] - t[0] - 20.0 * 1.4;
        assert!(
            gap > 30.0,
            "1em on an h1 is 1.8 body-ems (36px), not 20px: gap {gap}",
        );
    }

    /// A cell's margin must not depend on whether the book wrapped the cell's text in a `<p>`.
    #[test]
    fn a_cell_margin_does_not_depend_on_how_the_cell_is_written() {
        let b = blocks(
            "td { margin-top: 2em }",
            "<table><tr><td>bare</td><td><p>wrapped</p></td></tr></table>",
        );
        let Some(Block::Row { cells, .. }) = b.first() else {
            panic!("expected a row");
        };
        assert_eq!(
            cells[0][0].style().margin_top,
            cells[1][0].style().margin_top,
        );
    }

    /// A rule spans its own cell. The renderer takes the span off the line, so a cell rule that
    /// reported a page-wide column would be drawn across its neighbours.
    #[test]
    fn a_rule_in_a_cell_spans_only_that_cell() {
        let p = pages("", "<table><tr><td><hr/></td><td>b</td></tr></table>");
        let rule = p[0].lines.iter().find(|l| l.rule).expect("a rule line");
        assert!(
            rule.column_w < opts().content_w() * 0.6,
            "a cell rule must not span the page: {} of {}",
            rule.column_w,
            opts().content_w(),
        );
    }

    struct Tall;
    impl ImageSizer for Tall {
        fn size(&self, _s: &str) -> Option<(u32, u32)> {
            Some((100, 10_000))
        }
    }

    /// An unpaged flow has no vertical budget of its own, but the page it lands on still does — so
    /// an image inside a cell must be fitted to the page, not to the flow's infinity.
    #[test]
    fn an_image_in_a_cell_is_still_capped_by_the_page() {
        let o = opts();
        let p = paginate_with_images(
            &blocks("", "<table><tr><td><img src=\"a.png\"/></td></tr></table>"),
            &o,
            &Mono,
            &NoHyphen,
            &Tall,
        );
        let h = p[0].lines[0].height;
        assert!(h <= o.content_h(), "a 10000px plate must scale down: {h}");
    }

    /// A `page-break-inside: avoid` on a section wrapper binds every block in it into one run.
    /// Holding a whole chapter before emitting a page is what #186's incremental pagination exists
    /// to avoid, so a run stops being held once it can no longer fit a page anyway.
    #[test]
    fn a_chapter_wide_avoid_still_paginates_incrementally() {
        let body: String = (0..200).map(|i| format!("<p>para {i}</p>")).collect();
        let short = LayoutOpts {
            page_h: 100.0,
            ..opts()
        };
        let (p, complete) = paginate_upto(
            &blocks(
                ".c { page-break-inside: avoid }",
                &format!(r#"<div class="c">{body}</div>"#),
            ),
            &short,
            &Mono,
            &NoHyphen,
            &NoImages,
            1,
        );
        assert_eq!(
            p.len(),
            1,
            "asking for one page laid out {} of them",
            p.len()
        );
        assert!(!complete);
    }

    /// The reporter's own case: a poem laid out as a bilingual table, each canto asked to start on
    /// a fresh page. A cell's forced break cuts the row into segments rather than being ignored.
    #[test]
    fn a_forced_break_inside_a_cell_splits_the_row() {
        let p = pages(
            "h3 { page-break-before: always }",
            "<table><tr>\
               <td><h3>I</h3><p>original</p><h3>II</h3><p>more</p></td>\
               <td><h3>I</h3><p>translation</p><h3>II</h3><p>plus</p></td>\
             </tr></table>",
        );
        assert_eq!(p.len(), 2, "the second canto starts a new page");
        for page in &p {
            for line in &page.lines {
                assert_eq!(
                    line.runs.len(),
                    2,
                    "both languages stay paired across the break: {:?}",
                    line.runs,
                );
            }
        }
    }

    /// The correspondence a parallel text is about: when only ONE cell forces a break, the other
    /// cell must be cut at the same vertical position, not run on past it.
    #[test]
    fn a_break_in_one_cell_cuts_the_whole_row() {
        let p = pages(
            ".pb { page-break-before: always }",
            "<table><tr>\
               <td><p>L1</p><h3 class=\"pb\">L2</h3><p>L3</p></td>\
               <td><p>R1</p><p>R2</p><p>R3</p></td>\
             </tr></table>",
        );
        let text = |i: usize| -> Vec<String> {
            p[i].lines
                .iter()
                .flat_map(|l| l.runs.iter().map(|r| r.text.clone()))
                .collect()
        };
        assert_eq!(p.len(), 2, "the break splits the row: {p:?}");
        assert_eq!(text(0), ["L1", "R1"], "R2/R3 must not run past the break");
        // The two cells are deliberately mismatched here — a heading opposite a paragraph — so
        // they wrap to different line heights and do not pair up line for line. Pairing is what
        // `matching_cells_stay_aligned_line_for_line` covers; what matters here is that everything
        // below the break moved, and moved together.
        let mut after = text(1);
        after.sort();
        assert_eq!(after, ["L2", "L3", "R2", "R3"]);
    }

    /// Where both cells break but their segments differ in height, the row's stage boundary is the
    /// taller of the two — so the shorter language waits rather than opening a near-empty page.
    #[test]
    fn uneven_segments_break_once_at_the_taller_cell() {
        let p = pages(
            ".pb { page-break-before: always }",
            "<table><tr>\
               <td><p>L1</p><h3 class=\"pb\">L2</h3></td>\
               <td><p>R1</p><p>R1b</p><p>R1c</p><h3 class=\"pb\">R2</h3></td>\
             </tr></table>",
        );
        assert_eq!(p.len(), 2, "one break, not one per cell: {p:?}");
        let last: Vec<String> = p[1]
            .lines
            .iter()
            .flat_map(|l| l.runs.iter().map(|r| r.text.clone()))
            .collect();
        assert!(
            last.contains(&"L2".to_string()) && last.contains(&"R2".to_string()),
            "both second cantos land on the same page: {last:?}",
        );
    }

    /// Several breaks in one row produce several stages, and nothing is lost or duplicated.
    #[test]
    fn a_row_with_several_breaks_places_every_line_once() {
        let p = pages(
            ".pb { page-break-before: always }",
            "<table><tr>\
               <td><p>a1</p><h3 class=\"pb\">a2</h3><p>a3</p><h3 class=\"pb\">a4</h3><p>a5</p></td>\
               <td><p>b1</p><p>b2</p><p>b3</p><p>b4</p><p>b5</p></td>\
             </tr></table>",
        );
        let mut all: Vec<String> = p
            .iter()
            .flat_map(|pg| {
                pg.lines
                    .iter()
                    .flat_map(|l| l.runs.iter().map(|r| r.text.clone()))
            })
            .collect();
        all.sort();
        assert_eq!(
            all,
            ["a1", "a2", "a3", "a4", "a5", "b1", "b2", "b3", "b4", "b5"],
            "every line placed exactly once across {} pages",
            p.len(),
        );
        assert_eq!(p.len(), 3, "two breaks make three stages");
    }

    /// A forced break must not leave a blank page when the row's first block declares it.
    #[test]
    fn a_forced_break_on_a_cells_first_block_does_not_blank_a_page() {
        let p = pages(
            "h3 { page-break-before: always }",
            "<table><tr><td><h3>I</h3><p>x</p></td></tr></table>",
        );
        assert_eq!(p.len(), 1);
        assert!(!p[0].lines.is_empty());
    }

    /// In two-column mode the pager produces columns, which `combine_columns` then pairs into
    /// pages — so a forced break starts a new *column*, not a new page. Pinned because it is the
    /// kind of interaction that otherwise gets discovered on a device.
    #[test]
    fn a_forced_break_in_two_column_mode_starts_a_column() {
        let two = LayoutOpts {
            columns: 2,
            ..opts()
        };
        let b = blocks("h2 { page-break-before: always }", "<p>a</p><h2>B</h2>");
        let p = paginate_with_images(&b, &two, &Mono, &NoHyphen, &NoImages);
        assert_eq!(p.len(), 1, "two columns still make one page: {p:?}");
        let xs: Vec<f32> = p[0].lines.iter().map(|l| l.runs[0].x).collect();
        assert!(
            xs[1] > xs[0],
            "the break moved the heading into the second column: {xs:?}",
        );
    }

    /// The block-level `<em>` fix (a walker that descended into the tag rather than dispatching on
    /// it) has no other test: the cell test that covers it reaches the same code by another route.
    #[test]
    fn a_block_level_emphasis_tag_keeps_its_own_emphasis() {
        let b = blocks("", "<p>a</p><em>stressed</em><p>b</p>");
        let Some(Block::Paragraph { content, .. }) = b.get(1) else {
            panic!("expected an anonymous paragraph: {b:?}");
        };
        assert!(
            content
                .iter()
                .any(|i| matches!(i, Inline::Run(r) if r.italic)),
            "the <em> itself must italicise what it wraps: {content:?}",
        );
    }
}

// ── A book's own heading size (#251) ──────────────────────────────────────────────────────────

mod declared_font_size {
    use super::*;

    /// inkread scales a heading by level; a book that states a size overrides that, which is the
    /// difference between a heading it designed and one we imposed.
    #[test]
    fn a_declared_size_beats_inkreads_heading_scale() {
        let heading = |style: BlockStyle| {
            let blocks = [Block::Heading {
                level: 3,
                content: vec![Inline::Run(TextRun {
                    text: "H".to_string(),
                    bold: false,
                    italic: false,
                    href: None,
                })],
                style,
            }];
            paginate(&blocks, &LayoutOpts::new(1200.0, 1600.0, 20.0), &Mono)[0].lines[0].runs[0]
                .size_px
        };
        let scaled = heading(BlockStyle::default());
        let declared = heading(BlockStyle {
            font_size: Some(Length::Em(1.0)),
            ..BlockStyle::default()
        });
        assert!(scaled > declared, "h3 defaults to 1.3x the body ({scaled})");
        assert_eq!(declared, 20.0, "1em is the body size");
    }

    /// And a declared `em` margin on that block resolves against the size the book set, not the
    /// size inkread would have chosen.
    #[test]
    fn a_margin_resolves_against_the_declared_size() {
        let gap = |font_size: Option<Length>| {
            let blocks = [
                para("a", BlockStyle::default()),
                Block::Heading {
                    level: 3,
                    content: vec![Inline::Run(TextRun {
                        text: "H".to_string(),
                        bold: false,
                        italic: false,
                        href: None,
                    })],
                    style: BlockStyle {
                        font_size,
                        margin_top: Some(Length::Em(1.0)),
                        ..BlockStyle::default()
                    },
                },
            ];
            let t = line_tops(&blocks, &LayoutOpts::new(1200.0, 1600.0, 20.0));
            t[1] - t[0]
        };
        assert!(
            gap(None) > gap(Some(Length::Em(1.0))),
            "1em against a 1.3x heading is more than 1em against a body-sized one",
        );
    }
}
