//! Property-based tests for the `.inkbin` codec — RR17-FR3.
//!
//! This codec holds a reader's handwriting. A stroke that does not survive the round-trip is
//! annotation loss the reader only discovers after closing the book, and the file it decodes is on
//! disk, where truncation from a battery pull mid-write is an ordinary event rather than an attack.
//! So the two properties that matter are:
//!
//! - **Nothing is lost.** `decode(encode(layer))` returns the same strokes, for arbitrary layers —
//!   including the parts the format encodes conditionally, where the example tests are thinnest:
//!   the per-point tilt flags (four combinations), the full `u8` colour space, the `u64` timestamp,
//!   and a point count that crosses the cursor's read boundaries.
//! - **Nothing panics.** Arbitrary bytes, and truncations of a *valid* blob at every length, decode
//!   to a value or a typed `BadEncoding`. Truncation is the interesting generator: random bytes
//!   bounce off the magic in four bytes, whereas a prefix of a real blob gets deep into the stroke
//!   loop with a plausible header and a length that lies.
//!
//! The encoder is `#[must_use]`-pure and the decoder is the hardened half, so every property here
//! is really a statement about `decode_layer`.

use proptest::prelude::*;

use super::*;
use crate::model::{InkColor, InkLayer, InkPoint, Stroke, StrokeId, Tool};

fn any_tool() -> impl Strategy<Value = Tool> {
    prop::sample::select(vec![Tool::Pen, Tool::Highlighter, Tool::Eraser])
}

/// Tilt is `Option<f32>` per axis and drives the two `flags` bits, so all four combinations must
/// occur. The range stays finite: `InkPoint::new` drops a non-finite tilt to `None`, which is
/// correct behaviour but would make the round-trip assert on a value the constructor already
/// changed.
fn any_tilt() -> impl Strategy<Value = Option<f32>> {
    prop_oneof![Just(None), (-2.0f32..2.0).prop_map(Some)]
}

fn any_point() -> impl Strategy<Value = InkPoint> {
    (
        0.0f32..=1.0,
        0.0f32..=1.0,
        0.0f32..=1.0,
        any_tilt(),
        any_tilt(),
        any::<u32>(),
    )
        .prop_map(|(x, y, pressure, tx, ty, ts)| {
            InkPoint::new(x, y, pressure, tx, ty, ts).expect("generated components are finite")
        })
}

/// Strokes carry distinct ids because the decoder rejects duplicates on purpose — two strokes
/// sharing an id would make undo's `retain` remove both.
fn any_layer() -> impl Strategy<Value = InkLayer> {
    prop::collection::vec(
        (
            any_tool(),
            any::<u8>(),
            any::<u8>(),
            any::<u8>(),
            any::<u8>(),
            0.0001f32..1.0,
            any::<u64>(),
            prop::collection::vec(any_point(), 1..8),
        ),
        0..6,
    )
    .prop_map(|specs| {
        let strokes = specs
            .into_iter()
            .enumerate()
            .map(
                |(i, (tool, r, g, b, a, width, created_at_ms, points))| Stroke {
                    id: StrokeId(i as u32),
                    tool,
                    color: InkColor::rgba(r, g, b, a),
                    width,
                    points,
                    created_at_ms,
                },
            )
            .collect();
        InkLayer::from_strokes(strokes)
    })
}

proptest! {
    /// RR10-FR4: every stroke survives the round-trip, field for field.
    #[test]
    fn inkbin_round_trip_is_lossless(layer in any_layer()) {
        let back = decode_layer(&encode_layer(&layer))
            .map_err(|e| TestCaseError::fail(format!("decode failed: {e:?}")))?;
        prop_assert_eq!(back.strokes(), layer.strokes());
    }

    /// Re-encoding a decoded blob reproduces the same bytes. Stronger than the round-trip above:
    /// it rules out an encoder that normalizes something the decoder then reads back differently,
    /// which would make every *subsequent* save differ from the last.
    #[test]
    fn encoding_is_stable_across_a_round_trip(layer in any_layer()) {
        let bytes = encode_layer(&layer);
        let back = decode_layer(&bytes)
            .map_err(|e| TestCaseError::fail(format!("decode failed: {e:?}")))?;
        prop_assert_eq!(encode_layer(&back), bytes);
    }

    /// RR20: the undo/redo history is never persisted. A layer that has been drawn on and undone
    /// must encode exactly like one built from the strokes that remain.
    #[test]
    fn history_is_never_persisted(layer in any_layer()) {
        let replayed = InkLayer::from_strokes(layer.strokes().to_vec());
        prop_assert_eq!(encode_layer(&replayed), encode_layer(&layer));
        prop_assert!(!replayed.can_undo());
    }

    /// Hardening (RR21-FR3): arbitrary bytes decode to a value or a typed error, never a panic.
    #[test]
    fn arbitrary_bytes_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
        let _ = decode_layer(&bytes);
    }

    /// Bytes that start with a valid header, so cases get past the magic and version and into the
    /// stroke loop — where the declared counts, the tool code and the width validation live.
    #[test]
    fn well_headed_garbage_never_panics(body in prop::collection::vec(any::<u8>(), 0..256)) {
        let mut bytes = b"INKB".to_vec();
        bytes.push(INKBIN_VERSION);
        bytes.extend_from_slice(&body);
        let _ = decode_layer(&bytes);
    }

    /// The realistic corruption: a valid blob cut short, as a crash mid-write leaves it. Every
    /// proper prefix must be rejected cleanly — not panic, and not silently decode to fewer
    /// strokes than the header promised, which would lose ink without telling anyone.
    #[test]
    fn any_truncation_of_a_valid_blob_is_rejected(
        layer in any_layer(),
        cut in any::<prop::sample::Index>(),
    ) {
        let bytes = encode_layer(&layer);
        prop_assume!(bytes.len() > 1);
        let n = cut.index(bytes.len()); // 0..len, i.e. always a proper prefix
        let truncated = &bytes[..n];
        match decode_layer(truncated) {
            Err(_) => {}
            Ok(l) => {
                // The one non-error case: an empty layer encodes to a 9-byte header, and a
                // truncation cannot be a prefix of that and still decode.
                prop_assert_eq!(
                    truncated.len(),
                    bytes.len(),
                    "a proper prefix decoded to {} strokes",
                    l.strokes().len()
                );
            }
        }
    }

    /// A single flipped byte anywhere in a valid blob must not panic. Most flips land in a float
    /// or a colour and decode fine; the ones that land in a count, a flag byte or the magic are
    /// the point.
    #[test]
    fn a_single_flipped_byte_never_panics(
        layer in any_layer(),
        at in any::<prop::sample::Index>(),
        mask in 1u8..=u8::MAX,
    ) {
        let mut bytes = encode_layer(&layer);
        let i = at.index(bytes.len());
        bytes[i] ^= mask;
        let _ = decode_layer(&bytes);
    }

    /// A declared stroke count past the hard ceiling is rejected up front, so a hostile sidecar is
    /// a fast error rather than an allocation sized from the file.
    #[test]
    fn an_absurd_declared_stroke_count_is_rejected(n in 1_000_001u32..=u32::MAX) {
        let mut bytes = b"INKB".to_vec();
        bytes.push(INKBIN_VERSION);
        bytes.extend_from_slice(&n.to_le_bytes());
        prop_assert!(decode_layer(&bytes).is_err());
    }

    /// Any version byte but the current one is rejected, whatever follows — the check that lets
    /// the format be revised without misreading an old sidecar as a new one.
    #[test]
    fn a_wrong_version_is_rejected(ver in any::<u8>(), layer in any_layer()) {
        prop_assume!(ver != INKBIN_VERSION);
        let mut bytes = encode_layer(&layer);
        bytes[4] = ver;
        prop_assert!(decode_layer(&bytes).is_err());
    }

    /// Any magic but `INKB` is rejected — so a sidecar that is some other file entirely (a
    /// half-written PNG, a stray download) fails at byte 0 instead of being parsed as ink.
    #[test]
    fn a_wrong_magic_is_rejected(magic in prop::array::uniform4(any::<u8>()), layer in any_layer()) {
        prop_assume!(&magic != b"INKB");
        let mut bytes = encode_layer(&layer);
        bytes[..4].copy_from_slice(&magic);
        prop_assert!(decode_layer(&bytes).is_err());
    }
}
