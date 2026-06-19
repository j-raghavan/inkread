//! The `.inkbin` codec — a compact, versioned, little-endian encoding of a page [`InkLayer`]
//! (RR10-FR4). It mirrors `reader-core`'s wire codecs (`encode_links_wire`/`encode_toc_wire`):
//! a magic + version prefix, little-endian fields, and **hardened decode** that returns
//! [`InkError::BadEncoding`] on any malformed/truncated input rather than panicking or
//! over-allocating (RR21-FR3).
//!
//! Layout (all integers/floats little-endian):
//! ```text
//! ["INKB"][ver: u8 = 1][stroke_count: u32]
//! per stroke:
//!   [id: u32][tool: u8][r u8][g u8][b u8][a u8][width: f32][created_at_ms: u64]
//!   [point_count: u32]
//!   per point:
//!     [x: f32][y: f32][pressure: f32][flags: u8][tilt_x: f32?][tilt_y: f32?][ts: u32]
//! ```
//! `flags` bit 0 = a `tilt_x` field follows, bit 1 = a `tilt_y` field follows. Only the strokes
//! persist — never the undo/redo history (RR20).

use crate::model::{InkColor, InkError, InkLayer, InkPoint, InkResult, Stroke, StrokeId, Tool};

/// Magic prefix identifying an `.inkbin` blob.
const MAGIC: &[u8; 4] = b"INKB";
/// Current `.inkbin` format version.
pub const INKBIN_VERSION: u8 = 1;

const FLAG_TILT_X: u8 = 0b0000_0001;
const FLAG_TILT_Y: u8 = 0b0000_0010;

/// Encode a layer's committed strokes to `.inkbin` bytes (RR10-FR4). Pure; never panics.
#[must_use]
pub fn encode_layer(layer: &InkLayer) -> Vec<u8> {
    let strokes = layer.strokes();
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.push(INKBIN_VERSION);
    out.extend_from_slice(&(u32::try_from(strokes.len()).unwrap_or(u32::MAX)).to_le_bytes());
    for s in strokes.iter().take(u32::MAX as usize) {
        out.extend_from_slice(&s.id.0.to_le_bytes());
        out.push(s.tool.code());
        out.extend_from_slice(&[s.color.r, s.color.g, s.color.b, s.color.a]);
        out.extend_from_slice(&s.width.to_le_bytes());
        out.extend_from_slice(&s.created_at_ms.to_le_bytes());
        let pcount = u32::try_from(s.points.len()).unwrap_or(u32::MAX);
        out.extend_from_slice(&pcount.to_le_bytes());
        for p in s.points.iter().take(pcount as usize) {
            out.extend_from_slice(&p.x.to_le_bytes());
            out.extend_from_slice(&p.y.to_le_bytes());
            out.extend_from_slice(&p.pressure.to_le_bytes());
            let mut flags = 0u8;
            if p.tilt_x.is_some() {
                flags |= FLAG_TILT_X;
            }
            if p.tilt_y.is_some() {
                flags |= FLAG_TILT_Y;
            }
            out.push(flags);
            if let Some(tx) = p.tilt_x {
                out.extend_from_slice(&tx.to_le_bytes());
            }
            if let Some(ty) = p.tilt_y {
                out.extend_from_slice(&ty.to_le_bytes());
            }
            out.extend_from_slice(&p.timestamp_ms.to_le_bytes());
        }
    }
    out
}

/// Decode `.inkbin` bytes into an [`InkLayer`] (RR10-FR4). Returns [`InkError::BadEncoding`] on a
/// bad magic, unknown version, unknown tool code, a zero-point stroke, or truncation — never
/// panics and never pre-allocates from an untrusted count.
pub fn decode_layer(bytes: &[u8]) -> InkResult<InkLayer> {
    let mut c = Cursor::new(bytes);
    if c.take(4)? != MAGIC {
        return Err(InkError::BadEncoding("bad magic".into()));
    }
    let ver = c.u8()?;
    if ver != INKBIN_VERSION {
        return Err(InkError::BadEncoding(format!("unsupported version {ver}")));
    }
    let stroke_count = c.u32()?;
    let mut strokes = Vec::new();
    for _ in 0..stroke_count {
        let id = StrokeId(c.u32()?);
        let tool = Tool::from_code(c.u8()?)
            .ok_or_else(|| InkError::BadEncoding("unknown tool code".into()))?;
        let color = InkColor::rgba(c.u8()?, c.u8()?, c.u8()?, c.u8()?);
        let width = c.f32()?;
        let created_at_ms = c.u64()?;
        let point_count = c.u32()?;
        if point_count == 0 {
            return Err(InkError::BadEncoding("stroke has no points".into()));
        }
        let mut points = Vec::new();
        for _ in 0..point_count {
            let x = c.f32()?;
            let y = c.f32()?;
            let pressure = c.f32()?;
            let flags = c.u8()?;
            let tilt_x = if flags & FLAG_TILT_X != 0 {
                Some(c.f32()?)
            } else {
                None
            };
            let tilt_y = if flags & FLAG_TILT_Y != 0 {
                Some(c.f32()?)
            } else {
                None
            };
            let timestamp_ms = c.u32()?;
            points.push(InkPoint::new(x, y, pressure, tilt_x, tilt_y, timestamp_ms)?);
        }
        strokes.push(Stroke {
            id,
            tool,
            color,
            width,
            points,
            created_at_ms,
        });
    }
    Ok(InkLayer::from_strokes(strokes))
}

/// A bounds-checked little-endian reader. Every read that would run past the end returns
/// [`InkError::BadEncoding`] — the source of the codec's no-panic guarantee.
struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn take(&mut self, n: usize) -> InkResult<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&e| e <= self.buf.len())
            .ok_or(InkError::BadEncoding(String::from(
                "unexpected end of data",
            )))?;
        let slice = &self.buf[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> InkResult<u8> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> InkResult<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> InkResult<u64> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn f32(&mut self) -> InkResult<f32> {
        let b = self.take(4)?;
        Ok(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_layer() -> InkLayer {
        let mut l = InkLayer::new();
        l.start_stroke(Tool::Pen, InkColor::rgba(10, 20, 30, 255), 0.012, 42)
            .unwrap();
        l.push_point(InkPoint::new(0.1, 0.2, 0.5, None, None, 0).unwrap())
            .unwrap();
        l.push_point(InkPoint::new(0.3, 0.4, 0.8, Some(0.1), Some(-0.2), 16).unwrap())
            .unwrap();
        l.finish_stroke().unwrap();
        l.start_stroke(
            Tool::Highlighter,
            InkColor::rgba(255, 230, 0, 128),
            0.03,
            99,
        )
        .unwrap();
        l.push_point(InkPoint::new(0.6, 0.6, 1.0, None, None, 0).unwrap())
            .unwrap();
        l.finish_stroke().unwrap();
        l
    }

    #[test]
    fn round_trip_preserves_strokes() {
        let layer = sample_layer();
        let bytes = encode_layer(&layer);
        let back = decode_layer(&bytes).unwrap();
        assert_eq!(back.strokes(), layer.strokes());
    }

    #[test]
    fn empty_layer_round_trips() {
        let layer = InkLayer::new();
        let bytes = encode_layer(&layer);
        let back = decode_layer(&bytes).unwrap();
        assert!(back.is_empty());
        // header only: magic(4) + ver(1) + count(4)
        assert_eq!(bytes.len(), 9);
    }

    #[test]
    fn decoded_layer_continues_ids_safely() {
        let bytes = encode_layer(&sample_layer());
        let mut back = decode_layer(&bytes).unwrap();
        // sample has ids 0 and 1 → next must be 2
        back.start_stroke(Tool::Pen, InkColor::BLACK, 0.01, 0)
            .unwrap();
        back.push_point(InkPoint::new(0.5, 0.5, 1.0, None, None, 0).unwrap())
            .unwrap();
        assert_eq!(back.finish_stroke().unwrap(), StrokeId(2));
    }

    #[test]
    fn bad_magic_is_rejected() {
        let err = decode_layer(b"XXXX\x01\x00\x00\x00\x00").unwrap_err();
        assert!(matches!(err, InkError::BadEncoding(_)));
    }

    #[test]
    fn unknown_version_is_rejected() {
        let mut bytes = encode_layer(&sample_layer());
        bytes[4] = 0xFE; // corrupt the version byte
        assert!(matches!(
            decode_layer(&bytes),
            Err(InkError::BadEncoding(_))
        ));
    }

    #[test]
    fn truncation_is_rejected_not_panicked() {
        let full = encode_layer(&sample_layer());
        // Every shorter prefix (past the header) must error cleanly, never panic.
        for len in 0..full.len() {
            assert!(
                decode_layer(&full[..len]).is_err(),
                "prefix len {len} should be rejected"
            );
        }
    }

    #[test]
    fn unknown_tool_code_is_rejected() {
        let mut bytes = encode_layer(&sample_layer());
        // first stroke's tool byte sits right after magic(4)+ver(1)+count(4)+id(4) = offset 13
        bytes[13] = 0x7F;
        assert!(matches!(
            decode_layer(&bytes),
            Err(InkError::BadEncoding(_))
        ));
    }
}
