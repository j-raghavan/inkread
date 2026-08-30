//! Lasso selection and the ink clipboard (ADR-INKREAD-0010).
//!
//! Stroke ids cross as `int[]` (u32 reinterpreted), the polygon and selection bounds as `float[]`.
//! Every mutating op autosaves in the session, so a selection edit is durable the moment it lands
//! rather than at the next explicit save.

use super::*;

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
