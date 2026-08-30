//! Document lifecycle and navigation (RR1-FR3, RR5, RR11).
//!
//! Open, close, and everything that asks the session where it is or moves it: page count, the
//! render and prefetch calls, the gesture round-trip, the outline, page links, and position
//! save/restore.

use super::*;

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
// The shell passes a filesystem path and the core reads the bytes. That is the shipped design, not
// a stopgap: a SAF pick is copied into app storage by the shell first (RR22), so the core sees one
// kind of input, stays IO-simple, and never learns about content URIs.
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

        let bytes = read_document_file(&path).map_err(|e| throw(env, &e))?;

        let opened = match DocFormat::resolve(&path, &bytes) {
            DocFormat::Epub => ReaderSession::open_epub(bytes, caps, viewport),
            DocFormat::Cbz => ReaderSession::open_cbz(bytes, caps, viewport),
            DocFormat::Text => ReaderSession::open_txt(bytes, caps, viewport),
            DocFormat::Pdf => ReaderSession::open_pdf(bytes, caps, viewport),
        };
        match opened {
            Ok(session) => Ok(Box::into_raw(Box::new(session)) as jlong),
            Err(e) => Err(throw(env, &e)),
        }
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

/// Hard ceiling on a document file opened over the JNI boundary (RR21-FR3 / RR22). A multi-GB or
/// decompression-bomb file would OOM the process if read whole; we stat first and reject before the
/// read. 2 GiB is far above any real PDF/EPUB while still bounding the allocation.
const MAX_DOCUMENT_BYTES: u64 = 2 << 30;

/// Read a document file for open, defensively (RR21-FR3): resolve the path (canonicalize, so `..`
/// and symlinks can't redirect us), require a **regular file** (reject `/dev/*`, FIFOs, and
/// directories), and **cap the size** before pulling the bytes into RAM. The shell still owns which
/// roots it hands us (scoped storage / SAF, RR22); this closes the native boundary against a
/// malformed path and an oversized/streaming file.
fn read_document_file(path: &str) -> CoreResult<Vec<u8>> {
    let resolved = std::fs::canonicalize(path)
        .map_err(|e| CoreError::InvalidArgument(format!("resolve {path}: {e}")))?;
    let meta = std::fs::metadata(&resolved)
        .map_err(|e| CoreError::InvalidArgument(format!("stat {path}: {e}")))?;
    if !meta.is_file() {
        return Err(CoreError::InvalidArgument(format!(
            "{path} is not a regular file"
        )));
    }
    if meta.len() > MAX_DOCUMENT_BYTES {
        return Err(CoreError::InvalidArgument(format!(
            "{path} is {} bytes, over the {MAX_DOCUMENT_BYTES}-byte open limit",
            meta.len()
        )));
    }
    std::fs::read(&resolved).map_err(|e| CoreError::InvalidArgument(format!("read {path}: {e}")))
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
    scale: jfloat,
    font_id: jint,
    line_spacing: jfloat,
    align_code: jint,
    columns: jint,
    margin_pct: jint,
) -> jlong {
    env.with_env(|env| -> jni::errors::Result<jlong> {
        let path: String = path.try_to_string(env)?;
        let db_path: String = db_path.try_to_string(env)?;
        let book_id: String = book_id.try_to_string(env)?;
        let caps = read_caps(env, &caps_bytes)?;
        let viewport = read_viewport(env, width, height, dpi)?;
        // The reader's saved typography, applied as the document opens. Values are clamped by the
        // backend, so out-of-range input is settings, not an error (RR21-FR3).
        let typography = Typography {
            scale,
            font_id,
            line_spacing,
            align_code,
            columns,
            margin_pct,
        };

        let bytes = read_document_file(&path).map_err(|e| throw(env, &e))?;

        let book = BookId::new(book_id).map_err(|e| throw(env, &e))?;
        // A fresh document: no pagination of the previous one is in flight, and no stale cancel
        // may be left standing or the first re-layout would abandon itself.
        PAGINATION_CANCEL.store(false, Ordering::Relaxed);
        PAGINATION_DONE.store(0, Ordering::Relaxed);
        PAGINATION_TOTAL.store(0, Ordering::Relaxed);
        let store = SqliteStore::open(Path::new(&db_path)).map_err(|e| throw(env, &e))?;
        let store: Arc<dyn ReaderStore> = Arc::new(store);

        let opened = match DocFormat::resolve(&path, &bytes) {
            DocFormat::Epub => {
                ReaderSession::open_epub_with_store(bytes, caps, viewport, store, book, typography)
            }
            DocFormat::Cbz => {
                ReaderSession::open_cbz_with_store(bytes, caps, viewport, store, book, typography)
            }
            DocFormat::Text => {
                ReaderSession::open_txt_with_store(bytes, caps, viewport, store, book, typography)
            }
            DocFormat::Pdf => {
                ReaderSession::open_pdf_with_store(bytes, caps, viewport, store, book, typography)
            }
        };
        match opened {
            Ok(session) => {
                // Report pagination progress (and accept cancellation) for whatever this document
                // lays out from here on (#161).
                session.set_pagination_progress(Box::new(AtomicPaginationProgress));
                Ok(Box::into_raw(Box::new(session)) as jlong)
            }
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
// nativeOnTrimMemory(handle, level) — shed bounded caches under memory pressure (RR24-FR3).
// `level` is the core severity code (0 = moderate, >=1 = critical). Best-effort: a null/closed
// handle is a silent no-op (onTrimMemory can fire after the reader tore down its document).
// =====================================================================================
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeOnTrimMemory<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    level: jint,
) {
    env.with_env(|_env| -> jni::errors::Result<()> {
        if handle != 0 {
            // SAFETY: borrowed, not owned (Amendment 2).
            let session = unsafe { &mut *(handle as *mut ReaderSession) };
            session.on_trim_memory(TrimLevel::from_code(level));
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

// nativeDocTitle(handle) : String — the document's title from its metadata, or "" if none. The
// shell stores it so the library shows the real title (not the filename).
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeDocTitle<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
) -> JString<'local> {
    env.with_env(|env| -> jni::errors::Result<JString<'local>> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        env.new_string(session.metadata().title.unwrap_or_default())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// nativeDocAuthor(handle) : String — the document's author from its metadata, or "" if none.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeDocAuthor<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
) -> JString<'local> {
    env.with_env(|env| -> jni::errors::Result<JString<'local>> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        env.new_string(session.metadata().author.unwrap_or_default())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// =====================================================================================
// nativeRenderPage(handle, directBuffer) — render the current page into the direct
// ByteBuffer the shell locked. The PixelBuffer borrow never outlives this call (Amendment 5).
// NOTE: this is NOT a read-only render — it serves/populates the session's render cache
// (RR4-FR6), so it mutates session state and must run on the engine thread like every other
// handle-taking call (the session is not thread-safe).
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
// nativePrefetchPage(handle, page) — render `page` into the session's render cache WITHOUT
// displaying it, so a turn to it is a cache hit (RR24 read-ahead). Best-effort: a prefetch
// failure is swallowed (it must never disturb reading). Mutates the cache, so engine-thread only.
// =====================================================================================
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativePrefetchPage<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    page: jint,
) {
    env.with_env(|env| -> jni::errors::Result<()> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        let _ = session.prefetch_page(page.max(0) as usize); // best-effort; never throws
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
