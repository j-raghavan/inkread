//! Plugin integration — the `reader-core` **adapter** for the `inkread-lua` runtime (RR13/RR14 /
//! ADR-INKREAD-0006 Decision 1).
//!
//! `inkread-lua` defines the [`HostServices`] *ports*; this module is the adapter that satisfies
//! them over the live reader. The bridge is a small **shared context** ([`PluginContext`]) rather
//! than the whole [`ReaderSession`]: the session pushes a snapshot of reader state into it
//! ([`PluginManager::sync`]), and the plugin-facing services read it. This avoids a self-reference
//! cycle (session → runtime → services → session) and keeps the core vendor-neutral (IR-7) — a
//! plugin's `inkread.ui.show_message` just **enqueues** a message into an outbox the shell drains,
//! it never draws.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use inkread_lua::koreader::KoPluginRuntime;
use inkread_lua::{DocumentService, HostServices, UiService, ViewService};

use crate::error::{CoreError, CoreResult};

/// The snapshot of reader state the plugin API reads, plus the channels back to the reader (a zoom
/// request and a UI-message outbox). Refreshed by [`PluginManager::sync`] before each plugin call.
#[derive(Debug, Default, Clone)]
struct PluginState {
    page_count: usize,
    current_page: usize,
    viewport: (u32, u32),
    zoom: f32,
    /// Aspect (w/h) of the current page, if known (for fit/zoom plugins). `None` otherwise.
    current_aspect: Option<f32>,
    /// A plugin's pending `view.set_zoom(zoom, pan_x, pan_y)` request for the session to apply.
    requested_zoom: Option<(f32, f32, f32)>,
    /// Messages a plugin asked to show (`inkread.ui.show_message`), drained by the shell.
    ui_outbox: Vec<String>,
}

/// A cheap, cloneable handle to the shared plugin state (single-threaded; reader thread only).
#[derive(Clone, Default)]
struct PluginContext(Rc<RefCell<PluginState>>);

/// The adapter object: implements every `inkread-lua` service port by reading/writing the shared
/// [`PluginContext`]. One instance is shared (as `Rc<dyn HostServices>`) with the runtime.
struct ContextServices {
    ctx: PluginContext,
}

impl DocumentService for ContextServices {
    fn page_count(&self) -> usize {
        self.ctx.0.borrow().page_count
    }
    fn current_page(&self) -> usize {
        self.ctx.0.borrow().current_page
    }
    fn page_aspect(&self, page: usize) -> Option<f32> {
        let s = self.ctx.0.borrow();
        // We snapshot only the current page's aspect; other pages are unknown for now.
        (page == s.current_page)
            .then_some(s.current_aspect)
            .flatten()
    }
}

impl ViewService for ContextServices {
    fn viewport(&self) -> (u32, u32) {
        self.ctx.0.borrow().viewport
    }
    fn zoom(&self) -> f32 {
        self.ctx.0.borrow().zoom
    }
    fn set_zoom(&self, zoom: f32, pan_x: f32, pan_y: f32) {
        self.ctx.0.borrow_mut().requested_zoom = Some((zoom, pan_x, pan_y));
    }
}

impl UiService for ContextServices {
    fn show_message(&self, text: &str) {
        self.ctx.0.borrow_mut().ui_outbox.push(text.to_string());
    }
}

impl HostServices for ContextServices {
    fn document(&self) -> &dyn DocumentService {
        self
    }
    fn view(&self) -> &dyn ViewService {
        self
    }
    fn ui(&self) -> &dyn UiService {
        self
    }
}

/// Owns the KOReader-compatible Lua runtime and the shared context. Held by [`ReaderSession`].
pub struct PluginManager {
    runtime: KoPluginRuntime,
    ctx: PluginContext,
}

impl PluginManager {
    /// Build a manager with a fresh runtime bound to a shared context.
    pub fn new() -> CoreResult<Self> {
        let ctx = PluginContext::default();
        let services: Rc<dyn HostServices> = Rc::new(ContextServices { ctx: ctx.clone() });
        let runtime = KoPluginRuntime::new(services).map_err(to_core_err)?;
        Ok(Self { runtime, ctx })
    }

    /// Load a KOReader plugin from its `_meta.lua` + `main.lua` sources and run its lifecycle.
    pub fn load_koplugin(&self, meta_src: &str, main_src: &str) -> CoreResult<()> {
        self.runtime
            .load_koplugin(meta_src, main_src)
            .map_err(to_core_err)
    }

    /// Load a `.koplugin` directory (`<dir>/_meta.lua` + `<dir>/main.lua`).
    pub fn load_koplugin_dir(&self, dir: &Path) -> CoreResult<()> {
        let meta = std::fs::read_to_string(dir.join("_meta.lua")).unwrap_or_default();
        let main = std::fs::read_to_string(dir.join("main.lua"))
            .map_err(|e| CoreError::Plugin(format!("read {}/main.lua: {e}", dir.display())))?;
        self.load_koplugin(&meta, &main)
    }

    /// The loaded plugin's main-menu items as `(key, label)` pairs.
    pub fn menu_items(&self) -> Vec<(String, String)> {
        self.runtime.menu_items().unwrap_or_default()
    }

    /// Fire a menu item's callback by key; returns whether it ran.
    pub fn invoke_menu_item(&self, key: &str) -> CoreResult<bool> {
        self.runtime.invoke_menu_item(key).map_err(to_core_err)
    }

    /// Push the current reader state into the shared context before a plugin call.
    pub fn sync(
        &self,
        page_count: usize,
        current_page: usize,
        viewport: (u32, u32),
        zoom: f32,
        current_aspect: Option<f32>,
    ) {
        let mut s = self.ctx.0.borrow_mut();
        s.page_count = page_count;
        s.current_page = current_page;
        s.viewport = viewport;
        s.zoom = zoom;
        s.current_aspect = current_aspect;
    }

    /// Take a plugin's pending zoom request, if any (the session applies it).
    pub fn take_requested_zoom(&self) -> Option<(f32, f32, f32)> {
        self.ctx.0.borrow_mut().requested_zoom.take()
    }

    /// Drain the queued plugin UI messages (the shell shows them, e.g. as toasts).
    pub fn drain_ui_messages(&self) -> Vec<String> {
        std::mem::take(&mut self.ctx.0.borrow_mut().ui_outbox)
    }
}

/// Map a plugin-runtime error onto the core's typed plugin error (never panics across the boundary).
fn to_core_err(e: inkread_lua::PluginError) -> CoreError {
    CoreError::Plugin(e.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A faithful slice of KOReader's `hello` plugin, exercised through the reader-core adapter.
    const HELLO_MAIN: &str = r#"
        local InfoMessage = require("ui/widget/infomessage")
        local UIManager = require("ui/uimanager")
        local WidgetContainer = require("ui/widget/container/widgetcontainer")
        local _ = require("gettext")
        local Hello = WidgetContainer:extend{ name = "hello" }
        function Hello:init() self.ui.menu:registerToMainMenu(self) end
        function Hello:addToMainMenu(menu_items)
            menu_items.hello_world = {
                text = _("Hello World"),
                callback = function()
                    UIManager:show(InfoMessage:new{ text = _("Hello, plugin world") })
                end,
            }
        end
        return Hello
    "#;

    #[test]
    fn manager_loads_hello_and_routes_ui_message() {
        let pm = PluginManager::new().unwrap();
        pm.load_koplugin("", HELLO_MAIN).unwrap();

        let items = pm.menu_items();
        assert_eq!(items, vec![("hello_world".into(), "Hello World".into())]);

        assert!(pm.invoke_menu_item("hello_world").unwrap());
        assert_eq!(
            pm.drain_ui_messages(),
            vec!["Hello, plugin world".to_string()]
        );
        // Drained: a second drain is empty.
        assert!(pm.drain_ui_messages().is_empty());
    }

    #[test]
    fn plugin_reads_synced_state_and_requests_zoom() {
        let pm = PluginManager::new().unwrap();
        pm.sync(42, 7, (1000, 1200), 1.0, Some(0.75));
        pm.load_koplugin(
            "",
            r#"
            local WidgetContainer = require("ui/widget/container/widgetcontainer")
            local _ = require("gettext")
            local P = WidgetContainer:extend{ name = "zoomy" }
            function P:init() self.ui.menu:registerToMainMenu(self) end
            function P:addToMainMenu(items)
                items.zoom2x = { text = _("Zoom 2x"), callback = function()
                    inkread.log("page " .. (inkread.document.current_page()+1) .. "/" .. inkread.document.page_count())
                    inkread.view.set_zoom(2.0, 0.5, 0.5)
                end }
            end
            return P
        "#,
        )
        .unwrap();
        assert!(pm.invoke_menu_item("zoom2x").unwrap());
        assert_eq!(pm.take_requested_zoom(), Some((2.0, 0.5, 0.5)));
        // The plugin saw the synced document facts.
        assert!(pm
            .runtime
            .log_sink()
            .messages()
            .contains(&"page 8/42".to_string()));
    }

    #[test]
    fn unsupported_api_is_a_typed_plugin_error() {
        let pm = PluginManager::new().unwrap();
        let err = pm
            .load_koplugin("", r#"require("ffi/framebuffer") return {}"#)
            .unwrap_err();
        assert!(matches!(err, CoreError::Plugin(_)));
        assert!(err.to_string().contains("not supported"));
    }
}
