//! `PinPosition` + `PageRange` — the reflow-stable position locator (RR6, M2).
//!
//! A reflowable document has no fixed page number: the same word lands on a different rendered page
//! when the font size, margins, or viewport change. A [`PinPosition`] anchors to **content**, not
//! pixels — a chapter plus a DOM/byte offset and an XPath — so it re-resolves to the *same words*
//! across a re-layout (RR6-FR1; the headline re-anchoring guarantee of RR9/RR12).
//!
//! This module is the foundation `RR8` (pagination), `RR11` (selection/TOC), and `RR12`
//! (annotations, reading-position resume, the EPUB Digest anchor) build on. It is **pure and
//! dependency-free** (serde only) so it is fully host-tested without the reflow engine: the type,
//! its **total order** (reading order), a lexicographically-comparable **compare key** for use as a
//! cache/index key, and lossless **JSON** round-trip.
//!
//! Clean-room (RR18): the field set and ordering semantics mirror the documented NeoReader locator
//! contract; no decompiled code is reused.

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, CoreResult};

/// Bias that maps the full `i32` range onto a non-negative `i64` so a fixed-width zero-padded
/// decimal encoding sorts lexicographically the same way the integer sorts numerically
/// (`i32::MIN + BIAS == 0`, `i32::MAX + BIAS == u32::MAX`).
const PIN_INT_BIAS: i64 = 2_147_483_648; // 2^31
/// Width of `u32::MAX` (`4294967295`) in decimal digits — the fixed field width after biasing.
const PIN_INT_WIDTH: usize = 10;

/// Bias for `position_int()`, whose floor (`chapter_start + node_position + text_offset`, each i32)
/// can reach `3 * i32::MIN`. Adding `3 * 2^31` keeps the whole reachable range non-negative.
const PIN_INT64_BIAS: i64 = 3 * PIN_INT_BIAS;
/// Width of the biased `position_int()` (`i32::MAX + 3*2^31` ≈ 8.6e9 → 10 digits; 11 for headroom).
const PIN_INT64_WIDTH: usize = 11;

/// Field separator in [`PinPosition::compare_key`]. `0x01` sorts below every digit and the xpath
/// element separator, so a shorter xpath prefix sorts before a longer one (matching `Vec` order).
const KEY_FIELD_SEP: char = '\u{1}';
/// Separator between xpath elements in the compare key (sorts above [`KEY_FIELD_SEP`]).
const KEY_XPATH_SEP: char = ',';

/// A reflow-stable locator into a document (RR6-FR1).
///
/// `position_int()` (= `chapter_start + node_position + text_offset`, clamped to `chapter_end`) is
/// the chapter-relative reading offset used for ordering and progress; `xpath` re-anchors the exact
/// node across a re-layout. Two positions order by `(chapter_index, position_int())` first
/// (RR6-AC2); the remaining fields are deterministic tie-breaks that make the order **total** and
/// consistent with structural equality.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PinPosition {
    /// Zero-based spine/chapter index — the primary ordering key.
    pub chapter_index: i32,
    /// Stable chapter identifier (e.g. the OPF spine id / href), preserved across re-pagination.
    pub chapter_id: String,
    /// Document-relative offset where this chapter begins (for `position_int`/progress).
    pub chapter_start: i32,
    /// Document-relative offset where this chapter ends (the upper clamp for `position_int`).
    pub chapter_end: i32,
    /// Offset of the anchored node within the chapter.
    pub node_position: i32,
    /// Character offset within the anchored node.
    pub text_offset: i32,
    /// DOM path (child-index chain from the chapter root) used to re-anchor across re-layouts.
    pub xpath: Vec<i32>,
}

impl PinPosition {
    /// The chapter-relative reading offset: `chapter_start + node_position + text_offset`, clamped
    /// to `chapter_end` (RR6-FR1). Computed in `i64` so the sum can't overflow `i32`.
    #[must_use]
    pub fn position_int(&self) -> i64 {
        let sum = i64::from(self.chapter_start)
            + i64::from(self.node_position)
            + i64::from(self.text_offset);
        sum.min(i64::from(self.chapter_end))
    }

    /// The ordering tuple: `(chapter_index, position_int())` first (RR6-AC2), then every remaining
    /// field so the order is **total** and `cmp == Equal` iff the positions are structurally equal
    /// (the `Ord`/`Eq` contract). The compare key (RR6-FR3) encodes this same field order.
    fn order_cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.chapter_index, self.position_int())
            .cmp(&(other.chapter_index, other.position_int()))
            .then_with(|| self.node_position.cmp(&other.node_position))
            .then_with(|| self.text_offset.cmp(&other.text_offset))
            .then_with(|| self.chapter_start.cmp(&other.chapter_start))
            .then_with(|| self.chapter_end.cmp(&other.chapter_end))
            .then_with(|| self.xpath.cmp(&other.xpath))
            .then_with(|| self.chapter_id.cmp(&other.chapter_id))
    }

    /// A lexicographically-comparable string whose `str` ordering equals [`Ord`] (RR6-FR3), so
    /// positions can be used directly as cache/index keys. Ints are biased + zero-padded to a fixed
    /// width; the xpath uses a separator that preserves `Vec`'s prefix-is-less rule.
    #[must_use]
    pub fn compare_key(&self) -> String {
        // Field order MUST match `order_cmp` exactly so the lexicographic key order equals `Ord`.
        let mut key = String::new();
        key.push_str(&pad_int(self.chapter_index));
        key.push(KEY_FIELD_SEP);
        // `position_int()` is i64 (its floor can sum below `i32::MIN` for adversarial input), so it is
        // encoded at full i64 width — casting to i32 here would wrap and disagree with `order_cmp`.
        key.push_str(&pad_i64(self.position_int()));
        key.push(KEY_FIELD_SEP);
        for v in [
            self.node_position,
            self.text_offset,
            self.chapter_start,
            self.chapter_end,
        ] {
            key.push_str(&pad_int(v));
            key.push(KEY_FIELD_SEP);
        }
        for (i, seg) in self.xpath.iter().enumerate() {
            if i > 0 {
                key.push(KEY_XPATH_SEP);
            }
            key.push_str(&pad_int(*seg));
        }
        key.push(KEY_FIELD_SEP);
        key.push_str(&self.chapter_id);
        key
    }

    /// Serialize to JSON (RR6-FR4). Infallible for this primitive-only shape.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("PinPosition serializes infallibly")
    }

    /// Parse from JSON, validating at the boundary (RR21-FR3): a malformed locator blob (e.g. a
    /// corrupt `resume_blob`) is reported as a corrupt document rather than panicking.
    pub fn from_json(s: &str) -> CoreResult<Self> {
        serde_json::from_str(s)
            .map_err(|e| CoreError::CorruptDocument(format!("pin-position: {e}")))
    }
}

impl Ord for PinPosition {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.order_cmp(other)
    }
}

impl PartialOrd for PinPosition {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// A page as a **half-open** range of positions `[start, end)` (RR6-FR2). A reflow page is defined
/// by where it begins and where the next page begins, so the start is inclusive and the end is
/// exclusive — adjacent pages tile the document without overlap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageRange {
    /// First position on the page (inclusive).
    pub start: PinPosition,
    /// First position of the *next* page (exclusive).
    pub end: PinPosition,
}

impl PageRange {
    /// Build a range. `start` is expected to order at or before `end`; an inverted range simply
    /// contains nothing (callers paginate in reading order so this is a defensive no-op).
    #[must_use]
    pub fn new(start: PinPosition, end: PinPosition) -> Self {
        Self { start, end }
    }

    /// Whether `pos` falls on this page: `start <= pos < end` (RR6-FR2/AC3).
    #[must_use]
    pub fn contains(&self, pos: &PinPosition) -> bool {
        *pos >= self.start && *pos < self.end
    }
}

/// Bias + zero-pad an `i32` to a fixed width so decimal string order matches numeric order across
/// the whole `i32` range (negatives included).
fn pad_int(v: i32) -> String {
    format!(
        "{:0width$}",
        i64::from(v) + PIN_INT_BIAS,
        width = PIN_INT_WIDTH
    )
}

/// Bias + zero-pad an i64 `position_int()` so its decimal string order matches numeric order across
/// its full reachable range (its floor can sum down to `3 * i32::MIN`).
fn pad_i64(v: i64) -> String {
    format!("{:0width$}", v + PIN_INT64_BIAS, width = PIN_INT64_WIDTH)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A terse builder so tests read as reading-order intent, not field soup.
    fn pin(chapter: i32, node: i32, offset: i32, xpath: &[i32]) -> PinPosition {
        PinPosition {
            chapter_index: chapter,
            chapter_id: format!("ch{chapter}"),
            chapter_start: 0,
            chapter_end: i32::MAX,
            node_position: node,
            text_offset: offset,
            xpath: xpath.to_vec(),
        }
    }

    #[test]
    fn position_int_sums_and_clamps_to_chapter_end() {
        let p = PinPosition {
            chapter_start: 100,
            node_position: 20,
            text_offset: 5,
            chapter_end: i32::MAX,
            ..pin(0, 0, 0, &[])
        };
        assert_eq!(p.position_int(), 125);

        let clamped = PinPosition {
            chapter_end: 110,
            ..p
        };
        assert_eq!(
            clamped.position_int(),
            110,
            "clamped to chapter_end (RR6-FR1)"
        );
    }

    #[test]
    fn position_int_cannot_overflow_i32() {
        // chapter_start + node + offset would overflow i32 if summed in i32; i64 keeps it sound.
        let p = PinPosition {
            chapter_start: i32::MAX,
            node_position: i32::MAX,
            text_offset: i32::MAX,
            chapter_end: i32::MAX,
            ..pin(0, 0, 0, &[])
        };
        assert_eq!(p.position_int(), i64::from(i32::MAX));
    }

    #[test]
    fn orders_by_chapter_then_offset() {
        // RR6-AC2: chapter_index dominates; within a chapter, the document offset orders.
        let a = pin(0, 10, 0, &[1]);
        let b = pin(0, 50, 0, &[1]);
        let c = pin(1, 0, 0, &[1]);
        assert!(a < b, "earlier offset, same chapter, sorts first");
        assert!(b < c, "earlier chapter sorts first regardless of offset");
        assert!(a < c);
    }

    #[test]
    fn ordering_is_a_total_order() {
        // RR6 DoD: ordering totality. A representative cross-product must satisfy trichotomy and
        // transitivity, and cmp==Equal must coincide with structural equality.
        let mut all = Vec::new();
        for chapter in 0..3 {
            for node in [0, 5, 5] {
                for offset in [0, 3] {
                    for xpath in [vec![1], vec![1, 2], vec![2]] {
                        all.push(pin(chapter, node, offset, &xpath));
                    }
                }
            }
        }
        for a in &all {
            for b in &all {
                let ord = a.cmp(b);
                assert_eq!(
                    ord == std::cmp::Ordering::Equal,
                    a == b,
                    "cmp Equal iff structurally equal"
                );
                assert_eq!(ord, b.cmp(a).reverse(), "antisymmetry");
                for c in &all {
                    if a <= b && b <= c {
                        assert!(a <= c, "transitivity");
                    }
                }
            }
        }
    }

    #[test]
    fn compare_key_order_matches_cmp() {
        // RR6-FR3: the string compare key sorts identically to the type's reading order, so it can
        // be used as a cache/index key. Includes negative coordinates to exercise the bias.
        let mut positions = vec![
            pin(2, 0, 0, &[1]),
            pin(0, 5, 0, &[1, 2]),
            pin(0, 5, 0, &[1]),
            pin(0, 5, 0, &[2]),
            pin(0, 0, 0, &[1]),
            pin(1, 0, 0, &[]),
            PinPosition {
                node_position: -7,
                ..pin(0, 0, 0, &[3])
            },
            // Adversarial: a position_int() that underflows i32 (sum < i32::MIN). Encoded at full i64
            // width, so its compare_key still sorts consistently with Ord (regression for the old
            // `as i32` cast that wrapped).
            PinPosition {
                node_position: i32::MIN,
                text_offset: -1,
                ..pin(0, 0, 0, &[1])
            },
            PinPosition {
                node_position: i32::MIN,
                text_offset: -100,
                ..pin(0, 0, 0, &[1])
            },
        ];
        let mut by_cmp = positions.clone();
        by_cmp.sort();
        positions.sort_by_key(PinPosition::compare_key);
        assert_eq!(positions, by_cmp, "compare_key order must equal Ord");
    }

    #[test]
    fn json_round_trip_is_lossless_and_equal() {
        // RR6-AC1: serialize → deserialize is byte-identical and compares equal.
        let p = PinPosition {
            chapter_index: 4,
            chapter_id: "OEBPS/ch04.xhtml".to_string(),
            chapter_start: 12_000,
            chapter_end: 18_500,
            node_position: 240,
            text_offset: 17,
            xpath: vec![0, 3, 1, 9],
        };
        let json = p.to_json();
        let back = PinPosition::from_json(&json).expect("round-trips");
        assert_eq!(back, p);
        assert_eq!(back.to_json(), json, "re-serialization is byte-identical");
    }

    #[test]
    fn from_json_rejects_garbage_without_panicking() {
        let err = PinPosition::from_json("{not json").unwrap_err();
        assert!(matches!(err, CoreError::CorruptDocument(_)));
    }

    #[test]
    fn page_range_serializes_to_the_selection_pins_wire_shape() {
        // nativeSelectionPins reuses PageRange's JSON for the digest anchor (#46), which the shell
        // consumes as `{"start":{…},"end":{…}}`. Pin that wire shape + a lossless round trip so the
        // JNI contract can't drift silently.
        let range = PageRange::new(pin(2, 100, 5, &[0, 3]), pin(2, 140, 9, &[0, 3]));
        let json = serde_json::to_string(&range).expect("PageRange serializes");
        assert!(
            json.contains("\"start\"") && json.contains("\"end\""),
            "wire shape carries start+end: {json}"
        );
        let back: PageRange = serde_json::from_str(&json).expect("round-trips");
        assert_eq!(back, range, "selection-pins JSON round-trips losslessly");
    }

    #[test]
    fn page_range_contains_is_half_open() {
        // RR6-AC3: start inclusive, end exclusive; before/after excluded.
        let start = pin(0, 10, 0, &[1]);
        let end = pin(0, 30, 0, &[1]);
        let range = PageRange::new(start.clone(), end.clone());

        assert!(range.contains(&start), "start is inclusive");
        assert!(range.contains(&pin(0, 20, 0, &[1])), "interior");
        assert!(!range.contains(&end), "end is exclusive");
        assert!(!range.contains(&pin(0, 5, 0, &[1])), "before");
        assert!(!range.contains(&pin(1, 0, 0, &[1])), "after (next chapter)");
    }

    #[test]
    fn empty_range_contains_nothing() {
        let p = pin(0, 10, 0, &[1]);
        let range = PageRange::new(p.clone(), p.clone());
        assert!(!range.contains(&p));
    }
}
