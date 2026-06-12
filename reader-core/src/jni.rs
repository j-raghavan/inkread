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

use jni::objects::{JByteArray, JByteBuffer, JClass, JString};
use jni::strings::JNIString;
use jni::sys::{jint, jlong};
use jni::{Env, EnvUnowned};

use device_eink::{decode_capabilities, encode_commands, DeviceCapabilities};

use std::path::Path;
use std::sync::Arc;

use crate::document::encode_toc_wire;
use crate::error::{CoreError, CoreResult};
use crate::persistence::sqlite::SqliteStore;
use crate::persistence::{BookId, ReaderStore};
use crate::render::{PixelBuffer, Viewport};
use crate::session::{Gesture, ReaderSession};

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

// =====================================================================================
// nativeHello() : String  — proves the JNI boundary end to end (RR1-AC2).
// =====================================================================================
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeHello<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
) -> JString<'local> {
    env.with_env(|env| -> jni::errors::Result<JString<'local>> {
        let v = concat!("inkread reader-core ", env!("CARGO_PKG_VERSION"));
        env.new_string(v)
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// =====================================================================================
// nativeInit(capsBytes: ByteArray) : Boolean — decode the caps wire format (Fork 3, RR2-FR2).
// Returns true if the caps decoded; throws on a malformed message.
// =====================================================================================
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeInit<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    caps_bytes: JByteArray<'local>,
) -> jni::sys::jboolean {
    env.with_env(|env| -> jni::errors::Result<jni::sys::jboolean> {
        let bytes = env.convert_byte_array(&caps_bytes)?;
        match decode_capabilities(&bytes) {
            Ok(_caps) => Ok(jni::sys::JNI_TRUE),
            Err(e) => Err(throw(
                env,
                &CoreError::InvalidArgument(format!("caps decode: {e:?}")),
            )),
        }
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// =====================================================================================
// nativeOpenDocument(path, capsBytes, w, h, dpi) : long  — returns the opaque handle.
// For M0 the shell passes a filesystem path and the core reads the bytes, keeping the
// Kotlin side minimal; the SAF/scoped-storage byte path is RR22 (M1a, out of M0 scope).
// =====================================================================================
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeOpenDocument<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    path: JString<'local>,
    caps_bytes: JByteArray<'local>,
    width: jint,
    height: jint,
    dpi: jint,
) -> jlong {
    env.with_env(|env| -> jni::errors::Result<jlong> {
        let path: String = path.try_to_string(env)?;
        let caps = read_caps(env, &caps_bytes)?;
        let viewport = read_viewport(env, width, height, dpi)?;

        let bytes = std::fs::read(&path)
            .map_err(|e| CoreError::InvalidArgument(format!("read {path}: {e}")))
            .map_err(|e| throw(env, &e))?;

        match ReaderSession::open_pdf(bytes, caps, viewport) {
            Ok(session) => Ok(Box::into_raw(Box::new(session)) as jlong),
            Err(e) => Err(throw(env, &e)),
        }
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// =====================================================================================
// nativeOpenDocumentWithStore(path, capsBytes, w, h, dpi, dbPath, bookId) : long
// Opens a PDF AND attaches a SQLite-backed store (RR12 / RR27 session restore): the saved
// reading position for `bookId` is resumed (clamped to the document range) and persisted
// e-ink settings are applied to the policy (RR23 ↔ RR3). `dbPath` is a host filesystem path
// the shell owns under app storage; `bookId` is the stable per-book identity (≤512 chars).
// =====================================================================================
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeOpenDocumentWithStore<
    'local,
>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    path: JString<'local>,
    caps_bytes: JByteArray<'local>,
    width: jint,
    height: jint,
    dpi: jint,
    db_path: JString<'local>,
    book_id: JString<'local>,
) -> jlong {
    env.with_env(|env| -> jni::errors::Result<jlong> {
        let path: String = path.try_to_string(env)?;
        let db_path: String = db_path.try_to_string(env)?;
        let book_id: String = book_id.try_to_string(env)?;
        let caps = read_caps(env, &caps_bytes)?;
        let viewport = read_viewport(env, width, height, dpi)?;

        let bytes = std::fs::read(&path)
            .map_err(|e| CoreError::InvalidArgument(format!("read {path}: {e}")))
            .map_err(|e| throw(env, &e))?;

        let book = BookId::new(book_id).map_err(|e| throw(env, &e))?;
        let store = SqliteStore::open(Path::new(&db_path)).map_err(|e| throw(env, &e))?;
        let store: Arc<dyn ReaderStore> = Arc::new(store);

        match ReaderSession::open_pdf_with_store(bytes, caps, viewport, store, book) {
            Ok(session) => Ok(Box::into_raw(Box::new(session)) as jlong),
            Err(e) => Err(throw(env, &e)),
        }
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// =====================================================================================
// nativeCloseDocument(handle) — frees the session. Null-safe + double-close tolerant.
// The ONLY place that takes ownership (Box::from_raw) — Amendment 2.
// =====================================================================================
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeCloseDocument<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
) {
    env.with_env(|_env| -> jni::errors::Result<()> {
        if handle != 0 {
            // SAFETY: a non-zero handle is a Box we created in open; reclaiming it here drops
            // the session. The shell zeroes its field on close so it never calls us twice
            // with the same non-zero value (double-close becomes a no-op).
            unsafe {
                drop(Box::from_raw(handle as *mut ReaderSession));
            }
        }
        Ok(())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// =====================================================================================
// nativePageCount(handle) : int
// =====================================================================================
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativePageCount<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
) -> jint {
    env.with_env(|env| -> jni::errors::Result<jint> {
        // SAFETY: borrowed, not owned (Amendment 2).
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        Ok(session.page_count() as jint)
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// =====================================================================================
// nativeRenderPage(handle, directBuffer) — render the current page into the direct
// ByteBuffer the shell locked. The PixelBuffer borrow never outlives this call (Amendment 5).
// =====================================================================================
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeRenderPage<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    buffer: JByteBuffer<'local>,
) {
    env.with_env(|env| -> jni::errors::Result<()> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;

        let addr = env.get_direct_buffer_address(&buffer)?;
        let cap = env.get_direct_buffer_capacity(&buffer)?;
        if addr.is_null() {
            return Err(throw(
                env,
                &CoreError::BufferMismatch("render buffer is not a direct ByteBuffer".into()),
            ));
        }
        // SAFETY: `addr`/`cap` describe the direct buffer's contiguous memory, valid for the
        // duration of this JNI call; we form a slice over exactly `cap` bytes and drop the
        // PixelBuffer before returning (Amendment 5). The shell must not mutate it concurrently.
        let slice = unsafe { std::slice::from_raw_parts_mut(addr, cap) };
        let (w, h) = session_dims(session);
        let mut pb = PixelBuffer::from_rgba(slice, w, h).map_err(|e| throw(env, &e))?;
        session
            .render_current(&mut pb)
            .map_err(|e| throw(env, &e))?;
        Ok(())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// =====================================================================================
// nativeOnGesture(handle, code) : ByteArray  — apply the gesture, return the encoded
// RefreshCommand stream (Fork 2, Amendment 6). Returns an empty array on an unknown code
// (after throwing), per the resolve default.
// =====================================================================================
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeOnGesture<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    code: jint,
) -> JByteArray<'local> {
    env.with_env(|env| -> jni::errors::Result<JByteArray<'local>> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        let gesture = Gesture::from_code(code).ok_or_else(|| {
            throw(
                env,
                &CoreError::InvalidArgument(format!("unknown gesture code {code}")),
            )
        })?;
        let commands = session.on_gesture(gesture);
        let bytes = encode_commands(&commands);
        env.byte_array_from_slice(&bytes)
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// =====================================================================================
// nativeSavePosition(handle) — persist the current reading position (RR12-FR3 / RR27).
// A store-less session is a silent no-op; a persistence error throws so the shell can log
// it without losing the in-memory position.
// =====================================================================================
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeSavePosition<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
) {
    env.with_env(|env| -> jni::errors::Result<()> {
        // SAFETY: borrowed, not owned (Amendment 2).
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        session.save_position().map_err(|e| throw(env, &e))?;
        Ok(())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// =====================================================================================
// nativeCurrentPage(handle) : int — the current 0-based page index (RR11). Drives the page
// indicator and lets the shell verify a resumed position after open-with-store.
// =====================================================================================
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeCurrentPage<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
) -> jint {
    env.with_env(|env| -> jni::errors::Result<jint> {
        // SAFETY: borrowed, not owned (Amendment 2).
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        Ok(session.current_page() as jint)
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// =====================================================================================
// nativeToc(handle) : ByteArray — the document outline as the flattened pre-order wire
// (RR11-FR2). Decode with WireCodec.decodeToc. An outline-less document yields the header
// with a zero count (an empty list), never an error.
// =====================================================================================
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeToc<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
) -> JByteArray<'local> {
    env.with_env(|env| -> jni::errors::Result<JByteArray<'local>> {
        // SAFETY: borrowed, not owned (Amendment 2).
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        let bytes = encode_toc_wire(&session.toc());
        env.byte_array_from_slice(&bytes)
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// =====================================================================================
// nativeJumpToPage(handle, page) : ByteArray — jump to an absolute page index (clamped to
// the document range in the core), returning the encoded RefreshCommand stream (RR11-FR1).
// A negative index clamps to 0. Used by TOC/scrubber jumps.
// =====================================================================================
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeJumpToPage<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    page: jint,
) -> JByteArray<'local> {
    env.with_env(|env| -> jni::errors::Result<JByteArray<'local>> {
        // SAFETY: borrowed, not owned (Amendment 2).
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        let target = if page < 0 { 0usize } else { page as usize };
        let commands = session.jump_to_page(target);
        let bytes = encode_commands(&commands);
        env.byte_array_from_slice(&bytes)
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
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
