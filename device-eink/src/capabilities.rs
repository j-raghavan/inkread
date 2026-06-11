//! `DeviceCapabilities` — the agnostic seam each adapter advertises (RR2-FR2).
//!
//! The policy emits only what the capabilities permit and degrades the rest (RR3-FR10);
//! it **never branches on a vendor name** (IR-7). The field set and **declaration order**
//! are canonical: the JNI caps codec (`wire.rs`) serializes the flags in this exact order
//! (Fork 3), so adding/reordering a flag is a wire-format change.

/// The capability flags an e-ink device advertises at init.
///
/// Field count and order are load-bearing — they define the caps wire format (Fork 3).
/// All fields are `bool`; [`Self::FLAG_COUNT`] and [`Self::flags`] expose them in
/// declaration order for the codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceCapabilities {
    /// Is an e-ink panel at all (vs LCD/desktop).
    pub eink: bool,
    /// FULL refresh control (partial/fast/flash) vs basic-only. `false` is the honest
    /// Supernote baseline — the policy collapses to periodic full refreshes (RR3-FR10).
    pub eink_full: bool,
    /// Anti-ghost waveform available (Partial → REAGL/REGAL).
    pub regal: bool,
    /// A2/DU fast path for scroll/keyboard.
    pub fast_mode: bool,
    /// Partial-rect refresh (vs full-screen-only — the Rockchip quirk, RR2-FR4).
    pub regional_update: bool,
    /// Hardware night-mode inversion.
    pub hw_invert: bool,
    /// Hardware dithering (else software, RR4).
    pub hw_dither: bool,
    /// Kaleido color waveform.
    pub kaleido_wfm: bool,
    /// Color panel.
    pub color_screen: bool,
    /// Page-turn animation waveform.
    pub swipe_animation: bool,
    /// Dedicated low-latency stylus path.
    pub pen_low_latency: bool,
    /// Force a full refresh on resume.
    pub needs_refresh_after_resume: bool,
}

impl DeviceCapabilities {
    /// The number of `bool` flags — the canonical `nflags` of the caps wire format (Fork 3).
    pub const FLAG_COUNT: usize = 12;

    /// The flags in **declaration order** (= serialization order for the caps codec).
    #[must_use]
    pub fn flags(&self) -> [bool; Self::FLAG_COUNT] {
        [
            self.eink,
            self.eink_full,
            self.regal,
            self.fast_mode,
            self.regional_update,
            self.hw_invert,
            self.hw_dither,
            self.kaleido_wfm,
            self.color_screen,
            self.swipe_animation,
            self.pen_low_latency,
            self.needs_refresh_after_resume,
        ]
    }

    /// Build capabilities from flags in **declaration order**, defaulting any flag the
    /// shell did not send to `false` and ignoring any extra trailing flags (Fork 3).
    ///
    /// `flags.len()` may be smaller (older shell) or larger (newer shell) than
    /// [`Self::FLAG_COUNT`]; only the first `FLAG_COUNT` are read, the rest default false.
    #[must_use]
    pub fn from_flags(flags: &[bool]) -> Self {
        let g = |i: usize| flags.get(i).copied().unwrap_or(false);
        Self {
            eink: g(0),
            eink_full: g(1),
            regal: g(2),
            fast_mode: g(3),
            regional_update: g(4),
            hw_invert: g(5),
            hw_dither: g(6),
            kaleido_wfm: g(7),
            color_screen: g(8),
            swipe_animation: g(9),
            pen_low_latency: g(10),
            needs_refresh_after_resume: g(11),
        }
    }

    /// The honest Supernote M0 baseline: an e-ink panel without full refresh control
    /// (`eink_full = false`) — the policy collapses to periodic full-screen refreshes
    /// (RR2-FR2, RR3-AC3).
    #[must_use]
    pub const fn supernote_baseline() -> Self {
        Self {
            eink: true,
            eink_full: false,
            regal: false,
            fast_mode: false,
            regional_update: false,
            hw_invert: false,
            hw_dither: false,
            kaleido_wfm: false,
            color_screen: false,
            swipe_animation: false,
            pen_low_latency: false,
            needs_refresh_after_resume: true,
        }
    }

    /// The aspirational Supernote profile once the RR19-FR4b spike proves the fast path:
    /// full refresh control + regional partial updates + a fast scroll mode.
    #[must_use]
    pub const fn supernote_full() -> Self {
        Self {
            eink: true,
            eink_full: true,
            regal: false,
            fast_mode: true,
            regional_update: true,
            hw_invert: false,
            hw_dither: false,
            kaleido_wfm: false,
            color_screen: false,
            swipe_animation: false,
            pen_low_latency: true,
            needs_refresh_after_resume: true,
        }
    }

    /// The host/desktop mock profile: not an e-ink panel, full control for testing.
    #[must_use]
    pub const fn desktop_mock() -> Self {
        Self {
            eink: false,
            eink_full: true,
            regal: false,
            fast_mode: true,
            regional_update: true,
            hw_invert: false,
            hw_dither: false,
            kaleido_wfm: false,
            color_screen: true,
            swipe_animation: false,
            pen_low_latency: false,
            needs_refresh_after_resume: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_count_matches_declared_fields() {
        // Guards against adding a field without updating FLAG_COUNT / flags()/from_flags().
        assert_eq!(
            DeviceCapabilities::supernote_full().flags().len(),
            DeviceCapabilities::FLAG_COUNT
        );
    }

    #[test]
    fn flags_round_trip_in_declaration_order() {
        for caps in [
            DeviceCapabilities::supernote_baseline(),
            DeviceCapabilities::supernote_full(),
            DeviceCapabilities::desktop_mock(),
        ] {
            let flags = caps.flags();
            assert_eq!(DeviceCapabilities::from_flags(&flags), caps);
        }
    }

    #[test]
    fn from_flags_defaults_missing_to_false() {
        // Older shell sends only the first 3 flags; the rest default to false.
        let caps = DeviceCapabilities::from_flags(&[true, true, true]);
        assert!(caps.eink && caps.eink_full && caps.regal);
        assert!(!caps.fast_mode && !caps.needs_refresh_after_resume);
    }

    #[test]
    fn from_flags_ignores_extra_trailing_flags() {
        let mut flags = DeviceCapabilities::supernote_full().flags().to_vec();
        flags.push(true); // unknown future flag
        flags.push(true);
        assert_eq!(
            DeviceCapabilities::from_flags(&flags),
            DeviceCapabilities::supernote_full()
        );
    }

    #[test]
    fn baseline_is_not_full() {
        assert!(!DeviceCapabilities::supernote_baseline().eink_full);
        assert!(DeviceCapabilities::supernote_baseline().eink);
    }
}
