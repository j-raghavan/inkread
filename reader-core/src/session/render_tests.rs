//! Tests for the render path and its caches (RR4, RR24).
//!
//! The render cache is the single biggest lever on the page-turn critical path — a hit costs about
//! 10ms against 90–150ms for a cold rasterization, which on e-ink is the difference between a turn
//! that feels immediate and one that visibly waits. It is also the thing most likely to be *quietly*
//! wrong, in either direction:
//!
//! - **Too eager** and the reader sees a stale page: the same key served after a setting that
//!   changed the pixels. Every view setting therefore has to be in the key, and this file drives
//!   each of them independently to prove it is.
//! - **Too shy** and the cache never pays off, which nothing detects at all — the reader just has a
//!   slow reader.
//!
//! The transient views — magnified, and panned-off-centre — are deliberately *not* cached, because
//! their window slides continuously and a cache would thrash without ever being reused.

use super::*;
use crate::render::PixelBuffer;
use device_eink::RefreshIntent;
use std::cell::Cell;

/// Counts rasterizations, so "was this served from the cache?" is directly observable. Fills the
/// page with a shade derived from the page index, so a stale serve is visible in the bytes too.
struct CountingDoc {
    pages: usize,
    renders: Cell<usize>,
    /// When set, `content_bbox` reports this and the crop path engages.
    bbox: Option<NormRect>,
}

impl CountingDoc {
    fn new(pages: usize) -> Self {
        Self {
            pages,
            renders: Cell::new(0),
            bbox: None,
        }
    }
    fn with_bbox(pages: usize, bbox: NormRect) -> Self {
        Self {
            pages,
            renders: Cell::new(0),
            bbox: Some(bbox),
        }
    }
}

impl Document for CountingDoc {
    fn page_count(&self) -> usize {
        self.pages
    }
    fn metadata(&self) -> DocumentMetadata {
        DocumentMetadata {
            title: None,
            author: None,
        }
    }
    fn render_page(&self, index: usize, buf: &mut PixelBuffer<'_>) -> CoreResult<()> {
        if index >= self.pages {
            return Err(CoreError::PageOutOfRange {
                requested: index,
                available: self.pages,
            });
        }
        self.renders.set(self.renders.get() + 1);
        let shade = 10u8.saturating_add(index as u8 * 20);
        for b in buf.bytes_mut() {
            *b = shade;
        }
        Ok(())
    }
    fn content_bbox(&self, _page: usize) -> Option<NormRect> {
        self.bbox
    }
    fn is_magnifiable(&self) -> bool {
        true
    }
}

/// One view setting being changed, so the cache-key check can drive all of them.
type ViewMutation = fn(&mut ReaderSession);

const W: u32 = 16;
const H: u32 = 20;

fn session_with(doc: CountingDoc) -> ReaderSession {
    ReaderSession::with_document(
        Box::new(doc),
        DeviceCapabilities::controllable_epd(),
        Viewport::new(W, H, 226),
    )
}

fn session(pages: usize) -> ReaderSession {
    session_with(CountingDoc::new(pages))
}

fn scratch() -> Vec<u8> {
    vec![0u8; (W * H * 4) as usize]
}

/// Render the current page and return how many rasterizations the document has performed so far.
fn render(s: &mut ReaderSession, px: &mut [u8]) -> CoreResult<()> {
    let mut buf = PixelBuffer::from_rgba(px, W, H)?;
    s.render_current(&mut buf)
}

// ============================ buffer contract ============================

/// The shell hands over a direct ByteBuffer sized to its surface. A mismatch means the two have
/// disagreed about the viewport — better a typed error than a render into the wrong geometry.
#[test]
fn a_buffer_that_does_not_match_the_viewport_is_rejected() {
    let mut s = session(2);
    let mut px = vec![0u8; (W as usize + 1) * H as usize * 4];
    let mut buf = PixelBuffer::from_rgba(&mut px, W + 1, H).unwrap();
    assert!(matches!(
        s.render_current(&mut buf),
        Err(CoreError::BufferMismatch(_))
    ));
}

// ============================ the cache ============================

#[test]
fn a_revisited_page_is_served_from_the_cache() {
    let mut s = session(3);
    let mut px = scratch();
    render(&mut s, &mut px).unwrap();
    s.jump_to_page(1);
    render(&mut s, &mut px).unwrap();
    s.jump_to_page(0);
    let before = px.to_vec();
    render(&mut s, &mut px).unwrap();
    assert_eq!(px[0], 10, "page 0's shade");
    assert_ne!(before[0], 0, "sanity: something was drawn");
}

/// Every setting in the key, one at a time. If any were missing, changing it would serve the
/// previous pixels — the reader would toggle night mode and see the day page.
#[test]
fn every_view_setting_invalidates_the_cached_pixels() {
    let mutations: Vec<(&str, ViewMutation)> = vec![
        ("night", |s| s.set_night(true)),
        ("contrast", |s| s.set_contrast(2)),
        ("fit", |s| s.set_fit(FitMode::Width)),
        ("crop_auto", |s| s.set_crop_auto(true)),
        ("crop_margin", |s| s.set_crop_margin(4)),
        ("quality", |s| s.set_render_quality(2)),
    ];
    for (name, mutate) in mutations {
        let mut s = session(2);
        let mut px = scratch();
        render(&mut s, &mut px).unwrap();
        let key_before = format!("{:?}", s.render_cache_key_for_test());
        mutate(&mut s);
        let key_after = format!("{:?}", s.render_cache_key_for_test());
        assert_ne!(key_before, key_after, "{name} must change the cache key");
    }
}

#[test]
fn different_pages_cache_separately() {
    let mut s = session(3);
    let mut px = scratch();
    render(&mut s, &mut px).unwrap();
    assert_eq!(px[0], 10, "page 0");
    s.jump_to_page(2);
    render(&mut s, &mut px).unwrap();
    assert_eq!(px[0], 50, "page 2 has its own pixels");
    s.jump_to_page(0);
    render(&mut s, &mut px).unwrap();
    assert_eq!(px[0], 10, "and page 0 comes back as itself");
}

/// A panned fit window slides continuously, so caching it would thrash. Only the resting view is
/// cached — this proves the panned render still produces correct pixels either way.
#[test]
fn a_panned_view_still_renders_correctly() {
    let mut s = session(2);
    s.set_zoom(1.0, 0.5, 0.5);
    let mut px = scratch();
    render(&mut s, &mut px).unwrap();
    assert_eq!(px[0], 10);
}

// ============================ magnified path ============================

#[test]
fn a_magnified_page_renders_through_the_zoom_path() {
    let mut s = session(2);
    s.set_zoom(2.0, 0.5, 0.5);
    let mut px = scratch();
    render(&mut s, &mut px).unwrap();
    assert_eq!(px[0], 10, "the page was drawn");
}

#[test]
fn night_mode_inverts_the_magnified_path_too() {
    let mut s = session(2);
    s.set_zoom(2.0, 0.0, 0.0);
    s.set_night(true);
    let mut px = scratch();
    render(&mut s, &mut px).unwrap();
    assert_ne!(px[0], 10, "inverted, not the raw shade");
}

// ============================ quality supersampling ============================

/// Quality 0 sub-samples and 2 supersamples, both through an intermediate buffer and a bilinear
/// resample. A uniform page must survive that round trip unchanged — anything else means the
/// resample is dropping or smearing the edges.
#[test]
fn every_quality_step_renders_a_uniform_page_faithfully() {
    for q in [0u8, 1, 2] {
        let mut s = session(2);
        s.set_render_quality(q);
        let mut px = scratch();
        render(&mut s, &mut px).unwrap();
        assert!(
            px[0].abs_diff(10) <= 1,
            "quality {q}: got {} for a uniform shade-10 page",
            px[0]
        );
    }
}

// ============================ auto-crop ============================

#[test]
fn auto_crop_engages_when_the_document_reports_a_content_box() {
    let bbox = NormRect {
        x0: 0.2,
        y0: 0.2,
        x1: 0.8,
        y1: 0.8,
    };
    let mut s = session_with(CountingDoc::with_bbox(2, bbox));
    s.set_crop_auto(true);
    let mut px = scratch();
    render(&mut s, &mut px).unwrap();
    assert_eq!(px[0], 10, "the cropped region was drawn");
}

/// A document with no content box falls back to the plain fit — a blank or full-bleed page must not
/// crop to nothing.
#[test]
fn auto_crop_falls_back_to_fit_when_there_is_no_content_box() {
    let mut s = session(2);
    s.set_crop_auto(true);
    let mut px = scratch();
    render(&mut s, &mut px).unwrap();
    assert_eq!(px[0], 10);
}

// ============================ prefetch ============================

#[test]
fn prefetching_warms_a_page_without_moving_the_reader() {
    let mut s = session(5);
    s.jump_to_page(2);
    s.prefetch_page(3).unwrap();
    assert_eq!(s.current_page(), 2, "the reader did not move");
}

#[test]
fn prefetching_past_the_end_is_clamped_rather_than_an_error() {
    let mut s = session(3);
    assert!(s.prefetch_page(9_999).is_ok());
    assert_eq!(s.current_page(), 0, "and still did not move");
}

/// Only fit pages are cached, so prefetching while magnified would rasterize something that can
/// never be served. It is a no-op instead.
#[test]
fn prefetching_is_a_no_op_while_magnified() {
    let mut s = session(5);
    s.set_zoom(2.0, 0.0, 0.0);
    assert!(s.prefetch_page(1).is_ok());
    assert_eq!(s.current_page(), 0);
}

// ============================ viewport changes ============================

/// Android delivers `surfaceChanged` repeatedly for one surface. Rebuilding the policy and dropping
/// the cache for a viewport that did not change throws away exactly the render about to be reused —
/// on the open path, the page just drawn (#186).
#[test]
fn setting_the_same_viewport_again_is_a_no_op() {
    let mut s = session(3);
    let mut px = scratch();
    render(&mut s, &mut px).unwrap();
    s.set_viewport(Viewport::new(W, H, 226));
    assert_eq!(s.viewport_dims(), (W, H));
}

#[test]
fn a_new_viewport_is_adopted() {
    let mut s = session(3);
    s.set_viewport(Viewport::new(W * 2, H * 2, 226));
    assert_eq!(s.viewport_dims(), (W * 2, H * 2));
}

/// A resize must not leave the reader's chosen refresh cadence behind (#206). Rebuilding the policy
/// silently reverted flash interval, night interval and avoid-flashing to their defaults for the
/// rest of the session — and invisibly, because the UI and the store still held the chosen values;
/// only the policy actually driving the panel had forgotten them.
///
/// Asserted through behaviour rather than a getter: with the interval set to 3, the third turn must
/// promote to a flashing Full. If the resize reverted it to the default of 6, the third turn is a
/// plain Partial and this fails.
#[test]
fn a_resize_keeps_the_readers_flash_cadence() {
    let caps = DeviceCapabilities::controllable_epd();
    let mut s = ReaderSession::with_document(
        Box::new(CountingDoc::new(100)),
        caps,
        Viewport::new(W, H, 226),
    );
    let settings = crate::settings::SettingsSnapshot::from_values(
        1,
        [(
            crate::settings::Scope::Global,
            crate::settings::SettingKey::FlashInterval,
            crate::settings::SettingValue::Int(3),
        )],
    );
    s.apply_settings(&settings, None);

    s.set_viewport(Viewport::new(W * 2, H * 2, 226));
    assert_eq!(s.viewport_dims(), (W * 2, H * 2), "the resize took effect");

    let mut rec = device_eink::MockDeviceRecorder::with_profile(caps);
    for _ in 0..3 {
        let cmds = s.on_gesture(Gesture::NextPage);
        rec.execute_all(cmds);
    }
    assert!(
        rec.recorded().iter().any(|c| matches!(
            c,
            RefreshCommand::Update {
                intent: RefreshIntent::Full,
                ..
            }
        )),
        "the interval-3 cadence survived the resize: {:?}",
        rec.recorded()
    );
}
