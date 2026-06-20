//! Draw-wire for ink strokes (ADR-INKREAD-0010). The Kotlin shell bakes these onto the page and
//! passes selected stroke ids back to the lasso ops — so unlike the `.inkbin` persistence codec,
//! this wire carries the per-stroke **id** plus just what drawing needs (tool, color, width, path).
//!
//! Little-endian, mirroring the other JNI wire codecs. Layout:
//! `[ver=1][count: u16]` then per stroke
//! `[id: u32][tool: u8][rgba: u32][width: f32][nPoints: u16]` then `nPoints × [x: f32][y: f32]`.

use inkread_ink::{InkLayer, Stroke};

const WIRE_VERSION: u8 = 1;

/// Encode a page's committed strokes to the draw-wire (decode with `WireCodec.decodeStrokes`).
#[must_use]
pub fn encode_strokes_draw_wire(layer: &InkLayer) -> Vec<u8> {
    let strokes = layer.strokes();
    let count = strokes.len().min(u16::MAX as usize);
    let mut out = Vec::with_capacity(3 + count * 16);
    out.push(WIRE_VERSION);
    out.extend_from_slice(&(count as u16).to_le_bytes());
    for s in strokes.iter().take(count) {
        encode_stroke(&mut out, s);
    }
    out
}

fn encode_stroke(out: &mut Vec<u8>, s: &Stroke) {
    out.extend_from_slice(&s.id.0.to_le_bytes());
    out.push(s.tool.code());
    let c = s.color;
    let rgba = ((c.r as u32) << 24) | ((c.g as u32) << 16) | ((c.b as u32) << 8) | (c.a as u32);
    out.extend_from_slice(&rgba.to_le_bytes());
    out.extend_from_slice(&s.width.to_le_bytes());
    let n = s.points.len().min(u16::MAX as usize);
    out.extend_from_slice(&(n as u16).to_le_bytes());
    for p in s.points.iter().take(n) {
        out.extend_from_slice(&p.x.to_le_bytes());
        out.extend_from_slice(&p.y.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use inkread_ink::{InkColor, InkPoint, Tool};

    #[test]
    fn encodes_header_and_one_stroke() {
        let mut layer = InkLayer::new();
        layer
            .start_stroke(Tool::Highlighter, InkColor::rgba(1, 2, 3, 4), 0.02, 0)
            .unwrap();
        layer
            .push_point(InkPoint::new(0.25, 0.5, 1.0, None, None, 0).unwrap())
            .unwrap();
        let id = layer.finish_stroke().unwrap();

        let w = encode_strokes_draw_wire(&layer);
        assert_eq!(w[0], WIRE_VERSION);
        assert_eq!(u16::from_le_bytes([w[1], w[2]]), 1); // one stroke
                                                         // id (u32) then tool code (Highlighter = 1).
        assert_eq!(u32::from_le_bytes([w[3], w[4], w[5], w[6]]), id.0);
        assert_eq!(w[7], Tool::Highlighter.code());
        // rgba packed big-endian-in-u32 then stored LE.
        assert_eq!(u32::from_le_bytes([w[8], w[9], w[10], w[11]]), 0x0102_0304);
    }

    #[test]
    fn empty_layer_is_header_only() {
        let w = encode_strokes_draw_wire(&InkLayer::new());
        assert_eq!(w, vec![WIRE_VERSION, 0, 0]);
    }
}
