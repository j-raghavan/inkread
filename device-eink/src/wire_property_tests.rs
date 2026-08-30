//! Property-based tests for the JNI wire codecs — RR17-FR3.
//!
//! These bytes cross a language boundary. Kotlin writes the caps message and reads the command
//! stream, so a codec that is merely self-consistent on the cases someone thought to write down is
//! not enough: the round-trip has to hold for every value the encoder can produce, and *decode*
//! has to be total over bytes it did not produce, because the other side of the boundary is not
//! this crate and a truncated or renegotiated buffer is a normal failure, not a hostile one.
//!
//! Two shapes of property, then:
//!
//! - **Round-trip**, over arbitrary values: `decode(encode(x)) == x`, including the framing
//!   guarantees (the `u8` command-count truncation, `Rect`'s signed origin, trailing bytes ignored
//!   by the Fork-3 caps decoder).
//! - **Totality**, over arbitrary bytes: decode returns `Ok` or a typed `WireError` and never
//!   panics, over-allocates, or reads past the end.

use proptest::prelude::*;

use super::*;

fn any_caps() -> impl Strategy<Value = DeviceCapabilities> {
    prop::collection::vec(any::<bool>(), DeviceCapabilities::FLAG_COUNT)
        .prop_map(|flags| DeviceCapabilities::from_flags(&flags))
}

fn any_intent() -> impl Strategy<Value = RefreshIntent> {
    prop::sample::select(vec![
        RefreshIntent::Full,
        RefreshIntent::Partial,
        RefreshIntent::Ui,
        RefreshIntent::Fast,
        RefreshIntent::FlashUi,
        RefreshIntent::FlashPartial,
    ])
}

/// `Rect` carries a **signed** origin on purpose — content scrolling onto the panel is at a
/// negative x/y — so the generator spans the full `i32`, not just the visible quadrant.
fn any_rect() -> impl Strategy<Value = Rect> {
    (any::<i32>(), any::<i32>(), any::<u32>(), any::<u32>())
        .prop_map(|(x, y, w, h)| Rect::new(x, y, w, h))
}

fn any_command() -> impl Strategy<Value = RefreshCommand> {
    prop_oneof![
        (any_rect(), any_intent(), any::<bool>()).prop_map(|(rect, intent, dither)| {
            RefreshCommand::Update {
                rect,
                intent,
                dither,
            }
        }),
        Just(RefreshCommand::WaitForLast),
        Just(RefreshCommand::EnterFastMode),
        Just(RefreshCommand::LeaveFastMode),
    ]
}

proptest! {
    /// Every capability set survives the caps codec unchanged.
    #[test]
    fn capabilities_round_trip(caps in any_caps()) {
        let back = decode_capabilities(&encode_capabilities(&caps))
            .map_err(|e| TestCaseError::fail(format!("decode failed: {e:?}")))?;
        prop_assert_eq!(back, caps);
    }

    /// Fork 3's forward-compatibility rule: a decoder must ignore flags it does not know, so a
    /// newer shell can add one without breaking an older core. Appending arbitrary bytes must not
    /// change the decoded value.
    #[test]
    fn trailing_flags_are_ignored(caps in any_caps(), extra in prop::collection::vec(any::<u8>(), 0..8)) {
        let mut bytes = encode_capabilities(&caps);
        bytes[1] = bytes[1].saturating_add(extra.len() as u8);
        bytes.extend_from_slice(&extra);
        let back = decode_capabilities(&bytes)
            .map_err(|e| TestCaseError::fail(format!("decode failed: {e:?}")))?;
        prop_assert_eq!(back, caps);
    }

    /// The other half of that rule: a *shorter* message defaults the missing flags to `false`
    /// rather than failing, so an older shell still talks to a newer core.
    #[test]
    fn a_short_flag_run_defaults_the_rest_to_false(n in 0usize..DeviceCapabilities::FLAG_COUNT) {
        let mut bytes = vec![WIRE_VERSION, n as u8];
        bytes.extend(std::iter::repeat_n(1u8, n));
        let caps = decode_capabilities(&bytes)
            .map_err(|e| TestCaseError::fail(format!("decode failed: {e:?}")))?;
        for (i, flag) in caps.flags().iter().enumerate() {
            prop_assert_eq!(*flag, i < n, "flag {} should be {}", i, i < n);
        }
    }

    /// Every command stream the policy can emit survives the command codec unchanged — up to the
    /// documented `u8` count truncation.
    #[test]
    fn commands_round_trip(cmds in prop::collection::vec(any_command(), 0..40)) {
        let back = decode_commands(&encode_commands(&cmds))
            .map_err(|e| TestCaseError::fail(format!("decode failed: {e:?}")))?;
        prop_assert_eq!(back, cmds);
    }

    /// The framing is exactly `4 + 20*N`, which is what the Kotlin reader strides over.
    #[test]
    fn command_stream_framing_is_fixed_width(cmds in prop::collection::vec(any_command(), 0..40)) {
        let bytes = encode_commands(&cmds);
        prop_assert_eq!(bytes.len(), COMMAND_HEADER_LEN + cmds.len() * COMMAND_RECORD_LEN);
        prop_assert_eq!(bytes[0], WIRE_VERSION);
        prop_assert_eq!(bytes[1] as usize, cmds.len());
    }

    /// Hardening (RR21-FR3): arbitrary bytes decode to a value or a typed error, never a panic and
    /// never a read past the end — whatever Kotlin hands over, however it was truncated.
    #[test]
    fn decode_capabilities_is_total(bytes in prop::collection::vec(any::<u8>(), 0..64)) {
        let _ = decode_capabilities(&bytes);
    }

    /// The same for the command stream. The generator is seeded with a valid header so cases get
    /// past the version check often enough to exercise the per-record tag and intent validation,
    /// rather than bouncing off byte 0.
    #[test]
    fn decode_commands_is_total(
        n in any::<u8>(),
        body in prop::collection::vec(any::<u8>(), 0..200),
    ) {
        let mut bytes = vec![WIRE_VERSION, n, 0, 0];
        bytes.extend_from_slice(&body);
        let _ = decode_commands(&bytes);
    }

    /// A declared count far beyond the bytes present must be `Truncated`, not an allocation sized
    /// from an attacker-controlled length.
    #[test]
    fn an_overlong_declared_count_is_rejected(n in 1u8..=u8::MAX) {
        let bytes = vec![WIRE_VERSION, n, 0, 0];
        prop_assert!(decode_commands(&bytes).is_err());
    }

    /// An unknown record tag or intent is a typed `BadDiscriminant`, so a shell that emits
    /// something this core does not know fails loudly instead of silently decoding a neighbour.
    #[test]
    fn an_unknown_tag_is_rejected(tag in 4u8..=u8::MAX) {
        let mut bytes = vec![WIRE_VERSION, 1, 0, 0];
        bytes.extend(std::iter::repeat_n(0u8, COMMAND_RECORD_LEN));
        bytes[COMMAND_HEADER_LEN] = tag;
        prop_assert!(decode_commands(&bytes).is_err());
    }

    /// A wrong version byte is rejected whatever follows it — the check that lets the format be
    /// revised at all.
    #[test]
    fn a_wrong_version_is_rejected(v in any::<u8>(), body in prop::collection::vec(any::<u8>(), 1..40)) {
        prop_assume!(v != WIRE_VERSION);
        let mut bytes = vec![v];
        bytes.extend_from_slice(&body);
        prop_assert!(decode_capabilities(&bytes).is_err());
        prop_assert!(decode_commands(&bytes).is_err());
    }
}
