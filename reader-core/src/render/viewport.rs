//! `Viewport` — the panel geometry the shell passes in (RR4-FR4).
//!
//! The render buffer matches the viewport so the blit is 1:1 (no hot-path scaling).

/// The target surface geometry: pixel dimensions + DPI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Dots per inch (used by the document backend to pick a render scale).
    pub dpi: u32,
}

impl Viewport {
    /// A viewport of `width × height` at `dpi`.
    #[must_use]
    pub const fn new(width: u32, height: u32, dpi: u32) -> Self {
        Self { width, height, dpi }
    }

    /// The number of RGBA bytes a tightly-packed buffer for this viewport must hold
    /// (`width * height * 4`).
    #[must_use]
    pub const fn byte_len(&self) -> usize {
        (self.width as usize) * (self.height as usize) * 4
    }
}
