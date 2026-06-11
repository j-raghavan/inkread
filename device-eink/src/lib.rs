//! `device-eink` — the vendor-neutral device seam (RR2).
//!
//! This crate holds the **contracts** the Rust core speaks — [`RefreshIntent`],
//! [`RefreshCommand`], [`DeviceCapabilities`], the [`RefreshPolicy`] trait, and the
//! [`Rect`] geometry — plus the host-side [`MockDeviceRecorder`] and the JNI **wire
//! codecs** (caps in, command stream out). The real Supernote/Boox executors live in
//! the Kotlin app, never here (RR1-FR1). No vendor name appears anywhere in this crate
//! (IR-7); it builds and tests entirely on the host with no Android SDK (RR1-AC3).

// Scaffold only at this commit; modules land per the M0 commit order.
