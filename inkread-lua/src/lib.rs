//! `inkread-lua` — the embedded Lua plugin runtime (RR13 / ADR-INKREAD-0006 Decisions 1–2).
//!
//! Plugins are **front-ends over the core service layer** ([`services`]): the heavy work (render,
//! crop, contrast, reflow) lives in `reader-core` behind the [`HostServices`] ports; a plugin reads
//! state and sets parameters through the `inkread.*` API. So a Lua control (e.g. "fit to width",
//! "zoom 1.5×") and the native UI drive the *same* capability and stay in lock-step (RR12-AC2).
//!
//! `mlua` embeds Lua 5.4 via the `vendored` feature (compiled from source by `cc`, like rusqlite's
//! bundled SQLite) so it cross-compiles to `aarch64-linux-android`. No device/JNI types here (IR-4),
//! so the whole plugin↔service loop is host-testable with a mock `HostServices` (see tests).
//!
//! **Status:** L1 (embedding de-risk) + the first L2/L3 slice — `inkread.{log,document,view}` bound
//! to the service ports, exercised by a real example plugin. The capability sandbox (L3) and the
//! `.koplugin` shim (L4) build on this.

pub mod koreader;
pub mod services;

use std::rc::Rc;
use std::sync::{Arc, Mutex};

use mlua::{Function, Lua, Result as LuaResult};

pub use services::{DocumentService, HostServices, UiService, ViewService};

/// The plugin runtime's public error — a message, **decoupled from `mlua`** so dependents
/// (`reader-core`) need not depend on `mlua` to handle plugin failures (a syntax error, an
/// unsupported KOReader API in strict mode, or a runtime error inside a plugin).
#[derive(Debug, Clone)]
pub struct PluginError(pub String);

impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for PluginError {}

impl From<mlua::Error> for PluginError {
    fn from(e: mlua::Error) -> Self {
        PluginError(e.to_string())
    }
}

/// Result alias for the plugin runtime's public API (KOReader shim, etc.).
pub type PluginResult<T> = Result<T, PluginError>;

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

/// An embedded Lua runtime hosting plugin code, bound to the core [`HostServices`].
///
/// Single-threaded by design: the host owns it on the reader/engine thread (the same thread the
/// session renders on), so services are held by `Rc` and need not be `Send`/`Sync`.
pub struct PluginHost {
    lua: Lua,
    log: LogSink,
}

impl PluginHost {
    /// Build a runtime with the `inkread` API table installed over `services`.
    ///
    /// Exposes `inkread.log`, `inkread.document.{page_count,current_page,page_aspect}`, and
    /// `inkread.view.{viewport,zoom,set_zoom}` — enough for the first real controls (fit / zoom).
    pub fn new(services: Rc<dyn HostServices>) -> LuaResult<Self> {
        let lua = Lua::new();
        let log = LogSink::default();
        let inkread = lua.create_table()?;

        // inkread.log(msg)
        let sink = log.clone();
        inkread.set(
            "log",
            lua.create_function(move |_, msg: String| {
                sink.push(msg);
                Ok(())
            })?,
        )?;

        // inkread.document.*
        let doc = lua.create_table()?;
        let s = services.clone();
        doc.set(
            "page_count",
            lua.create_function(move |_, ()| Ok(s.document().page_count()))?,
        )?;
        let s = services.clone();
        doc.set(
            "current_page",
            lua.create_function(move |_, ()| Ok(s.document().current_page()))?,
        )?;
        let s = services.clone();
        doc.set(
            "page_aspect",
            lua.create_function(move |_, page: usize| Ok(s.document().page_aspect(page)))?,
        )?;
        inkread.set("document", doc)?;

        // inkread.view.*
        let view = lua.create_table()?;
        let s = services.clone();
        view.set(
            "viewport",
            lua.create_function(move |_, ()| Ok(s.view().viewport()))?,
        )?;
        let s = services.clone();
        view.set(
            "zoom",
            lua.create_function(move |_, ()| Ok(s.view().zoom()))?,
        )?;
        let s = services.clone();
        view.set(
            "set_zoom",
            lua.create_function(move |_, (z, px, py): (f32, f32, f32)| {
                s.view().set_zoom(z, px, py);
                Ok(())
            })?,
        )?;
        inkread.set("view", view)?;

        // inkread.ui.*
        let ui = lua.create_table()?;
        let s = services.clone();
        ui.set(
            "show_message",
            lua.create_function(move |_, text: String| {
                s.ui().show_message(&text);
                Ok(())
            })?,
        )?;
        inkread.set("ui", ui)?;

        lua.globals().set("inkread", inkread)?;
        Ok(Self { lua, log })
    }

    /// The log sink, for the host to read what the plugin emitted.
    #[must_use]
    pub fn log_sink(&self) -> &LogSink {
        &self.log
    }

    /// The embedded Lua state — used by the KOReader compat layer in this crate to install its
    /// prelude and drive a loaded plugin. Crate-internal (plugins reach the host only via `inkread.*`).
    pub(crate) fn lua(&self) -> &Lua {
        &self.lua
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
    use std::cell::Cell;

    /// A mock host: a fixed document + a recorded zoom, so a plugin's calls are observable.
    struct MockHost {
        doc: MockDoc,
        view: MockView,
        ui: MockUi,
    }
    #[derive(Default)]
    struct MockUi {
        messages: std::cell::RefCell<Vec<String>>,
    }
    impl UiService for MockUi {
        fn show_message(&self, text: &str) {
            self.messages.borrow_mut().push(text.to_string());
        }
    }
    struct MockDoc {
        pages: usize,
        current: usize,
        aspect: f32,
    }
    struct MockView {
        size: (u32, u32),
        zoom: Cell<f32>,
        pan: Cell<(f32, f32)>,
    }
    impl DocumentService for MockDoc {
        fn page_count(&self) -> usize {
            self.pages
        }
        fn current_page(&self) -> usize {
            self.current
        }
        fn page_aspect(&self, page: usize) -> Option<f32> {
            (page < self.pages).then_some(self.aspect)
        }
    }
    impl ViewService for MockView {
        fn viewport(&self) -> (u32, u32) {
            self.size
        }
        fn zoom(&self) -> f32 {
            self.zoom.get()
        }
        fn set_zoom(&self, zoom: f32, pan_x: f32, pan_y: f32) {
            self.zoom.set(zoom);
            self.pan.set((pan_x, pan_y));
        }
    }
    impl HostServices for MockHost {
        fn document(&self) -> &dyn DocumentService {
            &self.doc
        }
        fn view(&self) -> &dyn ViewService {
            &self.view
        }
        fn ui(&self) -> &dyn UiService {
            &self.ui
        }
    }

    fn mock() -> Rc<MockHost> {
        Rc::new(MockHost {
            doc: MockDoc {
                pages: 10,
                current: 3,
                aspect: 0.75,
            },
            view: MockView {
                size: (1000, 1200),
                zoom: Cell::new(1.0),
                pan: Cell::new((0.0, 0.0)),
            },
            ui: MockUi::default(),
        })
    }

    #[test]
    fn runs_a_plugin_script_and_captures_log_output() {
        let host = PluginHost::new(mock()).unwrap();
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
        let host = PluginHost::new(mock()).unwrap();
        host.load("x = 1").unwrap();
        host.call_hook("on_load").unwrap(); // not defined → no error
        assert!(host.log_sink().messages().is_empty());
    }

    #[test]
    fn lua_runtime_evaluates_real_lua() {
        // Proves the vendored Lua 5.4 VM actually executes (not a stub): arithmetic + string lib.
        let host = PluginHost::new(mock()).unwrap();
        host.load(r#"inkread.log(tostring(2 + 3) .. "-" .. string.upper("ok"))"#)
            .unwrap();
        assert_eq!(host.log_sink().messages(), vec!["5-OK".to_string()]);
    }

    #[test]
    fn plugin_reads_document_state_through_the_service_port() {
        let host = PluginHost::new(mock()).unwrap();
        host.load(
            r#"
            function report()
                local p = inkread.document.current_page()
                local n = inkread.document.page_count()
                inkread.log("page " .. (p + 1) .. "/" .. n)
            end
        "#,
        )
        .unwrap();
        host.call_hook("report").unwrap();
        assert_eq!(host.log_sink().messages(), vec!["page 4/10".to_string()]);
    }

    #[test]
    fn zoom_preset_plugin_drives_the_view_service_end_to_end() {
        // The first *real* control as a plugin: a "zoom preset" reads the viewport, sets a centred
        // zoom through the service port — proving plugin → core capability works (the dogfood loop).
        let host = mock();
        let plugin = PluginHost::new(host.clone()).unwrap();
        plugin
            .load(
                r#"
                function zoom_preset(factor)
                    local w, h = inkread.view.viewport()
                    inkread.log("viewport " .. w .. "x" .. h)
                    inkread.view.set_zoom(factor, 0.5, 0.5) -- centred
                end
            "#,
            )
            .unwrap();
        // Call the plugin function with an argument (exercises FromLuaMulti args).
        let f: Function = plugin.lua.globals().get("zoom_preset").unwrap();
        f.call::<()>(2.0f32).unwrap();

        assert_eq!(plugin.log_sink().messages(), vec!["viewport 1000x1200"]);
        assert_eq!(host.view.zoom.get(), 2.0, "plugin set zoom via the service");
        assert_eq!(host.view.pan.get(), (0.5, 0.5));
    }

    #[test]
    fn page_aspect_returns_nil_for_out_of_range() {
        let host = PluginHost::new(mock()).unwrap();
        host.load(
            r#"
            function check()
                local a = inkread.document.page_aspect(999) -- out of range → nil
                inkread.log(a == nil and "nil" or tostring(a))
            end
        "#,
        )
        .unwrap();
        host.call_hook("check").unwrap();
        assert_eq!(host.log_sink().messages(), vec!["nil".to_string()]);
    }
}
