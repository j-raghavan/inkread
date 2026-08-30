//! Rendering pipeline: borrowed [`PixelBuffer`], [`Viewport`], grayscale + dithering (RR4).
//!
//! A single-copy full-page render (Fork 4) into a bounded [`cache`], with the next page prefetched
//! behind the current one so a page turn is usually a cache hit. Dirty-rect rendering is not here
//! and will not be: the panel path a sideloaded app can reach refreshes full-screen only, so a
//! partial render would still cost a full flash — see `docs/EINK-LIMITS.md`.

pub mod cache;
pub mod contrast;
pub mod gray;
pub mod pixel_buffer;
pub mod resample;
pub mod viewport;

pub use cache::{ByteLru, PageHash, RenderCache};
pub use gray::{invert_in_place, to_grayscale, DitherMode, GRAY_LEVELS};
pub use pixel_buffer::{ChannelOrder, PixelBuffer, BYTES_PER_PIXEL, CHANNEL_ORDER};
pub use viewport::Viewport;
