//! `ReaderSession` — the open→render→gesture→commands round-trip (RR21, Amendment 6).
//!
//! Owns the open [`Document`], the current page position, the panel [`Viewport`], and the
//! [`EinkRefreshPolicy`]. A gesture advances/retreats the position then **delegates to the
//! policy's `on_page_turn`** so the Partial/ghost-clear-Full promotion and `partial_count`
//! stay consistent (Amendment 6 — no separately hand-rolled stream).
//!
//! The session is the object the JNI `long` handle points at (Amendment 2): created by
//! open, freed only by close. It never stores a [`PixelBuffer`] (Amendment 5): render
//! borrows the shell's buffer for one call and drops it.

use device_eink::{DeviceCapabilities, Rect, RefreshCommand, RefreshPolicy};

use std::sync::Arc;

use crate::budget::{Caches, ResourceBudget, TrimLevel};
use crate::document::fixed::{CbzBackend, PdfBackend};
use crate::document::{
    Document, DocumentMetadata, ExportMode, ExportStroke, FitMode, NormRect, PageInk, PageLink,
    TextSelection, TocEntry, Typography,
};
use crate::error::{CoreError, CoreResult};
use crate::persistence::identity::DocIdentity;
use crate::persistence::ink_store::InkStore;
use crate::persistence::sidecar::SidecarMetadata;
use crate::persistence::{
    BookId, PaginationProgress, ReaderStore, ReadingPosition, StorePaginationCache,
};
use crate::policy::EinkRefreshPolicy;
use crate::render::{PixelBuffer, Viewport};
use crate::settings::SettingsSnapshot;

use inkread_ink::{
    encode_layer, select_all, select_in_polygon, selection_bounds, InkColor, InkLayer, InkPoint,
    SelectMode, Stroke, StrokeId, Tool,
};

// The session is one type with one set of invariants, split across files by what a method is *for*
// rather than by trait — Rust allows several `impl ReaderSession` blocks in a crate, and a private
// field stays visible to a child module, so nothing had to be widened to make this compile.
//
// The struct, its lifecycle, and the plain accessors stay here; each submodule opens with
// `use super::*` and adds no imports of its own.
mod ink;
mod navigate;
mod open;
mod render;
mod select;
mod view;

/// Maximum pinch-zoom factor (RR5-FR3) — beyond this, e-ink legibility gains nothing.
const MAX_ZOOM: f32 = 5.0;

/// A navigation gesture (Amendment 6). The int↔enum mapping is defined **once** here and
/// documented at the JNI boundary; `nativeOnGesture` decodes an int into this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gesture {
    /// Advance to the next page.
    NextPage,
    /// Retreat to the previous page.
    PrevPage,
}

impl Gesture {
    /// Decode the wire integer code into a gesture (the single source of truth).
    ///
    /// `0 = NextPage`, `1 = PrevPage`. Unknown codes yield `None` so the boundary can
    /// surface a typed error rather than guess (RR21-FR3).
    #[must_use]
    pub fn from_code(code: i32) -> Option<Gesture> {
        match code {
            0 => Some(Gesture::NextPage),
            1 => Some(Gesture::PrevPage),
            _ => None,
        }
    }

    /// The wire integer code for this gesture (inverse of [`Self::from_code`]).
    #[must_use]
    pub fn code(self) -> i32 {
        match self {
            Gesture::NextPage => 0,
            Gesture::PrevPage => 1,
        }
    }
}

/// A reader session over one open document.
pub struct ReaderSession {
    document: Box<dyn Document>,
    policy: EinkRefreshPolicy,
    viewport: Viewport,
    page: usize,
    /// Persistence store (RR12-FR3); `None` for a store-less session (tests, and any open that
    /// asks for no persistence).
    store: Option<Arc<dyn ReaderStore>>,
    /// The book identity this session persists under (set with the store).
    book: Option<BookId>,
    /// Bounded render + cover caches under the resource budget (RR24); trimmed on memory
    /// pressure. [`Self::render_current`] serves/populates the render cache on the fit path.
    caches: Caches,
    /// The annotation store for this document's sidecar (RR10); `None` = ink not persisted.
    ink: Option<Arc<dyn InkStore>>,
    /// The current page's ink layer (RR6). Reloaded on page change; empty without ink.
    layer: InkLayer,
    /// Pinch-zoom factor (1.0 = fit, the render_page baseline) and normalized pan `[0,1]` of the
    /// off-screen overscan (RR5-FR3). Reset to fit on a page change.
    zoom: f32,
    pan_x: f32,
    pan_y: f32,
    /// The tool of the in-progress stroke — routes [`Self::ink_add_point`] (ink vs. erase).
    active_tool: Tool,
    /// Width of the in-progress ink stroke, or the erase radius for the eraser (normalized).
    active_width: f32,
    /// Whether the in-progress eraser gesture has removed anything yet — gates the autosave so a
    /// no-op erase doesn't rewrite an unchanged page (needless e-ink flash / IO).
    erase_changed: bool,
    /// When true, edits don't fsync the sidecar on every stroke-end; they mark the page dirty and
    /// the shell flushes on a trailing-edge debounce (and on pause/page-change/close). A
    /// power/flash-wear knob (the review's per-stroke-fsync finding) — **off by default** so the
    /// RR7-FR6/RR20-FR2 save-on-stroke-end durability contract holds unless the shell opts in.
    autosave_deferred: bool,
    /// In deferred mode, whether the current page has unsaved edits awaiting [`Self::flush_ink`].
    ink_dirty: bool,
    /// The page index the in-memory [`Self::layer`] belongs to. Saves target this, not `page` — so a
    /// deferred flush triggered *after* `page` has advanced (a page turn) still writes the outgoing
    /// page. Equals `page` during normal editing; updated whenever the layer is (re)loaded.
    layer_page: usize,
    /// The lasso clipboard (ADR-INKREAD-0010): strokes copied/cut from any page, held on the
    /// session so a paste can land on a **different** page (NeoReader's cross-page clipboard).
    clipboard: Vec<Stroke>,
    /// The opened document's content identity (RR10-FR6), computed from its bytes at open. `None`
    /// for a byte-less test session ([`Self::with_document`]). Used to stamp/verify the sidecar.
    identity: Option<DocIdentity>,
    /// Contrast/display-enhancement step (`0` = off; RR4 — KOReader's "Contrast"). Applied as a
    /// per-pixel remap after render so faint scans read better on e-ink.
    contrast: u8,
    /// Night mode: invert the rendered page (light text on dark) after contrast (RR4). Part of the
    /// cache key so a cached page isn't served at the wrong polarity.
    night: bool,
    /// How a fixed-layout page is fit to the viewport (RR4 — KOReader's "Fit"). Default: contain.
    fit_mode: FitMode,
    /// Auto-crop the page's white margins (RR4 — KOReader Crop = auto). `false` = full page.
    crop_auto: bool,
    /// Margin kept around the auto-crop, in 1%-of-page steps (RR4 — KOReader Margin).
    crop_margin: u8,
    /// Per-page content-bbox memo for auto-crop (recomputed when the page changes). Interior-mutable
    /// so the `&self` render path can cache the probe render.
    crop_cache: std::cell::RefCell<Option<(usize, Option<NormRect>)>>,
    /// Render quality (RR4 — KOReader): `0` = low (sub-sample), `1` = default, `2` = high
    /// (supersample then downscale → smoother e-ink text).
    render_quality: u8,
}

/// Render-quality step → render-scale factor (RR4): low `0.75×`, default `1.0×`, high `1.5×`.
fn render_quality_factor(q: u8) -> f32 {
    match q {
        0 => 0.75,
        2 => 1.5,
        _ => 1.0,
    }
}

impl ReaderSession {
    /// Apply a settings snapshot for `book` to the refresh policy — flash interval, night
    /// interval, and avoid-flashing all come from settings (RR23 ↔ RR3-FR3/FR6/FR7). The shell
    /// calls this on open and whenever a relevant setting changes.
    pub fn apply_settings(&mut self, settings: &SettingsSnapshot, book: Option<&BookId>) {
        self.policy.set_interval(settings.flash_interval(book));
        self.policy
            .set_night_interval(settings.night_flash_interval(book));
        self.policy
            .set_avoid_flashing(settings.avoid_flashing(book));
    }

    /// Persist the current reading position (RR12-FR3). For a reflowable document it also stores the
    /// page's reflow-stable [`PinPosition`] JSON in `resume_blob` so the next open re-anchors across
    /// a font-size change (RR12-FR4 / #46); fixed-layout PDF stores the integer page only. A
    /// store-less session is a no-op.
    pub fn save_position(&self) -> CoreResult<()> {
        if let (Some(store), Some(book)) = (&self.store, &self.book) {
            let blob = self
                .document
                .page_pin(self.page)
                .map(|pin| pin.to_json().into_bytes());
            let pos = ReadingPosition::new(self.page, self.page_count()).with_resume_blob(blob);
            store.save_position(book, &pos)?;
        }
        Ok(())
    }

    /// The bounded render + cover caches (RR24). [`Self::render_current`] consults/fills the render
    /// cache; the shell uses the cover cache for the library grid.
    pub fn caches(&mut self) -> &mut Caches {
        &mut self.caches
    }

    /// React to platform memory pressure (`onTrimMemory`, RR24-FR3): trims the caches by
    /// severity. Always leaves the reader usable; never panics.
    pub fn on_trim_memory(&mut self, level: TrimLevel) {
        self.caches.trim(level);
    }

    /// Total page count.
    #[must_use]
    pub fn page_count(&self) -> usize {
        self.document.page_count()
    }

    /// The current page index.
    #[must_use]
    pub fn current_page(&self) -> usize {
        self.page
    }

    /// The session viewport's pixel dimensions `(width, height)` — used by the JNI bridge
    /// to size the render buffer without reaching into private state.
    #[must_use]
    pub fn viewport_dims(&self) -> (u32, u32) {
        (self.viewport.width, self.viewport.height)
    }

    /// Document metadata.
    #[must_use]
    pub fn metadata(&self) -> DocumentMetadata {
        self.document.metadata()
    }

    // ===== Ink annotation lifecycle (RR6/RR7/RR10/RR20) =====

    // ===== Lasso selection over the current page's ink (ADR-INKREAD-0010) =====
}

/// Contain the PDF-export write target before it reaches pdfium's `save_to_file` (IR security, the
/// review's "export path lacks native containment"). The shell chooses the path with all-files
/// access, so the core can't know Android's storage roots — but it *can* reject the shapes a buggy
/// or compromised shell should never produce: a relative path, a `..` traversal component, or a
/// parent directory that doesn't already exist (export creates a file, never a directory tree).
/// This bounds "write anywhere the UID can reach via traversal" without second-guessing legitimate
/// user-chosen destinations.
fn validate_export_path(out_path: &str) -> CoreResult<()> {
    use std::path::{Component, Path};
    let bad = |why: &str| {
        Err(CoreError::InvalidArgument(format!(
            "export path {why}: {out_path}"
        )))
    };
    if out_path.is_empty() {
        return bad("is empty");
    }
    let path = Path::new(out_path);
    if !path.is_absolute() {
        return bad("must be absolute");
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return bad("must not contain `..`");
    }
    match path.parent() {
        Some(dir) if dir.is_dir() => Ok(()),
        _ => bad("parent directory does not exist"),
    }
}

/// Map a **viewport-normalized** point to **page-normalized** using the affine `(sx, ox, sy, oy)`
/// (the inverse of the page→viewport fit map). A zero scale (degenerate) leaves the axis unchanged.
fn view_to_page_pt(p: (f32, f32), t: (f32, f32, f32, f32)) -> (f32, f32) {
    let (sx, ox, sy, oy) = t;
    let px = if sx.abs() > f32::EPSILON {
        (p.0 - ox) / sx
    } else {
        p.0
    };
    let py = if sy.abs() > f32::EPSILON {
        (p.1 - oy) / sy
    } else {
        p.1
    };
    (px, py)
}

/// Map a viewport-normalized rect to page-normalized (corner-wise inverse fit map).
fn view_to_page_rect(r: NormRect, t: (f32, f32, f32, f32)) -> NormRect {
    let (x0, y0) = view_to_page_pt((r.x0, r.y0), t);
    let (x1, y1) = view_to_page_pt((r.x1, r.y1), t);
    NormRect { x0, y0, x1, y1 }
}

/// Map a page-space [`TextSelection`]'s boxes up to viewport space via the affine `(sx, ox, sy, oy)`
/// so they align with the rendered pixels; the text is unchanged.
fn map_selection_to_view(sel: TextSelection, t: (f32, f32, f32, f32)) -> TextSelection {
    let (sx, ox, sy, oy) = t;
    let boxes = sel
        .boxes
        .into_iter()
        .map(|b| NormRect {
            x0: b.x0 * sx + ox,
            y0: b.y0 * sy + oy,
            x1: b.x1 * sx + ox,
            y1: b.y1 * sy + oy,
        })
        .collect();
    TextSelection {
        text: sel.text,
        boxes,
    }
}

/// Stroke ids cross the JNI boundary as plain `u32`; these convert to/from the typed [`StrokeId`].
fn ids_to_u32(ids: &[StrokeId]) -> Vec<u32> {
    ids.iter().map(|s| s.0).collect()
}

fn u32_to_ids(ids: &[u32]) -> Vec<StrokeId> {
    ids.iter().map(|&i| StrokeId(i)).collect()
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "view_tests.rs"]
mod view_tests;

#[cfg(test)]
#[path = "open_tests.rs"]
mod open_tests;

#[cfg(test)]
#[path = "navigate_tests.rs"]
mod navigate_tests;

#[cfg(test)]
#[path = "select_tests.rs"]
mod select_tests;
