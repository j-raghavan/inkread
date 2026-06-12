//! `EinkRefreshPolicy` — the M0 content-aware refresh state machine (RR3).
//!
//! Pure Rust: given an interaction and the device's [`DeviceCapabilities`], it returns the
//! `Vec<RefreshCommand>` the adapter should execute, mutating only its own counter. It
//! never touches the panel and never names a vendor (IR-2, IR-7), so it is fully testable
//! against the [`MockDeviceRecorder`](device_eink::MockDeviceRecorder).

use device_eink::{DeviceCapabilities, Rect, RefreshCommand, RefreshIntent, RefreshPolicy};

/// Default partial-refreshes-before-flash (RR3-FR3; KOReader `DEFAULT_FULL_REFRESH_COUNT`).
pub const DEFAULT_GHOST_CLEAR_INTERVAL: u32 = 6;

/// The reader's refresh policy. Constructed with the device capabilities + the panel size
/// (used for the full-screen fallback) + the ghost-clear interval.
#[derive(Debug, Clone)]
pub struct EinkRefreshPolicy {
    caps: DeviceCapabilities,
    /// Full-screen rect, for the `!eink_full` collapse and the Rockchip full quirk (RR2-FR4).
    screen: Rect,
    /// Partial refreshes accumulated since the last flash (RR3-FR3).
    partial_count: u32,
    /// Flash promotion threshold; user-configurable (default 6).
    ghost_clear_interval: u32,
    /// Whether to request dithering on page-turn updates (honored per `hw_dither`, RR2-FR5).
    dither: bool,
    /// A scroll/fling is in progress (RR3-FR4): while set, the flash counter does not advance
    /// and page-turn promotion is suppressed (a long scroll never mid-flashes).
    currently_scrolling: bool,
    /// Whether night mode is active; selects the night counter/interval (RR3-FR6).
    night_mode: bool,
    /// Partial refreshes since the last flash in night mode — independent of the day counter.
    night_partial_count: u32,
    /// Night-mode flash promotion threshold, independent of the day interval (RR3-FR6).
    night_ghost_clear_interval: u32,
    /// When set, downgrade Full/Flash* promotions to Partial for a flash-free experience
    /// (more ghosting, no flash) (RR3-FR7).
    avoid_flashing: bool,
}

impl EinkRefreshPolicy {
    /// A policy for `caps` on a `screen`-sized panel, using the default ghost-clear interval.
    #[must_use]
    pub fn new(caps: DeviceCapabilities, screen: Rect) -> Self {
        Self::with_interval(caps, screen, DEFAULT_GHOST_CLEAR_INTERVAL)
    }

    /// A policy with an explicit ghost-clear interval (RR3-FR3, user-configurable).
    ///
    /// An interval of 0 is treated as 1 (every page flashes) to avoid a divide-by-never.
    #[must_use]
    pub fn with_interval(
        caps: DeviceCapabilities,
        screen: Rect,
        ghost_clear_interval: u32,
    ) -> Self {
        Self {
            caps,
            screen,
            partial_count: 0,
            ghost_clear_interval: ghost_clear_interval.max(1),
            dither: false,
            currently_scrolling: false,
            night_mode: false,
            night_partial_count: 0,
            night_ghost_clear_interval: ghost_clear_interval.max(1),
            avoid_flashing: false,
        }
    }

    /// Set whether page-turn updates request dithering (default off).
    #[must_use]
    pub fn with_dither(mut self, dither: bool) -> Self {
        self.dither = dither;
        self
    }

    /// Set the night-mode flash-promotion interval, independent of the day interval (RR3-FR6).
    ///
    /// An interval of 0 is clamped to 1 (every night-mode turn flashes).
    #[must_use]
    pub fn with_night_interval(mut self, night_ghost_clear_interval: u32) -> Self {
        self.night_ghost_clear_interval = night_ghost_clear_interval.max(1);
        self
    }

    /// Enable the avoid-flashing downgrade: Full/Flash* promotions become Partial (RR3-FR7).
    #[must_use]
    pub fn with_avoid_flashing(mut self, avoid: bool) -> Self {
        self.avoid_flashing = avoid;
        self
    }

    /// The current partial-refresh counter (test/diagnostic accessor).
    #[must_use]
    pub fn partial_count(&self) -> u32 {
        self.partial_count
    }

    /// The current night-mode partial-refresh counter (test/diagnostic accessor).
    #[must_use]
    pub fn night_partial_count(&self) -> u32 {
        self.night_partial_count
    }

    /// Whether night mode is currently active (RR3-FR6).
    #[must_use]
    pub fn is_night(&self) -> bool {
        self.night_mode
    }

    /// The capabilities this policy was built with.
    #[must_use]
    pub fn capabilities(&self) -> DeviceCapabilities {
        self.caps
    }
}

impl RefreshPolicy for EinkRefreshPolicy {
    fn on_page_turn(&mut self, page_rect: Rect) -> Vec<RefreshCommand> {
        // `!eink_full` (the Supernote baseline): collapse to a periodic full-screen Full
        // refresh — the only correct stream a basic panel can execute (RR3-FR10 / RR3-AC3).
        if !self.caps.eink_full {
            return vec![RefreshCommand::Update {
                rect: self.screen,
                intent: RefreshIntent::Full,
                dither: self.dither,
            }];
        }

        // Full-control panel: Partial per turn, promoting to a flashing Full every
        // `ghost_clear_interval` turns to clear ghosting (RR3-FR3). Night mode keeps a SEPARATE
        // counter + interval (RR3-FR6). A discrete page turn means any prior scroll/fling has
        // ended, so clear the scrolling flag here — this also guards against a lost
        // on_scroll_end leaving promotion suppressed (and the counter climbing) forever
        // (RR3-FR4). A continuous fling never mid-flashes: it drives on_scroll_* only, which
        // reset the counter at start and never advance it.
        self.currently_scrolling = false;
        let interval = if self.night_mode {
            self.night_ghost_clear_interval
        } else {
            self.ghost_clear_interval
        };
        let count = if self.night_mode {
            &mut self.night_partial_count
        } else {
            &mut self.partial_count
        };
        *count += 1;
        let promote = *count >= interval;
        if promote {
            *count = 0;
        }
        // avoid_flashing keeps the cadence (counter still reset) but downgrades the flash to a
        // Partial — more ghosting, no flash (RR3-FR7).
        if promote && !self.avoid_flashing {
            // WaitForLast guards the flash against an in-flight partial (RR3-FR8).
            vec![
                RefreshCommand::WaitForLast,
                RefreshCommand::Update {
                    rect: page_rect,
                    intent: RefreshIntent::Full,
                    dither: self.dither,
                },
            ]
        } else {
            vec![RefreshCommand::Update {
                rect: page_rect,
                intent: RefreshIntent::Partial,
                dither: self.dither,
            }]
        }
    }

    // ---- Scroll/fling (RR3-FR4): Fast intents while moving, flash counter reset. ----

    fn on_scroll_start(&mut self) -> Vec<RefreshCommand> {
        self.currently_scrolling = true;
        // Reset the active flash counter so a long scroll never mid-flashes (RR3-FR4).
        self.partial_count = 0;
        // On a fast-mode panel, advise the adapter to pin a fast region; otherwise no
        // advisory (the no-fast-mode degradation is refined in RR3-FR10).
        if self.caps.fast_mode {
            vec![RefreshCommand::EnterFastMode]
        } else {
            Vec::new()
        }
    }

    fn on_scroll_update(&mut self, dirty: Rect) -> Vec<RefreshCommand> {
        // Capability-aware degradation (RR3-FR10), never branching on a vendor:
        //   !eink_full     → no mid-scroll update; on_scroll_end settles with a Full.
        //   !fast_mode     → scroll uses Partial instead of the 1-bit Fast waveform.
        //   !regional_update → coalesce the dirty rect to a full-screen update (RR2-FR4).
        if !self.caps.eink_full {
            return Vec::new();
        }
        let intent = if self.caps.fast_mode {
            RefreshIntent::Fast
        } else {
            RefreshIntent::Partial
        };
        let rect = if self.caps.regional_update {
            dirty
        } else {
            self.screen
        };
        // The Fast waveform is 1-bit; only a Partial honors the dither request.
        let dither = intent == RefreshIntent::Partial && self.dither;
        vec![RefreshCommand::Update {
            rect,
            intent,
            dither,
        }]
    }

    fn on_scroll_end(&mut self, settle_rect: Rect) -> Vec<RefreshCommand> {
        self.currently_scrolling = false;
        // A basic panel can only do a full-screen Full (RR3-FR10 / RR2-FR4).
        if !self.caps.eink_full {
            return vec![RefreshCommand::Update {
                rect: self.screen,
                intent: RefreshIntent::Full,
                dither: self.dither,
            }];
        }
        // Leave the advisory fast region (if entered) and settle the page with a Partial.
        let mut cmds = Vec::new();
        if self.caps.fast_mode {
            cmds.push(RefreshCommand::LeaveFastMode);
        }
        cmds.push(RefreshCommand::Update {
            rect: settle_rect,
            intent: RefreshIntent::Partial,
            dither: self.dither,
        });
        cmds
    }

    fn on_menu(&mut self, open: bool, region: Rect) -> Vec<RefreshCommand> {
        // Light Ui on open; FlashUi on close so chrome leaves no ghost. Neither touches
        // the page-turn flash counter (RR3-FR5). On a basic panel both collapse to Full.
        let (intent, rect) = if self.caps.eink_full {
            // avoid_flashing downgrades the closing FlashUi to a plain Ui (RR3-FR7).
            let close_intent = if self.avoid_flashing {
                RefreshIntent::Ui
            } else {
                RefreshIntent::FlashUi
            };
            (
                if open {
                    RefreshIntent::Ui
                } else {
                    close_intent
                },
                region,
            )
        } else {
            (RefreshIntent::Full, self.screen)
        };
        vec![RefreshCommand::Update {
            rect,
            intent,
            dither: self.dither,
        }]
    }

    fn on_night_mode(&mut self, on: bool) -> Vec<RefreshCommand> {
        // First-class night mode (RR3-FR6): switch the active theme and reset the counter of
        // the mode being entered so its promotion cadence starts fresh. A theme flip emits a
        // Full to clear the inverted/non-inverted residue; on the Rockchip EBC a Full is
        // full-screen regardless (RR2-FR4).
        self.night_mode = on;
        if on {
            self.night_partial_count = 0;
        } else {
            self.partial_count = 0;
        }
        // Honor avoid_flashing consistently: a capable panel downgrades the flip flush to a
        // Partial (RR3-FR7); a basic panel can only do a full-screen Full.
        let intent = if self.avoid_flashing && self.caps.eink_full {
            RefreshIntent::Partial
        } else {
            RefreshIntent::Full
        };
        vec![RefreshCommand::Update {
            rect: self.screen,
            intent,
            dither: self.dither,
        }]
    }
}

#[cfg(test)]
#[path = "eink_policy_tests.rs"]
mod tests;
