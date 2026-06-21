//! `inkread-lua` — the embedded Lua plugin runtime (RR13 / ADR-INKREAD-0006 Decision 2).
//!
//! **Phase L1 (de-risk):** prove the `mlua` embedding (vendored Lua 5.4, compiled from source so it
//! cross-compiles to `aarch64-linux-android` like rusqlite's bundled SQLite) builds and runs a real
//! plugin script end to end. The full `inkread.{document,selection,annotations,ui,settings,storage,
//! network}` API modules and the capability sandbox arrive in L2/L3 over the core **service layer**;
//! L1 ships only `inkread.log` to exercise the host↔Lua seam.
//!
//! This crate holds **no device/JNI types** (IR-4): plugins reach the core only through services, so
//! the runtime stays host-testable without hardware.

use std::sync::{Arc, Mutex};

use mlua::{Function, Lua, Result as LuaResult};

/// A sink the host reads plugin log output from — a test seam now, the plugin console later.
#[derive(Clone, Default)]
pub struct LogSink(Arc<Mutex<Vec<String>>>);

impl LogSink {
    /// A snapshot of the messages logged so far (poison-safe: returns empty on a poisoned lock).
    #[must_use]
    pub fn messages(&self) -> Vec<String> {
        self.0.lock().map(|g| g.clone()).unwrap_or_default()
    }

    fn push(&self, msg: String) {
        if let Ok(mut g) = self.0.lock() {
            g.push(msg);
        }
    }
}

/// An embedded Lua runtime hosting plugin code. L1 installs a minimal `inkread` global table; later
/// phases bind `inkread.*` modules to core services behind a capability sandbox.
pub struct PluginHost {
    lua: Lua,
    log: LogSink,
}

impl PluginHost {
    /// Build a runtime with the `inkread` API table installed (L1: just `inkread.log`).
    pub fn new() -> LuaResult<Self> {
        let lua = Lua::new();
        let log = LogSink::default();

        let inkread = lua.create_table()?;
        let sink = log.clone();
        let log_fn = lua.create_function(move |_, msg: String| {
            sink.push(msg);
            Ok(())
        })?;
        inkread.set("log", log_fn)?;
        lua.globals().set("inkread", inkread)?;

        Ok(Self { lua, log })
    }

    /// The log sink, for the host to read what the plugin emitted.
    #[must_use]
    pub fn log_sink(&self) -> &LogSink {
        &self.log
    }

    /// Load + execute a plugin's source (defining its functions and lifecycle hooks).
    pub fn load(&self, src: &str) -> LuaResult<()> {
        self.lua.load(src).exec()
    }

    /// Call a global no-argument lifecycle hook by `name` if the plugin defines one (e.g. `on_load`).
    /// A missing hook is a no-op (not an error) so plugins opt into only the hooks they need.
    pub fn call_hook(&self, name: &str) -> LuaResult<()> {
        if let Ok(f) = self.lua.globals().get::<Function>(name) {
            f.call::<()>(())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_a_plugin_script_and_captures_log_output() {
        let host = PluginHost::new().unwrap();
        host.load(
            r#"
            function on_load()
                inkread.log("hello from lua")
                inkread.log("plugin loaded")
            end
        "#,
        )
        .unwrap();
        host.call_hook("on_load").unwrap();
        assert_eq!(
            host.log_sink().messages(),
            vec!["hello from lua".to_string(), "plugin loaded".to_string()]
        );
    }

    #[test]
    fn missing_hook_is_a_noop() {
        let host = PluginHost::new().unwrap();
        host.load("x = 1").unwrap();
        host.call_hook("on_load").unwrap(); // not defined → no error
        assert!(host.log_sink().messages().is_empty());
    }

    #[test]
    fn lua_runtime_evaluates_real_lua() {
        // Proves the vendored Lua 5.4 VM actually executes (not a stub): arithmetic + string lib.
        let host = PluginHost::new().unwrap();
        host.load(r#"inkread.log(tostring(2 + 3) .. "-" .. string.upper("ok"))"#)
            .unwrap();
        assert_eq!(host.log_sink().messages(), vec!["5-OK".to_string()]);
    }
}
