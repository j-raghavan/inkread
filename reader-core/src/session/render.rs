//! The render path and its caches (RR4, RR24).
//!
//! A page is rendered once into a bounded cache, fit or cropped to the viewport, and the next page
//! is prefetched behind it so a page turn is usually a cache hit. The cache key carries everything
//! that changes pixels, which is why invalidation is a single call rather than a list of places to
//! remember.

use super::*;

impl ReaderSession {
    /// Update the viewport (e.g. `surfaceChanged`/rotation, RR21-FR4); rebuilds the
    /// policy's full-screen rect. Returns nothing; the shell re-renders + re-asks for
    /// a refresh afterward.
    pub fn set_viewport(&mut self, viewport: Viewport) {
        // A surface can be handed the same size more than once (Android delivers surfaceChanged
        // repeatedly for one surface). Rebuilding the policy and dropping every cached render for a
        // viewport that did not change throws away exactly the work that was about to be reused —
        // on the open path, that is the page just rendered (#186).
        if self.viewport == viewport {
            return;
        }
        self.viewport = viewport;
        // Re-point the policy at the new panel rather than rebuilding it. A rebuild reverted the
        // reader's flash interval, night interval and avoid-flashing to their defaults, silently
        // and for the rest of the session — the UI and the store still held the chosen values, so
        // only the policy actually driving the panel had forgotten them (#206). `set_screen` still
        // restarts the flash counters, which is all the rebuild was wanted for: a fresh full is
        // expected after a metrics change anyway (RR21-FR4).
        self.policy
            .set_screen(Rect::full(viewport.width, viewport.height));
        // Cached renders are sized to the old viewport and laid out for it — drop them.
        self.invalidate_render_cache();
    }

    /// Render the current page into the shell's borrowed buffer (RR4 / Amendment 5).
    ///
    /// The buffer must match the session viewport; the borrow does not outlive this call. The
    /// non-magnified page render is served from the bounded render cache when an identical
    /// `(page + view-settings)` buffer is held (RR4-FR6 / RR24) — re-rasterization is skipped on a
    /// revisit (e.g. paging back and forth). `&mut self` because a hit/insert mutates the cache.
    pub fn render_current(&mut self, buf: &mut PixelBuffer<'_>) -> CoreResult<()> {
        if buf.width() != self.viewport.width || buf.height() != self.viewport.height {
            return Err(CoreError::BufferMismatch(format!(
                "buffer {}x{} != viewport {}x{}",
                buf.width(),
                buf.height(),
                self.viewport.width,
                self.viewport.height
            )));
        }
        if self.zoom > 1.0 + 1e-3 {
            // Magnified view: content is buf*zoom; show a buf-sized window panned over the overscan.
            // (Render quality is not applied during a transient pinch-zoom.) Not cached — the pan
            // window slides continuously, so a cache would thrash without ever paying off.
            let bw = self.viewport.width as f32;
            let bh = self.viewport.height as f32;
            let off_x = (self.pan_x * bw * (self.zoom - 1.0)).round() as i32;
            let off_y = (self.pan_y * bh * (self.zoom - 1.0)).round() as i32;
            self.document
                .render_zoom(self.page, buf, self.zoom, off_x, off_y)?;
            crate::render::contrast::apply_contrast(
                buf,
                crate::render::contrast::step_to_gamma(self.contrast),
            );
            if self.night {
                crate::render::gray::invert_in_place(buf);
            }
            return Ok(());
        }
        // Non-magnified page render — the page-turn / revisit case. The rendered bytes are a pure
        // function of (page + view-settings): ink is composited by the shell, never baked here, and
        // page content is immutable. So an identical key may be served straight from the cache,
        // skipping the pdfium rasterization. Only the resting view (no pan) is cached; a panned fit
        // window is transient like the zoom case.
        let cacheable = self.pan_x == 0.0 && self.pan_y == 0.0;
        let key = self.render_cache_key();
        if cacheable {
            if let Some(bytes) = self.caches.render().get(&key) {
                if bytes.len() == buf.bytes().len() {
                    buf.bytes_mut().copy_from_slice(bytes);
                    return Ok(());
                }
            }
        }
        self.render_fit_pixels(buf)?;
        // Grayscale + dithering are deliberately NOT applied here. The shell blits this RGBA buffer
        // to a SurfaceView and the device's EPD controller does the waveform grayscale + dithering
        // in hardware; pre-quantizing in the core would double-process (fighting the panel) for no
        // gain. The `render::gray` module (to_grayscale / DitherMode) is retained for host/emulator
        // rendering + golden tests, and for a future direct-framebuffer path (KOReader-style fb
        // ioctl) that WOULD bypass the panel's conversion and need to dither itself — that is the only
        // path where the DitherMode setting becomes live. The cache key fixes DitherMode::None to keep
        // the key honest about this. (Reflow/EPUB is already grayscale-native via inkread-epub's
        // GrayCanvas; this note is about the fixed-layout/PDF RGBA path.)
        if cacheable {
            self.caches.render().insert(key, buf.bytes().to_vec());
        }
        Ok(())
    }

    /// Rasterize the current page's fit/crop pixels (honoring render-quality supersampling) and apply
    /// contrast, into `buf`. The shared core of [`Self::render_current`]'s non-magnified path and
    /// [`Self::prefetch_page`]; touches neither the cache nor zoom.
    fn render_fit_pixels(&self, buf: &mut PixelBuffer<'_>) -> CoreResult<()> {
        let q = render_quality_factor(self.render_quality);
        if (q - 1.0).abs() < 1e-3 {
            self.render_fit_or_crop(buf)?;
        } else {
            // Render at q× the panel resolution, then bilinear-resample down/up to the panel —
            // supersampling (high) smooths e-ink text; sub-sampling (low) is faster/softer.
            let qw = ((self.viewport.width as f32 * q).round() as u32).clamp(1, 8000);
            let qh = ((self.viewport.height as f32 * q).round() as u32).clamp(1, 8000);
            let mut tmp = vec![0u8; (qw as usize) * (qh as usize) * 4];
            {
                let mut tbuf = PixelBuffer::from_rgba(&mut tmp, qw, qh)?;
                self.render_fit_or_crop(&mut tbuf)?;
            }
            crate::render::resample::resample_bilinear(&tmp, qw, qh, buf);
        }
        // Display enhancement (RR4): remap pixels for contrast after the backend renders.
        crate::render::contrast::apply_contrast(
            buf,
            crate::render::contrast::step_to_gamma(self.contrast),
        );
        if self.night {
            crate::render::gray::invert_in_place(buf); // light-on-dark (night mode)
        }
        Ok(())
    }

    /// Render-ahead: rasterize `page` into the render cache **without displaying it**, so a turn to it
    /// is a cache hit (the biggest page-turn cost is the pdfium/reflow raster — RR24). No-op when
    /// magnified (only fit pages are cached) or when the page is already warm. Meant to be called on
    /// the engine thread right after the visible page is rendered, so it never delays the current turn
    /// (and if the reader turns before it finishes, the queued render simply serves the next turn).
    pub fn prefetch_page(&mut self, page: usize) -> CoreResult<()> {
        if self.zoom > 1.0 + 1e-3 {
            return Ok(());
        }
        let page = page.min(self.page_count().saturating_sub(1));
        let saved = self.page;
        self.page = page;
        let result = self.prefetch_current_into_cache();
        self.page = saved; // never leave the displayed page changed, even on error
        result
    }

    /// Render the (temporarily swapped-in) current page into the cache if not already present.
    fn prefetch_current_into_cache(&mut self) -> CoreResult<()> {
        let key = self.render_cache_key();
        if self.caches.render().get(&key).is_some() {
            return Ok(()); // already warm — nothing to do
        }
        let (w, h) = (self.viewport.width, self.viewport.height);
        let mut scratch = vec![0u8; (w as usize) * (h as usize) * 4];
        {
            let mut buf = PixelBuffer::from_rgba(&mut scratch, w, h)?;
            self.render_fit_pixels(&mut buf)?;
        }
        self.caches.render().insert(key, scratch);
        Ok(())
    }

    /// The render-cache key for the current page + view-settings (RR4-FR6). The pixel-pipeline axes
    /// (zoom/rotation/invert/dither/gamma) are at their non-magnified defaults — the cache is only
    /// consulted on the fit path — so only the page and the view-settings vary.
    /// The cache key, for tests that assert a setting participates in it.
    #[cfg(test)]
    pub(super) fn render_cache_key_for_test(&self) -> crate::render::PageHash {
        self.render_cache_key()
    }

    fn render_cache_key(&self) -> crate::render::PageHash {
        crate::render::PageHash::new(
            self.page as u32,
            1.0,
            0,
            self.night, // invert flag — night pages cache separately from day
            crate::render::DitherMode::None,
            1.0,
        )
        .with_view(
            self.fit_mode.code(),
            self.crop_auto,
            self.crop_margin,
            self.render_quality,
            self.contrast,
        )
    }

    /// Drop cached page renders whose content/geometry changed underneath their key — a reflow
    /// toggle, a repagination, or a viewport resize. The view-setting axes live *in* the key, so a
    /// fit/crop/quality/contrast change needs no invalidation; only a change to what a given page
    /// index renders to (or the buffer size) does.
    pub(super) fn invalidate_render_cache(&mut self) {
        self.caches.render().clear();
    }

    /// Render the current page fit (or auto-cropped) into `buf` (RR4). With auto-crop on, the white
    /// margins are trimmed to the detected content box; otherwise an aspect-preserving fit. PDF
    /// honors both; reflowable backends fall back to a full-buffer render.
    fn render_fit_or_crop(&self, buf: &mut PixelBuffer<'_>) -> CoreResult<()> {
        match self
            .crop_auto
            .then(|| self.cached_crop_bbox(self.page))
            .flatten()
        {
            Some(b) => {
                let crop = self.expand_crop(b);
                self.document.render_cropped(
                    self.page,
                    buf,
                    crop,
                    self.fit_mode,
                    self.pan_x,
                    self.pan_y,
                )
            }
            None => self
                .document
                .render_fit(self.page, buf, self.fit_mode, self.pan_x, self.pan_y),
        }
    }

    /// The content bounding box for `page`, memoized per page (recomputed on a page change).
    pub(super) fn cached_crop_bbox(&self, page: usize) -> Option<NormRect> {
        if let Some((p, b)) = self.crop_cache.borrow().as_ref() {
            if *p == page {
                return *b;
            }
        }
        let b = self.document.content_bbox(page);
        *self.crop_cache.borrow_mut() = Some((page, b));
        b
    }

    /// Expand a content box by the current margin (kept within the page).
    pub(super) fn expand_crop(&self, b: NormRect) -> NormRect {
        let m = f32::from(self.crop_margin) * 0.01;
        NormRect {
            x0: (b.x0 - m).clamp(0.0, 1.0),
            y0: (b.y0 - m).clamp(0.0, 1.0),
            x1: (b.x1 + m).clamp(0.0, 1.0),
            y1: (b.y1 + m).clamp(0.0, 1.0),
        }
    }
}
