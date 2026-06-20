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
use std::sync::Arc;

use inkread_ink::{InkColor, Tool};

use crate::dict::{encode_definition_wire, Dict};
use crate::document::{encode_links_wire, encode_selection_wire, encode_toc_wire, NormRect};
use crate::error::{CoreError, CoreResult};
use crate::persistence::ink_store::{FsInkStore, InkStore};
use crate::persistence::sidecar::SidecarPaths;
use crate::persistence::sqlite::SqliteStore;
use crate::persistence::{BookId, ReaderStore};
use crate::render::{PixelBuffer, Viewport};
use crate::session::{Gesture, ReaderSession};
use inkread_dict::import::import_stardict;

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

        let opened = if is_epub(&path) {
            ReaderSession::open_epub(bytes, caps, viewport)
        } else {
            ReaderSession::open_pdf(bytes, caps, viewport)
        };
        match opened {
            Ok(session) => Ok(Box::into_raw(Box::new(session)) as jlong),
            Err(e) => Err(throw(env, &e)),
        }
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

/// Pick the backend by file extension: `.epub` (case-insensitive) → reflowable EPUB, else PDF.
fn is_epub(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("epub"))
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

        let opened = if is_epub(&path) {
            ReaderSession::open_epub_with_store(bytes, caps, viewport, store, book)
        } else {
            ReaderSession::open_pdf_with_store(bytes, caps, viewport, store, book)
        };
        match opened {
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
// nativePageLinks(handle, page) : ByteArray — the clickable links on `page`, normalized to
// the rendered page (RR11-FR3). Decode with WireCodec.decodeLinks; the shell hit-tests a tap
// against these and jumps (internal) or opens the URI (external). Empty header on no links.
// =====================================================================================
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativePageLinks<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    page: jint,
) -> JByteArray<'local> {
    env.with_env(|env| -> jni::errors::Result<JByteArray<'local>> {
        // SAFETY: borrowed, not owned (Amendment 2).
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        let target = if page < 0 { 0usize } else { page as usize };
        let bytes = encode_links_wire(&session.page_links(target));
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

// =====================================================================================
// Ink annotation seam (RR6/RR7/RR10/RR20). The Kotlin shell feeds stylus geometry through
// these; the Rust core owns the model + sidecar persistence. The live firmware-ink *render*
// (ADR-SUPERNOTE-INK) is a separate device path and does NOT cross this seam.
//
//   nativeAttachInkStore(handle, docPath)        — bind the document's book.inkread/ sidecar
//   nativeInkBeginStroke(handle, tool, rgba, width, createdAtMs)
//   nativeInkAddPoint(handle, x, y, pressure, tiltX, tiltY, timestampMs)  (NaN tilt = absent)
//   nativeInkEndStroke(handle)                   — commit + autosave
//   nativeInkStrokesForPage(handle, page): bytes — .inkbin wire (decode with the ink codec)
//   nativeInkUndo / nativeInkRedo(handle): bool  — autosaves on change
//   nativeInkSave(handle)                        — explicit flush (onPause/close)
//
// `tool`: 0=Pen, 1=Highlighter, 2=Eraser. `rgba`: 0xRRGGBBAA packed into an int.
// =====================================================================================

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeAttachInkStore<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    doc_path: JString<'local>,
) {
    env.with_env(|env| -> jni::errors::Result<()> {
        // SAFETY: borrowed, not owned (Amendment 2).
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        let doc_path: String = doc_path.try_to_string(env)?;
        let paths = SidecarPaths::for_document(Path::new(&doc_path));
        let store: Arc<dyn InkStore> = Arc::new(FsInkStore::new(paths));
        session
            .attach_ink_store(store)
            .map_err(|e| throw(env, &e))?;
        Ok(())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// nativeExportPdf(handle, outPath, flatten) — write every page's ink into the PDF at `outPath`
// (ADR-INKREAD-0005). flatten=true bakes the ink into the page content (shows in every viewer);
// false writes editable Ink annotations. Colours are preserved. Throws on failure.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeExportPdf<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    out_path: JString<'local>,
    flatten: jboolean,
) {
    env.with_env(|env| -> jni::errors::Result<()> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        let out_path: String = out_path.try_to_string(env)?;
        session
            .export_pdf(&out_path, flatten)
            .map_err(|e| throw(env, &e))?;
        Ok(())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// nativeSetZoom(handle, zoom, panX, panY) — set the pinch-zoom factor (>=1; 1=fit) and normalized
// pan [0,1] (RR5-FR3). The next nativeRenderPage renders the magnified/panned view. Never throws.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeSetZoom<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    zoom: jfloat,
    pan_x: jfloat,
    pan_y: jfloat,
) {
    env.with_env(|env| -> jni::errors::Result<()> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        session.set_zoom(zoom, pan_x, pan_y);
        Ok(())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeInkBeginStroke<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    tool: jint,
    color_rgba: jint,
    width: jfloat,
    created_at_ms: jlong,
) {
    env.with_env(|env| -> jni::errors::Result<()> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        // Validate, don't truncate: `tool as u8` would silently fold 256 → Pen, 258 → Eraser.
        let tool = u8::try_from(tool)
            .ok()
            .and_then(Tool::from_code)
            .ok_or_else(|| {
                throw(
                    env,
                    &CoreError::InvalidArgument(format!("unknown ink tool {tool}")),
                )
            })?;
        let c = color_rgba as u32;
        let color = InkColor::rgba((c >> 24) as u8, (c >> 16) as u8, (c >> 8) as u8, c as u8);
        session
            .ink_begin_stroke(tool, color, width, created_at_ms.max(0) as u64)
            .map_err(|e| throw(env, &e))?;
        Ok(())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeInkAddPoint<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    x: jfloat,
    y: jfloat,
    pressure: jfloat,
    tilt_x: jfloat,
    tilt_y: jfloat,
    timestamp_ms: jint,
) {
    env.with_env(|env| -> jni::errors::Result<()> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        // NaN tilt means "not reported"; the model also drops any non-finite tilt to None.
        let tx = if tilt_x.is_nan() { None } else { Some(tilt_x) };
        let ty = if tilt_y.is_nan() { None } else { Some(tilt_y) };
        session
            .ink_add_point(x, y, pressure, tx, ty, timestamp_ms.max(0) as u32)
            .map_err(|e| throw(env, &e))?;
        Ok(())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeInkEndStroke<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
) {
    env.with_env(|env| -> jni::errors::Result<()> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        session.ink_end_stroke().map_err(|e| throw(env, &e))?;
        Ok(())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeInkStrokesForPage<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    page: jint,
) -> JByteArray<'local> {
    env.with_env(|env| -> jni::errors::Result<JByteArray<'local>> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        let target = if page < 0 { 0usize } else { page as usize };
        let bytes = session
            .ink_strokes_wire(target)
            .map_err(|e| throw(env, &e))?;
        env.byte_array_from_slice(&bytes)
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeInkStrokesForDraw<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    page: jint,
) -> JByteArray<'local> {
    env.with_env(|env| -> jni::errors::Result<JByteArray<'local>> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        let target = if page < 0 { 0usize } else { page as usize };
        let bytes = session.ink_draw_wire(target).map_err(|e| throw(env, &e))?;
        env.byte_array_from_slice(&bytes)
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeInkUndo<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
) -> jboolean {
    env.with_env(|env| -> jni::errors::Result<jboolean> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        let changed = session.ink_undo().map_err(|e| throw(env, &e))?;
        Ok(jboolean::from(changed))
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeInkRedo<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
) -> jboolean {
    env.with_env(|env| -> jni::errors::Result<jboolean> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        let changed = session.ink_redo().map_err(|e| throw(env, &e))?;
        Ok(jboolean::from(changed))
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeInkSave<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
) {
    env.with_env(|env| -> jni::errors::Result<()> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        session.save_ink().map_err(|e| throw(env, &e))?;
        Ok(())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// =====================================================================================
// Lasso selection (ADR-INKREAD-0010). Stroke ids cross as `int[]` (u32 reinterpreted), the
// polygon + selection bounds as `float[]`. All mutating ops autosave in the session.
//   nativeInkSelectInPolygon(handle, float[] xyPairs, int mode): int[]  (0=Smart, 1=Freehand)
//   nativeInkSelectAll(handle): int[]
//   nativeInkSelectionBounds(handle, int[] ids): float[]  (len 4 [x0,y0,x1,y1] or 0)
//   nativeInkMoveSelection(handle, int[] ids, float dx, float dy): boolean
//   nativeInkDeleteSelection / nativeInkCutSelection(handle, int[] ids): int[]  (removed ids)
//   nativeInkRecolorSelection(handle, int[] ids, int rgba): boolean
//   nativeInkCopySelection(handle, int[] ids): int   (clipboard count)
//   nativeInkPaste(handle, float dx, float dy): int[]  (new ids)
//   nativeInkHasClipboard(handle): boolean
// =====================================================================================

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

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeInkSelectInPolygon<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    polygon: JFloatArray<'local>,
    mode: jint,
) -> JIntArray<'local> {
    env.with_env(|env| -> jni::errors::Result<JIntArray<'local>> {
        let flat = read_f32_array(env, &polygon)?;
        let poly: Vec<(f32, f32)> = flat.chunks_exact(2).map(|c| (c[0], c[1])).collect();
        let mode_code = u8::try_from(mode).unwrap_or(u8::MAX);
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        let ids = session
            .ink_select_in_polygon(&poly, mode_code)
            .map_err(|e| throw(env, &e))?;
        new_u32_array(env, &ids)
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeInkSelectAll<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
) -> JIntArray<'local> {
    env.with_env(|env| -> jni::errors::Result<JIntArray<'local>> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        let ids = session.ink_select_all();
        new_u32_array(env, &ids)
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeInkSelectionBounds<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    ids: JIntArray<'local>,
) -> JFloatArray<'local> {
    env.with_env(|env| -> jni::errors::Result<JFloatArray<'local>> {
        let ids = read_u32_array(env, &ids)?;
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        let bounds = session.ink_selection_bounds(&ids);
        new_f32_array(env, &bounds)
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeInkMoveSelection<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    ids: JIntArray<'local>,
    dx: jfloat,
    dy: jfloat,
) -> jboolean {
    env.with_env(|env| -> jni::errors::Result<jboolean> {
        let ids = read_u32_array(env, &ids)?;
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        let changed = session
            .ink_move_selection(&ids, dx, dy)
            .map_err(|e| throw(env, &e))?;
        Ok(jboolean::from(changed))
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeInkDeleteSelection<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    ids: JIntArray<'local>,
) -> JIntArray<'local> {
    env.with_env(|env| -> jni::errors::Result<JIntArray<'local>> {
        let ids = read_u32_array(env, &ids)?;
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        let removed = session
            .ink_delete_selection(&ids)
            .map_err(|e| throw(env, &e))?;
        new_u32_array(env, &removed)
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeInkRecolorSelection<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    ids: JIntArray<'local>,
    color_rgba: jint,
) -> jboolean {
    env.with_env(|env| -> jni::errors::Result<jboolean> {
        let ids = read_u32_array(env, &ids)?;
        let c = color_rgba as u32;
        let color = InkColor::rgba((c >> 24) as u8, (c >> 16) as u8, (c >> 8) as u8, c as u8);
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        let changed = session
            .ink_recolor_selection(&ids, color)
            .map_err(|e| throw(env, &e))?;
        Ok(jboolean::from(changed))
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeInkCopySelection<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    ids: JIntArray<'local>,
) -> jint {
    env.with_env(|env| -> jni::errors::Result<jint> {
        let ids = read_u32_array(env, &ids)?;
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        Ok(session.ink_copy_selection(&ids) as jint)
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeInkCutSelection<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    ids: JIntArray<'local>,
) -> JIntArray<'local> {
    env.with_env(|env| -> jni::errors::Result<JIntArray<'local>> {
        let ids = read_u32_array(env, &ids)?;
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        let removed = session
            .ink_cut_selection(&ids)
            .map_err(|e| throw(env, &e))?;
        new_u32_array(env, &removed)
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeInkPaste<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    dx: jfloat,
    dy: jfloat,
) -> JIntArray<'local> {
    env.with_env(|env| -> jni::errors::Result<JIntArray<'local>> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        let new_ids = session.ink_paste(dx, dy).map_err(|e| throw(env, &e))?;
        new_u32_array(env, &new_ids)
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeInkHasClipboard<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
) -> jboolean {
    env.with_env(|env| -> jni::errors::Result<jboolean> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        Ok(jboolean::from(session.ink_has_clipboard()))
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// =====================================================================================
// Text selection + dictionary seam (RR11/RR12 / ADR-INKREAD-0009 D3). The shell turns a
// tap/drag into a selection, then looks the word up in the on-device corpus; an on-device miss
// is the shell's cue to try its (opt-in) online source and cache via nativeDictPut.
//
//   nativeWordAt(handle, page, x, y) : bytes        — selection wire (decode: WireCodec.decodeSelection)
//   nativeTextInRect(handle, page, x0,y0,x1,y1) : bytes
//   nativeDictOpen(path) : long                     — open the dict.db corpus; long handle
//   nativeDictClose(handle)                         — free it
//   nativeDefine(dictHandle, word, langsCsv) : bytes — definition wire (on-device only)
//   nativeDictPut(dictHandle, lang, headword, defn) — cache an online result for next time
//   nativeDictImport(dictHandle, stardictDir, lang, syn) : int — install a user StarDict folder
//                                                          (KOReader-style); returns record count
//
// Coordinates are normalized [0,1], top-left origin (matching the render + links).
// =====================================================================================

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeWordAt<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    page: jint,
    x: jfloat,
    y: jfloat,
) -> JByteArray<'local> {
    env.with_env(|env| -> jni::errors::Result<JByteArray<'local>> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        let target = if page < 0 { 0usize } else { page as usize };
        let sel = session.word_at(target, x, y).unwrap_or_default();
        env.byte_array_from_slice(&encode_selection_wire(&sel))
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeTextInRect<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    page: jint,
    x0: jfloat,
    y0: jfloat,
    x1: jfloat,
    y1: jfloat,
) -> JByteArray<'local> {
    env.with_env(|env| -> jni::errors::Result<JByteArray<'local>> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        let target = if page < 0 { 0usize } else { page as usize };
        let sel = session.text_in_rect(target, NormRect { x0, y0, x1, y1 });
        env.byte_array_from_slice(&encode_selection_wire(&sel))
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeDictOpen<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    path: JString<'local>,
) -> jlong {
    env.with_env(|env| -> jni::errors::Result<jlong> {
        let path: String = path.try_to_string(env)?;
        match Dict::open(&path) {
            Ok(d) => Ok(Box::into_raw(Box::new(d)) as jlong),
            Err(e) => Err(throw(
                env,
                &CoreError::Persistence(format!("dict open {path}: {e}")),
            )),
        }
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeDictClose<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
) {
    env.with_env(|_env| -> jni::errors::Result<()> {
        if handle != 0 {
            // SAFETY: a non-zero handle is a Box<Dict> from nativeDictOpen; reclaim + drop it. The
            // shell zeroes its field on close, so a double-close never reaches here with the same value.
            unsafe {
                drop(Box::from_raw(handle as *mut Dict));
            }
        }
        Ok(())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeDefine<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    dict_handle: jlong,
    word: JString<'local>,
    langs_csv: JString<'local>,
) -> JByteArray<'local> {
    env.with_env(|env| -> jni::errors::Result<JByteArray<'local>> {
        let dict = unsafe { dict_ref(dict_handle) }.map_err(|e| throw(env, &e))?;
        let word: String = word.try_to_string(env)?;
        let langs_csv: String = langs_csv.try_to_string(env)?;
        let langs: Vec<&str> = langs_csv
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        // On-device only (online = None); a miss is the shell's cue to try its online source.
        let def = dict.lookup(&word, &langs, None);
        env.byte_array_from_slice(&encode_definition_wire(def.as_ref()))
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeDictImport<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    dict_handle: jlong,
    stardict_dir: JString<'local>,
    lang: JString<'local>,
    syn: jboolean,
) -> jint {
    env.with_env(|env| -> jni::errors::Result<jint> {
        let dict = unsafe { dict_ref(dict_handle) }.map_err(|e| throw(env, &e))?;
        let dir: String = stardict_dir.try_to_string(env)?;
        let lang: String = lang.try_to_string(env)?;
        if lang.trim().is_empty() {
            return Err(throw(
                env,
                &CoreError::InvalidArgument("dict import: empty lang/source tag".into()),
            ));
        }
        // KOReader-style on-device install: import a StarDict folder into the writable dict.db the
        // shell already opened. `syn` marks a Moby-style thesaurus bundle (bodies are synonym lists).
        match import_stardict(std::path::Path::new(&dir), dict, &lang, syn) {
            Ok(n) => Ok(n as jint),
            Err(e) => Err(throw(
                env,
                &CoreError::Persistence(format!("dict import {dir}: {e}")),
            )),
        }
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeDictForget<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    dict_handle: jlong,
    lang: JString<'local>,
) -> jint {
    env.with_env(|env| -> jni::errors::Result<jint> {
        let dict = unsafe { dict_ref(dict_handle) }.map_err(|e| throw(env, &e))?;
        let lang: String = lang.try_to_string(env)?;
        match dict.forget(&lang) {
            Ok(n) => Ok(n as jint),
            Err(e) => Err(throw(
                env,
                &CoreError::Persistence(format!("dict forget {lang}: {e}")),
            )),
        }
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeDictPut<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    dict_handle: jlong,
    lang: JString<'local>,
    headword: JString<'local>,
    defn: JString<'local>,
) {
    env.with_env(|env| -> jni::errors::Result<()> {
        let dict = unsafe { dict_ref(dict_handle) }.map_err(|e| throw(env, &e))?;
        let lang: String = lang.try_to_string(env)?;
        let headword: String = headword.try_to_string(env)?;
        let defn: String = defn.try_to_string(env)?;
        dict.put_entry(&lang, &headword, &defn)
            .map_err(|e| throw(env, &CoreError::Persistence(format!("dict put: {e}"))))?;
        Ok(())
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
