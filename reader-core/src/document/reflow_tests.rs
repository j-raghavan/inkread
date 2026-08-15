//! Tests for the reflowable EPUB backend (RR2/RR4/RR12; #161/#162), split out to keep
//! `reflow.rs` nearer the size guideline. Included via `#[path]` so `super::*` resolves to
//! the reflow module.

use super::*;
use crate::render::Viewport;

const SAMPLE: &[u8] = include_bytes!("../../tests/fixtures/sample.epub");

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
    let page = b.set_typography(1.25, 1, 1.7, 2, 0);
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
    assert_eq!(batched.set_typography(1.5, 2, 1.2, 3, 0), Some(0));

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
    let chapters = b.chapters.len();
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
        b.chapters.len(),
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
    let chapters = b.chapters.len();

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
                let _ = b.set_typography(scale, 1, spacing, align_code, 0);

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
