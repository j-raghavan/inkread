//! `reader-core` — the inkread engine (`libreader.so`).
//!
//! Owns document parsing/render, the e-ink [`RefreshPolicy`](device_eink::RefreshPolicy)
//! implementation, and the reader session. The Android shell drives it over a thin,
//! **feature-gated** JNI bridge ([`mod@jni`], behind `jni-bridge`) — the core itself is
//! Android-type-free (RR1-FR4) and builds host-only without an Android SDK (RR1-AC3).
//!
//! IR-7: no vendor name appears here; all device specifics live in the Kotlin adapters.

pub mod budget;
pub mod dict;
pub mod document;
pub mod error;
pub mod persistence;
pub mod policy;
pub mod render;
pub mod session;
pub mod settings;

// The JNI bridge compiles ONLY under `--features jni-bridge`, so the host gate
// (`cargo test -p reader-core`) never resolves the `jni` crate (Amendment 1 / RR1-AC3).
#[cfg(feature = "jni-bridge")]
mod jni;
