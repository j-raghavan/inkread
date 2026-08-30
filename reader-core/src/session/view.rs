//! View settings and reflow typography (RR2-FR5, RR4, RR8).
//!
//! The two halves of what the Adjust sheet drives. Display settings — contrast, night, fit, crop,
//! quality, zoom, pan — change how the page is drawn. Typography settings — text scale, typeface,
//! line spacing, columns, margins, alignment — change where the words fall, and so trigger a
//! repagination and have to keep the reader on the same words across it.

use super::*;

impl ReaderSession {
    /// Set the contrast/display-enhancement step (`0` = off, clamped to
    /// [`MAX_CONTRAST_STEP`](crate::render::contrast::MAX_CONTRAST_STEP)) — RR4. Re-render to apply.
    pub fn set_contrast(&mut self, step: u8) {
        self.contrast = step.min(crate::render::contrast::MAX_CONTRAST_STEP);
    }

    /// The current contrast step (`0` = off).
    #[must_use]
    pub fn contrast(&self) -> u8 {
        self.contrast
    }

    /// Enable/disable night mode (invert the page; RR4). Re-render to apply.
    pub fn set_night(&mut self, on: bool) {
        self.night = on;
    }

    /// Whether night mode (invert) is on.
    #[must_use]
    pub fn night(&self) -> bool {
        self.night
    }

    /// Set the page fit mode (RR4 — KOReader's "Fit"). Re-render to apply.
    pub fn set_fit(&mut self, mode: FitMode) {
        self.fit_mode = mode;
    }

    /// The current page fit mode.
    #[must_use]
    pub fn fit_mode(&self) -> FitMode {
        self.fit_mode
    }

    /// Enable/disable auto-crop of the page's white margins (RR4). Re-render to apply.
    pub fn set_crop_auto(&mut self, auto: bool) {
        self.crop_auto = auto;
    }

    /// Whether auto-crop is on.
    #[must_use]
    pub fn crop_auto(&self) -> bool {
        self.crop_auto
    }

    /// Set the margin kept around the auto-crop (1%-of-page steps, clamped 0..=8). Re-render to apply.
    pub fn set_crop_margin(&mut self, step: u8) {
        self.crop_margin = step.min(8);
    }

    /// The current crop margin step.
    #[must_use]
    pub fn crop_margin(&self) -> u8 {
        self.crop_margin
    }

    /// Set render quality (`0` = low, `1` = default, `2` = high; clamped) — RR4. Re-render to apply.
    pub fn set_render_quality(&mut self, q: u8) {
        self.render_quality = q.min(2);
    }

    /// The current render quality step.
    #[must_use]
    pub fn render_quality(&self) -> u8 {
        self.render_quality
    }

    /// Set the pinch-zoom factor (clamped to `[1.0, MAX_ZOOM]`) and normalized pan `[0,1]`
    /// (RR5-FR3). The shell drives this from pinch + drag; render uses it on the next frame.
    pub fn set_zoom(&mut self, zoom: f32, pan_x: f32, pan_y: f32) {
        self.zoom = if zoom.is_finite() {
            zoom.clamp(1.0, MAX_ZOOM)
        } else {
            1.0
        };
        self.pan_x = pan_x.clamp(0.0, 1.0);
        self.pan_y = pan_y.clamp(0.0, 1.0);
    }

    /// The current zoom factor (1.0 = fit).
    #[must_use]
    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    /// The current horizontal pan `[0,1]` over the magnified overscan (0 at fit).
    #[must_use]
    pub fn pan_x(&self) -> f32 {
        self.pan_x
    }

    /// The current vertical pan `[0,1]` over the magnified overscan (0 at fit / top of page).
    #[must_use]
    pub fn pan_y(&self) -> f32 {
        self.pan_y
    }

    /// Set the reflow **text scale** (font size; `1.0` = default) for a reflowable document,
    /// repaginating and preserving the reading position by chapter (RR2-FR5). Returns `true` if the
    /// format supports reflow (EPUB); `false` for fixed-layout PDF (no change). The shell re-renders
    /// the (possibly new) current page afterward.
    pub fn set_text_scale(&mut self, scale: f32) -> bool {
        match self.document.set_text_scale(scale, self.page) {
            Some(new_page) => {
                self.page = new_page.min(self.page_count().saturating_sub(1));
                self.invalidate_render_cache(); // repagination changes what each page index renders
                self.load_ink_for_current_page();
                true
            }
            None => false,
        }
    }

    /// Set the reflow font family (`font_id` indexes the bundled faces); repaginates + keeps the
    /// chapter. Returns false for a fixed-layout document (RR4 — font select). Re-render after.
    pub fn set_font(&mut self, font_id: i32) -> bool {
        match self.document.set_font(font_id, self.page) {
            Some(new_page) => {
                self.page = new_page.min(self.page_count().saturating_sub(1));
                self.invalidate_render_cache(); // a new face re-lays-out → page bitmaps are stale
                self.load_ink_for_current_page();
                true
            }
            None => false,
        }
    }

    /// Set the reflow line-spacing multiplier (RR4); repaginates EPUB preserving the chapter.
    /// `false` for a fixed-layout PDF. Re-render after.
    pub fn set_line_spacing(&mut self, mult: f32) -> bool {
        match self.document.set_line_spacing(mult, self.page) {
            Some(new_page) => {
                self.page = new_page.min(self.page_count().saturating_sub(1));
                self.invalidate_render_cache(); // repagination changes what each page index renders
                self.load_ink_for_current_page();
                true
            }
            None => false,
        }
    }

    /// Columns the layout is actually using — see [`Document::effective_columns`] (#194).
    #[must_use]
    pub fn effective_columns(&self) -> i32 {
        self.document.effective_columns()
    }

    /// Set the reflow column count (1 or 2; #194); repaginates EPUB preserving the chapter.
    /// `false` for a fixed-layout PDF. Re-render after.
    pub fn set_columns(&mut self, columns: i32) -> bool {
        match self.document.set_columns(columns, self.page) {
            Some(new_page) => {
                self.page = new_page.min(self.page_count().saturating_sub(1));
                self.invalidate_render_cache(); // repagination changes what each page index renders
                self.load_ink_for_current_page();
                true
            }
            None => false,
        }
    }

    /// Set the reflow page margin as a percentage of page width and repaginate, preserving the
    /// chapter (RR16-FR2 / RR9-FR4 / #167). `true` if the document reflowed; `false` for a
    /// fixed-layout PDF. Re-render after.
    pub fn set_margin(&mut self, margin_pct: i32) -> bool {
        match self.document.set_margin(margin_pct, self.page) {
            Some(new_page) => {
                self.page = new_page.min(self.page_count().saturating_sub(1));
                self.invalidate_render_cache(); // repagination changes what each page index renders
                self.load_ink_for_current_page();
                true
            }
            None => false,
        }
    }

    /// Set the reflow alignment (`0=Left,1=Justify,2=Center,3=Right`; RR4); repaginates EPUB
    /// preserving the chapter. `false` for a fixed-layout PDF. Re-render after.
    pub fn set_alignment(&mut self, align_code: i32) -> bool {
        match self.document.set_alignment(align_code, self.page) {
            Some(new_page) => {
                self.page = new_page.min(self.page_count().saturating_sub(1));
                self.invalidate_render_cache(); // repagination changes what each page index renders
                self.load_ink_for_current_page();
                true
            }
            None => false,
        }
    }

    /// Report pagination progress to `progress`, and let it cancel a re-pagination (#161). Only
    /// reflowable documents have anything slow to report; the rest ignore it.
    pub fn set_pagination_progress(&self, progress: Box<dyn PaginationProgress>) {
        self.document.set_pagination_progress(progress);
    }

    /// Apply the whole persisted typography set (text scale, face, line spacing, alignment) in one
    /// operation, repaginating once (RR4). The open path uses this instead of four separate setters
    /// so restoring a reader's saved settings costs a single layout pass (#161/#162). `false` for a
    /// fixed-layout document. Re-render after.
    #[allow(clippy::too_many_arguments)]
    pub fn set_typography(
        &mut self,
        scale: f32,
        font_id: i32,
        line_spacing: f32,
        align_code: i32,
        columns: i32,
        margin_pct: i32,
    ) -> bool {
        match self.document.set_typography(
            scale,
            font_id,
            line_spacing,
            align_code,
            columns,
            margin_pct,
            self.page,
        ) {
            Some(new_page) => {
                self.page = new_page.min(self.page_count().saturating_sub(1));
                self.invalidate_render_cache(); // repagination changes what each page index renders
                self.load_ink_for_current_page();
                true
            }
            None => false,
        }
    }

    /// Whether the open document can be **reflowed** (ADR-INKREAD-0011) — a text-layer PDF. The
    /// shell uses this to enable/disable the Reflow control (disabled for scanned PDFs / EPUB).
    #[must_use]
    pub fn supports_reflow(&self) -> bool {
        self.document.supports_reflow()
    }

    /// Whether the **current** view honors zoom (a fixed-layout page that is not reflowed, RR25-FR3).
    /// The shell gates its zoom entry points (pinch / +−buttons / double-tap) on this so a gesture on
    /// a reflowable view can't strand the shell's zoom factor and skew tap hit-testing.
    #[must_use]
    pub fn is_magnifiable(&self) -> bool {
        self.document.is_magnifiable()
    }

    /// Toggle **reflow mode** on the open PDF (ADR-INKREAD-0011): reconstructs the text and flows it
    /// like a book so the font-size/line-spacing/alignment controls take effect; toggling off
    /// restores the fixed page. Preserves the reading position across the changing page count and
    /// invalidates the (now stale-keyed) crop cache. Returns `true` if the toggle applied, `false`
    /// if reflow is unavailable (no text layer / unsupported format). Re-render after.
    pub fn set_reflow(&mut self, on: bool) -> bool {
        match self.document.set_reflow(on, self.page) {
            Some(new_page) => {
                self.page = new_page.min(self.page_count().saturating_sub(1));
                *self.crop_cache.borrow_mut() = None; // page indices change meaning across the toggle
                self.invalidate_render_cache(); // ...so do the cached page renders
                                                // A reflowed view is never magnified (zoom is fixed-layout only, RR25-FR3). Drop a
                                                // zoomed fixed-layout view to fit BEFORE the load (whose page-turn preserve would
                                                // otherwise carry zoom > 1 into reflow mode — keeping the render off the cached fit
                                                // path on every reflowed turn) (#52 review).
                self.zoom = 1.0;
                self.pan_x = 0.0;
                self.pan_y = 0.0;
                self.load_ink_for_current_page();
                true
            }
            None => false,
        }
    }
}
