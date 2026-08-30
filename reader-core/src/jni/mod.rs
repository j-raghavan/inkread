//! JNI bridge (feature `jni-bridge`) — the thin Android boundary (RR1-FR3, RR21).
//!
//! Compiles **only** under `--features jni-bridge`; the host gate never sees it (RR1-AC3),
//! so no `jni`/Android types leak into the host-tested core (RR1-FR4 / IR-7). Every export:
//! - catches panics at the boundary and converts them — never unwinds into the JVM
//!   (RR21-FR3); `EnvUnowned::with_env` wraps the closure in `catch_unwind` for us.
//! - validates inputs (null handle, bad ByteBuffer) and returns a typed result; on a
//!   [`CoreError`] it throws a Java `RuntimeException` carrying the status code + message
//!   and returns a sentinel default.
//!
//! ## Handle model (Amendment 2)
//! The document handle is a `jlong` = `Box::into_raw(Box::new(ReaderSession)) as jlong`.
//! Every handle-taking export checks `!= 0` and reconstructs `&mut *(h as *mut _)` **without**
//! taking ownership. **Only** [`Java_..._nativeCloseDocument`] does `Box::from_raw`; it is
//! null-safe and tolerates a double-close (the shell zeroes its handle field on close).
//!
//! ## Render buffer (Amendment 5)
//! The shell passes a direct `java.nio.ByteBuffer`; we form a `&mut [u8]` from its address
//! for the duration of the call only, build a [`PixelBuffer`], render, and drop it before
//! returning — never stored across the boundary.
//!
//! ## Gesture mapping (Amendment 6)
//! The gesture int code is decoded by [`Gesture::from_code`] (the single source of truth).

use jni::objects::{JByteArray, JByteBuffer, JClass, JFloatArray, JIntArray, JString};
use jni::strings::JNIString;
use jni::sys::{jboolean, jfloat, jint, jlong};
use jni::{Env, EnvUnowned};

use device_eink::{decode_capabilities, encode_commands, DeviceCapabilities};

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use inkread_ink::{InkColor, Tool};

use crate::budget::TrimLevel;
use crate::dict::{encode_definition_wire, Dict};
use crate::document::{
    encode_links_wire, encode_search_wire, encode_selection_wire, encode_toc_wire, DocFormat,
    NormRect, Typography,
};
use crate::error::{CoreError, CoreResult};
use crate::persistence::ink_store::{FsInkStore, InkStore};
use crate::persistence::sidecar::SidecarPaths;
use crate::persistence::sqlite::SqliteStore;
use crate::persistence::{BookId, PaginationProgress, ReaderStore};
use crate::render::{PixelBuffer, Viewport};
use crate::session::{Gesture, ReaderSession};
use inkread_dict::import::import_stardict;

// The bridge is split by the domain each export serves, so a change to (say) the ink seam does not
// mean scrolling past the whole boundary to find it. `#[unsafe(no_mangle)]` symbols are exported by
// name, not by module path, so the ABI the Kotlin side links against is byte-for-byte unchanged.
//
// Everything below this point is shared: the panic/throw conversion, the handle reconstruction, and
// the array marshalling. Each submodule opens with `use super::*` and adds nothing of its own.
mod display;
mod document;
mod ink;
mod lasso;
mod services;
mod text;

/// Throw a Java `RuntimeException` for a [`CoreError`] (status code prefixed) so the shell
/// surfaces it; returns `jni::errors::Error` so the `with_env` closure short-circuits.
fn throw(env: &mut Env<'_>, e: &CoreError) -> jni::errors::Error {
    let msg = format!("[{}] {e}", e.status_code());
    // Best-effort: if resolving the class or throwing itself fails there is nothing more we
    // can do safely — the resolve default still returns a sentinel.
    if let Ok(class) = env.find_class(JNIString::new("java/lang/RuntimeException")) {
        let _ = env.throw_new(class, JNIString::new(msg));
    }
    jni::errors::Error::JavaException
}

/// Reconstruct a borrowed `&mut ReaderSession` from a non-null handle (Amendment 2).
///
/// # Safety
/// `handle` must be a value previously returned by `nativeOpenDocument` and not yet closed.
unsafe fn session_mut<'a>(handle: jlong) -> CoreResult<&'a mut ReaderSession> {
    if handle == 0 {
        return Err(CoreError::InvalidArgument("null document handle".into()));
    }
    Ok(&mut *(handle as *mut ReaderSession))
}

/// Reconstruct a borrowed `&Dict` from a non-null dictionary handle (RR12 / D3). Lookup is `&self`,
/// so a shared reference suffices; the handle is a `Box<Dict>` from `nativeDictOpen`.
///
/// # Safety
/// `handle` must be a value previously returned by `nativeDictOpen` and not yet closed.
unsafe fn dict_ref<'a>(handle: jlong) -> CoreResult<&'a Dict> {
    if handle == 0 {
        return Err(CoreError::InvalidArgument("null dictionary handle".into()));
    }
    Ok(&*(handle as *const Dict))
}

// Pagination progress + cancel (#161).
//
// Laying out a large book runs on the shell's engine thread, so the UI thread cannot ask the
// session about it — that would alias the `&mut` the engine thread is holding. These are plain
// process-wide atomics instead: no handle, nothing borrowed, safe to poll from any thread while a
// pagination is in flight. One document is paginated at a time, so one set of counters suffices.
//
// Shared rather than living beside the progress exports in `display`: opening a document clears
// the cancel flag and installs the sink, so `document` reaches them too.
// =====================================================================================
static PAGINATION_DONE: AtomicUsize = AtomicUsize::new(0);
static PAGINATION_TOTAL: AtomicUsize = AtomicUsize::new(0);
static PAGINATION_CANCEL: AtomicBool = AtomicBool::new(false);

/// The [`PaginationProgress`] the shell sees, backed by the atomics above.
struct AtomicPaginationProgress;

impl PaginationProgress for AtomicPaginationProgress {
    fn chapter_done(&self, done: usize, total: usize) {
        PAGINATION_TOTAL.store(total, Ordering::Relaxed);
        PAGINATION_DONE.store(done, Ordering::Relaxed);
    }

    fn cancelled(&self) -> bool {
        PAGINATION_CANCEL.load(Ordering::Relaxed)
    }
}

/// Read a Java `int[]` of stroke ids into `Vec<u32>` (jint bits reinterpreted as u32).
fn read_u32_array(env: &mut Env<'_>, arr: &JIntArray<'_>) -> jni::errors::Result<Vec<u32>> {
    let len = arr.len(env)?;
    let mut buf = vec![0i32; len];
    if len > 0 {
        arr.get_region(env, 0, &mut buf)?;
    }
    Ok(buf.into_iter().map(|i| i as u32).collect())
}

/// Read a Java `float[]` into `Vec<f32>`.
fn read_f32_array(env: &mut Env<'_>, arr: &JFloatArray<'_>) -> jni::errors::Result<Vec<f32>> {
    let len = arr.len(env)?;
    let mut buf = vec![0f32; len];
    if len > 0 {
        arr.get_region(env, 0, &mut buf)?;
    }
    Ok(buf)
}

/// Build a Java `int[]` from stroke ids.
fn new_u32_array<'l>(env: &mut Env<'l>, ids: &[u32]) -> jni::errors::Result<JIntArray<'l>> {
    let arr = JIntArray::new(env, ids.len())?;
    let buf: Vec<i32> = ids.iter().map(|&i| i as i32).collect();
    arr.set_region(env, 0, &buf)?;
    Ok(arr)
}

/// Build a Java `float[]` from a slice.
fn new_f32_array<'l>(env: &mut Env<'l>, v: &[f32]) -> jni::errors::Result<JFloatArray<'l>> {
    let arr = JFloatArray::new(env, v.len())?;
    arr.set_region(env, 0, v)?;
    Ok(arr)
}

// ---- small helpers (kept out of the export bodies for readability) ----

fn read_caps(
    env: &mut Env<'_>,
    caps_bytes: &JByteArray<'_>,
) -> jni::errors::Result<DeviceCapabilities> {
    let bytes = env.convert_byte_array(caps_bytes)?;
    decode_capabilities(&bytes).map_err(|e| {
        throw(
            env,
            &CoreError::InvalidArgument(format!("caps decode: {e:?}")),
        )
    })
}

fn read_viewport(
    env: &mut Env<'_>,
    width: jint,
    height: jint,
    dpi: jint,
) -> jni::errors::Result<Viewport> {
    if width <= 0 || height <= 0 || dpi <= 0 {
        return Err(throw(
            env,
            &CoreError::InvalidArgument(format!("bad viewport {width}x{height}@{dpi}")),
        ));
    }
    Ok(Viewport::new(width as u32, height as u32, dpi as u32))
}

/// The session's viewport dimensions (for the render buffer geometry).
fn session_dims(session: &ReaderSession) -> (u32, u32) {
    // `render_current` re-validates the buffer against the viewport; we mirror the
    // dimensions here so the PixelBuffer constructs at the right size.
    session.viewport_dims()
}
