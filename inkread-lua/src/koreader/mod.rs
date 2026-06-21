//! KOReader `.koplugin` compatibility shim (RR14 / ADR-INKREAD-0006 Decision 4).
//!
//! Loads a KOReader plugin (`_meta.lua` + `main.lua`) into [`crate::PluginHost`] over a pure-Lua
//! **compat prelude** (`prelude.lua`) that emulates the common-core KOReader API on top of inkread's
//! `inkread.*` services. The shim is **selective**: it supports a curated surface and **fails
//! loudly** on anything else (strict `require`), with a probe mode that records gaps for the
//! compatibility matrix (`docs/koreader-compatibility-matrix.md`). It adds no new core capability —
//! only a second dialect over the same services, so the sandbox stays intact.
//!
//! How we *guarantee* coverage (the "make sure" method): a curated set of target plugins is loaded
//! by conformance tests; every symbol they touch is implemented here; anything outside errors
//! loudly; CI runs the conformance suite. Probe mode mechanically extracts a plugin's exact API
//! surface so the matrix is derived from real plugins, not guessed.

use std::rc::Rc;

use mlua::{Function, Result as LuaResult, Table};

use crate::{HostServices, LogSink, PluginHost};

/// The pure-Lua KOReader compatibility prelude, embedded at build time.
const PRELUDE: &str = include_str!("prelude.lua");

/// A KOReader-compatible plugin runtime: a [`PluginHost`] with the compat prelude installed.
pub struct KoPluginRuntime {
    host: PluginHost,
}

impl KoPluginRuntime {
    /// Build the runtime over `services` and install the compat prelude.
    pub fn new(services: Rc<dyn HostServices>) -> LuaResult<Self> {
        let host = PluginHost::new(services)?;
        host.load(PRELUDE)?;
        Ok(Self { host })
    }

    /// Enable/disable probe mode (records unsupported `require`s/symbols instead of erroring).
    pub fn set_probe(&self, on: bool) -> LuaResult<()> {
        let ko: Table = self.host.lua().globals().get("__inkread_ko")?;
        ko.set("probe", on)
    }

    /// Load a KOReader plugin and run its lifecycle (instantiate → `init` → `addToMainMenu`).
    /// `meta_src` is the plugin's `_meta.lua` (executed for its metadata; may be empty); `main_src`
    /// is `main.lua`. In strict mode an unsupported API raises a clear `mlua` error.
    pub fn load_koplugin(&self, meta_src: &str, main_src: &str) -> LuaResult<()> {
        if !meta_src.trim().is_empty() {
            // _meta.lua returns a metadata table; executing it also exercises its `require`s.
            self.host.lua().load(meta_src).exec()?;
        }
        let ko: Table = self.host.lua().globals().get("__inkread_ko")?;
        let run: Function = ko.get("run")?;
        run.call::<bool>(main_src.to_string())?;
        Ok(())
    }

    /// The plugin's registered main-menu items as `(key, label)` pairs.
    pub fn menu_items(&self) -> LuaResult<Vec<(String, String)>> {
        let ko: Table = self.host.lua().globals().get("__inkread_ko")?;
        let f: Function = ko.get("menu_keys")?;
        let s: String = f.call(())?;
        Ok(s.lines()
            .filter_map(|l| {
                l.split_once('=')
                    .map(|(k, v)| (k.to_string(), v.to_string()))
            })
            .collect())
    }

    /// Fire a menu item's callback by key; returns whether it ran.
    pub fn invoke_menu_item(&self, key: &str) -> LuaResult<bool> {
        let ko: Table = self.host.lua().globals().get("__inkread_ko")?;
        let f: Function = ko.get("invoke")?;
        f.call(key.to_string())
    }

    /// The probe-recorded unsupported API paths (for generating the compatibility matrix).
    pub fn probed_paths(&self) -> LuaResult<Vec<String>> {
        let ko: Table = self.host.lua().globals().get("__inkread_ko")?;
        let f: Function = ko.get("probed_list")?;
        let s: String = f.call(())?;
        Ok(s.lines()
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// The log sink, for asserting on plugin output.
    #[must_use]
    pub fn log_sink(&self) -> &LogSink {
        self.host.log_sink()
    }
}
