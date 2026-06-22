//! The JNI wire codecs (Forks 2 & 3) — host-testable byte marshaling.
//!
//! Living in `device-eink` (not the JNI bridge) means `mock_desktop` golden-byte tests
//! assert the exact bytes with no JNI in the loop. Both codecs are **little-endian** and
//! pin LE explicitly (`to_le_bytes`/`from_le_bytes`) so the Kotlin side (also LE) agrees
//! regardless of host endianness.
//!
//! ## Fork 3 — capabilities IN (`nativeInit(capsBytes)`)
//! ```text
//! [0]      u8  version = 0x01
//! [1]      u8  nflags  (= the exact flag count the shell sent)
//! [2..2+n] u8  flags[] (0|1) in DECLARATION ORDER (DeviceCapabilities::flags())
//! ```
//! The core reads `nflags`, decodes that many, defaults any flag it knows but the shell
//! did not send to `false`, and ignores unrecognized trailing bytes.
//!
//! ## Fork 2 — commands OUT (`nativeOnGesture -> ByteArray`)
//! ```text
//! Header (4 bytes): [0] u8 version=0x01  [1] u8 count=N  [2..4] u16 reserved=0 (LE)
//! Then N × 20-byte records (LE):
//!   [0]  u8  tag    (0=Update,1=WaitForLast,2=EnterFastMode,3=LeaveFastMode)
//!   [1]  u8  intent (0=Full,1=Partial,2=Ui,3=Fast,4=FlashUi,5=FlashPartial; 0 if not Update)
//!   [2]  u8  dither (0|1)   [3] u8 pad=0
//!   [4..8]   i32 rect.x   [8..12] i32 rect.y   [12..16] i32 rect.w   [16..20] i32 rect.h
//! ```
//! Total = 4 + 20*N.

use crate::capabilities::DeviceCapabilities;
use crate::command::{RefreshCommand, RefreshIntent};
use crate::geometry::Rect;

/// The wire-format version byte shared by both codecs.
pub const WIRE_VERSION: u8 = 0x01;

/// Bytes per encoded [`RefreshCommand`] record (Fork 2).
pub const COMMAND_RECORD_LEN: usize = 20;

/// Bytes in the command-stream header (Fork 2).
pub const COMMAND_HEADER_LEN: usize = 4;

// ---- command tags (Fork 2) ----
const TAG_UPDATE: u8 = 0;
const TAG_WAIT_FOR_LAST: u8 = 1;
const TAG_ENTER_FAST: u8 = 2;
const TAG_LEAVE_FAST: u8 = 3;

/// Errors decoding a wire message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    /// The buffer was shorter than the header/record framing requires.
    Truncated,
    /// The version byte did not match [`WIRE_VERSION`].
    BadVersion(u8),
    /// A record carried an unknown command tag or intent discriminant.
    BadDiscriminant(u8),
}

/// Map a [`RefreshIntent`] to its wire discriminant.
const fn intent_code(intent: RefreshIntent) -> u8 {
    intent as u8
}

/// Map a wire discriminant back to a [`RefreshIntent`].
const fn intent_from_code(code: u8) -> Option<RefreshIntent> {
    match code {
        0 => Some(RefreshIntent::Full),
        1 => Some(RefreshIntent::Partial),
        2 => Some(RefreshIntent::Ui),
        3 => Some(RefreshIntent::Fast),
        4 => Some(RefreshIntent::FlashUi),
        5 => Some(RefreshIntent::FlashPartial),
        _ => None,
    }
}

// =====================================================================================
// Fork 3 — DeviceCapabilities codec
// =====================================================================================

/// Encode `caps` to the Fork-3 caps wire format.
#[must_use]
pub fn encode_capabilities(caps: &DeviceCapabilities) -> Vec<u8> {
    let flags = caps.flags();
    let mut out = Vec::with_capacity(2 + flags.len());
    out.push(WIRE_VERSION);
    out.push(flags.len() as u8);
    out.extend(flags.iter().map(|&b| u8::from(b)));
    out
}

/// Decode a Fork-3 caps message: validate the version, read `nflags` flag bytes, default
/// any missing known flag to `false`, ignore trailing bytes (Fork 3).
pub fn decode_capabilities(bytes: &[u8]) -> Result<DeviceCapabilities, WireError> {
    if bytes.len() < 2 {
        return Err(WireError::Truncated);
    }
    if bytes[0] != WIRE_VERSION {
        return Err(WireError::BadVersion(bytes[0]));
    }
    let nflags = bytes[1] as usize;
    let flags_end = 2usize.checked_add(nflags).ok_or(WireError::Truncated)?;
    if bytes.len() < flags_end {
        return Err(WireError::Truncated);
    }
    let flags: Vec<bool> = bytes[2..flags_end].iter().map(|&b| b != 0).collect();
    Ok(DeviceCapabilities::from_flags(&flags))
}

// =====================================================================================
// Fork 2 — RefreshCommand stream codec
// =====================================================================================

/// Encode one command into a 20-byte LE record.
fn encode_record(cmd: &RefreshCommand) -> [u8; COMMAND_RECORD_LEN] {
    let mut r = [0u8; COMMAND_RECORD_LEN];
    match *cmd {
        RefreshCommand::Update {
            rect,
            intent,
            dither,
        } => {
            r[0] = TAG_UPDATE;
            r[1] = intent_code(intent);
            r[2] = u8::from(dither);
            r[3] = 0; // pad
            r[4..8].copy_from_slice(&rect.x.to_le_bytes());
            r[8..12].copy_from_slice(&rect.y.to_le_bytes());
            r[12..16].copy_from_slice(&rect.w.to_le_bytes());
            r[16..20].copy_from_slice(&rect.h.to_le_bytes());
        }
        RefreshCommand::WaitForLast => r[0] = TAG_WAIT_FOR_LAST,
        RefreshCommand::EnterFastMode => r[0] = TAG_ENTER_FAST,
        RefreshCommand::LeaveFastMode => r[0] = TAG_LEAVE_FAST,
    }
    r
}

/// Encode a command stream to the Fork-2 wire format (`4 + 20*N` bytes).
///
/// `commands.len()` is written as a `u8` count; a stream of >255 commands is not expected
/// for a single interaction and is truncated to 255 (the policy never emits that many).
#[must_use]
pub fn encode_commands(commands: &[RefreshCommand]) -> Vec<u8> {
    let n = commands.len().min(u8::MAX as usize);
    let mut out = Vec::with_capacity(COMMAND_HEADER_LEN + n * COMMAND_RECORD_LEN);
    out.push(WIRE_VERSION);
    out.push(n as u8);
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    for cmd in &commands[..n] {
        out.extend_from_slice(&encode_record(cmd));
    }
    out
}

/// Decode a Fork-2 command stream (used by host round-trip tests; the Kotlin side is the
/// real consumer). Validates version + framing and rejects unknown tags/intents.
pub fn decode_commands(bytes: &[u8]) -> Result<Vec<RefreshCommand>, WireError> {
    if bytes.len() < COMMAND_HEADER_LEN {
        return Err(WireError::Truncated);
    }
    if bytes[0] != WIRE_VERSION {
        return Err(WireError::BadVersion(bytes[0]));
    }
    let n = bytes[1] as usize;
    let need = COMMAND_HEADER_LEN + n * COMMAND_RECORD_LEN;
    if bytes.len() < need {
        return Err(WireError::Truncated);
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let off = COMMAND_HEADER_LEN + i * COMMAND_RECORD_LEN;
        let r = &bytes[off..off + COMMAND_RECORD_LEN];
        let cmd = match r[0] {
            TAG_UPDATE => {
                let intent = intent_from_code(r[1]).ok_or(WireError::BadDiscriminant(r[1]))?;
                let x = i32::from_le_bytes([r[4], r[5], r[6], r[7]]);
                let y = i32::from_le_bytes([r[8], r[9], r[10], r[11]]);
                let w = u32::from_le_bytes([r[12], r[13], r[14], r[15]]);
                let h = u32::from_le_bytes([r[16], r[17], r[18], r[19]]);
                RefreshCommand::Update {
                    rect: Rect::new(x, y, w, h),
                    intent,
                    dither: r[2] != 0,
                }
            }
            TAG_WAIT_FOR_LAST => RefreshCommand::WaitForLast,
            TAG_ENTER_FAST => RefreshCommand::EnterFastMode,
            TAG_LEAVE_FAST => RefreshCommand::LeaveFastMode,
            other => return Err(WireError::BadDiscriminant(other)),
        };
        out.push(cmd);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- Fork 3: capabilities ----------

    #[test]
    fn caps_golden_bytes_supernote_baseline() {
        // 12 flags, declaration order. baseline: eink=1, eink_full=0, ..., needs_resume=1.
        let caps = DeviceCapabilities::supernote_baseline();
        let bytes = encode_capabilities(&caps);
        assert_eq!(
            bytes,
            vec![
                0x01, // version
                12,   // nflags
                1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                1, // flags (eink..needs_refresh_after_resume)
            ]
        );
    }

    #[test]
    fn caps_round_trip_all_profiles() {
        for caps in [
            DeviceCapabilities::supernote_baseline(),
            DeviceCapabilities::supernote_full(),
            DeviceCapabilities::desktop_mock(),
        ] {
            let bytes = encode_capabilities(&caps);
            assert_eq!(decode_capabilities(&bytes).unwrap(), caps);
        }
    }

    #[test]
    fn caps_decode_defaults_short_message() {
        // Older shell: only 3 flags present; rest default false (Fork 3).
        let bytes = [WIRE_VERSION, 3, 1, 1, 1];
        let caps = decode_capabilities(&bytes).unwrap();
        assert_eq!(caps, DeviceCapabilities::from_flags(&[true, true, true]));
    }

    #[test]
    fn caps_decode_ignores_trailing_bytes() {
        let mut bytes = encode_capabilities(&DeviceCapabilities::supernote_full());
        bytes.push(0xFF); // unrecognized trailing byte beyond nflags
                          // Decoder reads only nflags flags, so trailing is ignored.
        assert_eq!(
            decode_capabilities(&bytes).unwrap(),
            DeviceCapabilities::supernote_full()
        );
    }

    #[test]
    fn caps_decode_rejects_bad_version_and_truncation() {
        assert_eq!(
            decode_capabilities(&[0x02, 0]),
            Err(WireError::BadVersion(0x02))
        );
        assert_eq!(decode_capabilities(&[0x01]), Err(WireError::Truncated));
        assert_eq!(
            decode_capabilities(&[0x01, 5, 1, 0]),
            Err(WireError::Truncated)
        );
    }

    // ---------- Fork 2: command stream ----------

    #[test]
    fn commands_golden_bytes_partial_update() {
        let cmds = [RefreshCommand::Update {
            rect: Rect::new(1, 2, 3, 4),
            intent: RefreshIntent::Partial,
            dither: true,
        }];
        let bytes = encode_commands(&cmds);
        assert_eq!(
            bytes,
            vec![
                // header: version, count=1, reserved=0 (LE u16)
                0x01, 0x01, 0x00, 0x00, //
                // record: tag=Update(0), intent=Partial(1), dither=1, pad=0
                0x00, 0x01, 0x01, 0x00, //
                // rect.x=1 (LE i32)
                0x01, 0x00, 0x00, 0x00, //
                // rect.y=2
                0x02, 0x00, 0x00, 0x00, //
                // rect.w=3
                0x03, 0x00, 0x00, 0x00, //
                // rect.h=4
                0x04, 0x00, 0x00, 0x00,
            ]
        );
        assert_eq!(bytes.len(), COMMAND_HEADER_LEN + COMMAND_RECORD_LEN);
    }

    #[test]
    fn commands_golden_bytes_negative_rect_origin() {
        // -1 as LE i32 == 0xFF 0xFF 0xFF 0xFF — pins the signed encoding.
        let cmds = [RefreshCommand::Update {
            rect: Rect::new(-1, -2, 5, 6),
            intent: RefreshIntent::Full,
            dither: false,
        }];
        let bytes = encode_commands(&cmds);
        assert_eq!(&bytes[4..8], &[0x00, 0x00, 0x00, 0x00]); // tag=0,intent=0,dither=0,pad=0
        assert_eq!(&bytes[8..12], &[0xFF, 0xFF, 0xFF, 0xFF]); // x = -1
        assert_eq!(&bytes[12..16], &[0xFE, 0xFF, 0xFF, 0xFF]); // y = -2
    }

    #[test]
    fn commands_golden_bytes_barrier_and_fast_mode() {
        let cmds = [
            RefreshCommand::WaitForLast,
            RefreshCommand::EnterFastMode,
            RefreshCommand::LeaveFastMode,
        ];
        let bytes = encode_commands(&cmds);
        assert_eq!(bytes[1], 3); // count
                                 // first record tag byte = WaitForLast(1)
        assert_eq!(bytes[COMMAND_HEADER_LEN], TAG_WAIT_FOR_LAST);
        assert_eq!(
            bytes[COMMAND_HEADER_LEN + COMMAND_RECORD_LEN],
            TAG_ENTER_FAST
        );
        assert_eq!(
            bytes[COMMAND_HEADER_LEN + 2 * COMMAND_RECORD_LEN],
            TAG_LEAVE_FAST
        );
        assert_eq!(bytes.len(), COMMAND_HEADER_LEN + 3 * COMMAND_RECORD_LEN);
    }

    #[test]
    fn commands_round_trip_mixed_stream() {
        let cmds = vec![
            RefreshCommand::WaitForLast,
            RefreshCommand::Update {
                rect: Rect::new(-5, 7, 800, 600),
                intent: RefreshIntent::FlashPartial,
                dither: false,
            },
            RefreshCommand::EnterFastMode,
            RefreshCommand::Update {
                rect: Rect::new(0, 0, 1, 1),
                intent: RefreshIntent::Ui,
                dither: true,
            },
            RefreshCommand::LeaveFastMode,
        ];
        let bytes = encode_commands(&cmds);
        assert_eq!(decode_commands(&bytes).unwrap(), cmds);
    }

    #[test]
    fn commands_empty_stream_is_header_only() {
        let bytes = encode_commands(&[]);
        assert_eq!(bytes, vec![0x01, 0x00, 0x00, 0x00]);
        assert_eq!(decode_commands(&bytes).unwrap(), vec![]);
    }

    #[test]
    fn commands_decode_rejects_bad_framing() {
        assert_eq!(decode_commands(&[0x01]), Err(WireError::Truncated));
        assert_eq!(
            decode_commands(&[0x02, 0, 0, 0]),
            Err(WireError::BadVersion(0x02))
        );
        // count=1 but no record bytes follow
        assert_eq!(decode_commands(&[0x01, 1, 0, 0]), Err(WireError::Truncated));
        // valid framing, unknown tag 9
        let mut bad = encode_commands(&[RefreshCommand::WaitForLast]);
        bad[COMMAND_HEADER_LEN] = 9;
        assert_eq!(decode_commands(&bad), Err(WireError::BadDiscriminant(9)));
    }
}
