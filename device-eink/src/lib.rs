//! `device-eink` — the vendor-neutral device seam (RR2).
//!
//! This crate holds the **contracts** the Rust core speaks — [`RefreshIntent`],
//! [`RefreshCommand`], [`DeviceCapabilities`], the [`RefreshPolicy`] trait, and the
//! [`Rect`] geometry — plus the host-side [`MockDeviceRecorder`] and the JNI **wire
//! codecs** (caps in, command stream out). The real Supernote/Boox executors live in
//! the Kotlin app, never here (RR1-FR1). No vendor name appears anywhere in this crate
//! (IR-7); it builds and tests entirely on the host with no Android SDK (RR1-AC3).

mod capabilities;
mod command;
mod geometry;
mod mock_desktop;
mod policy;

pub use capabilities::DeviceCapabilities;
pub use command::{RefreshCommand, RefreshIntent};
pub use geometry::Rect;
pub use mock_desktop::MockDeviceRecorder;
pub use policy::RefreshPolicy;
