//! Tests for the reflow layout/pagination stage (RR4/RR12; #163/#188), split out to keep
//! `layout.rs` nearer the size guideline. Included via `#[path]` so `super::*` resolves to
//! the layout module.

#[test]
fn layout_digest_is_stable_and_sensitive_to_layout_fields() {
    let base = LayoutOpts::new(400.0, 600.0, 16.0);
    // Deterministic: identical opts → identical digest (same process AND across
    // processes — the persisted cache key contract).
    assert_eq!(
        base.layout_digest(),
        LayoutOpts::new(400.0, 600.0, 16.0).layout_digest()
    );

    // Every layout-affecting field flips the digest.
    let d = base.layout_digest();
    assert_ne!(
        d,
        LayoutOpts {
            page_w: 401.0,
            ..base
        }
        .layout_digest(),
        "width"
    );
    assert_ne!(
        d,
        LayoutOpts {
            page_h: 601.0,
            ..base
        }
        .layout_digest(),
        "height"
    );
    assert_ne!(
        d,
        LayoutOpts {
            margin: base.margin + 1.0,
            ..base
        }
        .layout_digest(),
        "margin"
    );
    assert_ne!(
        d,
        LayoutOpts {
            font_px: 17.0,
            ..base
        }
        .layout_digest(),
        "font"
    );
    assert_ne!(
        d,
        LayoutOpts {
            line_spacing: 1.5,
            ..base
        }
        .layout_digest(),
        "line spacing"
    );
    assert_ne!(
        d,
        LayoutOpts {
            para_gap: base.para_gap + 1.0,
            ..base
        }
        .layout_digest(),
        "para gap"
    );
    assert_ne!(
        d,
        LayoutOpts {
            align: Align::Justify,
            ..base
        }
        .layout_digest(),
        "align"
    );
}

#[test]
fn layout_digest_is_pinned_against_algorithm_drift() {
    // The digest keys persisted paginations (ADR-INKREAD-0013 D1); pin a known value so a future
    // change to the FNV constants/algorithm — which would silently orphan every cached
    // pagination — is caught here, exactly as reader-core's identity fingerprint is pinned.
    let opts = LayoutOpts {
        page_w: 400.0,
        page_h: 600.0,
        margin: 24.0,
        font_px: 16.0,
        line_spacing: 1.4,
        para_gap: 11.2,
        align: Align::Left,
    };
    assert_eq!(opts.layout_digest(), 17_685_407_801_978_826_572);
}

use super::*;
use crate::content::{parse_blocks, TextRun};
use crate::css::BlockStyle;

/// Fixed-pitch metrics: every char advances `0.5 * size` (bold/italic ignored). Deterministic, so
/// wrapping/pagination can be asserted exactly without a font.
struct Mono;
impl Metrics for Mono {
    fn advance(&self, text: &str, size_px: f32, _b: bool, _i: bool) -> f32 {
        text.chars().count() as f32 * size_px * 0.5
    }
}

/// One unstyled text run — the inline content nearly every block in these tests is built from.
fn runs(text: &str) -> Vec<Inline> {
    vec![Inline::Run(TextRun {
        text: text.into(),
        bold: false,
        italic: false,
        href: None,
    })]
}

fn para(text: &str) -> Block {
    styled_para(text, BlockStyle::default())
}

/// Build the opts used by the #188 declared-style tests: 10px Mono font (5px/char), content
/// width 100 → 20 chars per line, and a reader whose global alignment is the default Left.
fn style_opts() -> LayoutOpts {
    LayoutOpts {
        page_w: 100.0,
        page_h: 10_000.0,
        margin: 0.0,
        font_px: 10.0,
        line_spacing: 1.0,
        para_gap: 0.0,
        align: Align::Left,
    }
}

fn styled_para(text: &str, style: BlockStyle) -> Block {
    Block::Paragraph {
        content: runs(text),
        style,
    }
}

fn styled_heading(text: &str, style: BlockStyle) -> Block {
    Block::Heading {
        level: 1,
        content: runs(text),
        style,
    }
}

/// #188: the title page rendered hard-left because `text-align: center` never reached layout.
#[test]
fn a_book_declared_centre_centres_even_when_the_reader_prefers_left() {
    let opts = style_opts();
    let style = BlockStyle {
        align: Some(Align::Center),
        ..Default::default()
    };
    let pages = paginate(&[styled_heading("PRIDE", style)], &opts, &Mono);
    let x = pages[0].lines[0].runs[0].x;
    // "PRIDE" at heading scale is narrower than the content box, so it must be inset.
    assert!(x > 0.0, "declared centre was ignored: x = {x}");

    // …and an undeclared heading still honours the reader's Left.
    let plain = paginate(
        &[styled_heading("PRIDE", BlockStyle::default())],
        &opts,
        &Mono,
    );
    assert_eq!(plain[0].lines[0].runs[0].x, 0.0);
}

/// The other half of the #188 policy: a book that justifies its prose must not override the
/// alignment the reader chose for the text they actually read.
#[test]
fn a_book_declared_justify_or_left_defers_to_the_reader() {
    let opts = style_opts();
    for declared in [Align::Justify, Align::Left] {
        let style = BlockStyle {
            align: Some(declared),
            ..Default::default()
        };
        // A long paragraph so justification would visibly spread the first line's runs.
        let text = "alpha bravo charlie delta echo foxtrot golf hotel";
        let book = paginate(&[styled_para(text, style)], &opts, &Mono);
        let user = paginate(&[styled_para(text, BlockStyle::default())], &opts, &Mono);
        assert_eq!(
            book[0].lines[0]
                .runs
                .iter()
                .map(|r| r.x)
                .collect::<Vec<_>>(),
            user[0].lines[0]
                .runs
                .iter()
                .map(|r| r.x)
                .collect::<Vec<_>>(),
            "book-declared {declared:?} should defer to the reader's Left"
        );
    }
}

/// The reader's own Justify still applies to blocks the book says nothing about — the policy
/// suppresses the *book's* left/justify, not the setting.
#[test]
fn the_readers_justify_still_applies_to_undeclared_blocks() {
    let opts = LayoutOpts {
        align: Align::Justify,
        ..style_opts()
    };
    let text = "alpha bravo charlie delta echo foxtrot golf hotel";
    let pages = paginate(&[styled_para(text, BlockStyle::default())], &opts, &Mono);
    let first = &pages[0].lines[0];
    assert!(first.runs.len() > 1);
    let last_x = first.runs.last().unwrap().x;
    let flush = paginate(
        &[styled_para(text, BlockStyle::default())],
        &LayoutOpts {
            align: Align::Left,
            ..style_opts()
        },
        &Mono,
    );
    assert!(
        last_x > flush[0].lines[0].runs.last().unwrap().x,
        "justification did not spread the line"
    );
}

/// #188 / #163: the paragraph indent is applied even to blocks whose stylesheet turns it off.
#[test]
fn a_declared_zero_text_indent_drops_the_first_line_indent() {
    let opts = style_opts();
    let indented = paginate(
        &[styled_para("short line", BlockStyle::default())],
        &opts,
        &Mono,
    );
    assert_eq!(indented[0].lines[0].runs[0].x, 12.0, "1.2em of 10px");

    let style = BlockStyle {
        indent: Some(false),
        ..Default::default()
    };
    let flat = paginate(&[styled_para("short line", style)], &opts, &Mono);
    assert_eq!(
        flat[0].lines[0].runs[0].x, 0.0,
        "text-indent: 0 was ignored"
    );
}

/// A centred block must not also carry the synthetic prose indent, which would push it off
/// centre by half the indent.
#[test]
fn a_centred_paragraph_drops_the_indent_it_would_otherwise_carry() {
    let opts = style_opts();
    let centred = BlockStyle {
        align: Some(Align::Center),
        ..Default::default()
    };
    let text = "abcd"; // 4 chars * 5px = 20px wide in a 100px box
    let pages = paginate(&[styled_para(text, centred)], &opts, &Mono);
    assert_eq!(
        pages[0].lines[0].runs[0].x, 40.0,
        "expected (100 - 20) / 2 with no indent skewing it"
    );
}

/// #188: headings were laid out with `bold_all = true` unconditionally.
#[test]
fn a_declared_normal_font_weight_unbolds_a_heading() {
    let opts = style_opts();
    let bold = paginate(
        &[styled_heading("Title", BlockStyle::default())],
        &opts,
        &Mono,
    );
    assert!(bold[0].lines[0].runs[0].bold, "headings default to bold");

    let style = BlockStyle {
        bold: Some(false),
        ..Default::default()
    };
    let normal = paginate(&[styled_heading("Title", style)], &opts, &Mono);
    assert!(
        !normal[0].lines[0].runs[0].bold,
        "font-weight: normal was ignored"
    );

    // …and the converse: a paragraph the book bolds comes out bold.
    let style = BlockStyle {
        bold: Some(true),
        ..Default::default()
    };
    let p = paginate(&[styled_para("x", style)], &opts, &Mono);
    assert!(p[0].lines[0].runs[0].bold);
}

#[test]
fn long_paragraph_wraps_to_multiple_lines() {
    // 10px font → 5px/char. content_w = 100 → 20 chars/line. 60-char paragraph → 3 lines.
    let opts = LayoutOpts {
        page_w: 100.0 + 2.0 * 0.0,
        page_h: 10_000.0,
        margin: 0.0,
        font_px: 10.0,
        line_spacing: 1.0,
        para_gap: 0.0,
        align: Align::Left,
    };
    let words = "aaaa ".repeat(12); // 12 words of 4 chars → ~ wraps
    let pages = paginate(&[para(words.trim())], &opts, &Mono);
    assert_eq!(pages.len(), 1);
    assert!(
        pages[0].lines.len() >= 3,
        "wrapped: {}",
        pages[0].lines.len()
    );
    // No run exceeds the content width.
    for line in &pages[0].lines {
        for r in &line.runs {
            let w = r.x + r.text.chars().count() as f32 * 5.0;
            assert!(w <= 100.0 + 0.01, "run overflows: {w}");
        }
    }
}

/// The narrow test viewport for the unspaced-script (CJK) wrapping tests: 10px font → 5px/char
/// (Mono); content 50px. The paragraph's 12px (1.2em) indent applies to the **first line only**,
/// so 7 chars fit there and 10 on every line after it.
fn cjk_opts() -> LayoutOpts {
    LayoutOpts {
        page_w: 50.0,
        page_h: 10_000.0,
        margin: 0.0,
        font_px: 10.0,
        line_spacing: 1.0,
        para_gap: 0.0,
        align: Align::Left,
    }
}

/// Each line's runs joined (the CJK tests place one run per line).
fn line_texts(pages: &[Page]) -> Vec<String> {
    pages
        .iter()
        .flat_map(|p| &p.lines)
        .map(|l| l.runs.iter().map(|r| r.text.as_str()).collect())
        .collect()
}

/// #163: the first-line indent must land on the first line and nowhere else.
///
/// This typography omits the blank line between paragraphs on purpose, so the indent is the
/// *only* thing marking where one paragraph ends and the next begins. Applying it to every line
/// — which is what the code did — left prose with no paragraph breaks at all, and spent 1.2em of
/// every line's width on nothing.
#[test]
fn only_a_paragraphs_first_line_is_indented() {
    let pages = paginate(
        &[para("alpha bravo charlie delta echo foxtrot")],
        &cjk_opts(),
        &Mono,
    );
    let lines: Vec<&LayoutLine> = pages.iter().flat_map(|p| &p.lines).collect();
    assert!(
        lines.len() > 1,
        "need a wrapped paragraph to test: {lines:?}"
    );

    assert_eq!(
        lines[0].runs[0].x, 12.0,
        "the first line carries the 1.2em indent"
    );
    for (i, line) in lines.iter().enumerate().skip(1) {
        assert_eq!(line.runs[0].x, 0.0, "line {i} must start flush left");
    }
}

/// The counterpart: a list item's indent is a *hanging* indent, so it applies to every line and
/// the marker hangs to the left of the first. The two must not be conflated.
#[test]
fn a_list_items_body_stays_indented_on_every_line() {
    let item = Block::ListItem {
        ordered: false,
        index: 1,
        content: runs("alpha bravo charlie delta echo"),
        style: BlockStyle::default(),
    };
    let pages = paginate(&[item], &cjk_opts(), &Mono);
    let lines: Vec<&LayoutLine> = pages.iter().flat_map(|p| &p.lines).collect();
    assert!(lines.len() > 1, "need a wrapped item to test");

    // The marker hangs at the content origin; the body starts indented past it.
    assert_eq!(lines[0].runs[0].text, "•");
    assert_eq!(lines[0].runs[0].x, 0.0, "the marker hangs left");
    let body_x = lines[0].runs[1].x;
    assert!(body_x > 0.0, "the body is inset past the marker");
    for (i, line) in lines.iter().enumerate().skip(1) {
        assert_eq!(
            line.runs[0].x, body_x,
            "wrapped line {i} stays under the body, not the marker"
        );
    }
}

#[test]
fn cjk_paragraph_wraps_between_characters() {
    // No spaces, so the whole paragraph is one token — before UAX #14 it overflowed as a single
    // unbreakable "word". 22 Han chars wrap greedily to 7 (the indented first line) + 10 + 5,
    // nothing lost.
    let text = "书山有路勤为径学海无涯苦作舟读万卷书行万里路";
    let pages = paginate(&[para(text)], &cjk_opts(), &Mono);
    let lines = line_texts(&pages);
    assert_eq!(lines.len(), 3, "22 chars as 7+10+5: {lines:?}");
    assert_eq!(
        lines[0].chars().count(),
        7,
        "the indented first line is narrower"
    );
    assert_eq!(
        lines[1].chars().count(),
        10,
        "later lines get the full column"
    );
    assert_eq!(lines.concat(), text, "no characters dropped or reordered");
    // No line overflows the content box, indented or not.
    for line in pages.iter().flat_map(|p| &p.lines) {
        for r in &line.runs {
            assert!(r.x + r.text.chars().count() as f32 * 5.0 <= 50.0 + 0.01);
        }
    }
}

#[test]
fn cjk_break_honors_kinsoku_and_anchors_stay_font_invariant() {
    // Naive 7-chars-per-line breaking would start line 2 with 。— UAX #14 forbids a break
    // before a closing form, so the break retreats to after 六 and 。hangs onto 七's line.
    let pages = paginate(&[para("一二三四五六七。八九十")], &cjk_opts(), &Mono);
    let lines = line_texts(&pages);
    assert_eq!(lines, ["一二三四五六", "七。八九十"]);
    assert!(lines.iter().all(|l| !l.starts_with('。')));
    // The continuation run keeps a chapter-relative anchor = chars consumed before it
    // (ADR-INKREAD-0012 — a highlight re-resolves across reflow).
    let runs: Vec<&PlacedRun> = pages
        .iter()
        .flat_map(|p| &p.lines)
        .flat_map(|l| &l.runs)
        .collect();
    assert_eq!(runs[0].anchor.char_offset, 0);
    assert_eq!(runs[1].anchor.char_offset, 6);
}

#[test]
fn mixed_latin_cjk_breaks_at_the_script_boundary_opportunities() {
    // "Hello你好世界" is one token (no spaces). Latin-internal positions are not eligible —
    // only UAX #14 opportunities adjacent to an unspaced-script char — so the line fills to
    // "Hello你好" (7 chars, 35px ≤ 38px) and wraps cleanly, no overflow, no hyphen.
    let pages = paginate(&[para("Hello你好世界")], &cjk_opts(), &Mono);
    let lines = line_texts(&pages);
    assert_eq!(lines, ["Hello你好", "世界"]);
    assert!(!lines[0].ends_with('-'), "no hyphen for an unspaced break");
}

#[test]
fn content_overflow_breaks_into_pages() {
    // line height 10px, content_h 30px → 3 lines/page. 7 short paragraphs → 3 pages.
    let opts = LayoutOpts {
        page_w: 1000.0,
        page_h: 30.0,
        margin: 0.0,
        font_px: 10.0,
        line_spacing: 1.0,
        para_gap: 0.0,
        align: Align::Left,
    };
    let blocks: Vec<Block> = (0..7).map(|_| para("x")).collect();
    let pages = paginate(&blocks, &opts, &Mono);
    assert_eq!(pages.len(), 3, "7 lines / 3 per page");
    assert_eq!(pages[0].lines.len(), 3);
    assert_eq!(pages[2].lines.len(), 1);
}

#[test]
fn heading_uses_a_larger_line_height() {
    let opts = LayoutOpts {
        page_w: 10_000.0,
        page_h: 10_000.0,
        margin: 0.0,
        font_px: 10.0,
        line_spacing: 1.0,
        para_gap: 0.0,
        align: Align::Left,
    };
    let pages = paginate(
        &[Block::Heading {
            level: 1,
            content: runs("Title"),
            style: BlockStyle::default(),
        }],
        &opts,
        &Mono,
    );
    let line = &pages[0].lines[0];
    assert_eq!(line.height, 18.0, "h1 = 1.8 * 10"); // heading_scale(1)=1.8
    assert!(line.runs[0].bold, "headings render bold");
}

#[test]
fn list_item_has_marker_and_hanging_indent() {
    let opts = LayoutOpts::new(1000.0, 1000.0, 10.0);
    let pages = paginate(
        &[Block::ListItem {
            ordered: true,
            index: 3,
            content: runs("item text"),
            style: BlockStyle::default(),
        }],
        &opts,
        &Mono,
    );
    let runs = &pages[0].lines[0].runs;
    assert_eq!(runs[0].text, "3.", "ordered marker");
    assert_eq!(runs[0].x, 0.0, "marker at content origin");
    assert!(runs[1].x > 0.0, "body hangs past the marker");
}

#[test]
fn integrates_with_phase2_parsing() {
    let blocks = parse_blocks("<html><body><h2>Hi</h2><p>one two three</p></body></html>");
    let opts = LayoutOpts::new(400.0, 600.0, 16.0);
    let pages = paginate(&blocks, &opts, &Mono);
    assert!(!pages.is_empty());
    let all_text: String = pages[0]
        .lines
        .iter()
        .flat_map(|l| l.runs.iter())
        .map(|r| r.text.clone())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(all_text.contains("Hi") && all_text.contains("three"));
}

#[test]
fn empty_blocks_make_no_pages() {
    assert!(paginate(&[], &LayoutOpts::new(400.0, 600.0, 16.0), &Mono).is_empty());
}

/// Collect every placed run as `(block, char_offset, text)` in reading order.
fn run_anchors(pages: &[Page]) -> Vec<(usize, usize, String)> {
    pages
        .iter()
        .flat_map(|p| p.lines.iter())
        .flat_map(|l| l.runs.iter())
        .map(|r| (r.anchor.block, r.anchor.char_offset, r.text.clone()))
        .collect()
}

fn wide(font_px: f32) -> LayoutOpts {
    LayoutOpts {
        page_w: 100_000.0,
        page_h: 100_000.0,
        margin: 0.0,
        font_px,
        line_spacing: 1.0,
        para_gap: 0.0,
        align: Align::Left,
    }
}

#[test]
fn run_anchors_track_chapter_character_offsets() {
    // "alpha"(5)@0, space@5, "beta"(4)@6, space@10, "gamma"@11.
    let pages = paginate(&[para("alpha beta gamma")], &wide(10.0), &Mono);
    assert_eq!(
        run_anchors(&pages),
        vec![
            (0, 0, "alpha".into()),
            (0, 6, "beta".into()),
            (0, 11, "gamma".into()),
        ]
    );
}

#[test]
fn block_index_increments_and_offset_continues_across_blocks() {
    // block 0 "one two": one@0, two@4 → cursor 7; block 1 "three"@7.
    let pages = paginate(&[para("one two"), para("three")], &wide(10.0), &Mono);
    assert_eq!(
        run_anchors(&pages),
        vec![
            (0, 0, "one".into()),
            (0, 4, "two".into()),
            (1, 7, "three".into()),
        ]
    );
}

#[test]
fn list_marker_shares_body_offset_and_does_not_consume_budget() {
    let pages = paginate(
        &[
            Block::ListItem {
                ordered: true,
                index: 1,
                content: runs("first"),
                style: BlockStyle::default(),
            },
            para("after"),
        ],
        &LayoutOpts::new(1000.0, 1000.0, 10.0),
        &Mono,
    );
    let anchors = run_anchors(&pages);
    // Marker "1." and body "first" both anchor at offset 0 of block 0; the marker adds no budget,
    // so "after" (block 1) starts at 5 (= len("first")), not 7.
    assert_eq!(anchors[0], (0, 0, "1.".into()), "marker shares body start");
    assert_eq!(anchors[1], (0, 0, "first".into()), "body at block start");
    assert_eq!(
        anchors[2],
        (1, 5, "after".into()),
        "marker consumed no offset"
    );
}

/// A test hyphenator allowing breaks at fixed byte offsets (regardless of the word).
struct HyphenAt(Vec<usize>);
impl Hyphenator for HyphenAt {
    fn opportunities(&self, _word: &str) -> Vec<usize> {
        self.0.clone()
    }
}

fn narrow_para() -> LayoutOpts {
    // Mono 10px → 5px/char; content 60 wide, paragraph indent 12 → 48px usable.
    LayoutOpts {
        page_w: 60.0,
        page_h: 10_000.0,
        margin: 0.0,
        font_px: 10.0,
        line_spacing: 1.0,
        para_gap: 0.0,
        align: Align::Left,
    }
}

#[test]
fn long_word_hyphenates_and_suffix_keeps_its_anchor() {
    // "hyphenation" (11 chars = 55px) overflows the 48px column; a break after 5 bytes fits
    // ("hyphe" 25px + hyphen 5px = 30 ≤ 48), so it splits and the suffix anchors after the prefix.
    let pages = paginate_with(
        &[para("hyphenation")],
        &narrow_para(),
        &Mono,
        &HyphenAt(vec![5]),
    );
    let runs: Vec<_> = pages[0].lines.iter().flat_map(|l| l.runs.iter()).collect();
    assert_eq!(runs[0].text, "hyphe-", "prefix carries a trailing hyphen");
    assert_eq!(runs[1].text, "nation", "suffix continues on the next line");
    assert_eq!(runs[0].anchor.char_offset, 0);
    assert_eq!(
        runs[1].anchor.char_offset, 5,
        "suffix anchored at prefix length (reflow-stable)"
    );
}

#[test]
fn default_paginate_never_hyphenates() {
    // The default ([`NoHyphen`]) places an overflowing word whole, on its own line.
    let pages = paginate(&[para("hyphenation")], &narrow_para(), &Mono);
    let runs: Vec<_> = pages[0].lines.iter().flat_map(|l| l.runs.iter()).collect();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].text, "hyphenation");
}

#[test]
fn run_anchors_are_font_size_invariant() {
    // Wrapping differs by size, but each word keeps its (block, char_offset) — the reflow-stable
    // property a highlight/Digest anchor relies on (ADR-INKREAD-0012).
    let blocks = [
        para("the quick brown fox jumps over"),
        para("the lazy dog sleeps soundly"),
    ];
    let narrow = |fp: f32| {
        let opts = LayoutOpts {
            page_w: 60.0,
            ..wide(fp)
        };
        run_anchors(&paginate(&blocks, &opts, &Mono))
    };
    assert_eq!(narrow(10.0), narrow(20.0));
}

// ---------------------------------------------------------------------------------------------
// #187 — illustrations laid out as boxes rather than `[image]` text.
// ---------------------------------------------------------------------------------------------

/// A sizer over a fixed table, so layout geometry is asserted without decoding anything.
struct Sizes(&'static [(&'static str, u32, u32)]);

impl ImageSizer for Sizes {
    fn size(&self, src: &str) -> Option<(u32, u32)> {
        self.0
            .iter()
            .find(|(s, _, _)| *s == src)
            .map(|(_, w, h)| (*w, *h))
    }
}

fn image_block(src: &str) -> Block {
    Block::Image {
        src: src.into(),
        alt: "a plate".into(),
    }
}

/// 200x200 content box, so fits are easy to read off.
fn image_opts() -> LayoutOpts {
    LayoutOpts {
        page_w: 200.0,
        page_h: 200.0,
        margin: 0.0,
        font_px: 10.0,
        line_spacing: 1.0,
        para_gap: 0.0,
        align: Align::Left,
    }
}

fn only_image(pages: &[Page]) -> &PlacedImage {
    pages
        .iter()
        .flat_map(|p| &p.lines)
        .find_map(|l| l.image.as_ref())
        .expect("no image was laid out")
}

#[test]
fn an_image_is_scaled_to_fit_and_centred() {
    let opts = image_opts();
    // Wider than the box: scaled by width, centred vertically-irrelevant, x = 0.
    let wide = paginate_with_images(
        &[image_block("w.png")],
        &opts,
        &Mono,
        &NoHyphen,
        &Sizes(&[("w.png", 400, 100)]),
    );
    let img = only_image(&wide);
    assert_eq!((img.width, img.height), (200, 50), "aspect ratio not kept");
    assert_eq!(img.x, 0, "a full-width image has no slack to centre in");

    // Narrower than the box: centred, never enlarged.
    let narrow = paginate_with_images(
        &[image_block("n.png")],
        &opts,
        &Mono,
        &NoHyphen,
        &Sizes(&[("n.png", 50, 20)]),
    );
    let img = only_image(&narrow);
    assert_eq!(
        (img.width, img.height),
        (50, 20),
        "a small image must not be upscaled"
    );
    assert_eq!(img.x, 75, "expected (200 - 50) / 2");
}

/// A plate taller than the page has to shrink to fit one, or it could never be shown at all.
#[test]
fn an_oversized_image_is_capped_to_a_single_page() {
    let opts = image_opts();
    let pages = paginate_with_images(
        &[image_block("tall.png")],
        &opts,
        &Mono,
        &NoHyphen,
        &Sizes(&[("tall.png", 100, 1000)]),
    );
    let img = only_image(&pages);
    assert!(img.height <= 200, "taller than the content box: {img:?}");
    assert_eq!((img.width, img.height), (20, 200), "scaled by height");
    assert_eq!(pages.len(), 1);
}

/// The image has to occupy real vertical space, or pagination would overlap it with the prose
/// after it — the whole point of a box rather than a text line.
#[test]
fn an_image_takes_its_height_from_the_page_and_pushes_prose_on() {
    let opts = image_opts();
    let blocks = vec![image_block("p.png"), para("after the plate")];
    let pages = paginate_with_images(
        &blocks,
        &opts,
        &Mono,
        &NoHyphen,
        &Sizes(&[("p.png", 200, 195)]),
    );
    // 195px image + its trailing gap leaves no room for a 10px line on a 200px page.
    assert_eq!(pages.len(), 2, "{pages:#?}");
    assert!(pages[0].lines.iter().any(|l| l.image.is_some()));
    assert!(pages[1].lines.iter().any(|l| !l.runs.is_empty()));
}

/// An unresolvable image must still tell the reader something is missing.
#[test]
fn an_unresolvable_image_falls_back_to_the_named_placeholder() {
    let opts = image_opts();
    let pages = paginate_with_images(
        &[image_block("gone.png")],
        &opts,
        &Mono,
        &NoHyphen,
        &Sizes(&[]),
    );
    assert!(
        pages
            .iter()
            .flat_map(|p| &p.lines)
            .all(|l| l.image.is_none()),
        "nothing should be placed"
    );
    let text: String = pages
        .iter()
        .flat_map(|p| &p.lines)
        .flat_map(|l| &l.runs)
        .map(|r| r.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(text.contains("a plate"), "alt text was dropped: {text:?}");

    // A zero-dimension image is treated as unresolvable rather than dividing by zero.
    let degenerate = paginate_with_images(
        &[image_block("z.png")],
        &opts,
        &Mono,
        &NoHyphen,
        &Sizes(&[("z.png", 0, 10)]),
    );
    assert!(degenerate
        .iter()
        .flat_map(|p| &p.lines)
        .all(|l| l.image.is_none()));
}

/// Offsets after an image must not depend on whether it resolved, or a reading position saved
/// with images available would move when one later fails (ADR-INKREAD-0012).
#[test]
fn source_offsets_after_an_image_do_not_depend_on_it_resolving() {
    let opts = image_opts();
    let blocks = vec![image_block("p.png"), para("after")];
    let offset = |sizer: &dyn ImageSizer| -> usize {
        paginate_with_images(&blocks, &opts, &Mono, &NoHyphen, sizer)
            .iter()
            .flat_map(|p| &p.lines)
            .flat_map(|l| &l.runs)
            .find(|r| r.text == "after")
            .expect("prose run")
            .anchor
            .char_offset
    };
    assert_eq!(
        offset(&Sizes(&[("p.png", 40, 40)])),
        offset(&Sizes(&[])),
        "an image occupies one character position either way"
    );
}

// ---------------------------------------------------------------------------------------------
// #186 — stopping pagination once the requested page exists.
// ---------------------------------------------------------------------------------------------

/// A page short enough that the fixture below runs to many pages: 100px wide (20 chars a line at
/// the 10px Mono metrics), 50px tall (5 lines a page).
fn paged_opts() -> LayoutOpts {
    LayoutOpts {
        page_h: 50.0,
        ..style_opts()
    }
}

/// Enough prose to run to many pages at the tiny test metrics.
fn long_book() -> Vec<Block> {
    (0..60)
        .map(|i| {
            para(&format!(
                "Paragraph {i} alpha bravo charlie delta echo foxtrot golf hotel"
            ))
        })
        .collect()
}

/// The property the whole optimisation rests on: a partial pass is a *prefix* of the full one, not
/// an approximation. A page break depends only on what precedes it, so stopping early cannot move
/// an earlier boundary — if it could, a resumed reading position would land on a different page
/// than the one the pagination index counted.
#[test]
fn a_partial_pagination_is_a_prefix_of_the_full_one() {
    let opts = paged_opts();
    let blocks = long_book();
    let (full, complete) = paginate_upto(&blocks, &opts, &Mono, &NoHyphen, &NoImages, usize::MAX);
    assert!(complete, "an unbounded pass is always complete");
    assert!(
        full.len() > 5,
        "need a multi-page fixture, got {}",
        full.len()
    );

    for want in 1..=5 {
        let (partial, complete) = paginate_upto(&blocks, &opts, &Mono, &NoHyphen, &NoImages, want);
        assert!(
            !complete,
            "{want} pages should not exhaust a {}-page book",
            full.len()
        );
        assert!(partial.len() >= want, "asked {want}, got {}", partial.len());
        for (i, page) in partial.iter().enumerate().take(want) {
            assert_eq!(page, &full[i], "page {i} differs when stopping at {want}");
        }
    }
}

#[test]
fn asking_for_more_pages_than_exist_returns_them_all_and_reports_complete() {
    let opts = paged_opts();
    let blocks = long_book();
    let full = paginate_with_images(&blocks, &opts, &Mono, &NoHyphen, &NoImages);
    let (all, complete) = paginate_upto(&blocks, &opts, &Mono, &NoHyphen, &NoImages, 10_000);
    assert!(complete);
    assert_eq!(all, full, "an over-large bound must not change the result");
}

/// The point of the change: laying out one page must not walk the whole chapter.
#[test]
fn stopping_early_does_less_work_than_a_full_pass() {
    use std::cell::Cell;
    struct Counting<'a>(&'a Cell<usize>);
    impl Metrics for Counting<'_> {
        fn advance(&self, text: &str, size_px: f32, _b: bool, _i: bool) -> f32 {
            self.0.set(self.0.get() + 1);
            text.chars().count() as f32 * size_px * 0.5
        }
    }
    let opts = paged_opts();
    let blocks = long_book();

    let full_calls = Cell::new(0);
    let _ = paginate_upto(
        &blocks,
        &opts,
        &Counting(&full_calls),
        &NoHyphen,
        &NoImages,
        usize::MAX,
    );

    let one_calls = Cell::new(0);
    let (pages, complete) = paginate_upto(
        &blocks,
        &opts,
        &Counting(&one_calls),
        &NoHyphen,
        &NoImages,
        1,
    );

    assert!(!complete);
    assert!(!pages.is_empty(), "one page must still be produced");
    assert!(
        one_calls.get() * 4 < full_calls.get(),
        "laying out one page took {} measurements vs {} for the whole chapter — not a saving",
        one_calls.get(),
        full_calls.get()
    );
}

#[test]
fn a_zero_bound_lays_nothing_out_and_an_empty_book_is_complete() {
    let opts = paged_opts();
    let (pages, complete) = paginate_upto(&long_book(), &opts, &Mono, &NoHyphen, &NoImages, 0);
    assert!(pages.is_empty(), "a zero bound must lay out nothing");
    assert!(!complete);

    let (pages, complete) = paginate_upto(&[], &opts, &Mono, &NoHyphen, &NoImages, 5);
    assert!(pages.is_empty());
    assert!(complete, "there was nothing left unlaid");
}
