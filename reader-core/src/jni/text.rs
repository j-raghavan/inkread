//! Text selection and the dictionary seam (RR11/RR12, ADR-INKREAD-0009 D3).
//!
//! The shell turns a tap or drag into a selection, then looks the word up in the on-device corpus.
//! An on-device miss is the shell's cue to try its opt-in online source and cache the result back
//! through `nativeDictPut`.

use super::*;

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

// nativeTextLineSpan(handle, page, sx,sy, ex,ey) : bytes — reading-order selection a drag sweeps
// from the start point (sx,sy) to the lift point (ex,ey), the multi-line drag. Whole lines from the
// start line through the line before the lift; the lift line clipped to the word under ex; gaps
// between line boxes filled. Decode with WireCodec.decodeSelection.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeTextLineSpan<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    page: jint,
    sx: jfloat,
    sy: jfloat,
    ex: jfloat,
    ey: jfloat,
) -> JByteArray<'local> {
    env.with_env(|env| -> jni::errors::Result<JByteArray<'local>> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        let target = if page < 0 { 0usize } else { page as usize };
        let sel = session.text_line_span(target, (sx, sy), (ex, ey));
        env.byte_array_from_slice(&encode_selection_wire(&sel))
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// nativeSelectionPins(handle, page, x0,y0,x1,y1) : String — the reflow-stable [start,end] PinPosition
// pair a selection rect covers on a reflowable page (the Digest anchor, #46). Returns a JSON object
// `{"start":<pin>,"end":<pin>}`, or an EMPTY string for fixed-layout PDF / an empty selection (the
// caller then falls back to a page anchor). Anchors to text locations, not pixels.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeSelectionPins<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    page: jint,
    x0: jfloat,
    y0: jfloat,
    x1: jfloat,
    y1: jfloat,
) -> JString<'local> {
    env.with_env(|env| -> jni::errors::Result<JString<'local>> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        let target = if page < 0 { 0usize } else { page as usize };
        let json = match session.selection_pins(target, NormRect { x0, y0, x1, y1 }) {
            // PageRange serializes to exactly `{"start":{…},"end":{…}}` (primitive-only → infallible);
            // reuse it rather than hand-building the JSON.
            Some((start, end)) => {
                serde_json::to_string(&crate::position::PageRange::new(start, end))
                    .unwrap_or_default()
            }
            None => String::new(),
        };
        env.new_string(json)
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

/// Find `query` on `page` (RR2 in-document search). Returns the search wire (decode:
/// `WireCodec.decodeSearch`): the page's matches as snippet + highlight boxes. The shell calls this
/// page-by-page so the scan stays memory-bounded.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeSearchPage<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    page: jint,
    query: JString<'local>,
) -> JByteArray<'local> {
    env.with_env(|env| -> jni::errors::Result<JByteArray<'local>> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        let query: String = query.try_to_string(env)?;
        let target = if page < 0 { 0usize } else { page as usize };
        let matches = session.search_page(target, &query);
        env.byte_array_from_slice(&encode_search_wire(&matches))
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
