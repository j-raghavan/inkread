//! The ink annotation seam (RR6/RR7/RR10/RR20).
//!
//! The Kotlin shell feeds stylus geometry through these; the Rust core owns the model and the
//! sidecar persistence. The live firmware-ink *render* is a separate device path and does NOT
//! cross this seam; the decision is recorded in ADR-SUPERNOTE-INK. IR-7-ALLOW: an annex-ADR id, not
//! a device assumption — the decision record's filename carries the name.

use super::*;

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

// nativeInkAddPoints(handle, float[] xy) — batched form of nativeInkAddPoint: `xy` is packed
// [x0,y0,x1,y1,…] (pressure 1.0, no tilt/timestamp). One JNI crossing per stroke instead of per
// point, cutting boundary overhead on the annotation hot path.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeInkAddPoints<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    xy: JFloatArray<'local>,
) {
    env.with_env(|env| -> jni::errors::Result<()> {
        let pts = read_f32_array(env, &xy)?;
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        session.ink_add_points(&pts).map_err(|e| throw(env, &e))?;
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

// nativeInkSetDeferredAutosave(handle, deferred) — opt into deferred-autosave mode (the shell's
// per-stroke-fsync power knob). When on, edits mark the page dirty and the shell flushes on a
// trailing-edge debounce via nativeInkSave; off (default) keeps save-on-stroke-end durability.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeInkSetDeferredAutosave<
    'local,
>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    deferred: jboolean,
) {
    env.with_env(|env| -> jni::errors::Result<()> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        session
            .set_autosave_deferred(deferred)
            .map_err(|e| throw(env, &e))?;
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

// nativeInkPages(handle) : int[] — the 0-based pages that carry ink, sorted (RR6). Drives the
// annotations list. Mutates nothing; engine-thread only (the session is not thread-safe).
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeInkPages<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
) -> JIntArray<'local> {
    env.with_env(|env| -> jni::errors::Result<JIntArray<'local>> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        let pages: Vec<jint> = session
            .ink_pages()
            .map_err(|e| throw(env, &e))?
            .into_iter()
            .map(|p| p as jint)
            .collect();
        let arr = env.new_int_array(pages.len())?;
        arr.set_region(env, 0, &pages)?;
        Ok(arr)
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
