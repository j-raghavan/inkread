//! The ink model and its sidecar persistence (RR6/RR7/RR10/RR20).
//!
//! Strokes are accumulated into the page's [`InkLayer`], and every edit persists. Autosave can be
//! deferred while the pen is down and flushed on lift, and a failed flush retries — losing
//! handwriting is the one failure this module exists to prevent.

use super::*;

impl ReaderSession {
    /// Attach an annotation [`InkStore`], **verify/stamp** the sidecar against the document's
    /// identity (RR10-FR6/AC3), then load the current page's ink (RR7-FR7). A corrupt landing page
    /// degrades to empty rather than blocking open — consistent with a page turn.
    pub fn attach_ink_store(&mut self, store: Arc<dyn InkStore>) -> CoreResult<()> {
        self.ink = Some(store);
        self.verify_or_stamp_identity();
        self.layer = self.load_layer_for_page(self.page);
        self.layer_page = self.page;
        Ok(())
    }

    /// Reconcile the attached sidecar with the open document's identity (RR10-AC3): stamp a fresh
    /// `metadata.json` if absent; if it belongs to a *different* document (same path, different
    /// content) move the stale ink aside and re-stamp, so the open document never adopts foreign
    /// strokes. Best-effort — a write failure here resurfaces on the first real autosave.
    fn verify_or_stamp_identity(&self) {
        let (Some(store), Some(id)) = (&self.ink, &self.identity) else {
            return;
        };
        match store.load_metadata() {
            Ok(Some(meta)) if meta.matches(id) => {} // ours — adopt the existing ink
            Ok(Some(_)) => {
                // Same path, different document: preserve the stale ink and start clean.
                let _ = store.reset_stale_annotations();
                let _ = store.save_metadata(&SidecarMetadata::from_identity(id, self.page_count()));
            }
            _ => {
                // Fresh or unreadable metadata → (re)stamp this document's identity.
                let _ = store.save_metadata(&SidecarMetadata::from_identity(id, self.page_count()));
            }
        }
    }

    /// The current page's committed strokes (RR6).
    #[must_use]
    pub fn ink_strokes(&self) -> &[Stroke] {
        self.layer.strokes()
    }

    /// `.inkbin` bytes for `page` — the open page's live layer, else loaded from the store
    /// (RR7-AC1). The shell decodes these with the same `inkread-ink` codec.
    pub fn ink_strokes_wire(&self, page: usize) -> CoreResult<Vec<u8>> {
        if page == self.page {
            Ok(encode_layer(&self.layer))
        } else if let Some(store) = &self.ink {
            Ok(encode_layer(&store.load_page(page)?))
        } else {
            Ok(encode_layer(&InkLayer::new()))
        }
    }

    /// Draw-wire bytes for `page` (ADR-INKREAD-0010): the open page's live layer, else loaded from
    /// the store. Carries per-stroke id + tool/color/width/path so the shell can bake the strokes
    /// **and** pass selected ids back to the lasso ops. Decode with `WireCodec.decodeStrokes`.
    pub fn ink_draw_wire(&self, page: usize) -> CoreResult<Vec<u8>> {
        if page == self.page {
            Ok(crate::ink_wire::encode_strokes_draw_wire(&self.layer))
        } else if let Some(store) = &self.ink {
            Ok(crate::ink_wire::encode_strokes_draw_wire(
                &store.load_page(page)?,
            ))
        } else {
            Ok(crate::ink_wire::encode_strokes_draw_wire(&InkLayer::new()))
        }
    }

    /// Pages that carry ink, sorted ascending (RR6) — drives the annotations list. The store's saved
    /// pages plus the open page when its live (possibly not-yet-saved) layer has strokes.
    pub fn ink_pages(&self) -> CoreResult<Vec<usize>> {
        let mut pages = match &self.ink {
            Some(store) => store.pages_with_ink()?,
            None => Vec::new(),
        };
        if !self.ink_strokes().is_empty() && !pages.contains(&self.page) {
            pages.push(self.page);
        }
        pages.sort_unstable();
        pages.dedup();
        Ok(pages)
    }

    /// Begin a stroke (RR6). Pen/Highlighter accumulate points; Eraser removes strokes under each
    /// subsequent point. `width` is the stroke width (ink) or the erase radius (eraser).
    pub fn ink_begin_stroke(
        &mut self,
        tool: Tool,
        color: InkColor,
        width: f32,
        created_at_ms: u64,
    ) -> CoreResult<()> {
        if tool == Tool::Eraser && (!width.is_finite() || width <= 0.0) {
            return Err(CoreError::InvalidArgument(format!(
                "eraser radius must be finite and positive, got {width}"
            )));
        }
        self.active_tool = tool;
        self.active_width = width;
        self.erase_changed = false;
        if tool.is_ink() {
            self.layer.start_stroke(tool, color, width, created_at_ms)?;
        } else {
            self.layer.cancel_stroke();
        }
        Ok(())
    }

    /// Add a sample to the in-progress stroke (ink) or erase at the point (eraser) — RR6-FR5.
    pub fn ink_add_point(
        &mut self,
        x: f32,
        y: f32,
        pressure: f32,
        tilt_x: Option<f32>,
        tilt_y: Option<f32>,
        timestamp_ms: u32,
    ) -> CoreResult<()> {
        if self.active_tool.is_ink() {
            self.layer
                .push_point(InkPoint::new(x, y, pressure, tilt_x, tilt_y, timestamp_ms)?)?;
        } else if !self.layer.erase_at(x, y, self.active_width).is_empty() {
            self.erase_changed = true;
        }
        Ok(())
    }

    /// Add a whole run of samples to the in-progress stroke (or erase along them) in one call — the
    /// batched form of [`Self::ink_add_point`]. `xy` is packed `[x0, y0, x1, y1, …]`; pressure
    /// defaults to 1.0 with no tilt/timestamp (the shell's stylus-capture path supplies none). This
    /// lets the shell hand a 200-point stroke across JNI once instead of paying 200 round-trips on
    /// the annotation hot path (the review's per-point-JNI finding). A trailing odd float is
    /// ignored; an invalid sample aborts the batch with the same error the per-point call raises.
    pub fn ink_add_points(&mut self, xy: &[f32]) -> CoreResult<()> {
        for pt in xy.chunks_exact(2) {
            self.ink_add_point(pt[0], pt[1], 1.0, None, None, 0)?;
        }
        Ok(())
    }

    /// Finish the in-progress stroke and autosave the page **only if it changed** (RR7-FR6 /
    /// RR20-FR2): an ink stroke that committed at least one point, or an eraser gesture that
    /// removed something. A no-op stroke/erase does not rewrite the page (saves e-ink flash + IO).
    pub fn ink_end_stroke(&mut self) -> CoreResult<()> {
        let changed = if self.active_tool.is_ink() {
            self.layer.finish_stroke().is_some()
        } else {
            self.erase_changed
        };
        // Consume the erase flag: once the gesture ends it's been persisted (or marked dirty), so a
        // lingering `erase_changed` must not later read as an *in-progress* erase on a page turn (#50).
        self.erase_changed = false;
        if changed {
            self.persist_after_edit()?;
        }
        Ok(())
    }

    /// Undo the last ink edit on the current page, autosaving if anything changed (RR6-FR3).
    pub fn ink_undo(&mut self) -> CoreResult<bool> {
        let changed = self.layer.undo();
        if changed {
            self.persist_after_edit()?;
        }
        Ok(changed)
    }

    /// Redo the last undone ink edit, autosaving if anything changed (RR6-FR3).
    pub fn ink_redo(&mut self) -> CoreResult<bool> {
        let changed = self.layer.redo();
        if changed {
            self.persist_after_edit()?;
        }
        Ok(changed)
    }

    /// Enable/disable **deferred autosave** (the shell's per-stroke-fsync power knob). When enabled,
    /// edits mark the page dirty instead of writing on each stroke-end; the shell is then responsible
    /// for flushing on a trailing-edge debounce (and the session itself flushes on page-change /
    /// export / explicit [`Self::save_ink`]). Switching back to immediate mode flushes any pending
    /// edit so nothing is left unsaved.
    pub fn set_autosave_deferred(&mut self, deferred: bool) -> CoreResult<()> {
        if !deferred && self.ink_dirty {
            self.flush_ink()?;
        }
        self.autosave_deferred = deferred;
        Ok(())
    }

    /// Persist after an edit: write now (immediate mode), or just mark the page dirty (deferred
    /// mode) so the shell's debounced [`Self::save_ink`] coalesces the fsync.
    fn persist_after_edit(&mut self) -> CoreResult<()> {
        if self.autosave_deferred {
            self.ink_dirty = true;
            Ok(())
        } else {
            self.autosave_ink()
        }
    }

    /// Flush the current page's ink to the store (RR20-FR2) — an explicit save for pause/close and
    /// the trailing-edge flush in deferred mode, complementing the per-edit autosave.
    pub fn save_ink(&mut self) -> CoreResult<()> {
        self.flush_ink()
    }

    /// Write the current page if it has pending edits (always writes in immediate mode, where
    /// `ink_dirty` is never set), clearing the dirty flag. No-op without a store.
    pub(super) fn flush_ink(&mut self) -> CoreResult<()> {
        self.autosave_ink()?;
        self.ink_dirty = false;
        Ok(())
    }

    /// Flush the outgoing page's ink on a page turn, re-issuing the write a few times so a transient
    /// failure that clears immediately (e.g. an `EINTR`-interrupted syscall) doesn't cost the user
    /// their ink. The retry is immediate (no backoff), so it does NOT help a sustained condition
    /// (`ENOSPC`, a held lock) — after a bounded number of attempts it gives up so navigation never
    /// blocks on a hard failure. Degrade-safely, RR20 / #50.
    pub(super) fn flush_ink_retrying(&mut self) {
        const ATTEMPTS: u32 = 3;
        for _ in 0..ATTEMPTS {
            if self.flush_ink().is_ok() {
                return;
            }
        }
    }

    /// Persist the current page's layer to the store (RR20-FR2). No-op without a store.
    pub(super) fn autosave_ink(&self) -> CoreResult<()> {
        if let Some(store) = &self.ink {
            store.save_page(self.layer_page, &self.layer)?;
        }
        Ok(())
    }

    /// Swap the in-memory layer to the current page's stored ink on a page change, persisting any
    /// in-progress edit to the outgoing page first; the load degrades safely (see
    /// [`Self::load_layer_for_page`]).
    pub(super) fn load_ink_for_current_page(&mut self) {
        // A page turn must not silently drop an in-progress edit (#50 — "disappearing strokes"):
        //  - a pending pen/highlighter stroke is COMMITTED to the outgoing page (not cancelled);
        //  - an in-progress eraser gesture has already mutated the layer (`erase_changed`) but isn't
        //    persisted until `ink_end_stroke` — the symmetric case — so it's flushed too;
        //  - deferred mode's pending edits (`ink_dirty`) are flushed, as before.
        // The flush saves under `layer_page` (the outgoing page), not the new one.
        let committed = self.layer.finish_stroke().is_some();
        if committed || self.erase_changed || self.ink_dirty {
            self.flush_ink_retrying();
        }
        // The outgoing layer (and its edit state) is about to be discarded: clear the per-page edit
        // flags so a flush that exhausted its retries can't leak a stale dirty bit onto the new page.
        self.erase_changed = false;
        self.ink_dirty = false;
        self.layer = self.load_layer_for_page(self.page);
        self.layer_page = self.page;
        // Preserve the reading magnification across a page turn (#52 — PDF nav responsiveness):
        // dropping a zoomed-in view back to full-page fit on every turn forced the user to re-zoom,
        // costing a second render to reach their intended view. Keep the zoom and the horizontal
        // column (pan_x), but land at the TOP of the new page (pan_y = 0) so a turn starts the same
        // column afresh. A magnified view is fixed-layout only (zoom > 1 never occurs in reflowed
        // text, RR25-FR3), so reflowed pages — always at fit — still reset cleanly.
        //
        // pan_x is a fraction of the magnified OVERSCAN, not an absolute column — so on a uniform
        // PDF it lands the same column, but if the next page is narrower or a different layout
        // (a title page, the last page) the same pan_x maps elsewhere. It stays numerically in
        // range (clamped [0,1]); only the "same column" intent is approximate across a layout
        // change. A precise mapping would need a column model the fixed-layout backend doesn't have.
        if self.zoom <= 1.0 + 1e-3 {
            self.zoom = 1.0;
            self.pan_x = 0.0;
        }
        self.pan_y = 0.0;
    }

    /// Load `page`'s ink, degrading safely so open/navigation never fails: a **corrupt** page is
    /// quarantined (its bytes preserved aside, RR20-FR1) and returns empty; a transient IO error
    /// also returns empty. The reader thus always opens and always turns.
    pub(super) fn load_layer_for_page(&self, page: usize) -> InkLayer {
        let Some(store) = &self.ink else {
            return InkLayer::new();
        };
        match store.load_page(page) {
            Ok(layer) => layer,
            Err(CoreError::CorruptDocument(_)) => {
                let _ = store.quarantine_page(page);
                InkLayer::new()
            }
            Err(_) => InkLayer::new(),
        }
    }
}
