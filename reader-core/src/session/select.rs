//! Lasso selection, the ink clipboard, and PDF export (ADR-INKREAD-0010, RR11).
//!
//! Selection ops mutate the layer and autosave, so an edit is durable when it lands rather than at
//! the next explicit save. Export is here because it is the other consumer of a selection's worth
//! of ink: the whole layer, written back into the PDF.

use super::*;

impl ReaderSession {
    /// Select the strokes a lasso `polygon` encloses/crosses under `mode_code` (`0`=Smart,
    /// `1`=Freehand). Returns the selected stroke ids. Non-destructive (records no edit).
    pub fn ink_select_in_polygon(
        &self,
        polygon: &[(f32, f32)],
        mode_code: u8,
    ) -> CoreResult<Vec<u32>> {
        let mode = SelectMode::from_code(mode_code)
            .ok_or_else(|| CoreError::InvalidArgument(format!("unknown lasso mode {mode_code}")))?;
        Ok(ids_to_u32(&select_in_polygon(&self.layer, polygon, mode)))
    }

    /// Every stroke id on the current page (NeoReader "Select All").
    #[must_use]
    pub fn ink_select_all(&self) -> Vec<u32> {
        ids_to_u32(&select_all(&self.layer))
    }

    /// Selection bounds as `[x0, y0, x1, y1]` (normalized), or empty if the selection is empty —
    /// the anchor/dirty-rect for the floating selection toolbar.
    #[must_use]
    pub fn ink_selection_bounds(&self, ids: &[u32]) -> Vec<f32> {
        match selection_bounds(&self.layer, &u32_to_ids(ids)) {
            Some(b) => vec![b.x0, b.y0, b.x1, b.y1],
            None => Vec::new(),
        }
    }

    /// Move the selection by `(dx, dy)` (clamped on-page), autosaving if anything moved (RR20-FR2).
    pub fn ink_move_selection(&mut self, ids: &[u32], dx: f32, dy: f32) -> CoreResult<bool> {
        let changed = self.layer.move_strokes(&u32_to_ids(ids), dx, dy).is_some();
        if changed {
            self.autosave_ink()?;
        }
        Ok(changed)
    }

    /// Delete the selection, autosaving if anything was removed. Returns the removed ids.
    pub fn ink_delete_selection(&mut self, ids: &[u32]) -> CoreResult<Vec<u32>> {
        let removed = self.layer.delete_strokes(&u32_to_ids(ids));
        if !removed.is_empty() {
            self.autosave_ink()?;
        }
        Ok(ids_to_u32(&removed))
    }

    /// Recolor the selection, autosaving if anything changed.
    pub fn ink_recolor_selection(&mut self, ids: &[u32], color: InkColor) -> CoreResult<bool> {
        let changed = self.layer.recolor_strokes(&u32_to_ids(ids), color);
        if changed {
            self.autosave_ink()?;
        }
        Ok(changed)
    }

    /// Copy the selection into the cross-page clipboard (non-destructive). Returns the count.
    pub fn ink_copy_selection(&mut self, ids: &[u32]) -> usize {
        self.clipboard = self.layer.copy_strokes(&u32_to_ids(ids));
        self.clipboard.len()
    }

    /// Cut = copy to the clipboard, then delete as one undoable edit. Returns the removed ids.
    pub fn ink_cut_selection(&mut self, ids: &[u32]) -> CoreResult<Vec<u32>> {
        self.clipboard = self.layer.copy_strokes(&u32_to_ids(ids));
        self.ink_delete_selection(ids)
    }

    /// Paste the clipboard onto the **current** page offset by `(dx, dy)` (NeoReader's cross-page
    /// paste), autosaving the new strokes. Returns the new ids; empty clipboard → no-op.
    pub fn ink_paste(&mut self, dx: f32, dy: f32) -> CoreResult<Vec<u32>> {
        if self.clipboard.is_empty() {
            return Ok(Vec::new());
        }
        let new_ids = self.layer.paste_strokes(&self.clipboard, dx, dy);
        if !new_ids.is_empty() {
            self.autosave_ink()?;
        }
        Ok(ids_to_u32(&new_ids))
    }

    /// Whether the clipboard holds strokes available to paste (gates the Paste control).
    #[must_use]
    pub fn ink_has_clipboard(&self) -> bool {
        !self.clipboard.is_empty()
    }

    /// Export every page's ink into the PDF at `out_path` (ADR-INKREAD-0005). `flatten` burns the
    /// ink into the page content (visible in every viewer); otherwise editable Ink annotations are
    /// written. Colours are preserved (true RGBA). Gathers all pages from the sidecar after first
    /// flushing the current page, so unsaved edits are included.
    pub fn export_pdf(&mut self, out_path: &str, flatten: bool) -> CoreResult<()> {
        validate_export_path(out_path)?; // contain the write target before touching the filesystem
        self.flush_ink()?; // flush the current page's edits to the sidecar first
        let mode = if flatten {
            ExportMode::Flatten
        } else {
            ExportMode::Annotations
        };
        let mut pages = Vec::new();
        for page in 0..self.page_count() {
            let layer = self.load_layer_for_page(page);
            if layer.strokes().is_empty() {
                continue;
            }
            let strokes = layer
                .strokes()
                .iter()
                .map(|s| ExportStroke {
                    points: s.points.iter().map(|p| (p.x, p.y)).collect(),
                    r: s.color.r,
                    g: s.color.g,
                    b: s.color.b,
                    a: s.color.a,
                    width: s.width,
                })
                .collect();
            pages.push(PageInk { page, strokes });
        }
        if pages.is_empty() {
            return Err(CoreError::RenderBackend(
                "no annotations to export".to_string(),
            ));
        }
        self.document.export_pdf(out_path, &pages, mode)
    }
}
