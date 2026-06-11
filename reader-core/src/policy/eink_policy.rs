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
        }
    }

    /// Set whether page-turn updates request dithering (default off).
    #[must_use]
    pub fn with_dither(mut self, dither: bool) -> Self {
        self.dither = dither;
        self
    }

    /// The current partial-refresh counter (test/diagnostic accessor).
    #[must_use]
    pub fn partial_count(&self) -> u32 {
        self.partial_count
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
        // `ghost_clear_interval` turns to clear ghosting (RR3-FR3).
        self.partial_count += 1;
        if self.partial_count >= self.ghost_clear_interval {
            self.partial_count = 0;
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

    // ---- M0: minimal-correct streams only; the rich behaviour is M1 (scope fence). ----

    fn on_scroll_start(&mut self) -> Vec<RefreshCommand> {
        // M0 has no scroll mode; emit nothing rather than a speculative Fast stream.
        Vec::new()
    }

    fn on_scroll_update(&mut self, _dirty: Rect) -> Vec<RefreshCommand> {
        Vec::new()
    }

    fn on_scroll_end(&mut self, settle_rect: Rect) -> Vec<RefreshCommand> {
        // Settle the region with a single Partial (or full-screen Full on a basic panel).
        if self.caps.eink_full {
            vec![RefreshCommand::Update {
                rect: settle_rect,
                intent: RefreshIntent::Partial,
                dither: self.dither,
            }]
        } else {
            vec![RefreshCommand::Update {
                rect: self.screen,
                intent: RefreshIntent::Full,
                dither: self.dither,
            }]
        }
    }

    fn on_menu(&mut self, open: bool, region: Rect) -> Vec<RefreshCommand> {
        // Light Ui on open; FlashUi on close so chrome leaves no ghost. Neither touches
        // the page-turn flash counter (RR3-FR5). On a basic panel both collapse to Full.
        let (intent, rect) = if self.caps.eink_full {
            (
                if open {
                    RefreshIntent::Ui
                } else {
                    RefreshIntent::FlashUi
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

    fn on_night_mode(&mut self, _on: bool) -> Vec<RefreshCommand> {
        // A theme flip emits a Full to clear the inverted/non-inverted residue (RR3-FR6).
        // On the Rockchip EBC a Full is full-screen regardless (RR2-FR4).
        vec![RefreshCommand::Update {
            rect: self.screen,
            intent: RefreshIntent::Full,
            dither: self.dither,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use device_eink::MockDeviceRecorder;

    fn screen() -> Rect {
        Rect::full(1404, 1872) // a representative Supernote-class panel
    }

    fn page() -> Rect {
        Rect::new(0, 0, 1404, 1872)
    }

    // RR3-AC1: 6 turns on a capable device => 5 Partial then a WaitForLast+Full, counter resets.
    #[test]
    fn six_page_turns_promote_to_flash_on_the_sixth() {
        let mut rec = MockDeviceRecorder::with_profile(DeviceCapabilities::supernote_full());
        let mut policy = EinkRefreshPolicy::new(rec.capabilities(), screen());

        for _ in 0..6 {
            let cmds = policy.on_page_turn(page());
            rec.execute_all(cmds);
        }

        let expected_partial = RefreshCommand::Update {
            rect: page(),
            intent: RefreshIntent::Partial,
            dither: false,
        };
        let recorded = rec.recorded();
        // 5 partials (turns 1..5) + WaitForLast + Full (turn 6) = 7 commands.
        assert_eq!(recorded.len(), 7);
        assert_eq!(&recorded[0..5], &[expected_partial; 5]);
        assert_eq!(recorded[5], RefreshCommand::WaitForLast);
        assert_eq!(
            recorded[6],
            RefreshCommand::Update {
                rect: page(),
                intent: RefreshIntent::Full,
                dither: false,
            }
        );
        // Counter reset after the flash.
        assert_eq!(policy.partial_count(), 0);
    }

    // RR3-AC3: on an eink_full=false profile, every turn is a full-screen Full.
    #[test]
    fn basic_panel_collapses_every_turn_to_full_screen_full() {
        let mut rec = MockDeviceRecorder::with_profile(DeviceCapabilities::supernote_baseline());
        let mut policy = EinkRefreshPolicy::new(rec.capabilities(), screen());

        for _ in 0..8 {
            let cmds = policy.on_page_turn(page());
            rec.execute_all(cmds);
        }

        let full_screen = RefreshCommand::Update {
            rect: screen(),
            intent: RefreshIntent::Full,
            dither: false,
        };
        assert_eq!(rec.recorded().len(), 8);
        assert!(rec.recorded().iter().all(|c| *c == full_screen));
        // The partial counter never advances on a basic panel.
        assert_eq!(policy.partial_count(), 0);
    }

    #[test]
    fn custom_interval_promotes_on_that_turn() {
        let caps = DeviceCapabilities::supernote_full();
        let mut policy = EinkRefreshPolicy::with_interval(caps, screen(), 3);
        // turns 1,2 => Partial; turn 3 => WaitForLast+Full.
        assert!(matches!(
            policy.on_page_turn(page()).as_slice(),
            [RefreshCommand::Update {
                intent: RefreshIntent::Partial,
                ..
            }]
        ));
        policy.on_page_turn(page());
        let third = policy.on_page_turn(page());
        assert_eq!(third.len(), 2);
        assert_eq!(third[0], RefreshCommand::WaitForLast);
        assert!(matches!(
            third[1],
            RefreshCommand::Update {
                intent: RefreshIntent::Full,
                ..
            }
        ));
        assert_eq!(policy.partial_count(), 0);
    }

    #[test]
    fn interval_zero_is_clamped_to_one() {
        let caps = DeviceCapabilities::supernote_full();
        let mut policy = EinkRefreshPolicy::with_interval(caps, screen(), 0);
        // Every turn flashes.
        let cmds = policy.on_page_turn(page());
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0], RefreshCommand::WaitForLast);
    }

    #[test]
    fn dither_flag_propagates_to_updates() {
        let caps = DeviceCapabilities::supernote_full();
        let mut policy = EinkRefreshPolicy::new(caps, screen()).with_dither(true);
        assert!(matches!(
            policy.on_page_turn(page()).as_slice(),
            [RefreshCommand::Update { dither: true, .. }]
        ));
    }

    #[test]
    fn night_mode_flip_emits_full_screen_full() {
        let caps = DeviceCapabilities::supernote_full();
        let mut policy = EinkRefreshPolicy::new(caps, screen());
        assert_eq!(
            policy.on_night_mode(true),
            vec![RefreshCommand::Update {
                rect: screen(),
                intent: RefreshIntent::Full,
                dither: false,
            }]
        );
    }
}
