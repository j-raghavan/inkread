//! Service **ports** the plugin runtime calls into (RR12-FR4 / ADR-INKREAD-0006 Decision 1).
//!
//! `inkread-lua` defines these traits (the ports); `reader-core` implements them over a live
//! `ReaderSession` (the adapter). This keeps the dependency arrow `reader-core → inkread-lua` (no
//! cycle) and means a plugin and the native UI drive the *same* capability layer — a Lua call
//! returns the same result as the native path (RR12-AC2). The traits carry **no UI/JNI/device
//! types** (IR-4), so the whole plugin↔service loop is host-testable with a mock (see the tests in
//! `lib.rs`). Pixel/layout work (crop, contrast, reflow) lives behind these services in the core;
//! the Lua side only passes parameters and reads state.

/// Read-only facts about the open document a plugin can query.
pub trait DocumentService {
    /// Total page count.
    fn page_count(&self) -> usize;
    /// The 0-based page the reader is currently on.
    fn current_page(&self) -> usize;
    /// The native width/height aspect ratio of `page` (for fit math), or `None` if unknown
    /// (e.g. an out-of-range page).
    fn page_aspect(&self, page: usize) -> Option<f32>;
}

/// The reader viewport + zoom/pan a plugin can drive (the basis for fit / zoom-to controls).
pub trait ViewService {
    /// The render viewport size in pixels, `(width, height)`.
    fn viewport(&self) -> (u32, u32);
    /// The current zoom factor (`1.0` = fit-to-viewport).
    fn zoom(&self) -> f32;
    /// Set the zoom factor (clamped `>= 1` by the core) and the normalized pan `[0,1]`. The next
    /// render shows the magnified/panned view (RR5-FR3).
    fn set_zoom(&self, zoom: f32, pan_x: f32, pan_y: f32);
}

/// User-facing UI a plugin can drive (KOReader's `InfoMessage`/`UIManager:show` route here).
pub trait UiService {
    /// Show a transient message / info popup (the shell renders it; the core stays vendor-neutral).
    fn show_message(&self, text: &str);
}

/// Everything the host exposes to plugins — the single object the `PluginHost` binds the
/// `inkread.*` API to. `reader-core` implements this over `ReaderSession`; tests use a mock.
pub trait HostServices {
    /// The document query service.
    fn document(&self) -> &dyn DocumentService;
    /// The view (zoom/pan/viewport) service.
    fn view(&self) -> &dyn ViewService;
    /// The user-facing UI service.
    fn ui(&self) -> &dyn UiService;
}
