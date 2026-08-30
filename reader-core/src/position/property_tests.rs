//! Property-based tests for [`PinPosition`] — the RR6-AC1 / RR17-FR3 acceptance criteria.
//!
//! The example tests next door pin the cases a human thought of. These pin the *laws*, over
//! arbitrary values `proptest` chooses and then shrinks to a minimal counterexample: a locator
//! survives JSON, its order is a real total order, and the compare key sorts exactly as `Ord`
//! does. That last one is the load-bearing claim — `compare_key` hand-rolls a biased zero-padded
//! encoding so positions can be used as cache/index keys, and an off-by-one in a field width or a
//! separator that sorts on the wrong side of a digit would put a page's cache entry in the wrong
//! place while every example test still passed.
//!
//! Ranges are chosen to hit the encoding's edges rather than to look realistic: `i32::MIN` and
//! `i32::MAX` are the biasing boundaries, and `position_int()` sums three of them in `i64`
//! precisely because an adversarial locator can push the sum past `i32`.

use proptest::prelude::*;

use super::{PageRange, PinPosition};

/// An arbitrary locator. `chapter_id` includes the compare key's separators (`\u{1}` and `,`) and
/// characters either side of them, so a key that leaked a separator into a field would be caught.
fn any_pin() -> impl Strategy<Value = PinPosition> {
    (
        any::<i32>(),
        prop::collection::vec(prop::char::range('\u{0}', '.'), 0..6),
        any::<i32>(),
        any::<i32>(),
        any::<i32>(),
        any::<i32>(),
        prop::collection::vec(any::<i32>(), 0..5),
    )
        .prop_map(
            |(
                chapter_index,
                id_chars,
                chapter_start,
                chapter_end,
                node_position,
                text_offset,
                xpath,
            )| PinPosition {
                chapter_index,
                chapter_id: id_chars.into_iter().collect(),
                chapter_start,
                chapter_end,
                node_position,
                text_offset,
                xpath,
            },
        )
}

/// A narrower generator whose fields cluster, so `prop_compose`d triples actually collide on the
/// leading keys and exercise the tie-break chain instead of always deciding on `chapter_index`.
fn clustered_pin() -> impl Strategy<Value = PinPosition> {
    (0i32..3, 0usize..3, 0i32..4, 0i32..4, 0i32..4, 0i32..4).prop_map(
        |(chapter_index, id, chapter_start, chapter_end, node_position, text_offset)| PinPosition {
            chapter_index,
            chapter_id: ["a", "b", "c"][id].to_string(),
            chapter_start,
            chapter_end,
            node_position,
            text_offset,
            xpath: vec![chapter_start, node_position],
        },
    )
}

proptest! {
    /// RR6-AC1: serialize then deserialize is the identity, and re-serializing is byte-identical.
    #[test]
    fn json_round_trip_is_lossless(p in any_pin()) {
        let json = p.to_json();
        let back = PinPosition::from_json(&json)
            .map_err(|e| TestCaseError::fail(format!("round-trip failed: {e}")))?;
        prop_assert_eq!(&back, &p);
        prop_assert_eq!(back.to_json(), json);
    }

    /// A locator that survives JSON must also keep its ordering identity: the round-tripped value
    /// compares `Equal` to the original and produces the same cache key.
    #[test]
    fn json_round_trip_preserves_order_and_key(p in any_pin()) {
        let back = PinPosition::from_json(&p.to_json())
            .map_err(|e| TestCaseError::fail(format!("round-trip failed: {e}")))?;
        prop_assert_eq!(back.cmp(&p), std::cmp::Ordering::Equal);
        prop_assert_eq!(back.compare_key(), p.compare_key());
    }

    /// RR6-AC1 (totality), reflexivity: every position equals itself.
    #[test]
    fn order_is_reflexive(p in any_pin()) {
        prop_assert_eq!(p.cmp(&p), std::cmp::Ordering::Equal);
    }

    /// RR6-AC1 (totality), antisymmetry: `a.cmp(b)` is the reverse of `b.cmp(a)`, always.
    #[test]
    fn order_is_antisymmetric(a in any_pin(), b in any_pin()) {
        prop_assert_eq!(a.cmp(&b), b.cmp(&a).reverse());
    }

    /// RR6-AC1 (totality), transitivity — over clustered values so the tie-break chain past
    /// `chapter_index` is the thing actually being exercised.
    #[test]
    fn order_is_transitive(a in clustered_pin(), b in clustered_pin(), c in clustered_pin()) {
        let mut v = [a, b, c];
        v.sort();
        prop_assert!(v[0] <= v[1]);
        prop_assert!(v[1] <= v[2]);
        prop_assert!(v[0] <= v[2]);
    }

    /// `Ord` agrees with `Eq`: `cmp == Equal` exactly when the positions are structurally equal.
    /// Without this a "total order" could still merge two distinct locators into one cache entry.
    #[test]
    fn order_equality_is_structural_equality(a in clustered_pin(), b in clustered_pin()) {
        prop_assert_eq!(a.cmp(&b) == std::cmp::Ordering::Equal, a == b);
    }

    /// RR6-FR3, the reason `compare_key` exists: its `str` ordering *is* `Ord`, so a position can
    /// be used directly as a lexicographically-sorted cache or index key.
    #[test]
    fn compare_key_order_matches_ord(a in any_pin(), b in any_pin()) {
        prop_assert_eq!(a.compare_key().cmp(&b.compare_key()), a.cmp(&b));
    }

    /// ...and over clustered values, where the decision falls past `chapter_index` into the padded
    /// integer fields and the xpath — the parts of the encoding most likely to be subtly wrong.
    #[test]
    fn compare_key_order_matches_ord_on_near_neighbours(
        a in clustered_pin(),
        b in clustered_pin(),
    ) {
        prop_assert_eq!(a.compare_key().cmp(&b.compare_key()), a.cmp(&b));
    }

    /// Equal positions produce equal keys (the other half of key/order agreement).
    #[test]
    fn equal_positions_share_a_compare_key(p in any_pin()) {
        prop_assert_eq!(p.clone().compare_key(), p.compare_key());
    }

    /// RR6-FR2/AC3: `contains` is exactly `start <= pos < end`. Half-open, so the page boundary
    /// belongs to the next page and no position lands on two pages.
    #[test]
    fn page_range_contains_is_half_open(
        a in clustered_pin(),
        b in clustered_pin(),
        p in clustered_pin(),
    ) {
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let range = PageRange::new(lo.clone(), hi.clone());
        prop_assert_eq!(range.contains(&p), p >= lo && p < hi);
        prop_assert!(!range.contains(&hi), "the end bound is exclusive");
    }

    /// An inverted range contains nothing — the documented defensive no-op.
    #[test]
    fn an_inverted_page_range_is_empty(a in clustered_pin(), b in clustered_pin(), p in clustered_pin()) {
        prop_assume!(a != b);
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        prop_assert!(!PageRange::new(hi, lo).contains(&p));
    }

    /// Hardening (RR21-FR3): arbitrary bytes are rejected as a corrupt locator, never a panic.
    /// The generator is deliberately dumb — most cases are not JSON at all, which is what a
    /// truncated or overwritten `resume_blob` on disk actually looks like.
    #[test]
    fn arbitrary_input_never_panics(s in ".{0,120}") {
        let _ = PinPosition::from_json(&s);
    }

    /// The same, over strings built from JSON's own alphabet, so the parser gets past its first
    /// byte often enough to exercise the field-level validation rather than only the lexer.
    #[test]
    fn json_shaped_input_never_panics(
        s in prop::collection::vec(
            prop::sample::select(vec!["{", "}", "[", "]", ",", ":", "\"", "chapter_id", "xpath", "0", "-1", "null"]),
            0..24,
        ),
    ) {
        let _ = PinPosition::from_json(&s.concat());
    }
}
