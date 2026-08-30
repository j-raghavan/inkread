//! View settings, typography, and reflow (RR2-FR5, RR4, RR8).
//!
//! Everything the Adjust sheet drives — zoom, fit, crop, contrast, night mode, render quality,
//! viewport — plus the reflow controls (text scale, typeface, line spacing, columns, alignment,
//! margins) and the font registry the reader's own imported faces land in.
//!
//! The pagination progress counters live here too, with the settings that trigger a repagination.

use super::*;

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

// nativeSetContrast(handle, step) — display-enhancement contrast (0 = off; RR4). Applied as a
// post-render pixel remap; the shell re-renders afterward. Never throws (clamped in the core).
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeSetContrast<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    step: jint,
) {
    env.with_env(|env| -> jni::errors::Result<()> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        session.set_contrast(u8::try_from(step.max(0)).unwrap_or(u8::MAX));
        Ok(())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// nativeSetNight(handle, on) — night mode: invert the page after contrast (RR4). The shell
// re-renders afterward. Never throws.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeSetNight<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    on: jboolean,
) {
    env.with_env(|env| -> jni::errors::Result<()> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        session.set_night(on);
        Ok(())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// nativeSetFit(handle, mode) — page fit mode (0=Page/contain, 1=Width, 2=Height; RR4). Aspect-
// preserving; the shell re-renders afterward. Never throws (mode decoded leniently).
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeSetFit<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    mode: jint,
) {
    env.with_env(|env| -> jni::errors::Result<()> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        session.set_fit(crate::document::FitMode::from_code(mode));
        Ok(())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// nativeSetRenderQuality(handle, q) — render quality (0=low, 1=default, 2=high; RR4). High
// supersamples then downscales for smoother e-ink text. Re-render after. Never throws (clamped).
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeSetRenderQuality<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    q: jint,
) {
    env.with_env(|env| -> jni::errors::Result<()> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        session.set_render_quality(u8::try_from(q.max(0)).unwrap_or(u8::MAX));
        Ok(())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// nativeSetCrop(handle, auto, marginStep) — auto-crop white margins (RR4). auto!=0 enables it;
// marginStep (0..8, 1%-of-page each) keeps a margin around the detected content. Re-render after.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeSetCrop<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    auto: jint,
    margin_step: jint,
) {
    env.with_env(|env| -> jni::errors::Result<()> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        session.set_crop_auto(auto != 0);
        session.set_crop_margin(u8::try_from(margin_step.max(0)).unwrap_or(u8::MAX));
        Ok(())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// nativeSetViewport(handle, width, height, dpi) — update the render viewport after a surface
// resize / screen rotation (RR21-FR4). Without this the core keeps the open-time viewport and a
// render into the new (resized) buffer is rejected as a size mismatch. PDF re-renders at the new
// size; EPUB repaginates on the next render. The shell re-renders afterward.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeSetViewport<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    width: jint,
    height: jint,
    dpi: jint,
) {
    env.with_env(|env| -> jni::errors::Result<()> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        let viewport = read_viewport(env, width, height, dpi)?;
        session.set_viewport(viewport);
        Ok(())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// nativeSetTextScale(handle, scale) : int — set reflow font size (1.0 = default) for an EPUB;
// repaginates, preserving the chapter. Returns the new current page index, or -1 for a fixed-layout
// document (PDF) that does not reflow. The shell re-renders afterward (RR2-FR5).
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeSetTextScale<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    scale: jfloat,
) -> jint {
    env.with_env(|env| -> jni::errors::Result<jint> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        if session.set_text_scale(scale) {
            Ok(session.current_page() as jint)
        } else {
            Ok(-1)
        }
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// nativeSetFont(handle, fontId) : int — reflow font family (RR4); repaginates EPUB. Returns the new
// page index, or -1 for a fixed-layout PDF. Re-render after.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeSetFont<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    font_id: jint,
) -> jint {
    env.with_env(|env| -> jni::errors::Result<jint> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        if session.set_font(font_id) {
            Ok(session.current_page() as jint)
        } else {
            Ok(-1)
        }
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// nativeFontNames() : String — the bundled reading-face display names, newline-joined, in id order
// (the index = the font_id for nativeSetFont). Static; no handle. Drives the font picker.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeFontNames<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
) -> JString<'local> {
    env.with_env(|env| -> jni::errors::Result<JString<'local>> {
        env.new_string(inkread_epub::reading_font_names().join("\n"))
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// nativeRegisterReadingFont(name, fontBytes) : int — register a user reading face (RR28-FR3) so it
// appears in the picker after the bundled families; returns its font_id for nativeSetFont, or -1 if
// the bytes don't parse. Static; no handle — the registry is process-wide, so the shell registers
// its `fonts/` directory once at startup, in a stable (sorted) order, because ids are positional.
// Never throws (RR21-FR3).
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeRegisterReadingFont<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    name: JString<'local>,
    font_bytes: JByteArray<'local>,
) -> jint {
    env.with_env(|env| -> jni::errors::Result<jint> {
        let id = match (name.try_to_string(env), env.convert_byte_array(&font_bytes)) {
            (Ok(name), Ok(bytes)) => inkread_epub::register_reading_font(name, bytes)
                .and_then(|id| i32::try_from(id).ok()),
            _ => None, // an unreadable name or bytes is invalid input, not an error
        };
        Ok(id.unwrap_or(-1))
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// nativeClearReadingFonts() : void — forget every registered user reading face, so the shell can
// re-register its `fonts/` directory after an import or a removal. Static; no handle.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeClearReadingFonts<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
) {
    env.with_env(|_env| -> jni::errors::Result<()> {
        inkread_epub::clear_reading_fonts();
        Ok(())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>();
}

// nativeRegisterFallbackFont(fontBytes, ttcIndex) : boolean — register a runtime fallback face
// (raw TTF/OTF/TTC bytes, e.g. a device CJK font) consulted for glyphs the reading faces lack, for
// documents opened after the call. `ttcIndex` selects the face inside a TrueType collection (0 for
// a plain TTF/OTF). Static; no handle — the chain is process-wide, so the shell registers once at
// startup. Returns false — never throws — for bytes that don't parse or an out-of-range index
// (RR21-FR3: validate at the boundary).
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeRegisterFallbackFont<
    'local,
>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    font_bytes: JByteArray<'local>,
    ttc_index: jint,
) -> jni::sys::jboolean {
    env.with_env(|env| -> jni::errors::Result<jni::sys::jboolean> {
        // A failed array conversion (null array etc.) also maps to `false` rather than `?`-ing
        // into a thrown RuntimeException — keeps the "never throws" contract airtight.
        let ok = match (
            env.convert_byte_array(&font_bytes),
            u32::try_from(ttc_index),
        ) {
            (Ok(bytes), Ok(index)) => inkread_epub::register_fallback_font(bytes, index),
            _ => false, // unreadable bytes / negative index are invalid input, not an error
        };
        Ok(if ok {
            jni::sys::JNI_TRUE
        } else {
            jni::sys::JNI_FALSE
        })
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// nativeSetLineSpacing(handle, mult) : int — reflow line spacing (RR4); repaginates EPUB. Returns
// the new page index, or -1 for a fixed-layout PDF. Re-render after.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeSetLineSpacing<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    mult: jfloat,
) -> jint {
    env.with_env(|env| -> jni::errors::Result<jint> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        if session.set_line_spacing(mult) {
            Ok(session.current_page() as jint)
        } else {
            Ok(-1)
        }
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// nativeEffectiveColumns(handle) : int — columns the layout is ACTUALLY using, which a narrow page
// can reduce to 1 whatever was asked for (#194). Lets the shell say the request was declined.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeEffectiveColumns<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
) -> jint {
    env.with_env(|env| -> jni::errors::Result<jint> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        Ok(session.effective_columns())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// nativeSetColumns(handle, columns) : int — reflow columns (1 or 2; #194); repaginates EPUB.
// Returns the new page index, or -1 for a fixed-layout PDF. Re-render after.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeSetColumns<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    columns: jint,
) -> jint {
    env.with_env(|env| -> jni::errors::Result<jint> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        if session.set_columns(columns) {
            Ok(session.current_page() as jint)
        } else {
            Ok(-1)
        }
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// nativeSetAlignment(handle, code) : int — reflow alignment (0=Left,1=Justify,2=Center,3=Right; RR4);
// repaginates EPUB. Returns the new page index, or -1 for a fixed-layout PDF. Re-render after.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeSetAlignment<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    code: jint,
) -> jint {
    env.with_env(|env| -> jni::errors::Result<jint> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        if session.set_alignment(code) {
            Ok(session.current_page() as jint)
        } else {
            Ok(-1)
        }
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// nativePaginationProgress() : long — chapters laid out so far, packed as `(done << 32) | total`.
// `total == 0` means nothing is in flight. Static and lock-free: the shell polls this from the UI
// thread while the engine thread is inside a repagination.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativePaginationProgress<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
) -> jlong {
    env.with_env(|_env| -> jni::errors::Result<jlong> {
        let done = PAGINATION_DONE.load(Ordering::Relaxed) as u64;
        let total = PAGINATION_TOTAL.load(Ordering::Relaxed) as u64;
        Ok((((done & 0xFFFF_FFFF) << 32) | (total & 0xFFFF_FFFF)) as jlong)
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// nativeCancelPagination(cancel) : void — ask the pagination in flight to stop (true), or clear the
// flag before starting one (false). A cancelled re-layout leaves the reader on the pagination they
// already had; the first pagination of a book ignores this, since there is nothing to fall back to.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeCancelPagination<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    cancel: jni::sys::jboolean,
) {
    env.with_env(|_env| -> jni::errors::Result<()> {
        let on = cancel == jni::sys::JNI_TRUE;
        PAGINATION_CANCEL.store(on, Ordering::Relaxed);
        if !on {
            // Starting a fresh pagination: clear the previous one's counters so a stale
            // "58/60" is never shown against the new run.
            PAGINATION_DONE.store(0, Ordering::Relaxed);
            PAGINATION_TOTAL.store(0, Ordering::Relaxed);
        }
        Ok(())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>();
}

// nativeSetTypography(handle, scale, fontId, lineSpacing, alignCode, columns, marginPct) : int —
// apply every reflow typography setting and repaginate ONCE (RR4). The open path restores persisted
// settings with this instead of one call each: one repagination instead of six, which on a large book is
// the difference between opening in seconds and opening in minutes (#161/#162). Returns the new
// page index, or -1 for a fixed-layout PDF. Re-render after.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeSetTypography<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    scale: jfloat,
    font_id: jint,
    line_spacing: jfloat,
    align_code: jint,
    columns: jint,
    margin_pct: jint,
) -> jint {
    env.with_env(|env| -> jni::errors::Result<jint> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        if session.set_typography(
            scale,
            font_id,
            line_spacing,
            align_code,
            columns,
            margin_pct,
        ) {
            Ok(session.current_page() as jint)
        } else {
            Ok(-1)
        }
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// nativeSetMargin(handle, marginPct) : int — reflow page margin as a percentage of page width
// (RR16-FR2 / #167); repaginates EPUB. Out-of-range values are clamped by the backend, not rejected
// (RR21-FR3). Returns the new page index, or -1 for a fixed-layout PDF. Re-render after.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeSetMargin<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    margin_pct: jint,
) -> jint {
    env.with_env(|env| -> jni::errors::Result<jint> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        if session.set_margin(margin_pct) {
            Ok(session.current_page() as jint)
        } else {
            Ok(-1)
        }
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// nativeSupportsReflow(handle) : boolean — whether the open document can be reflowed (a text-layer
// PDF). The shell uses this to enable/disable the Reflow control (ADR-INKREAD-0011).
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeSupportsReflow<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
) -> jboolean {
    env.with_env(|env| -> jni::errors::Result<jboolean> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        Ok(session.supports_reflow())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// nativeIsMagnifiable(handle) : boolean — whether the CURRENT view honors zoom (a fixed-layout page
// that is not reflowed, RR25-FR3). The shell gates every zoom entry point on this so a pinch /
// double-tap on a reflowable view can't strand its zoom factor and skew tap hit-testing.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeIsMagnifiable<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
) -> jboolean {
    env.with_env(|env| -> jni::errors::Result<jboolean> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        Ok(session.is_magnifiable())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// nativeSetReflow(handle, on) : int — toggle reflow mode on a text-layer PDF (ADR-INKREAD-0011):
// reconstruct the page text and flow it like a book so font/spacing/alignment take effect; off
// restores the fixed page. Returns the new current page index (page count changes across the
// toggle), or -1 if reflow is unavailable (no text layer / unsupported). Re-render afterward.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeSetReflow<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    on: jboolean,
) -> jint {
    env.with_env(|env| -> jni::errors::Result<jint> {
        let session = unsafe { session_mut(handle) }.map_err(|e| throw(env, &e))?;
        if session.set_reflow(on) {
            Ok(session.current_page() as jint)
        } else {
            Ok(-1)
        }
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}
