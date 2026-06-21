# KOReader plugin compatibility matrix (RR14 / ADR-INKREAD-0006 D4)

How inkread guarantees it exposes the KOReader plugin interface: **not by mirroring all of KOReader
(its "API" is its whole module tree), but by covering a curated surface, proving it against real
plugins, tracking every symbol here, and failing loudly on the rest.**

- **Pinned KOReader version:** target API as of KOReader 2024.x (the `WidgetContainer:extend` OO +
  `Dispatcher`/`UIManager`/`Menu` model). Re-baseline this row when bumping the target.
- **Shim:** `inkread-lua/src/koreader/` — a pure-Lua prelude (`prelude.lua`) over `inkread.*`
  services, a strict/probe `require` resolver, and a `.koplugin` lifecycle runner.
- **Modes:** *strict* (default/CI) → an unsupported `require`/symbol raises
  `inkread: KOReader API '…' is not supported (RR14)`. *probe* (dev) → records the gap into this
  matrix instead of failing (`KoPluginRuntime::probed_paths`).
- **Conformance:** every target plugin has a host test that loads it in strict mode and exercises
  it; CI fails if a shim change breaks a target (`inkread-lua/tests/koreader_hello.rs`).

## Target plugins (the curated "must run" set)

| Plugin | Source | Status | Conformance test |
|---|---|---|---|
| `hello` | KOReader bundled example | ✅ runs unmodified | `koreader_hello::koreader_hello_plugin_runs_unmodified` |
| _(utility plugin #2 — TBD)_ | — | ☐ planned | — |
| _(utility plugin #3 — TBD)_ | — | ☐ planned | — |

> Owner picked "common-core + 2–3 real utility plugins." `hello` covers the common core; the two
> utility targets are added next (their probe output extends the supported surface below).

## Supported API surface (common core)

| KOReader module / symbol | Status | inkread mapping |
|---|---|---|
| `ui/widget/eventlistener` (`:extend`, `:new`, `init`) | ✅ shimmed | pure-Lua OO base |
| `ui/widget/widget` | ✅ shimmed | extends EventListener |
| `ui/widget/container/widgetcontainer` | ✅ shimmed | plugin base class |
| `<plugin>.addToMainMenu(menu_items)` + `self.ui.menu:registerToMainMenu` | ✅ shimmed | menu registry → host menu |
| `ui/uimanager` `:show/:close/:setDirty/:scheduleIn/:nextTick` | ◑ partial | `:show{text}` → `inkread.ui.show_message`; scheduler is a stub |
| `ui/widget/infomessage` `:new{text}` | ✅ shimmed | routed via `UIManager:show` |
| `ui/widget/confirmbox` | ◑ partial | constructs; actions not yet wired |
| `dispatcher` `:registerAction` | ◑ partial | recorded; gesture routing not emulated |
| `gettext` (`_`) | ✅ shimmed | identity (no catalogs yet) |
| `logger` `.info/.warn/.err/.dbg` | ✅ shimmed | → `inkread.log` |
| `self.ui.document` / `self.view` / `self.document` | ◑ partial | injected; backed by `inkread.document`/`inkread.view` |

## Unsupported (loud failure by design)

`ffi/*` (LuaJIT FFI) · `ui/gesturemanager` internals · framebuffer / direct EPD · custom widgets
(beyond InfoMessage/ConfirmBox) · monkey-patching core modules · terminal/shell · network stack.
A `require` for any of these errors clearly in strict mode and is recorded in probe mode.

## How to add a target plugin

1. Vendor it under `inkread-lua/tests/fixtures/<name>.koplugin/`.
2. Run it in **probe** mode; `probed_paths()` lists the exact modules/symbols it needs.
3. Implement those in `prelude.lua` (pure Lua) or add a service to `inkread-lua/src/services.rs`
   (+ `reader-core` adapter) if it needs a real core capability.
4. Add a strict-mode conformance test; update this matrix.
