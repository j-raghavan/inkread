//! `PixelBuffer` — a borrowed, tightly-packed RGBA render target (RR4-FR1, Fork 4).
//!
//! The shell owns the backing memory (an Android `Bitmap`'s locked pixels or an
//! `ANativeWindow` buffer); the core borrows it for the duration of a single render call
//! and **never retains it** (RR21-FR2). The borrow MUST NOT outlive the JNI call: a
//! `PixelBuffer` is constructed from the locked slice, rendered into, and dropped before
//! the shell unlocks — it is never stored in a `ReaderSession` and never returned across
//! JNI (Amendment 5).
//!
//! ## Channel order — the explicit, asserted decision (Amendment 3)
//! The buffer is **RGBA**, 8 bits/channel, **stride == width * 4** (tightly packed). The
//! PDF backend renders pdfium with `set_reverse_byte_order(true)` so pdfium emits RGBA
//! straight into this buffer (matching the Android `ARGB_8888` lock's byte order and
//! tiny-skia's channel order); the grayscale step then reads channels in **R, G, B**
//! order ([`CHANNEL_ORDER`]). This is the single source of truth for the BGRA↔RGBA
//! mismatch and is pinned by a golden-image test.
//!
//! NOTE: when α<255 overlays land (M1b+ ink), the Android-locked bitmap is **premultiplied**
//! alpha — compositing must account for that (tiny-skia expects premultiplied; pdfium emits
//! straight alpha). M0 white-fills first (RR4-FR3) so α is always 255 and the distinction
//! is moot here.

use crate::error::{CoreError, CoreResult};
use crate::render::viewport::Viewport;

/// Bytes per pixel in the RGBA target.
pub const BYTES_PER_PIXEL: usize = 4;

/// The fixed channel byte order of a [`PixelBuffer`] (Amendment 3). Indices into each pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelOrder {
    /// Byte index of the red channel.
    pub r: usize,
    /// Byte index of the green channel.
    pub g: usize,
    /// Byte index of the blue channel.
    pub b: usize,
    /// Byte index of the alpha channel.
    pub a: usize,
}

/// inkread renders **RGBA**: pdfium is configured (reverse-byte-order) to emit this, and
/// `gray.rs` reads R,G,B accordingly. Changing this is a cross-cutting decision.
pub const CHANNEL_ORDER: ChannelOrder = ChannelOrder {
    r: 0,
    g: 1,
    b: 2,
    a: 3,
};

/// A mutable, tightly-packed RGBA pixel buffer borrowed from the shell (Fork 4).
///
/// Invariant: `pixels.len() == width * height * 4` and stride is exactly `width * 4`
/// (no row padding). Constructed per render call; never stored across the JNI boundary
/// (Amendment 5).
pub struct PixelBuffer<'a> {
    pixels: &'a mut [u8],
    width: u32,
    height: u32,
}

impl<'a> PixelBuffer<'a> {
    /// Borrow `pixels` as an RGBA buffer for a `width × height` surface.
    ///
    /// Returns [`CoreError::BufferMismatch`] if `pixels` is not exactly `width*height*4`
    /// bytes (tight packing, no stride padding — RR4-FR4).
    pub fn from_rgba(pixels: &'a mut [u8], width: u32, height: u32) -> CoreResult<Self> {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|p| p.checked_mul(BYTES_PER_PIXEL))
            .ok_or_else(|| CoreError::BufferMismatch("dimension overflow".into()))?;
        if pixels.len() != expected {
            return Err(CoreError::BufferMismatch(format!(
                "expected {expected} bytes ({width}x{height}x4), got {}",
                pixels.len()
            )));
        }
        Ok(Self {
            pixels,
            width,
            height,
        })
    }

    /// Borrow a buffer sized for `viewport` (convenience over [`Self::from_rgba`]).
    pub fn for_viewport(pixels: &'a mut [u8], viewport: Viewport) -> CoreResult<Self> {
        Self::from_rgba(pixels, viewport.width, viewport.height)
    }

    /// Buffer width in pixels.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Buffer height in pixels.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The row stride in bytes (`width * 4` — tightly packed by construction).
    #[must_use]
    pub fn stride(&self) -> usize {
        self.width as usize * BYTES_PER_PIXEL
    }

    /// Immutable view of the backing bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        self.pixels
    }

    /// Mutable view of the backing bytes (for the renderer / dither step).
    #[must_use]
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        self.pixels
    }

    /// White-fill the buffer (opaque white) before rendering so there are no alpha gaps
    /// (RR4-FR3). Writes per [`CHANNEL_ORDER`]; α set to 255.
    pub fn fill_white(&mut self) {
        for px in self.pixels.chunks_exact_mut(BYTES_PER_PIXEL) {
            px[CHANNEL_ORDER.r] = 0xFF;
            px[CHANNEL_ORDER.g] = 0xFF;
            px[CHANNEL_ORDER.b] = 0xFF;
            px[CHANNEL_ORDER.a] = 0xFF;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_wrong_length() {
        let mut buf = vec![0u8; 10];
        assert!(matches!(
            PixelBuffer::from_rgba(&mut buf, 2, 2),
            Err(CoreError::BufferMismatch(_))
        ));
    }

    #[test]
    fn accepts_tight_buffer_and_reports_stride() {
        let mut buf = vec![0u8; 2 * 3 * 4];
        let pb = PixelBuffer::from_rgba(&mut buf, 2, 3).unwrap();
        assert_eq!(pb.width(), 2);
        assert_eq!(pb.height(), 3);
        assert_eq!(pb.stride(), 8);
    }

    #[test]
    fn fill_white_sets_opaque_white_rgba() {
        let mut buf = vec![0u8; 4]; // one pixel
        let mut pb = PixelBuffer::from_rgba(&mut buf, 1, 1).unwrap();
        pb.fill_white();
        assert_eq!(pb.bytes(), &[0xFF, 0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn channel_order_is_rgba() {
        assert_eq!(
            CHANNEL_ORDER,
            ChannelOrder {
                r: 0,
                g: 1,
                b: 2,
                a: 3
            }
        );
    }
}
