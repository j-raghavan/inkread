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
        columns: 1,
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
        columns: 1,
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

/// #170: a declared `font-style` has to reach the placed run, or the renderer never learns to set
/// it in an italic face. Italic already arrived from `<i>`/`<em>`/`<cite>`; a stylesheet saying so
/// was parsed and then dropped on the floor between `BlockStyle` and the run.
#[test]
fn a_declared_font_style_reaches_the_placed_runs() {
    let opts = style_opts();

    let plain = paginate(&[styled_para("x", BlockStyle::default())], &opts, &Mono);
    assert!(!plain[0].lines[0].runs[0].italic, "prose defaults upright");

    let style = BlockStyle {
        italic: Some(true),
        ..Default::default()
    };
    let italic = paginate(&[styled_para("x", style)], &opts, &Mono);
    assert!(
        italic[0].lines[0].runs[0].italic,
        "font-style: italic was ignored"
    );

    // It applies to a heading too, which carries its own default weight but no default slant.
    let head = paginate(&[styled_heading("Title", style)], &opts, &Mono);
    assert!(head[0].lines[0].runs[0].italic);
    assert!(
        head[0].lines[0].runs[0].bold,
        "and does not disturb the weight"
    );
}

/// A block-level italic covers every run in the block, including ones the markup did not italicise,
/// exactly as a block-level weight covers every run.
#[test]
fn a_block_italic_covers_runs_the_markup_left_upright() {
    let opts = style_opts();
    let style = BlockStyle {
        italic: Some(true),
        ..Default::default()
    };
    let laid = paginate(&[styled_para("one two three", style)], &opts, &Mono);
    let runs = &laid[0].lines[0].runs;
    assert!(!runs.is_empty());
    assert!(
        runs.iter().all(|r| r.italic),
        "a run escaped the block style"
    );
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
        columns: 1,
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
        columns: 1,
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
        columns: 1,
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
        columns: 1,
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
        columns: 1,
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
        columns: 1,
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
fn soft_hyphenated_break_is_flagged_as_inserted() {
    let pages = paginate_with(
        &[para("hyphenation")],
        &narrow_para(),
        &Mono,
        &HyphenAt(vec![5]),
    );
    let runs: Vec<_> = pages[0].lines.iter().flat_map(|l| l.runs.iter()).collect();
    assert_eq!(runs[0].wrap, Some(Wrap::SoftHyphen), "we added that hyphen");
    assert_eq!(runs[1].wrap, None, "the last line of the word ends it");
}

#[test]
fn compound_broken_at_its_own_hyphen_gets_no_second_one() {
    // en-US patterns offer "well-known" exactly one break, at byte 5 — right after the hyphen it
    // already has. Printing "well--" there is wrong, and the hyphen shown is the source's own.
    let pages = paginate_with(
        &[para("well-known")],
        &narrow_para(),
        &Mono,
        &HyphenAt(vec![5]),
    );
    let runs: Vec<_> = pages[0].lines.iter().flat_map(|l| l.runs.iter()).collect();
    assert_eq!(runs[0].text, "well-", "no doubled hyphen");
    assert_eq!(runs[1].text, "known");
    assert_eq!(
        runs[0].wrap,
        Some(Wrap::Kept),
        "every character is the source's, so rejoining keeps the hyphen"
    );
}

#[test]
fn compound_broken_before_its_own_hyphen_keeps_it_on_the_next_line() {
    // The mirror: "self-evident" broken at byte 4 prints "self-" too, but that hyphen IS ours and
    // the source's own opens the continuation. Identical on the page, opposite in meaning.
    let pages = paginate_with(
        &[para("self-evident")],
        &narrow_para(),
        &Mono,
        &HyphenAt(vec![4]),
    );
    let runs: Vec<_> = pages[0].lines.iter().flat_map(|l| l.runs.iter()).collect();
    assert_eq!(runs[0].text, "self-");
    assert_eq!(runs[1].text, "-evident");
    assert_eq!(runs[0].wrap, Some(Wrap::SoftHyphen), "that hyphen is ours");
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
        style: BlockStyle::default(),
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
        columns: 1,
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

// ---------------------------------------------------------------------------------------------
// #194 — two-column layout.
// ---------------------------------------------------------------------------------------------

/// A page wide enough for two real columns at the test metrics: 400px wide, 50px tall, margin 0,
/// 10px font. Two columns = (400 - 0 gutter) / 2 = 200px each — well over the 18em floor.
fn two_col_opts() -> LayoutOpts {
    LayoutOpts {
        page_w: 400.0,
        page_h: 50.0,
        margin: 0.0,
        font_px: 10.0,
        line_spacing: 1.0,
        para_gap: 0.0,
        align: Align::Left,
        columns: 2,
    }
}

fn words_of(page: &Page) -> Vec<String> {
    page.lines
        .iter()
        .flat_map(|l| &l.runs)
        .map(|r| r.text.clone())
        .collect()
}

/// The heart of it: text must fill the left column, then the right, then move to the next page.
/// Getting this wrong reads as scrambled prose, not as a layout bug.
#[test]
fn text_fills_the_first_column_then_the_second() {
    let opts = two_col_opts();
    let blocks: Vec<Block> = (0..40).map(|i| para(&format!("w{i}"))).collect();
    let single = paginate(&blocks, &LayoutOpts { columns: 1, ..opts }, &Mono);
    let double = paginate(&blocks, &opts, &Mono);

    // Same words, same order — only their positions differ.
    let flat = |pages: &[Page]| -> Vec<String> { pages.iter().flat_map(words_of).collect() };
    assert_eq!(flat(&single), flat(&double), "reading order changed");

    // Two columns hold twice the text, so the page count roughly halves.
    assert!(
        double.len() < single.len(),
        "two columns produced {} pages vs {} single",
        double.len(),
        single.len()
    );

    // On page 1 the left column's words all precede the right column's, and the right column starts
    // at the second column's origin.
    let first = &double[0];
    let dx = opts.column_width() + opts.margin.max(0.0);
    let left: Vec<&PlacedRun> = first
        .lines
        .iter()
        .flat_map(|l| &l.runs)
        .filter(|r| r.x < dx.max(1.0))
        .collect();
    let right: Vec<&PlacedRun> = first
        .lines
        .iter()
        .flat_map(|l| &l.runs)
        .filter(|r| r.x >= dx.max(1.0))
        .collect();
    assert!(
        !left.is_empty() && !right.is_empty(),
        "expected both columns filled"
    );
    let last_left = left.last().unwrap().anchor.char_offset;
    let first_right = right.first().unwrap().anchor.char_offset;
    assert!(
        first_right > last_left,
        "the right column must continue from the left, not precede it ({first_right} vs {last_left})"
    );
}

/// Both columns start at the top of the page — that is what makes them columns rather than a
/// continuation down the page.
#[test]
fn both_columns_start_at_the_top_of_the_page() {
    let opts = two_col_opts();
    let blocks: Vec<Block> = (0..40).map(|i| para(&format!("w{i}"))).collect();
    let pages = paginate(&blocks, &opts, &Mono);
    let dx = opts.column_width();
    let top_of = |right: bool| -> f32 {
        pages[0]
            .lines
            .iter()
            .filter(|l| l.runs.iter().any(|r| (r.x >= dx) == right))
            .map(|l| l.top)
            .fold(f32::INFINITY, f32::min)
    };
    assert_eq!(top_of(false), top_of(true), "columns must share a top edge");
}

/// A page too narrow for a readable measure declines the request rather than honouring it badly.
#[test]
fn a_page_too_narrow_for_two_columns_stays_single() {
    // 200px wide at a 10px font: each column would be 100px = 10em, under the 18em floor.
    let narrow = LayoutOpts {
        page_w: 200.0,
        ..two_col_opts()
    };
    assert_eq!(narrow.effective_columns(), 1, "should have declined");
    assert_eq!(
        narrow.column_width(),
        LayoutOpts {
            columns: 1,
            ..narrow
        }
        .column_width(),
        "a declined request must lay out exactly as single-column"
    );

    // …and a page that IS wide enough honours it.
    assert_eq!(two_col_opts().effective_columns(), 2);
}

/// A larger font makes a column narrower in ems, so the same page can stop supporting two columns.
#[test]
fn the_fallback_follows_the_font_size_not_just_the_page_width() {
    let base = two_col_opts();
    assert_eq!(base.effective_columns(), 2);
    let huge = LayoutOpts {
        font_px: 20.0,
        ..base
    };
    assert_eq!(
        huge.effective_columns(),
        1,
        "200px column is 10em at a 20px font"
    );
}

/// The digest keys persisted paginations. Two columns must not be served a single-column index.
#[test]
fn the_column_count_changes_the_layout_digest() {
    let one = two_col_opts_with(1);
    let two = two_col_opts_with(2);
    assert_ne!(one.layout_digest(), two.layout_digest());
    // …but a single-column page digests exactly as it did before columns existed, so paginations
    // already cached are not thrown away.
    assert_eq!(
        one.layout_digest(),
        LayoutOpts { columns: 0, ..one }.layout_digest()
    );
}

fn two_col_opts_with(columns: u8) -> LayoutOpts {
    LayoutOpts {
        columns,
        ..two_col_opts()
    }
}

/// A rule must not run across the gutter — that reads as a divider between columns.
#[test]
fn a_rule_spans_only_its_own_column() {
    let opts = two_col_opts();
    let mut blocks: Vec<Block> = (0..20).map(|i| para(&format!("w{i}"))).collect();
    blocks.push(Block::Rule {
        style: BlockStyle::default(),
    });
    blocks.extend((20..40).map(|i| para(&format!("w{i}"))));
    let pages = paginate(&blocks, &opts, &Mono);
    let rules: Vec<f32> = pages
        .iter()
        .flat_map(|p| &p.lines)
        .filter(|l| l.rule)
        .map(|l| l.column_x)
        .collect();
    assert!(!rules.is_empty(), "the fixture should produce a rule");
    for x in &rules {
        assert!(
            *x == 0.0 || (*x - (opts.column_width() + opts.margin)).abs() < 0.5,
            "a rule sits at a column origin, got {x}"
        );
    }
}

/// The case #194 was actually reported for, and the one the first attempt got wrong: a Nomad at a
/// comfortable reading size. 1920px wide, 56px text (a 1.5x scale on the 38px base) — the column
/// comes out at 14 em, so a floor set any higher declines the feature precisely where it was asked
/// for. Pinned with real numbers rather than taste.
#[test]
fn a_nomad_at_a_comfortable_reading_size_gets_two_columns() {
    let opts = LayoutOpts {
        columns: 2,
        ..LayoutOpts::new(1920.0, 2560.0, 56.0)
    };
    assert_eq!(
        opts.effective_columns(),
        2,
        "column was {:.1} em",
        opts.column_width() / opts.font_px
    );
    // Comfortably inside newspaper territory: about 28 characters at half an em each.
    let ems = opts.column_width() / opts.font_px;
    assert!(
        (13.0..=16.0).contains(&ems),
        "expected ~14 em, got {ems:.1}"
    );

    // Raise the text far enough and it is declined again — the floor still protects the measure.
    let huge = LayoutOpts {
        columns: 2,
        ..LayoutOpts::new(1920.0, 2560.0, 90.0)
    };
    assert_eq!(
        huge.effective_columns(),
        1,
        "90px text leaves under 9 em a column"
    );
}

// ── Table rows laid out side by side (#200) ───────────────────────────────────────────────────

mod row_layout {
    use super::*;

    fn cell(text: &str) -> Vec<Block> {
        vec![Block::Paragraph {
            content: vec![Inline::Run(TextRun {
                text: text.to_string(),
                bold: false,
                italic: false,
                href: None,
            })],
            // Cells are built by `walk_cell`, which zeroes the prose indent; keep the fixture
            // faithful to that so measured `x` positions match what a book produces.
            style: BlockStyle {
                indent: Some(false),
                ..BlockStyle::default()
            },
        }]
    }

    fn row(cells: &[&str]) -> Block {
        Block::Row {
            cells: cells.iter().map(|c| cell(c)).collect(),
            style: BlockStyle::default(),
        }
    }

    /// A wide page, so two cells comfortably clear MIN_CELL_EM.
    fn opts() -> LayoutOpts {
        LayoutOpts::new(1200.0, 1600.0, 20.0)
    }

    fn lines(blocks: &[Block], opts: &LayoutOpts) -> Vec<LayoutLine> {
        paginate(blocks, opts, &Mono)
            .into_iter()
            .flat_map(|p| p.lines)
            .collect()
    }

    /// The whole point: the pair shares a line box, so the text reads across.
    #[test]
    fn a_two_cell_row_puts_both_cells_on_one_line() {
        let out = lines(&[row(&["alpha", "beta"])], &opts());
        assert_eq!(out.len(), 1, "one line, not one per cell");
        let texts: Vec<&str> = out[0].runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts, vec!["alpha", "beta"]);
        assert!(
            out[0].runs[0].x < out[0].runs[1].x,
            "the second cell must sit to the right of the first",
        );
    }

    /// The regression this replaces: cells used to become separate stacked blocks.
    #[test]
    fn cells_are_not_stacked_vertically() {
        let out = lines(&[row(&["alpha", "beta"])], &opts());
        let tops: Vec<f32> = out.iter().map(|l| l.top).collect();
        assert_eq!(tops.len(), 1, "stacked cells would give two tops: {tops:?}");
    }

    /// Cells occupy their own share of the measure and do not overlap.
    #[test]
    fn cells_divide_the_measure_without_overlapping() {
        let o = opts();
        let out = lines(&[row(&["alpha", "beta", "gamma"])], &o);
        let xs: Vec<f32> = out[0].runs.iter().map(|r| r.x).collect();
        assert_eq!(xs.len(), 3);
        assert!(xs[0] < xs[1] && xs[1] < xs[2], "cells out of order: {xs:?}");
        assert!(
            xs[2] < o.column_width(),
            "the last cell must start inside the measure",
        );
    }

    /// A row is as tall as its tallest cell; the short one leaves whitespace beneath it.
    #[test]
    fn a_row_is_as_tall_as_its_tallest_cell() {
        let long = "one two three four five six seven eight nine ten eleven twelve";
        let out = lines(&[row(&[long, "short"])], &opts());
        assert!(out.len() > 1, "the long cell should wrap to several lines");
        // Every line after the first belongs to the long cell alone.
        let first_line_cells = out[0].runs.len();
        assert!(first_line_cells >= 2, "both cells start on the first line");
    }

    /// Rows keep the reading order of their source, so anchors still map back to characters.
    #[test]
    fn character_offsets_advance_in_source_order() {
        let out = lines(&[row(&["alpha", "beta"])], &opts());
        let offsets: Vec<usize> = out[0].runs.iter().map(|r| r.anchor.char_offset).collect();
        assert!(
            offsets.windows(2).all(|w| w[0] <= w[1]),
            "offsets must not go backwards across cells: {offsets:?}",
        );
    }

    /// Too many cells to give any of them a readable measure: stack rather than shred the words.
    #[test]
    fn an_unusably_narrow_row_falls_back_to_stacking() {
        let cells = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l"];
        // A narrow page makes twelve cells hopeless.
        let narrow = LayoutOpts::new(300.0, 800.0, 20.0);
        let out = lines(&[row(&cells)], &narrow);
        assert!(
            out.len() > 1,
            "unusably narrow cells should stack, not share one line",
        );
    }

    /// The blocks inside a cell (#251). Built by hand rather than parsed: these tests are about
    /// layout, and `content` owns the lowering.
    fn para(text: &str) -> Block {
        cell(text).remove(0)
    }

    fn heading(level: u8, text: &str) -> Block {
        Block::Heading {
            level,
            content: vec![Inline::Run(TextRun {
                text: text.to_string(),
                bold: false,
                italic: false,
                href: None,
            })],
            style: BlockStyle::default(),
        }
    }

    fn row_of(cells: Vec<Vec<Block>>) -> Block {
        Block::Row {
            cells,
            style: BlockStyle::default(),
        }
    }

    /// #251(1): the heading was flattened into body text. It must now be set as a heading — bigger
    /// than the body, and bold by inkread's typography.
    #[test]
    fn a_heading_in_a_cell_is_set_as_a_heading() {
        let out = lines(
            &[row_of(vec![vec![heading(3, "Canto"), para("body")]])],
            &opts(),
        );
        let title = &out[0].runs[0];
        let body = out
            .iter()
            .flat_map(|l| &l.runs)
            .find(|r| r.text.contains("body"))
            .expect("the body paragraph should be laid out");
        assert!(
            title.size_px > body.size_px,
            "a heading in a cell should outsize the body ({} vs {})",
            title.size_px,
            body.size_px,
        );
        assert!(title.bold, "headings are bold by default");
    }

    /// #251(2): two paragraphs in a cell ran together on one line, because a cell was one flat
    /// inline run. They must now occupy separate line boxes.
    #[test]
    fn consecutive_blocks_in_a_cell_do_not_run_together() {
        let out = lines(&[row_of(vec![vec![para("one"), para("two")]])], &opts());
        let tops: Vec<f32> = out
            .iter()
            .filter(|l| !l.runs.is_empty())
            .map(|l| l.top)
            .collect();
        assert_eq!(tops.len(), 2, "one line box each: {tops:?}");
        assert!(tops[1] > tops[0], "the second must sit below the first");
    }

    /// A heading's block gap is real vertical space, and it has to survive the trip through the
    /// cell flow: merging cells by line *index* would drop it.
    #[test]
    fn a_cells_block_gaps_survive_into_the_page() {
        let o = opts();
        let plain = lines(&[row_of(vec![vec![para("a"), para("b")]])], &o);
        let spaced = lines(&[row_of(vec![vec![para("a"), heading(3, "b")]])], &o);
        let drop = |v: &[LayoutLine]| v[1].top - v[0].top;
        assert!(
            drop(&spaced) > drop(&plain),
            "a heading's 0.7em margin-before should widen the gap ({} vs {})",
            drop(&spaced),
            drop(&plain),
        );
    }

    /// The parallel-text guarantee, restated over blocks: cells whose structure matches stay
    /// side by side, line for line.
    #[test]
    fn matching_cells_stay_aligned_line_for_line() {
        let out = lines(
            &[row_of(vec![
                vec![heading(3, "Canto"), para("original")],
                vec![heading(3, "Chant"), para("translation")],
            ])],
            &opts(),
        );
        let tops: Vec<f32> = out.iter().map(|l| l.top).collect();
        assert_eq!(tops.len(), 2, "a heading row and a body row: {tops:?}");
        for line in &out {
            assert_eq!(
                line.runs.len(),
                2,
                "both languages share the line box: {:?}",
                line.runs,
            );
        }
    }

    /// A cell taller than the page splits at a line boundary, keeping the columns aligned rather
    /// than losing content or overflowing.
    #[test]
    fn a_row_taller_than_the_page_splits_across_pages() {
        let short = LayoutOpts::new(1200.0, 220.0, 20.0);
        let many: Vec<Block> = (0..12).map(|i| para(&format!("line{i}"))).collect();
        let pages = paginate(&[row_of(vec![many.clone(), many.clone()])], &short, &Mono);
        assert!(pages.len() > 1, "12 paragraphs cannot fit 220px");
        for page in &pages {
            for line in &page.lines {
                assert!(
                    line.top + line.height <= short.content_h() + 0.5,
                    "a line overflowed the content box: {line:?}",
                );
                assert_eq!(
                    line.runs.len(),
                    2,
                    "the two cells stay aligned across the break"
                );
            }
        }
        let placed: usize = pages.iter().map(|p| p.lines.len()).sum();
        assert_eq!(placed, 12, "every paragraph is placed exactly once");
    }

    /// A single-cell table is how a lot of EPUB2 does plain layout, so a lone cell must get the
    /// whole measure — not a half-width column with the other half left empty.
    #[test]
    fn a_one_cell_row_uses_the_full_measure() {
        let o = opts();
        let text = "one two three four five six seven eight nine ten eleven twelve thirteen";
        let alone = lines(&[row(&[text])], &o);
        let shared = lines(&[row(&[text, "x"])], &o);
        assert!(
            alone.len() < shared.len(),
            "a lone cell wrapped as much as a half-width one ({} vs {} lines): it is not              getting the full measure",
            alone.len(),
            shared.len(),
        );
        assert_eq!(
            alone[0].runs[0].x, 0.0,
            "a cell carries no first-line indent"
        );
    }
}

// ── Declared vertical margins (#251) ──────────────────────────────────────────────────────────

mod margins {
    use super::*;

    fn p(text: &str, style: BlockStyle) -> Block {
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

    fn tops(blocks: &[Block]) -> Vec<f32> {
        let o = LayoutOpts::new(1200.0, 1600.0, 20.0);
        paginate(blocks, &o, &Mono)
            .into_iter()
            .flat_map(|pg| pg.lines)
            .map(|l| l.top)
            .collect()
    }

    /// #251(3): prose is set dense, so without this a book's stanza spacing vanished entirely.
    #[test]
    fn a_declared_margin_separates_paragraphs_that_would_otherwise_be_dense() {
        let dense = tops(&[p("a", BlockStyle::default()), p("b", BlockStyle::default())]);
        let spaced = tops(&[
            p("a", BlockStyle::default()),
            p(
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
                p(
                    "a",
                    BlockStyle {
                        margin_bottom: Some(Length::Em(a)),
                        ..BlockStyle::default()
                    },
                ),
                p(
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
            p("a", BlockStyle::default()),
            p("b", BlockStyle::default()),
            p(
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
                p("a", BlockStyle::default()),
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

    /// A page whose content box holds exactly `lines` lines of `chars` characters, so a fixture
    /// can straddle a boundary deliberately. `Mono` advances half the font size per character.
    fn page_opts(lines: f32, chars: f32) -> LayoutOpts {
        const FONT: f32 = 20.0;
        LayoutOpts {
            page_w: chars * FONT * 0.5,
            page_h: lines * FONT * 1.4,
            margin: 0.0,
            ..LayoutOpts::new(100.0, 100.0, FONT)
        }
    }

    /// Four lines at a comfortable measure: nothing wraps unless the fixture means it to.
    fn four_line_page() -> LayoutOpts {
        page_opts(4.0, 40.0)
    }

    fn p(text: &str, style: BlockStyle) -> Block {
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
        let blocks = [p("a", BlockStyle::default()), p("b", always_before())];
        let pages = paginate(&blocks, &four_line_page(), &Mono);
        assert_eq!(pages.len(), 2, "both would otherwise share a page");
        assert_eq!(text_of(&pages[0]), ["a"]);
        assert_eq!(text_of(&pages[1]), ["b"]);
    }

    /// A forced break at the top of a page must not leave a blank one.
    #[test]
    fn a_forced_break_at_the_top_of_a_page_does_not_blank_it() {
        let pages = paginate(&[p("a", always_before())], &four_line_page(), &Mono);
        assert_eq!(pages.len(), 1);
        assert_eq!(text_of(&pages[0]), ["a"]);
    }

    /// `page-break-after: always` breaks on the far side of the block.
    #[test]
    fn a_forced_break_after_ends_the_page() {
        let blocks = [
            p(
                "a",
                BlockStyle {
                    break_after: Some(PageBreak::Always),
                    ..BlockStyle::default()
                },
            ),
            p("b", BlockStyle::default()),
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
                &[p("x", BlockStyle::default()), p(stanza, style)],
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
            &[p(
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
                p("a", BlockStyle::default()),
                p("b", BlockStyle::default()),
                p("c", BlockStyle::default()),
                p("heading", style),
                p("body", BlockStyle::default()),
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
            p(
                "a",
                BlockStyle {
                    break_after: Some(PageBreak::Avoid),
                    ..BlockStyle::default()
                },
            ),
            p("b", always_before()),
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
