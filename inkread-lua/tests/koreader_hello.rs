//! KOReader compatibility conformance test (RR14 / ADR-INKREAD-0006 D4).
//!
//! Loads KOReader's real bundled `hello.koplugin` (vendored under `tests/fixtures/`) into inkread's
//! KOReader shim in **strict** mode and runs its full lifecycle: instantiate → `init`
//! (`registerToMainMenu` + `Dispatcher:registerAction`) → `addToMainMenu` → fire the menu callback
//! → assert the `InfoMessage` text reached our UI service. If this passes unmodified, the
//! common-core API surface is genuinely covered (not guessed).

use std::cell::RefCell;
use std::rc::Rc;

use inkread_lua::koreader::KoPluginRuntime;
use inkread_lua::{DocumentService, HostServices, UiService, ViewService};

const HELLO_META: &str = include_str!("fixtures/hello.koplugin/_meta.lua");
const HELLO_MAIN: &str = include_str!("fixtures/hello.koplugin/main.lua");

#[derive(Default)]
struct TestUi {
    messages: RefCell<Vec<String>>,
}
impl UiService for TestUi {
    fn show_message(&self, text: &str) {
        self.messages.borrow_mut().push(text.to_string());
    }
}

struct TestDoc;
impl DocumentService for TestDoc {
    fn page_count(&self) -> usize {
        100
    }
    fn current_page(&self) -> usize {
        0
    }
    fn page_aspect(&self, _page: usize) -> Option<f32> {
        Some(0.75)
    }
}

struct TestView;
impl ViewService for TestView {
    fn viewport(&self) -> (u32, u32) {
        (1000, 1200)
    }
    fn zoom(&self) -> f32 {
        1.0
    }
    fn set_zoom(&self, _zoom: f32, _pan_x: f32, _pan_y: f32) {}
}

struct TestHost {
    ui: TestUi,
    doc: TestDoc,
    view: TestView,
}
impl HostServices for TestHost {
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

fn host() -> Rc<TestHost> {
    Rc::new(TestHost {
        ui: TestUi::default(),
        doc: TestDoc,
        view: TestView,
    })
}

#[test]
fn koreader_hello_plugin_runs_unmodified() {
    let host = host();
    let rt = KoPluginRuntime::new(host.clone()).unwrap();

    // Strict mode (default): any unsupported KOReader API would error here.
    rt.load_koplugin(HELLO_META, HELLO_MAIN)
        .expect("hello.koplugin loads on the common-core shim");

    // It registered exactly one main-menu item with the expected label.
    let items = rt.menu_items().unwrap();
    assert_eq!(items.len(), 1, "hello registers one menu item: {items:?}");
    let (key, label) = &items[0];
    assert_eq!(key, "hello_world");
    assert_eq!(label, "Hello World");

    // Firing it shows the InfoMessage through our UI service.
    assert!(rt.invoke_menu_item("hello_world").unwrap());
    assert_eq!(
        host.ui.messages.borrow().as_slice(),
        &["Hello, plugin world".to_string()],
        "InfoMessage routed to inkread.ui via UIManager:show"
    );
}

#[test]
fn unsupported_api_fails_loudly_in_strict_mode() {
    let rt = KoPluginRuntime::new(host()).unwrap();
    // A plugin reaching for an unsupported KOReader module must error clearly, not silently no-op.
    let err = rt
        .load_koplugin("", r#"local FB = require("ffi/framebuffer") return {}"#)
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("not supported") && msg.contains("framebuffer"),
        "loud, specific failure: {msg}"
    );
}

#[test]
fn probe_mode_records_the_unsupported_surface_for_the_matrix() {
    let rt = KoPluginRuntime::new(host()).unwrap();
    rt.set_probe(true).unwrap();
    // In probe mode an unknown require is recorded (not fatal) so we can extract the needed surface.
    // A valid plugin (extends a supported base) that also reaches for an UNsupported module/symbol.
    rt.load_koplugin(
        "",
        r#"
        local WidgetContainer = require("ui/widget/container/widgetcontainer")
        local G = require("ui/gesturemanager")
        local P = WidgetContainer:extend{ name = "probe" }
        function P:init() G:doThing() end
        return P
        "#,
    )
    .unwrap();
    let probed = rt.probed_paths().unwrap();
    assert!(
        probed.iter().any(|p| p == "ui/gesturemanager"),
        "probe recorded the module: {probed:?}"
    );
    assert!(
        probed.iter().any(|p| p == "ui/gesturemanager.doThing"),
        "probe recorded the accessed symbol: {probed:?}"
    );
}
