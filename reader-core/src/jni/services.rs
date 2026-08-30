//! Handle-free crate seams.
//!
//! The one group here that takes no document handle: the shell hands over bytes or a string, a
//! supporting crate ([`inkread_daily`], [`inkread_update`], [`inkread_opds`]) answers, and nothing
//! is retained. Grouped by that property rather than by subject — no session means no aliasing
//! rules to observe and nothing to close.

use super::*;

// nativeDailyParseFeed(xml) : String — parse an RSS/Atom feed into a JSON array of
// {title, url, published} (inkread-daily #66). Standalone (no document handle): the shell fetches
// the feed, the core parses it. Returns "[]" on junk input; never panics.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeDailyParseFeed<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    xml: JString<'local>,
) -> JString<'local> {
    env.with_env(|env| -> jni::errors::Result<JString<'local>> {
        let xml: String = xml.try_to_string(env)?;
        env.new_string(inkread_daily::parse_feed_json(&xml))
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// nativeDailyAssemble(issueJson) : bytes — assemble a daily-issue EPUB from the shell's fetched
// JSON ({title, date, articles:[{title, source, url, html}]}); the core extracts readable text from
// each article's html and composes the EPUB (inkread-daily #66). Malformed JSON throws (RR21-FR3).
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeDailyAssemble<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    issue_json: JString<'local>,
) -> JByteArray<'local> {
    env.with_env(|env| -> jni::errors::Result<JByteArray<'local>> {
        let json: String = issue_json.try_to_string(env)?;
        match inkread_daily::assemble_issue_from_json(&json) {
            Ok(bytes) => env.byte_array_from_slice(&bytes),
            Err(msg) => Err(throw(env, &CoreError::InvalidArgument(msg))),
        }
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// nativeUpdateDecide(installedVersion, releaseJson) : String — decide whether a fetched GitHub
// releases/latest payload is a newer build than the installed one (ADR-INKREAD-0014 UPD-FR2). The
// shell does the network fetch; the core only compares (semver) and returns the decision JSON
// (`{"updateAvailable":…}`). Junk in -> `{"updateAvailable":false}`, never a throw (RR21-FR3).
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeUpdateDecide<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    installed_version: JString<'local>,
    release_json: JString<'local>,
) -> JString<'local> {
    env.with_env(|env| -> jni::errors::Result<JString<'local>> {
        let installed: String = installed_version.try_to_string(env)?;
        let json: String = release_json.try_to_string(env)?;
        env.new_string(inkread_update::decide(&installed, &json))
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

// nativeOpdsParseCatalog(xml) : String — classify a fetched OPDS catalog document into the browse
// model the shell renders (ADR-INKREAD-0016 / #175): navigation vs acquisition entries, each book's
// formats ranked best-openable-first, cover art, and the paging/search links. Standalone (no
// document handle): the shell fetches the catalog and resolves the relative hrefs, the core only
// says what is in it. Junk in -> an empty catalog, never a throw (RR21-FR3).
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_jraghavan_inkread_NativeBridge_nativeOpdsParseCatalog<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    xml: JString<'local>,
) -> JString<'local> {
    env.with_env(|env| -> jni::errors::Result<JString<'local>> {
        let xml: String = xml.try_to_string(env)?;
        env.new_string(inkread_opds::parse_catalog_json(&xml))
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}
