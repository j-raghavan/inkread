//! Tests for the reflowable EPUB backend (RR2/RR4/RR12; #161/#162), split out to keep
//! `reflow.rs` nearer the size guideline. Included via `#[path]` so `super::*` resolves to
//! the reflow module.

use super::*;
use crate::render::Viewport;
use inkread_epub::parse_blocks;

const SAMPLE: &[u8] = include_bytes!("../../tests/fixtures/sample.epub");
/// A book carrying #188's stylesheet: `h1 { text-align: center; font-weight: normal }`, a `.c`
/// decorative paragraph, and `p { text-align: justify }` over ordinary prose.
const STYLED: &[u8] = include_bytes!("../../tests/fixtures/styled.epub");
/// A book with a 120x80 mid-grey PNG referenced from its chapter (#187).
const ILLUSTRATED: &[u8] = include_bytes!("../../tests/fixtures/illustrated.epub");

fn vp(w: u32, h: u32) -> Viewport {
    Viewport {
        width: w,
        height: h,
        dpi: 226,
    }
}

fn render(backend: &EpubBackend, index: usize, w: u32, h: u32) -> Vec<u8> {
    let mut bytes = vec![0u8; (w * h * 4) as usize];
    let mut buf = PixelBuffer::from_rgba(&mut bytes, w, h).unwrap();
    backend.render_page(index, &mut buf).unwrap();
    bytes
}

/// Host preview (#66): render a compiled Daily issue EPUB to PNGs so the reading experience can
/// be eyeballed on a host (the e-ink screencap is black). Run:
///   `INKREAD_ISSUE_EPUB=/path/issue.epub cargo test -p reader-core daily_render_dump -- --ignored --nocapture`
/// → writes `target/daily-preview/page-NN.png`. Driven by `scripts/daily-preview.sh`.
#[test]
#[ignore = "host preview: needs INKREAD_ISSUE_EPUB=<path>; run with --ignored --nocapture"]
fn daily_render_dump() {
    let Ok(path) = std::env::var("INKREAD_ISSUE_EPUB") else {
        eprintln!("SKIP daily_render_dump: set INKREAD_ISSUE_EPUB to a compiled issue .epub");
        return;
    };
    let bytes = std::fs::read(&path).expect("issue epub readable");
    let env_u32 = |k: &str, d: u32| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    let (w, h) = (env_u32("INKREAD_W", 790), env_u32("INKREAD_H", 1024)); // device panel for real preview
    let b = EpubBackend::open(bytes, vp(w, h)).expect("open issue epub");
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/daily-preview");
    std::fs::create_dir_all(&out).unwrap();
    let pages = b.page_count().min(10);
    for i in 0..pages {
        let px = render(&b, i, w, h);
        let f = std::io::BufWriter::new(
            std::fs::File::create(out.join(format!("page-{i:02}.png"))).unwrap(),
        );
        let mut enc = png::Encoder::new(f, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&px).unwrap();
    }
    eprintln!("wrote {pages} page PNGs to {}", out.display());
}

#[test]
fn opens_paginates_and_exposes_metadata() {
    let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
    assert_eq!(b.metadata().title.as_deref(), Some("Reflow Sample"));
    // Two chapters, each ≥ 1 page.
    assert!(b.page_count() >= 2, "pages = {}", b.page_count());
}

#[test]
fn renders_ink_and_respects_page_range() {
    let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
    let px = render(&b, 0, 400, 600);
    let inked = px.chunks_exact(4).filter(|p| p[0] < 250).count();
    assert!(inked > 50, "first page has rendered text: {inked}");

    let mut bytes = vec![0u8; 400 * 600 * 4];
    let mut buf = PixelBuffer::from_rgba(&mut bytes, 400, 600).unwrap();
    assert!(matches!(
        b.render_page(9999, &mut buf),
        Err(CoreError::PageOutOfRange { .. })
    ));
}

#[test]
fn toc_resolves_to_page_targets() {
    let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
    let toc = b.toc();
    assert_eq!(toc.len(), 2, "two nav points");
    assert_eq!(toc[0].title, "Chapter One");
    assert_eq!(toc[0].target_page, Some(0), "ch1 starts at page 0");
    assert!(
        toc[1].target_page.unwrap() >= 1,
        "ch2 starts on a later page: {:?}",
        toc[1].target_page
    );
}

#[test]
fn larger_text_scale_repaginates_and_keeps_the_chapter() {
    let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
    let _ = render(&b, 0, 400, 600); // settle the layout at this viewport
    let base_pages = b.page_count();
    // Reader is in chapter 2; bumping the size should land us back at chapter 2's new start.
    let ch2_start = b.toc()[1].target_page.unwrap();
    let new_page = b.set_text_scale(1.8, ch2_start).unwrap();
    assert!(
        b.page_count() >= base_pages,
        "bigger text ⇒ at least as many pages"
    );
    // The returned page is chapter 2's start under the new pagination.
    assert_eq!(new_page, b.toc()[1].target_page.unwrap());
    // PDF-style fixed layout would return None; EPUB returns Some.
    assert!(b.set_text_scale(1.0, 0).is_some());
}

#[test]
fn set_font_repaginates_and_lists_faces() {
    let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
    let _ = render(&b, 0, 400, 600);
    let ch2_start = b.toc()[1].target_page.unwrap();
    // Switching the reading face repaginates (EPUB → Some), landing back at chapter 2's start.
    let new_page = b.set_font(2, ch2_start).unwrap();
    assert_eq!(new_page, b.toc()[1].target_page.unwrap());
    // An out-of-range id falls back to the default face, still repaginating.
    assert!(b.set_font(999, 0).is_some());
    // The bundled set is exposed for the picker (Spectral + the KOReader OSS faces).
    let names = inkread_epub::reading_font_names();
    assert!(
        names.len() >= 4 && names[0] == "Spectral",
        "faces: {names:?}"
    );
}

/// The source character a pin currently resolves to: the glyph on `pin_to_page(pin)` whose anchor
/// matches the pin's block + offset.
fn char_at(b: &EpubBackend, pin: &PinPosition) -> Option<char> {
    let page = b.pin_to_page(pin);
    b.page_chars(page).into_iter().find_map(|c| {
        c.anchor.and_then(|a| {
            (a.block as i32 == pin.xpath[0] && a.char_offset as i32 == pin.text_offset)
                .then_some(c.ch)
        })
    })
}

#[test]
fn page_pin_anchors_to_the_first_glyph_and_first_page_is_chapter_start() {
    let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
    let _ = render(&b, 0, 400, 600);
    let pin = b.page_pin(0).expect("page 0 has text");
    assert_eq!(pin.chapter_index, 0, "page 0 is chapter 0");
    // The pin resolves to the first character actually painted on page 0.
    let first_ch = b.page_chars(0).into_iter().find(|c| c.anchor.is_some());
    assert_eq!(char_at(&b, &pin), first_ch.map(|c| c.ch));
}

#[test]
fn pin_re_anchors_to_the_same_character_across_a_font_size_change() {
    // The headline guarantee (golden SPEC-INKREAD.md RR8-AC1 / RR12-FR4): a pin minted at one
    // size re-resolves to the *same source character* after the page reflows at a new size.
    let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
    let _ = render(&b, 0, 400, 600);
    // Mint a pin partway through chapter 2 so re-pagination genuinely moves it.
    let ch2 = b.toc()[1].target_page.unwrap();
    let pin = b.page_pin(ch2).or_else(|| b.page_pin(0)).expect("a pin");
    let before = char_at(&b, &pin).expect("char before reflow");

    let moved_page = b.set_text_scale(1.9, ch2).unwrap();
    let after = char_at(&b, &pin).expect("char after reflow");

    assert_eq!(
        before, after,
        "pin re-resolves to the same source character after reflow"
    );
    // Sanity: the pin still lands inside its own chapter under the new pagination.
    assert_eq!(
        b.chapter_of(b.pin_to_page(&pin)),
        pin.chapter_index.max(0) as usize
    );
    let _ = moved_page;
}

#[test]
fn selection_pins_span_in_order_and_survive_a_font_size_change() {
    let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
    let _ = render(&b, 0, 400, 600);
    // A band over the top of page 0 selects the opening lines.
    let band = NormRect {
        x0: 0.0,
        y0: 0.0,
        x1: 1.0,
        y1: 0.5,
    };
    let (start, end) = b.selection_pins(0, band).expect("a selection on page 0");
    assert!(start <= end, "start pin precedes end pin");
    let (sc, ec) = (
        char_at(&b, &start).expect("start char"),
        char_at(&b, &end).expect("end char"),
    );

    // Reflow at a larger size: the same span endpoints re-resolve to the same characters.
    b.set_text_scale(1.7, 0).unwrap();
    assert_eq!(char_at(&b, &start), Some(sc), "start re-anchors");
    assert_eq!(char_at(&b, &end), Some(ec), "end re-anchors");
}

#[test]
fn epub_is_never_magnifiable() {
    // EPUB is always reflowed (font size, not zoom), so the shell must never magnify it (#61,
    // RR25-FR3) — the trait default for a reflowable backend.
    let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
    assert!(!b.is_magnifiable());
}

#[test]
fn text_line_span_selects_reflowed_lines() {
    // Regression (#46 device finding): EpubBackend didn't override text_line_span, so a multi-line
    // drag on a reflowed page hit the Document trait's empty default and selected nothing — the
    // bbox path worked, the line-span path returned ''. The override must select real text.
    // A realistic reading viewport (the default body size needs a real column; a tiny one would
    // hyphenate/clip every word).
    let b = EpubBackend::open(SAMPLE.to_vec(), vp(800, 1200)).unwrap();
    let _ = render(&b, 0, 800, 1200);
    let sel = b.text_line_span(0, (0.05, 0.02), (0.60, 0.30)); // a drag down the top of page 0
    assert!(
        !sel.boxes.is_empty(),
        "reflowed line-span produces highlight boxes"
    );
    // Correctness, not just wiring: the span must be REAL page text. Pull a word the page
    // actually has and confirm the top-of-page span contains it (the first line is taken whole,
    // so the page's opening words are in range) — a wrong-glyph override would not contain it.
    let page_text: String = b.page_chars(0).iter().map(|c| c.ch).collect();
    let word = page_text
        .split_whitespace()
        .find(|w| w.chars().all(|c| c.is_alphabetic()) && w.len() >= 4)
        .expect("a real word on page 0");
    assert!(
        sel.text.contains(word),
        "line-span selects actual page text containing {word:?}, got '{}'",
        sel.text
    );
}

#[test]
fn page_chars_recovers_words_for_selection() {
    let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
    let _ = render(&b, 0, 400, 600); // settle layout at this viewport
    let chars = b.page_chars(0);
    assert!(!chars.is_empty(), "first page exposes glyphs");
    let text: String = chars.iter().map(|c| c.ch).collect();
    assert!(
        text.contains(' '),
        "inter-word spaces are synthesized: {text:?}"
    );
    // Every box is on-page and non-degenerate horizontally for non-space glyphs.
    assert!(chars.iter().all(|c| c.rect.x0 >= 0.0 && c.rect.x1 <= 1.0));
}

#[test]
fn search_finds_text_on_the_page_it_lives_on() {
    let b = EpubBackend::open(SAMPLE.to_vec(), vp(800, 1200)).unwrap();
    let _ = render(&b, 0, 800, 1200);
    // Pull a real word off page 0 and search for it.
    let text: String = b.page_chars(0).iter().map(|c| c.ch).collect();
    let word = text
        .split_whitespace()
        .find(|w| w.chars().all(|c| c.is_alphabetic()) && w.len() >= 4)
        .expect("a searchable word on page 0")
        .to_string();
    let hits = b.search_page(0, &word);
    assert!(!hits.is_empty(), "found {word:?} on page 0");
    assert!(hits[0].boxes.iter().all(|bx| bx.x1 >= bx.x0));
    // The same query in a wildly different case still matches (case-insensitive).
    assert!(!b.search_page(0, &word.to_uppercase()).is_empty());
}

#[test]
fn smaller_viewport_repaginates_to_more_pages() {
    let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
    let wide = b.page_count();
    // Render into a much shorter buffer → fewer lines/page → more pages (lazy repagination).
    let _ = render(&b, 0, 400, 200);
    let tall = b.page_count();
    assert!(
        tall > wide,
        "narrower/shorter viewport paginates longer: {wide} → {tall}"
    );
}

// ---- pagination cost (#161/#162) ----------------------------------------------------------
// These assert *how often* the book is paginated, not what the pagination looks like. A
// redundant pass is invisible to every other test here — it yields an identical layout — but on
// a large book each one costs seconds, which is the whole substance of #161/#162.

#[test]
fn opening_a_book_does_not_paginate_until_something_reads_the_layout() {
    reset_layout_passes();
    let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
    assert_eq!(layout_passes(), 0, "open alone paginates nothing");
    // Metadata comes from the package, not the layout — the session reads it during open.
    let _ = b.metadata();
    assert_eq!(layout_passes(), 0, "metadata needs no pagination");
    let _ = b.page_count();
    assert_eq!(layout_passes(), 1, "the first read paginates once");
}

#[test]
fn restoring_saved_typography_over_a_cold_open_costs_one_pagination() {
    reset_layout_passes();
    let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
    // Exactly what the shell does on open: restore four persisted settings, then read the count.
    let page = b.set_typography(1.25, 1, 1.7, 2, 1, 0);
    let count = b.page_count();
    assert_eq!(
        layout_passes(),
        1,
        "a cold open restoring saved typography paginates once, not once per setting"
    );
    assert_eq!(page, Some(0), "a cold open resumes at the first page");
    assert!(count > 0);
}

#[test]
fn reapplying_the_settings_already_in_effect_does_not_repaginate() {
    let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
    let before = b.page_count();
    reset_layout_passes();
    // Re-picking the value already selected in the Adjust sheet must cost nothing.
    assert_eq!(b.set_text_scale(1.0, 0), Some(0));
    assert_eq!(b.set_line_spacing(DEFAULT_LINE_SPACING, 0), Some(0));
    assert_eq!(b.set_alignment(0, 0), Some(0));
    assert_eq!(b.set_font(0, 0), Some(0));
    assert_eq!(layout_passes(), 0, "no setting changed → no repagination");
    assert_eq!(b.page_count(), before, "and the pagination is untouched");
    // A real change still repaginates.
    let _ = b.set_text_scale(2.0, 0);
    assert_eq!(layout_passes(), 1, "a changed setting repaginates once");
}

#[test]
fn an_out_of_range_font_id_resolves_to_the_default_face_without_repaginating() {
    let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
    let before = b.page_count();
    reset_layout_passes();
    // `AbFont::for_face` maps both of these onto the default face, so neither is a face change
    // — the normalized id must reflect that rather than forcing a pass (RR21-FR3).
    assert_eq!(b.set_font(9999, 0), Some(0));
    assert_eq!(b.set_font(-3, 0), Some(0));
    assert_eq!(layout_passes(), 0, "out-of-range ids are the default face");
    assert_eq!(b.page_count(), before);
}

#[test]
fn set_typography_lays_out_the_same_book_as_the_four_setters_do() {
    // The batched path is an optimization, so it must be indistinguishable from the individual
    // setters it replaces — same page count, same page content.
    let batched = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
    assert_eq!(batched.set_typography(1.5, 2, 1.2, 3, 1, 0), Some(0));

    let stepwise = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
    let _ = stepwise.set_font(2, 0);
    let _ = stepwise.set_text_scale(1.5, 0);
    let _ = stepwise.set_line_spacing(1.2, 0);
    let _ = stepwise.set_alignment(3, 0);

    assert_eq!(batched.page_count(), stepwise.page_count());
    assert!(batched.page_count() > 0);
    for page in 0..batched.page_count() {
        assert_eq!(
            render(&batched, page, 400, 600),
            render(&stepwise, page, 400, 600),
            "page {page} renders identically either way"
        );
    }
}

#[test]
fn the_default_alignment_matches_every_other_reflow_default() {
    // The shell's persisted `alignment` defaults to 0 and it only pushes the value to the core
    // when it is non-zero — so the core's untouched default has to *be* code 0 (Left), or a
    // fresh book renders in an alignment the Adjust sheet never claimed (#161 triage).
    let untouched = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
    let explicit_left = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
    let _ = explicit_left.set_alignment(0, 0);

    assert_eq!(untouched.page_count(), explicit_left.page_count());
    for page in 0..untouched.page_count() {
        assert_eq!(
            render(&untouched, page, 400, 600),
            render(&explicit_left, page, 400, 600),
            "page {page} of an untouched book is laid out Left, as code 0 promises"
        );
    }
    // ...and justifying really is a different layout, so the check above has teeth.
    let justified = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
    let _ = justified.set_alignment(1, 0);
    assert!(
        (0..untouched.page_count())
            .any(|p| render(&justified, p, 400, 600) != render(&untouched, p, 400, 600)),
        "Justify differs from the default, so the comparison above is meaningful"
    );
}

// ---- lazy per-chapter materialization (#161/#162) -----------------------------------------

/// A book with more chapters than the chapter cache holds, so eviction actually happens.
fn synthetic_book() -> EpubBackend {
    let chapters = (0..CHAPTER_CACHE * 4)
        .map(|c| {
            (0..6)
                .map(|p| {
                    parse_blocks(&format!(
                        "<p>Chapter {c} paragraph {p}. The morning was unremarkable and the \
                         light through the window was the light of every other morning, and \
                         he remembered none of it at all, not one moment of any of it.</p>"
                    ))
                    .remove(0)
                })
                .collect()
        })
        .collect();
    EpubBackend::from_chapters(chapters, vp(400, 600))
}

#[test]
fn building_the_index_materializes_no_pages() {
    // The whole point of persisting an index (#162): page count and TOC are answerable without
    // holding a single laid-out page.
    let b = synthetic_book();
    assert!(b.page_count() > 0);
    let _ = b.toc();
    assert!(
        b.chapter_pages.borrow().is_empty(),
        "counting pages retains none of them"
    );
}

#[test]
fn pages_render_identically_however_the_reader_arrives_at_them() {
    // Materializing a chapter at a time only works if the cache is transparent. Render every
    // page forwards, then again in reverse — which evicts and re-materializes constantly — and
    // demand the bytes match.
    let b = synthetic_book();
    let count = b.page_count();
    assert!(count > 0);
    let forwards: Vec<Vec<u8>> = (0..count).map(|p| render(&b, p, 400, 600)).collect();

    for page in (0..count).rev() {
        assert_eq!(
            render(&b, page, 400, 600),
            forwards[page],
            "page {page} renders the same when reached backwards"
        );
    }
    // ...and jumping around (TOC navigation) is no different.
    for page in [count - 1, 0, count / 2, count - 1, 1] {
        assert_eq!(render(&b, page, 400, 600), forwards[page], "page {page}");
    }
    // The rendered pages are not all the same image, so the comparison means something.
    assert!(forwards.iter().any(|p| *p != forwards[0]));
}

#[test]
fn the_chapter_cache_stays_bounded_however_far_the_reader_reads() {
    let b = synthetic_book();
    for page in 0..b.page_count() {
        let _ = render(&b, page, 400, 600);
        assert!(
            b.chapter_pages.borrow().len() <= CHAPTER_CACHE,
            "chapter cache exceeded its bound at page {page}"
        );
    }
    assert!(
        b.chapter_pages.borrow().len() > 1,
        "the reader crossed chapters, so more than one was cached"
    );
}

#[test]
fn a_relayout_drops_pages_cached_under_the_old_settings() {
    let b = synthetic_book();
    let before = render(&b, 0, 400, 600);
    assert!(!b.chapter_pages.borrow().is_empty(), "page 0 was cached");

    let _ = b.set_text_scale(2.0, 0);
    assert!(
        b.chapter_pages.borrow().is_empty(),
        "a repagination invalidates every cached page"
    );
    assert_ne!(
        render(&b, 0, 400, 600),
        before,
        "and the page re-renders at the new size rather than serving a stale one"
    );
}

// ---- persisted pagination (#162) ---------------------------------------------------------

/// What a [`FakeCache`] was asked to store, as `(layout key, per-chapter page counts)`.
type SaveLog = std::rc::Rc<RefCell<Vec<(String, Vec<usize>)>>>;

/// A cache that hands back whatever it is told to and records what it is given, so the
/// backend's own validation can be tested without going near a database. The record is shared
/// so a test can still read it after handing the cache to the backend.
struct FakeCache {
    answer: Option<Vec<usize>>,
    saved: SaveLog,
}

impl PaginationCache for FakeCache {
    fn load(&self, _key: &str) -> Option<Vec<usize>> {
        self.answer.clone()
    }
    fn save(&self, key: &str, chapter_pages: &[usize]) {
        self.saved
            .borrow_mut()
            .push((key.to_string(), chapter_pages.to_vec()));
    }
}

/// A cache answering `answer` to every load, plus the log of what it was asked to save.
fn fake(answer: Option<Vec<usize>>) -> (Box<FakeCache>, SaveLog) {
    let saved: SaveLog = std::rc::Rc::new(RefCell::new(Vec::new()));
    (
        Box::new(FakeCache {
            answer,
            saved: saved.clone(),
        }),
        saved,
    )
}

#[test]
fn a_stored_pagination_for_the_wrong_number_of_chapters_is_refused() {
    // The dangerous failure is not a crash, it is silently believing a pagination that
    // describes a different book: the reader would resume at a page that means nothing.
    let truth = synthetic_book().page_count();
    for wrong in [vec![1usize], vec![2; 500], Vec::new()] {
        let b = synthetic_book();
        b.set_pagination_cache(fake(Some(wrong.clone())).0);
        assert_eq!(
            b.page_count(),
            truth,
            "a {}-chapter pagination was refused and the book laid out instead",
            wrong.len()
        );
    }
}

#[test]
fn a_stored_pagination_of_the_right_shape_is_used_verbatim() {
    // The flip side: a pagination that *does* match is trusted without a layout pass, which is
    // the entire saving in #162.
    let b = synthetic_book();
    let chapters = b.chapter_count();
    b.set_pagination_cache(fake(Some(vec![7; chapters])).0);
    reset_layout_passes();
    assert_eq!(b.page_count(), 7 * chapters);
    assert_eq!(layout_passes(), 0, "served from the cache, not laid out");
}

#[test]
fn each_computed_pagination_is_stored_under_its_own_key() {
    let b = synthetic_book();
    let (cache, saved) = fake(None); // always a miss, so every layout is offered for storage
    b.set_pagination_cache(cache);

    let first = b.page_count();
    let _ = b.set_text_scale(2.0, 0);
    let second = b.page_count();

    let saved = saved.borrow();
    assert_eq!(saved.len(), 2, "both layouts were offered to the cache");
    assert_ne!(saved[0].0, saved[1].0, "under different keys");
    assert_eq!(saved[0].1.iter().sum::<usize>(), first);
    assert_eq!(saved[1].1.iter().sum::<usize>(), second);
    assert_eq!(
        saved[0].1.len(),
        b.chapter_count(),
        "one entry per chapter — the shape the load side validates"
    );
}

#[test]
fn every_layout_parameter_changes_the_key() {
    // If two different layouts share a key, one of them gets the other's page boundaries.
    let base = LayoutOpts::new(400.0, 600.0, 56.0);
    let key = layout_key(&base, 0, 12);

    let mut wider = base;
    wider.page_w = 401.0;
    let mut taller = base;
    taller.page_h = 601.0;
    let mut margined = base;
    margined.margin += 1.0;
    let mut bigger = base;
    bigger.font_px = 56.5;
    let mut looser = base;
    looser.line_spacing = 1.5;
    let mut gapped = base;
    gapped.para_gap += 1.0;
    let mut justified = base;
    justified.align = Align::Justify;

    for (label, other) in [
        ("width", layout_key(&wider, 0, 12)),
        ("height", layout_key(&taller, 0, 12)),
        ("margin", layout_key(&margined, 0, 12)),
        ("font size", layout_key(&bigger, 0, 12)),
        ("line spacing", layout_key(&looser, 0, 12)),
        ("paragraph gap", layout_key(&gapped, 0, 12)),
        ("alignment", layout_key(&justified, 0, 12)),
        ("face", layout_key(&base, 1, 12)),
        ("chapter count", layout_key(&base, 0, 13)),
    ] {
        assert_ne!(key, other, "{label} must change the layout key");
    }
    // The same layout keys the same, or nothing would ever hit.
    assert_eq!(key, layout_key(&LayoutOpts::new(400.0, 600.0, 56.0), 0, 12));
}

// ---- pagination progress + cancel (#161) --------------------------------------------------

/// What a [`SharedWatcher`] observed, as `(chapters done, total)` per report.
type Seen = std::rc::Rc<RefCell<Vec<(usize, usize)>>>;

/// Records what it is told and asks to cancel once `cancel_after` chapters have been laid out.
/// The record is shared, since the backend takes ownership of the watcher.
struct SharedWatcher {
    seen: Seen,
    cancel_after: Option<usize>,
}

impl PaginationProgress for SharedWatcher {
    fn chapter_done(&self, done: usize, total: usize) {
        self.seen.borrow_mut().push((done, total));
    }
    fn cancelled(&self) -> bool {
        self.cancel_after
            .is_some_and(|n| self.seen.borrow().len() >= n)
    }
}

fn watcher(cancel_after: Option<usize>) -> (Box<SharedWatcher>, Seen) {
    let seen: Seen = std::rc::Rc::new(RefCell::new(Vec::new()));
    (
        Box::new(SharedWatcher {
            seen: seen.clone(),
            cancel_after,
        }),
        seen,
    )
}

#[test]
fn progress_is_reported_once_per_chapter_and_counts_up_to_the_total() {
    let b = synthetic_book();
    let (w, seen) = watcher(None);
    b.set_pagination_progress(w);
    let _ = b.page_count(); // the first pagination reports, it just cannot be cancelled
    let chapters = b.chapter_count();

    let seen = seen.borrow();
    assert_eq!(seen.len(), chapters, "one report per chapter");
    assert!(
        seen.iter()
            .enumerate()
            .all(|(i, &(done, total))| done == i + 1 && total == chapters),
        "progress counts 1..=n against a fixed total: {seen:?}"
    );
}

#[test]
fn the_first_pagination_of_a_book_cannot_be_cancelled() {
    // There is nothing to fall back to, so a cancel here would leave a book that cannot be
    // read at all. It must run to completion regardless.
    let b = synthetic_book();
    let (w, _) = watcher(Some(1)); // asks to cancel as early as possible
    b.set_pagination_progress(w);
    assert_eq!(b.page_count(), synthetic_book().page_count());
}

#[test]
fn a_cancelled_relayout_leaves_the_reader_on_the_pagination_they_had() {
    let b = synthetic_book();
    let before_pages = b.page_count();
    let before_render = render(&b, 1, 400, 600);

    let (w, _) = watcher(Some(2)); // give up two chapters in
    b.set_pagination_progress(w);
    let page = b.set_text_scale(2.5, 1);

    assert_eq!(b.page_count(), before_pages, "the old pagination survives");
    assert_eq!(
        render(&b, 1, 400, 600),
        before_render,
        "and pages still render as they did"
    );
    assert_eq!(
        page,
        Some(0),
        "the reader is left in the chapter they were in"
    );
}

#[test]
fn a_cancelled_relayout_does_not_restart_itself_on_the_next_read() {
    // The subtle failure: the requested settings were changed before the pass began, so unless
    // they are rolled back the pagination looks stale forever and every later read re-runs the
    // work the reader just cancelled.
    let b = synthetic_book();
    let _ = b.page_count();
    let (w, _) = watcher(Some(2));
    b.set_pagination_progress(w);
    let _ = b.set_text_scale(2.5, 0);

    reset_layout_passes();
    let _ = b.page_count();
    let _ = b.page_count();
    let _ = render(&b, 0, 400, 600);
    assert_eq!(
        layout_passes(),
        0,
        "reading after a cancel does not restart the abandoned pagination"
    );
}

#[test]
fn a_cancel_restores_the_request_exactly_across_the_whole_settings_range() {
    // `revert_request_to_laid` has to leave the surviving pagination reading as *current*.
    // If the restored request differed from the compared one by even one bit, the pagination
    // would look stale forever and every read would re-run the cancelled work — so sweep the
    // full range of every setting rather than trusting one example.
    // One book: every iteration cancels and must land back on the same settled request, so the
    // book is in the identical state each time round.
    let b = synthetic_book();
    let _ = b.page_count(); // establish a pagination to fall back to
    let settled = b.current_request();

    let mut scale = MIN_SCALE;
    while scale <= MAX_SCALE {
        for &spacing in &[1.0f32, 1.4, 1.9, 2.5] {
            for align_code in 0..4 {
                let (w, _) = watcher(Some(1));
                b.set_pagination_progress(w);
                let _ = b.set_typography(scale, 1, spacing, align_code, 1, 0);

                assert_eq!(
                    b.current_request(),
                    settled,
                    "cancelling scale={scale} spacing={spacing} align={align_code} \
                     must restore the request bit-for-bit"
                );
                reset_layout_passes();
                let _ = b.page_count();
                assert_eq!(layout_passes(), 0, "and leave nothing looking stale");
            }
        }
        scale += 0.05;
    }
}

#[test]
fn a_setting_can_still_be_changed_after_a_cancel() {
    // Cancelling must not wedge the document: a later change still takes effect.
    let b = synthetic_book();
    let original = b.page_count();
    let (w, _) = watcher(Some(2));
    b.set_pagination_progress(w);
    let _ = b.set_text_scale(2.5, 0);
    assert_eq!(b.page_count(), original, "cancelled");

    // A fresh watcher that never cancels — the change now goes through.
    let (w, _) = watcher(None);
    b.set_pagination_progress(w);
    let _ = b.set_text_scale(2.5, 0);
    assert_ne!(
        b.page_count(),
        original,
        "a re-tried change is applied normally"
    );
}

#[test]
fn a_page_past_the_end_is_a_typed_error_not_a_panic() {
    // RR21-FR3 — the page index arrives from the shell, so it is not trusted.
    let b = synthetic_book();
    let count = b.page_count();
    let mut bytes = vec![0u8; 400 * 600 * 4];
    let mut buf = PixelBuffer::from_rgba(&mut bytes, 400, 600).unwrap();
    assert!(matches!(
        b.render_page(count, &mut buf),
        Err(CoreError::PageOutOfRange { .. })
    ));
    assert!(matches!(
        b.render_page(usize::MAX, &mut buf),
        Err(CoreError::PageOutOfRange { .. })
    ));
    assert!(b.page_chars(count).is_empty());
    assert!(b.page_pin(count).is_none());
}

/// An empty chapter still occupies exactly one page, so the chapter -> page mapping stays 1:1 and
/// a TOC entry pointing at it lands somewhere real. Front matter and section dividers produce these
/// in real books, and the index build, the materialization path and the persisted counts each have
/// their own `max(1)` — they have to agree.
#[test]
fn an_empty_chapter_occupies_exactly_one_page() {
    let text = || parse_blocks("<p>Some ordinary paragraph of prose that occupies a line.</p>");
    let chapters = vec![
        text(),
        Vec::new(), // wholly empty chapter
        text(),
        parse_blocks(""), // parses to nothing
        text(),
    ];
    let b = EpubBackend::from_chapters(chapters, vp(400, 600));

    assert_eq!(
        b.laid().chapter_start.len(),
        5,
        "every chapter gets a start page, empty or not"
    );
    // Each empty chapter contributes exactly one page, so consecutive starts differ by >= 1 and
    // the empty ones differ by exactly 1.
    let starts = b.laid().chapter_start.clone();
    assert_eq!(starts[2] - starts[1], 1, "the empty chapter is one page");
    assert_eq!(
        starts[4] - starts[3],
        1,
        "the blank-parse chapter is one page"
    );
    assert!(
        starts.windows(2).all(|w| w[1] > w[0]),
        "starts increase: {starts:?}"
    );

    // Every page in the book renders, including the empty ones — no panic, no out-of-range.
    for page in 0..b.page_count() {
        let mut bytes = vec![0u8; 400 * 600 * 4];
        let mut buf = PixelBuffer::from_rgba(&mut bytes, 400, 600).unwrap();
        b.render_page(page, &mut buf)
            .unwrap_or_else(|e| panic!("page {page}: {e:?}"));
    }
    // ...and each page resolves back to the chapter that owns it.
    for (chapter, &start) in starts.iter().enumerate() {
        assert_eq!(
            b.chapter_of(start),
            chapter,
            "page {start} belongs to chapter {chapter}"
        );
    }
}

/// A book of nothing but empty chapters still presents pages rather than an unreadable void.
#[test]
fn a_book_of_only_empty_chapters_still_has_a_page_per_chapter() {
    let b = EpubBackend::from_chapters(vec![Vec::new(), Vec::new(), Vec::new()], vp(400, 600));
    assert_eq!(b.page_count(), 3);
    assert_eq!(b.laid().chapter_start.clone(), vec![0, 1, 2]);
}

/// A book with no chapters at all still presents one page — `page_count()` of 0 would make every
/// page index out of range and leave the reader staring at nothing.
#[test]
fn a_book_with_no_chapters_still_presents_a_page() {
    let b = EpubBackend::from_chapters(Vec::new(), vp(400, 600));
    assert_eq!(b.page_count(), 1);
    let mut bytes = vec![0u8; 400 * 600 * 4];
    let mut buf = PixelBuffer::from_rgba(&mut bytes, 400, 600).unwrap();
    assert!(b.render_page(0, &mut buf).is_ok());
    assert!(b.page_chars(0).is_empty());
}

/// Reading position must resolve to the right chapter at every chapter boundary — the page index
/// is now derived from the index rather than from a flat page list, so the boundaries are exactly
/// where an off-by-one would hide (RR12-FR4).
#[test]
fn pins_resolve_to_their_own_chapter_at_every_boundary() {
    let b = synthetic_book();
    let starts = b.laid().chapter_start.clone();
    for (chapter, &start) in starts.iter().enumerate() {
        let Some(pin) = b.page_pin(start) else {
            continue;
        };
        assert_eq!(
            pin.chapter_index as usize, chapter,
            "the pin at page {start} claims chapter {} but that page starts chapter {chapter}",
            pin.chapter_index,
        );
        let resolved = b.pin_to_page(&pin);
        assert_eq!(
            b.chapter_of(resolved),
            chapter,
            "a pin taken at the start of chapter {chapter} resolved into chapter {}",
            b.chapter_of(resolved),
        );
        // The last page of the preceding chapter must NOT resolve into this one.
        if start > 0 {
            assert_eq!(b.chapter_of(start - 1), chapter - 1);
        }
    }
}

/// A corrupt or foreign pin is clamped into range rather than panicking (RR21-FR3) — pins arrive
/// from persisted state, so they are not trusted.
#[test]
fn an_out_of_range_pin_is_clamped_not_panicked() {
    let b = synthetic_book();
    let last = b.page_count() - 1;
    for chapter_index in [i32::MIN, -1, 0, i32::MAX] {
        for text_offset in [i32::MIN, -1, 0, i32::MAX] {
            let pin = PinPosition {
                chapter_index,
                chapter_id: String::new(),
                chapter_start: 0,
                chapter_end: i32::MAX,
                node_position: 0,
                text_offset,
                xpath: Vec::new(),
            };
            let page = b.pin_to_page(&pin);
            assert!(
                page <= last,
                "pin {chapter_index}/{text_offset} -> {page} > {last}"
            );
        }
    }
}

// ---- lazy chapter parsing (#186) ---------------------------------------------------------

/// The cost that no cache used to remove: opening a book parsed **every** chapter to blocks, in
/// full, on every open — for chapters the reader never looked at. On a device that was measured at
/// ~3.2s of a ~6s open, paid again on every reopen.
///
/// With a cached pagination there is nothing that needs the whole book, so only the chapter being
/// read should be parsed.
#[test]
fn a_cached_pagination_parses_only_the_chapter_being_read() {
    let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
    // A pagination the backend will accept: one entry per chapter, so the shape check passes.
    let cached = vec![1usize; b.chapter_count()];
    b.set_pagination_cache(fake(Some(cached)).0);

    reset_chapter_parses();
    let mut buf = vec![0u8; 400 * 600 * 4];
    let mut px = PixelBuffer::from_rgba(&mut buf, 400, 600).unwrap();
    b.render_page(0, &mut px).expect("render first page");

    assert_eq!(
        chapter_parses(),
        1,
        "only the chapter holding page 0 should have been parsed",
    );
}

/// The counterpart: building a pagination from scratch genuinely needs every chapter, so that path
/// must still parse them all. Without this a "fix" that simply never parsed would look correct here
/// and produce a book with no pages.
#[test]
fn building_a_pagination_still_parses_every_chapter() {
    let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
    let chapters = b.chapter_count();

    reset_chapter_parses();
    let pages = b.page_count();

    assert_eq!(chapter_parses(), chapters, "every chapter parsed");
    assert!(pages > 0, "and the book has pages");
}

/// Parsing is memoized: re-reading a chapter must not re-parse it.
#[test]
fn a_chapter_is_parsed_at_most_once() {
    let b = EpubBackend::open(SAMPLE.to_vec(), vp(400, 600)).unwrap();
    let _ = b.page_count(); // parses everything once
    reset_chapter_parses();

    let mut buf = vec![0u8; 400 * 600 * 4];
    let mut px = PixelBuffer::from_rgba(&mut buf, 400, 600).unwrap();
    for page in 0..b.page_count().min(4) {
        b.render_page(page, &mut px).expect("render");
    }

    assert_eq!(
        chapter_parses(),
        0,
        "already-parsed chapters are not re-parsed"
    );
}

/// #188 end to end: a book's stylesheet has to survive the whole path — out of the zip, through
/// EpubPackage, into chapter parsing, and onto the laid-out runs — or the title page still renders
/// hard left. Asserted on real geometry, with the reader on the default Left alignment.
#[test]
fn a_books_declared_styles_reach_the_laid_out_page() {
    let doc = EpubBackend::open(STYLED.to_vec(), vp(600, 800)).expect("styled epub opens");
    doc.parse_chapter(0);
    let chapters = doc.chapters.borrow();
    let blocks = chapters[0].blocks();

    // The <h1> title: centred and unbolded by the book.
    let Some(Block::Heading { style, .. }) = blocks.first() else {
        panic!("expected the title heading, got {blocks:?}")
    };
    assert_eq!(style.align, Some(Align::Center), "text-align was dropped");
    assert_eq!(style.bold, Some(false), "font-weight: normal was dropped");

    // The .c decorative paragraph: centred, and opting out of the first-line indent.
    let Some(Block::Paragraph { style, .. }) = blocks.get(1) else {
        panic!("expected the decorative paragraph, got {blocks:?}")
    };
    assert_eq!(style.align, Some(Align::Center));
    assert_eq!(style.indent, Some(false), "text-indent: 0% was dropped");

    // The ordinary prose paragraph: the book justifies it, which must NOT override the reader.
    let Some(Block::Paragraph { style, .. }) = blocks.get(2) else {
        panic!("expected the prose paragraph, got {blocks:?}")
    };
    assert_eq!(style.align, Some(Align::Justify), "parsed faithfully…");
    drop(chapters);

    // …and the layout stage declines to apply it: the prose stays flush left with its indent,
    // while the centred blocks above are inset.
    let opts = EpubBackend::opts_for(&doc.current_request());
    let pages = doc.lay_out_chapter_upto(0, &opts, usize::MAX).0;
    let first_x: Vec<f32> = pages[0]
        .lines
        .iter()
        .filter(|l| !l.runs.is_empty())
        .map(|l| l.runs[0].x)
        .collect();
    assert!(
        first_x.iter().any(|x| *x > 0.0),
        "nothing was centred: {first_x:?}"
    );
    let prose_x = *first_x.last().expect("a prose line");
    assert!(
        prose_x < 30.0,
        "book-declared justify shifted the reader's left-aligned prose: {prose_x}"
    );
}

/// The pagination cache key must move whenever anything changes how much content fits on a page,
/// or a cache written by an older build is replayed against a layout it does not describe and the
/// reader lands on stale page boundaries. #163 bumped it to v2, #188 to v3, #187 to v4.
///
/// This is a tripwire, not a tautology: it fails until whoever changed line fitting bumps the key.
#[test]
fn the_pagination_cache_version_tracks_changes_to_line_fitting() {
    let opts = LayoutOpts::new(600.0, 800.0, 16.0);
    assert!(
        layout_key(&opts, 0, 1).starts_with("v4|"),
        "{}",
        layout_key(&opts, 0, 1)
    );
}

/// #187 end to end: an illustration has to survive the whole path — out of the container, through
/// layout as a box with real height, and onto the rasterized page as pixels.
#[test]
fn an_illustration_is_laid_out_and_drawn_rather_than_labelled() {
    let doc =
        EpubBackend::open(ILLUSTRATED.to_vec(), vp(400, 600)).expect("illustrated epub opens");
    doc.parse_chapter(0);

    // The container surfaced the image, and its intrinsic size is readable.
    assert_eq!(doc.images.hrefs().len(), 1, "{:?}", doc.images.hrefs());
    assert_eq!(
        ImageSizer::size(&doc, "images/plate.png"),
        Some((120, 80)),
        "intrinsic size not resolved"
    );

    // Layout placed a real box, not a line of text.
    let opts = EpubBackend::opts_for(&doc.current_request());
    let pages = doc.lay_out_chapter_upto(0, &opts, usize::MAX).0;
    let placed = pages
        .iter()
        .flat_map(|p| &p.lines)
        .find_map(|l| l.image.as_ref())
        .expect("no image box was laid out");
    assert_eq!(placed.width, 120, "a small plate must not be upscaled");
    assert_eq!(placed.height, 80);

    // …and no `[image]` placeholder text survives anywhere on the page.
    // Runs are per word and long words may be soft-hyphenated, so join loosely and look for words.
    let text: String = pages
        .iter()
        .flat_map(|p| &p.lines)
        .flat_map(|l| &l.runs)
        .map(|r| r.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        !text.contains("[image"),
        "placeholder text remains: {text:?}"
    );
    assert!(
        text.contains("before") && text.contains("after"),
        "prose around the plate was lost: {text:?}"
    );
}

/// The rendered page must actually carry the picture's grey, not just reserve space for it.
#[test]
fn a_rendered_illustrated_page_carries_the_images_pixels() {
    let doc =
        EpubBackend::open(ILLUSTRATED.to_vec(), vp(400, 600)).expect("illustrated epub opens");
    let mut bytes = vec![0u8; 400 * 600 * 4];
    let mut buf = PixelBuffer::from_rgba(&mut bytes, 400, 600).unwrap();
    let greys = {
        doc.render_page(0, &mut buf).expect("renders");
        // The fixture's plate is a flat mid-grey; nothing else on the page draws that tone.
        bytes.chunks_exact(4).filter(|px| px[0] == 90).count()
    };
    assert!(
        greys > 5_000,
        "expected the 120x80 plate's pixels, found {greys}"
    );
}

// ---------------------------------------------------------------------------------------------
// #186 — materializing only as far as the page being read.
// ---------------------------------------------------------------------------------------------

/// A book whose single chapter runs to many pages — the shape #186 measured, where showing page 0
/// used to cost the whole chapter.
fn one_long_chapter(paras: usize) -> EpubBackend {
    let blocks = parse_blocks(
        &(0..paras)
            .map(|i| format!("<p>Paragraph {i}. Alpha bravo charlie delta echo foxtrot golf.</p>"))
            .collect::<String>(),
    );
    EpubBackend::from_chapters(vec![blocks], vp(400, 600))
}

/// The core of #186: rendering one page must not lay out the whole chapter.
#[test]
fn opening_a_page_lays_out_only_as_far_as_that_page() {
    let doc = one_long_chapter(400);
    let total = doc.page_count();
    assert!(total > 20, "need a long chapter, got {total} pages");

    reset_chapter_layouts();
    let mut bytes = vec![0u8; 400 * 600 * 4];
    let mut buf = PixelBuffer::from_rgba(&mut bytes, 400, 600).unwrap();
    doc.render_page(0, &mut buf).expect("renders");

    let laid = chapter_pages_laid();
    assert!(laid >= 1, "the page asked for must exist");
    assert!(
        laid * 4 < total,
        "laying out page 0 produced {laid} of {total} pages — not lazy"
    );
}

/// Reading forward must not re-lay a growing prefix on every turn. Extending a partial chapter
/// finishes it in one pass, so a chapter costs at most two passes however far it is read.
#[test]
fn reading_forward_costs_at_most_two_layout_passes_per_chapter() {
    let doc = one_long_chapter(400);
    let total = doc.page_count();
    reset_chapter_layouts();

    let mut bytes = vec![0u8; 400 * 600 * 4];
    for page in 0..total.min(12) {
        let mut buf = PixelBuffer::from_rgba(&mut bytes, 400, 600).unwrap();
        doc.render_page(page, &mut buf).expect("renders");
    }
    assert!(
        chapter_layouts() <= 2,
        "{} layout passes for one chapter read forward — quadratic re-layout",
        chapter_layouts()
    );
}

/// Laziness must not change what the reader sees, or a resumed position lands on different text.
///
/// Page 0 is the one actually served from a *partial* layout: asking for any later page flips the
/// chapter to a full extension pass, so pages 1 and 5 below exercise that pass instead. Both
/// matter — the prefix and the extension have to agree with a single full pass.
#[test]
fn pages_served_lazily_match_a_single_full_layout() {
    let opts = EpubBackend::opts_for(&one_long_chapter(1).current_request());

    let lazy = one_long_chapter(400);
    let full = one_long_chapter(400);
    let all = full.lay_out_chapter_upto(0, &opts, usize::MAX).0; // whole chapter in one pass

    for page in [0usize, 1, 5] {
        let got = lazy
            .with_page(page, |p, _| p.clone())
            .unwrap_or_else(|| panic!("page {page} missing"));
        assert_eq!(&got, &all[page], "page {page} differs from the full layout");
    }
}

/// A page past the end of the book is refused by the `total_pages` guard, before the chapter cache
/// is consulted — so this costs no pagination at all.
#[test]
fn a_page_past_the_end_of_the_book_never_paginates() {
    let doc = one_long_chapter(3);
    let total = doc.page_count();
    let mut bytes = vec![0u8; 400 * 600 * 4];
    let mut buf = PixelBuffer::from_rgba(&mut bytes, 400, 600).unwrap();
    doc.render_page(total - 1, &mut buf)
        .expect("last page renders");

    reset_chapter_layouts();
    assert!(doc.with_page(total + 5, |_, _| ()).is_none());
    assert_eq!(
        chapter_layouts(),
        0,
        "an out-of-range page must not paginate"
    );
}

/// The defensive arm: a page the pagination *index* says exists but the *layout* does not produce.
/// That can only happen if the two have diverged — a cache written by a different layout, say — and
/// the index guard cannot catch it, because as far as the index is concerned the page is in range.
/// Reached here by handing the reader a cache that overcounts.
///
/// What must not happen is treating the missing page as "not laid out that far yet" and paginating
/// the chapter again on every attempt.
#[test]
fn a_page_the_index_claims_but_the_layout_lacks_is_refused_without_relaying_out() {
    let real_total = one_long_chapter(40).page_count();

    // Same chapter count, so the cache is accepted, but one page more than the chapter has.
    let (cache, _saved) = fake(Some(vec![real_total + 1]));
    let doc = one_long_chapter(40);
    doc.set_pagination_cache(cache);
    assert_eq!(
        doc.page_count(),
        real_total + 1,
        "the overcounting cache should be in force"
    );

    // The phantom page: in range per the index, absent from the layout.
    let phantom = real_total;
    assert!(doc.with_page(phantom, |_, _| ()).is_none(), "phantom page");
    let diverged_before = diverged();
    reset_chapter_layouts();
    assert!(doc.with_page(phantom, |_, _| ()).is_none(), "still refused");
    assert_eq!(
        chapter_layouts(),
        0,
        "a known-complete chapter must not be laid out again for a page it does not have"
    );
    assert!(
        diverged() > diverged_before,
        "the refusal should come from the divergence guard, not an out-of-range page"
    );
}

/// Resuming deep into a long chapter, the prefix costs almost as much as the whole chapter, and the
/// extending pass that follows is then pure overhead — measured at +66% total work, plus tens of
/// megabytes transiently held. Past the halfway mark the chapter is laid out once, complete.
#[test]
fn a_deep_resume_lays_the_chapter_out_once_instead_of_twice() {
    let doc = one_long_chapter(400);
    let total = doc.page_count();
    assert!(total > 20, "need a long chapter, got {total}");

    // Deep resume: one pass that FINISHES the chapter, so no extending pass can follow. Asserting
    // completeness rather than a page count is deliberate — a bounded pass can overshoot its bound,
    // since the check sits between blocks, so counts do not separate "stopped early" from
    // "finished".
    reset_chapter_layouts();
    // Three quarters in: past the halfway threshold, but with a real tail left. At total-2 the
    // bounded pass would run to the end anyway, so it could not tell the two policies apart.
    let deep = total * 3 / 4;
    assert!(doc.with_page(deep, |_, _| ()).is_some());
    assert_eq!(chapter_layouts(), 1, "one pass");
    assert!(
        doc.chapter_pages.borrow()[0].complete,
        "a deep resume must lay the chapter out completely, not take a near-full prefix that a \
         later page then pays to extend"
    );
    reset_chapter_layouts();
    assert!(doc.with_page(deep + 1, |_, _| ()).is_some());
    assert_eq!(
        chapter_layouts(),
        0,
        "the chapter was already complete; turning the page must not lay out again"
    );

    // A shallow resume still takes the cheap prefix — the threshold must not disable laziness.
    let doc = one_long_chapter(400);
    reset_chapter_layouts();
    assert!(doc.with_page(0, |_, _| ()).is_some());
    assert!(
        chapter_pages_laid() * 4 < total,
        "page 0 produced {} of {total} pages — the threshold broke laziness",
        chapter_pages_laid()
    );
    assert!(
        !doc.chapter_pages.borrow()[0].complete,
        "a shallow resume should still leave the chapter partial"
    );
}

/// The partial layout must be released before the extending pass runs, or a deep-ish resume holds
/// the prefix and the full chapter at once.
#[test]
fn extending_a_chapter_does_not_hold_the_prefix_and_the_full_layout_at_once() {
    let doc = one_long_chapter(400);
    let total = doc.page_count();

    // A prefix short enough to stay under the halfway threshold, then extend past it.
    assert!(doc.with_page(1, |_, _| ()).is_some());
    let cached_pages: usize = doc
        .chapter_pages
        .borrow()
        .iter()
        .map(|c| c.pages.len())
        .sum();
    assert!(
        cached_pages < total,
        "expected a partial entry, got {cached_pages}"
    );

    assert!(doc.with_page(total - 1, |_, _| ()).is_some());
    let entries = doc.chapter_pages.borrow();
    assert_eq!(
        entries.len(),
        1,
        "the partial entry should have been replaced, not kept alongside"
    );
    assert!(entries[0].complete);
}

/// #194: selecting two columns must repaginate and keep the reader where they were — the same
/// contract every other reflow setting honours (RR12-FR4).
#[test]
fn selecting_two_columns_repaginates_and_keeps_the_chapter() {
    let doc = EpubBackend::open(SAMPLE.to_vec(), vp(1200, 1600)).expect("sample opens");
    let before = doc.page_count();
    assert!(before > 0);

    let moved = doc
        .set_columns(2, 0)
        .expect("a reflowable document supports columns");
    assert!(
        moved < doc.page_count(),
        "position must land inside the new pagination"
    );

    // Two columns hold more text per page, so a book cannot get longer by asking for them.
    assert!(
        doc.page_count() <= before,
        "{} pages in two columns vs {before} in one",
        doc.page_count()
    );

    // Back to one column returns the original pagination exactly.
    doc.set_columns(1, 0);
    assert_eq!(
        doc.page_count(),
        before,
        "single column should be as it was"
    );
}

/// The request is stored even when the page is too narrow to honour it, so a later font-size
/// change can bring it into effect without the reader asking again.
#[test]
fn a_column_request_survives_being_declined() {
    // A narrow viewport at the default text size cannot give two readable columns.
    let doc = EpubBackend::open(SAMPLE.to_vec(), vp(300, 900)).expect("sample opens");
    let single = doc.page_count();
    doc.set_columns(2, 0);
    assert_eq!(
        doc.page_count(),
        single,
        "a declined request must lay out exactly as single-column"
    );

    // Widen the page — the backend takes its viewport from the render buffer — and the stored
    // request takes effect with no further call.
    let mut bytes = vec![0u8; 1600 * 1200 * 4];
    {
        let mut buf = PixelBuffer::from_rgba(&mut bytes, 1600, 1200).unwrap();
        doc.render_page(0, &mut buf)
            .expect("renders at the new size");
    }
    assert!(
        doc.page_count() <= single,
        "the stored two-column request should apply once the page can take it"
    );
}

/// Out-of-range column counts are clamped rather than trusted — the value crosses JNI.
#[test]
fn an_out_of_range_column_count_is_clamped() {
    let doc = EpubBackend::open(SAMPLE.to_vec(), vp(1200, 1600)).expect("sample opens");
    let one = doc.page_count();
    doc.set_columns(2, 0);
    let two = doc.page_count();

    for absurd in [99, i32::MAX] {
        doc.set_columns(absurd, 0);
        assert_eq!(
            doc.page_count(),
            two,
            "clamped to the widest supported ({absurd})"
        );
    }
    for absurd in [0, -1, i32::MIN] {
        doc.set_columns(absurd, 0);
        assert_eq!(doc.page_count(), one, "clamped to single column ({absurd})");
    }
}
