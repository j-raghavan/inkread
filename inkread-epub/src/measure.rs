//! Memoized text measurement — decorators over [`Metrics`] and [`Hyphenator`] (#161/#162).
//!
//! Laying out a book is dominated by measurement, not by line-breaking: greedy wrapping measures
//! every word, then re-measures the shrinking suffix of any word it has to split, and each
//! measurement walks the string glyph by glyph resolving a face and accumulating advances. Prose is
//! enormously repetitive, so nearly all of that work is recomputing widths already computed.
//!
//! These wrap a measurement source rather than caching inside it, so [`AbFont`](crate::AbFont)
//! stays a pure metrics implementation and the caller decides the cache's lifetime — which is what
//! keeps the cache correct. An [`AbFont`](crate::AbFont) freezes its fallback chain at
//! construction, so widths are stable for as long as the instance is, and a pagination pass holds
//! one instance. Wrap per pass; a face change builds a new [`AbFont`] and therefore a new cache.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::layout::{Hyphenator, Metrics};

/// The style dimension of a measurement: font size (bit pattern, so it can be hashed), bold,
/// italic. A layout pass uses a handful of these — body, headings, list markers.
type StyleKey = (u32, bool, bool);

/// Upper bound on memoized strings, across all styles. Word frequency is heavily skewed, so a cap
/// this size holds effectively the whole working set of a book while bounding the cache on a device
/// with a small heap. Past it, lookups still hit; only new entries stop being recorded.
const MAX_ENTRIES: usize = 1 << 16;

/// A [`Metrics`] that remembers the width of every string it has measured.
///
/// Keyed by style first so the inner map can be probed with a borrowed `&str` — a cache hit does
/// not allocate.
pub struct CachedMetrics<'a> {
    inner: &'a dyn Metrics,
    widths: RefCell<HashMap<StyleKey, HashMap<Box<str>, f32>>>,
    entries: RefCell<usize>,
}

impl<'a> CachedMetrics<'a> {
    /// Memoize `inner` for the lifetime of this wrapper.
    #[must_use]
    pub fn new(inner: &'a dyn Metrics) -> Self {
        Self {
            inner,
            widths: RefCell::new(HashMap::new()),
            entries: RefCell::new(0),
        }
    }
}

impl Metrics for CachedMetrics<'_> {
    fn advance(&self, text: &str, size_px: f32, bold: bool, italic: bool) -> f32 {
        let style = (size_px.to_bits(), bold, italic);
        if let Some(hit) = self
            .widths
            .borrow()
            .get(&style)
            .and_then(|by_text| by_text.get(text))
        {
            return *hit;
        }
        let width = self.inner.advance(text, size_px, bold, italic);
        let mut entries = self.entries.borrow_mut();
        if *entries < MAX_ENTRIES {
            *entries += 1;
            self.widths
                .borrow_mut()
                .entry(style)
                .or_default()
                .insert(text.into(), width);
        }
        width
    }
}

/// A [`Hyphenator`] that remembers the break opportunities it has computed. Only consulted for
/// words that overflow a line, so it sees far less traffic than [`CachedMetrics`] — but the words
/// that overflow are the long ones, which are the expensive ones to pattern-match.
pub struct CachedHyphenator<'a> {
    inner: &'a dyn Hyphenator,
    breaks: RefCell<HashMap<Box<str>, Vec<usize>>>,
}

impl<'a> CachedHyphenator<'a> {
    /// Memoize `inner` for the lifetime of this wrapper.
    #[must_use]
    pub fn new(inner: &'a dyn Hyphenator) -> Self {
        Self {
            inner,
            breaks: RefCell::new(HashMap::new()),
        }
    }
}

impl Hyphenator for CachedHyphenator<'_> {
    fn opportunities(&self, word: &str) -> Vec<usize> {
        if let Some(hit) = self.breaks.borrow().get(word) {
            return hit.clone();
        }
        let found = self.inner.opportunities(word);
        if self.breaks.borrow().len() < MAX_ENTRIES {
            self.breaks.borrow_mut().insert(word.into(), found.clone());
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{paginate_with, Align, LayoutOpts};
    use crate::parse_blocks;
    use crate::render::{AbFont, EnHyphenator};
    use std::cell::Cell;

    /// Prose chosen to exercise every measurement path the layout has: repeated function words
    /// (the cache's bread and butter), words long enough to need soft-hyphenation, an unspaced
    /// script that can only wrap at a UAX #14 opportunity, emphasis runs that key differently from
    /// body text, a heading at its own size, and a list with a synthetic marker.
    const PROSE: &str = "\
        <h1>Chapter One: An Incontrovertible Beginning</h1>\
        <p>The morning was unremarkable, and the light through the window was the light of every \
        other morning, and he remembered none of it. She said the word <em>extraordinarily</em> \
        and then said it again, and the <strong>incomprehensibility</strong> of the whole \
        arrangement settled over the room like dust.</p>\
        <p>counterrevolutionary antidisestablishmentarianism uncharacteristically \
        incomprehensibilities</p>\
        <p>日本語のテキストは単語の間に空白がないので、行の折り返しは別の規則に従います。</p>\
        <ul><li>the first of several items</li><li>the second, rather longer, of several items</li></ul>\
        <p>The the the the the of of of of and and and a a a to to in in it it that that was was.</p>";

    /// A metrics source that counts how often it is actually consulted.
    struct Counting {
        calls: Cell<usize>,
    }

    /// Passes measurements straight through to a real font, counting them — so a test can compare
    /// how much work the layout does with and without memoization, without timing anything.
    struct CountingProxy<'a> {
        inner: &'a dyn Metrics,
        calls: Cell<usize>,
    }

    impl Metrics for CountingProxy<'_> {
        fn advance(&self, text: &str, size_px: f32, bold: bool, italic: bool) -> f32 {
            self.calls.set(self.calls.get() + 1);
            self.inner.advance(text, size_px, bold, italic)
        }
    }

    impl Metrics for Counting {
        fn advance(&self, text: &str, size_px: f32, bold: bool, _italic: bool) -> f32 {
            self.calls.set(self.calls.get() + 1);
            // Deliberately style-sensitive so the cache can't conflate styles undetected.
            text.chars().count() as f32 * size_px * if bold { 2.0 } else { 1.0 }
        }
    }

    struct CountingHyphenator {
        calls: Cell<usize>,
    }

    impl Hyphenator for CountingHyphenator {
        fn opportunities(&self, word: &str) -> Vec<usize> {
            self.calls.set(self.calls.get() + 1);
            (1..word.len()).step_by(3).collect()
        }
    }

    #[test]
    fn a_repeated_measurement_is_taken_once_and_reported_identically() {
        let inner = Counting {
            calls: Cell::new(0),
        };
        let cached = CachedMetrics::new(&inner);
        let first = cached.advance("chapter", 16.0, false, false);
        for _ in 0..100 {
            assert_eq!(cached.advance("chapter", 16.0, false, false), first);
        }
        assert_eq!(inner.calls.get(), 1, "measured once, served 100 times");
    }

    #[test]
    fn every_component_of_the_style_key_is_distinguished() {
        let inner = Counting {
            calls: Cell::new(0),
        };
        let cached = CachedMetrics::new(&inner);
        // Same text, four different styles → four distinct measurements, none conflated.
        let a = cached.advance("word", 16.0, false, false);
        let b = cached.advance("word", 32.0, false, false);
        let c = cached.advance("word", 16.0, true, false);
        let d = cached.advance("word", 16.0, false, true);
        assert_eq!(inner.calls.get(), 4);
        assert_ne!(a, b, "size is part of the key");
        assert_ne!(a, c, "bold is part of the key");
        // Italic is keyed even where this source ignores it; re-reading must stay consistent.
        assert_eq!(cached.advance("word", 16.0, false, true), d);
        assert_eq!(inner.calls.get(), 4, "all four were already cached");
    }

    #[test]
    fn caching_never_changes_a_reported_width() {
        let inner = Counting {
            calls: Cell::new(0),
        };
        let cached = CachedMetrics::new(&inner);
        for text in ["", "a", "the", "extraordinarily", "  ", "日本語", "x y z"] {
            for &size in &[8.0f32, 16.0, 56.5] {
                for &bold in &[false, true] {
                    assert_eq!(
                        cached.advance(text, size, bold, false),
                        inner.advance(text, size, bold, false),
                        "{text:?} @ {size} bold={bold}"
                    );
                }
            }
        }
    }

    #[test]
    fn hyphenation_opportunities_are_computed_once_per_word() {
        let inner = CountingHyphenator {
            calls: Cell::new(0),
        };
        let cached = CachedHyphenator::new(&inner);
        let first = cached.opportunities("extraordinarily");
        for _ in 0..50 {
            assert_eq!(cached.opportunities("extraordinarily"), first);
        }
        assert_eq!(
            cached.opportunities("different"),
            inner.opportunities("different")
        );
        assert_eq!(
            inner.calls.get(),
            3,
            "one for the repeated word, two for the direct comparison of the other"
        );
    }

    /// THE gate for this optimization: memoizing measurement must not move a single glyph. Run
    /// over real prose and a real font, across the viewport / size / spacing / alignment matrix the
    /// reader can actually produce, comparing the full `Vec<Page>` — every line position, every run
    /// x, every source anchor — not just the page count.
    #[test]
    fn memoized_pagination_is_identical_to_unmemoized_pagination() {
        let font = AbFont::default_font();
        let hyph = EnHyphenator::new();
        let blocks = parse_blocks(PROSE);
        let mut compared = 0;
        for &(w, h) in &[(400.0f32, 600.0f32), (1404.0, 1872.0), (300.0, 420.0)] {
            for &font_px in &[14.0f32, 24.0, 56.0] {
                for &line_spacing in &[1.0f32, 1.4, 2.5] {
                    for align in [Align::Left, Align::Justify, Align::Center, Align::Right] {
                        let mut opts = LayoutOpts::new(w, h, font_px);
                        opts.line_spacing = line_spacing;
                        opts.align = align;

                        let plain = paginate_with(&blocks, &opts, &font, &hyph);
                        let memoized = paginate_with(
                            &blocks,
                            &opts,
                            &CachedMetrics::new(&font),
                            &CachedHyphenator::new(&hyph),
                        );
                        assert_eq!(
                            plain, memoized,
                            "{w}x{h} @ {font_px}px spacing {line_spacing} align {align:?}"
                        );
                        assert!(!plain.is_empty(), "the fixture lays out to something");
                        compared += 1;
                    }
                }
            }
        }
        assert_eq!(compared, 108, "the whole matrix ran");
    }

    /// The point of the cache, asserted as a ratio of work done rather than as a wall-clock time —
    /// so it means the same thing on a CI box and on a device.
    ///
    /// Laid out the way the backends do it: many chapters, one cache spanning all of them. That
    /// sharing is most of the benefit, because a book's vocabulary is nearly all established by the
    /// first chapter and every later one is measuring words already measured.
    #[test]
    fn memoization_removes_most_of_the_measurement_work() {
        const CHAPTERS: usize = 12;
        let font = AbFont::default_font();
        let hyph = EnHyphenator::new();
        let blocks = parse_blocks(PROSE);
        let opts = LayoutOpts::new(1404.0, 1872.0, 56.0);

        let uncached = CountingProxy {
            inner: &font,
            calls: Cell::new(0),
        };
        for _ in 0..CHAPTERS {
            let _ = paginate_with(&blocks, &opts, &uncached, &hyph);
        }

        let counted = CountingProxy {
            inner: &font,
            calls: Cell::new(0),
        };
        let shared = CachedMetrics::new(&counted);
        let shared_hyph = CachedHyphenator::new(&hyph);
        for _ in 0..CHAPTERS {
            let _ = paginate_with(&blocks, &opts, &shared, &shared_hyph);
        }

        let (before, after) = (uncached.calls.get(), counted.calls.get());
        assert!(before > 0 && after > 0, "both paths measured something");
        assert!(
            after * 5 <= before,
            "memoization removes at least 80% of the measurements reaching the font: \
             {before} → {after}"
        );
    }

    #[test]
    fn a_full_cache_still_serves_hits_and_still_measures_correctly() {
        let inner = Counting {
            calls: Cell::new(0),
        };
        let cached = CachedMetrics::new(&inner);
        // Fill past the cap, then confirm both halves still report the true width.
        for i in 0..(MAX_ENTRIES + 1_000) {
            cached.advance(&format!("w{i}"), 16.0, false, false);
        }
        assert_eq!(*cached.entries.borrow(), MAX_ENTRIES, "the cap holds");
        // An entry that made it in (served from cache) and one that did not (measured afresh).
        assert_eq!(
            cached.advance("w0", 16.0, false, false),
            inner.advance("w0", 16.0, false, false)
        );
        let overflowed = format!("w{}", MAX_ENTRIES + 500);
        assert_eq!(
            cached.advance(&overflowed, 16.0, false, false),
            inner.advance(&overflowed, 16.0, false, false)
        );
    }
}
