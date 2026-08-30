//! Where the reader is, and what is under their finger (RR11, RR6).
//!
//! Gestures and jumps move the position; the outline and page links say where they can go. The text
//! queries — word at a point, text in a rect, the line span — go through [`view_transform`], which
//! is the inverse of the fit map the render path applied, so a tap in viewport space lands on the
//! right glyph in page space.

use super::*;

impl ReaderSession {
    /// The `[start, end]` reflow-stable anchor pair a selection rectangle covers on `page`, for a
    /// reflowable document — the Digest/highlight locator (RR11-FR4 / #46). `None` for fixed-layout
    /// PDF or an empty selection; the caller falls back to a page anchor.
    #[must_use]
    pub fn selection_pins(
        &self,
        page: usize,
        rect: NormRect,
    ) -> Option<(crate::position::PinPosition, crate::position::PinPosition)> {
        self.document.selection_pins(page, rect)
    }

    /// Apply a navigation gesture: move the position (clamped at the document ends), then
    /// delegate to the policy's `on_page_turn` for the refresh stream (Amendment 6).
    ///
    /// At a boundary (next on the last page, prev on the first) the position does not move,
    /// but the policy is still asked so the panel repaints consistently. Returns the
    /// command stream for the shell to execute.
    pub fn on_gesture(&mut self, gesture: Gesture) -> Vec<RefreshCommand> {
        let last = self.page_count().saturating_sub(1);
        let prev = self.page;
        match gesture {
            Gesture::NextPage => {
                if self.page < last {
                    self.page += 1;
                }
            }
            Gesture::PrevPage => {
                self.page = self.page.saturating_sub(1);
            }
        }
        if self.page != prev {
            self.load_ink_for_current_page();
        }
        let page_rect = Rect::full(self.viewport.width, self.viewport.height);
        self.policy.on_page_turn(page_rect)
    }

    /// Jump to an absolute page index, clamped to `[0, page_count)`, then delegate to the
    /// policy's `on_page_turn` for the refresh stream (RR11-FR1). Used by TOC/scrubber jumps.
    pub fn jump_to_page(&mut self, page: usize) -> Vec<RefreshCommand> {
        let last = self.page_count().saturating_sub(1);
        let prev = self.page;
        self.page = page.min(last);
        if self.page != prev {
            self.load_ink_for_current_page();
        }
        let page_rect = Rect::full(self.viewport.width, self.viewport.height);
        self.policy.on_page_turn(page_rect)
    }

    /// The document outline (RR11-FR2), a pass-through to [`Document::toc`].
    #[must_use]
    pub fn toc(&self) -> Vec<TocEntry> {
        self.document.toc()
    }

    /// The clickable links on `page`, normalized to the rendered page (RR11-FR3) — a
    /// pass-through to [`Document::page_links`]. The shell hit-tests a tap against these.
    #[must_use]
    pub fn page_links(&self, page: usize) -> Vec<PageLink> {
        self.document.page_links(page)
    }

    /// The word under the normalized point `(x, y)` on `page` (RR11 / dictionary tap) — a
    /// pass-through to [`Document::word_at`]. The shell speaks **viewport-normalized** coords (where
    /// it renders + reads touch); the text layer speaks **page-normalized** coords. When the page is
    /// letterboxed in the viewport these differ, so map the input down to page space and the result
    /// boxes back up to viewport space (RR11 — see [`Self::view_transform`]).
    #[must_use]
    pub fn word_at(&self, page: usize, x: f32, y: f32) -> Option<TextSelection> {
        match self.view_transform() {
            Some(t) => {
                let (px, py) = view_to_page_pt((x, y), t);
                self.document
                    .word_at(page, px, py)
                    .map(|s| map_selection_to_view(s, t))
            }
            None => self.document.word_at(page, x, y),
        }
    }

    /// The text within the normalized `rect` on `page` (RR11 / drag-highlight) — viewport↔page mapped
    /// like [`Self::word_at`].
    #[must_use]
    pub fn text_in_rect(&self, page: usize, rect: NormRect) -> TextSelection {
        match self.view_transform() {
            Some(t) => map_selection_to_view(
                self.document.text_in_rect(page, view_to_page_rect(rect, t)),
                t,
            ),
            None => self.document.text_in_rect(page, rect),
        }
    }

    /// Reading-order selection a drag sweeps from `start` to `end` on `page` (RR11 / multi-line
    /// drag) — viewport↔page mapped like [`Self::word_at`].
    #[must_use]
    pub fn text_line_span(&self, page: usize, start: (f32, f32), end: (f32, f32)) -> TextSelection {
        match self.view_transform() {
            Some(t) => map_selection_to_view(
                self.document.text_line_span(
                    page,
                    view_to_page_pt(start, t),
                    view_to_page_pt(end, t),
                ),
                t,
            ),
            None => self.document.text_line_span(page, start, end),
        }
    }

    /// The page→viewport affine `(sx, ox, sy, oy)` for the current fit render (RR11), or `None` when
    /// text coords already equal viewport coords. Returns `None` for the render paths this fit map
    /// doesn't model — pinch-zoom (`zoom > 1`, uses `render_zoom`) and auto-crop (uses
    /// `render_cropped`) — so those fall back to the untransformed pass-through.
    fn view_transform(&self) -> Option<(f32, f32, f32, f32)> {
        // Pinch-zoom renders via render_zoom (different geometry) — skip; fit + auto-crop are both
        // handled by passing the active crop region (matching render_fit_or_crop's choice).
        if self.zoom > 1.0 + 1e-3 {
            return None;
        }
        let crop = if self.crop_auto {
            self.cached_crop_bbox(self.page)
                .map(|b| self.expand_crop(b))
        } else {
            None
        };
        self.document.page_fit_transform(
            self.page,
            self.viewport.width,
            self.viewport.height,
            self.fit_mode,
            self.pan_x,
            self.pan_y,
            crop,
        )
    }

    /// Find `query` on `page` (RR2 in-document search) — a pass-through to
    /// [`Document::search_page`]. The shell drives the scan page-by-page so it stays memory-bounded.
    #[must_use]
    pub fn search_page(&self, page: usize, query: &str) -> Vec<crate::document::SearchMatch> {
        self.document.search_page(page, query)
    }

    /// Navigate to a TOC entry's target page (RR11-AC1). An unresolved entry (no
    /// `target_page`) does not move and returns no refresh commands.
    pub fn jump_to_toc(&mut self, entry: &TocEntry) -> Vec<RefreshCommand> {
        match entry.target_page {
            Some(page) => self.jump_to_page(page),
            None => Vec::new(),
        }
    }
}
