//! The [`RefreshPolicy`] trait (RR2-FR1) — the pure-Rust contract the engine implements.
//!
//! A policy is constructed with the device's [`DeviceCapabilities`](crate::DeviceCapabilities)
//! and, per interaction, returns `Vec<RefreshCommand>` as plain data. It **never** touches
//! the panel and does not know which vendor it is (IR-2); the Kotlin adapter executes the
//! returned stream. This makes the policy unit-testable on the host via the
//! [`MockDeviceRecorder`](crate::MockDeviceRecorder).

use crate::command::RefreshCommand;
use crate::geometry::Rect;

/// The content-aware refresh state machine (RR3). Each method maps an interaction to the
/// command stream the adapter should execute; the policy mutates its own internal counters
/// (e.g. the partial→flash promotion counter) but never the device.
pub trait RefreshPolicy {
    /// A page turn landed on `page_rect` (RR3-FR3).
    fn on_page_turn(&mut self, page_rect: Rect) -> Vec<RefreshCommand>;
    /// A scroll/fling began (RR3-FR4).
    fn on_scroll_start(&mut self) -> Vec<RefreshCommand>;
    /// A scroll advanced, dirtying `dirty` (RR3-FR4).
    fn on_scroll_update(&mut self, dirty: Rect) -> Vec<RefreshCommand>;
    /// A scroll settled on `settle_rect` (RR3-FR4).
    fn on_scroll_end(&mut self, settle_rect: Rect) -> Vec<RefreshCommand>;
    /// A menu/dialog opened (`open = true`) or closed over `region` (RR3-FR5).
    fn on_menu(&mut self, open: bool, region: Rect) -> Vec<RefreshCommand>;
    /// Night mode toggled (RR3-FR6).
    fn on_night_mode(&mut self, on: bool) -> Vec<RefreshCommand>;
}
