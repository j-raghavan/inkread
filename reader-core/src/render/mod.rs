//! Rendering pipeline: borrowed [`PixelBuffer`], [`Viewport`], grayscale + dithering (RR4).
//!
//! M0 is the single-copy full-page render path (Fork 4); dirty-rect/cache/prefetch are M1b.

pub mod cache;
pub mod gray;
pub mod pixel_buffer;
pub mod viewport;

pub use cache::{PageHash, RenderCache};
pub use gray::{to_grayscale, DitherMode, GRAY_LEVELS};
pub use pixel_buffer::{ChannelOrder, PixelBuffer, BYTES_PER_PIXEL, CHANNEL_ORDER};
pub use viewport::Viewport;
