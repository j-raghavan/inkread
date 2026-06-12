//! Unit tests for [`EinkRefreshPolicy`] (RR3), split out to keep `eink_policy.rs` focused.
//! Included via `#[path]` so `super::*` resolves to the policy module.

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

// RR3-FR4 / RR3-AC2: a fling on a fast_mode device uses Fast intents and resets the flash
// counter; settle restores a Partial. EnterFastMode/LeaveFastMode bracket the motion.
#[test]
fn scroll_emits_fast_and_brackets_with_fast_mode() {
    let caps = DeviceCapabilities::supernote_full();
    let mut policy = EinkRefreshPolicy::new(caps, screen());

    assert_eq!(
        policy.on_scroll_start(),
        vec![RefreshCommand::EnterFastMode]
    );
    let dirty = Rect::new(0, 100, 1404, 400);
    assert_eq!(
        policy.on_scroll_update(dirty),
        vec![RefreshCommand::Update {
            rect: dirty,
            intent: RefreshIntent::Fast,
            dither: false,
        }]
    );
    let settle = Rect::new(0, 0, 1404, 1872);
    assert_eq!(
        policy.on_scroll_end(settle),
        vec![
            RefreshCommand::LeaveFastMode,
            RefreshCommand::Update {
                rect: settle,
                intent: RefreshIntent::Partial,
                dither: false,
            },
        ]
    );
}

// RR3-FR4: starting a scroll resets the flash counter (so a fling never mid-flashes), and
// scroll updates never advance it. A subsequent discrete page turn ends the scroll and
// resumes normal promotion.
#[test]
fn scroll_start_resets_counter_then_page_turn_resumes_promotion() {
    let caps = DeviceCapabilities::supernote_full();
    let mut policy = EinkRefreshPolicy::with_interval(caps, screen(), 3);
    // Advance the counter toward the flash threshold (2 of 3).
    policy.on_page_turn(page());
    policy.on_page_turn(page());
    assert_eq!(policy.partial_count(), 2);

    // Scrolling resets the counter to 0; its updates never advance it.
    policy.on_scroll_start();
    assert_eq!(policy.partial_count(), 0);
    policy.on_scroll_update(Rect::new(0, 0, 1404, 100));
    assert_eq!(policy.partial_count(), 0);

    // A discrete page turn ends the scroll: turns 1,2 Partial; turn 3 promotes (interval 3).
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
}

// RR3-FR4 robustness: if on_scroll_end is lost (currently_scrolling stuck true), a discrete
// page turn must still un-stick promotion — never suppress ghost-clears forever.
#[test]
fn page_turn_unsticks_a_lost_scroll_end() {
    let caps = DeviceCapabilities::supernote_full();
    let mut policy = EinkRefreshPolicy::with_interval(caps, screen(), 2);
    policy.on_scroll_start(); // ... and suppose on_scroll_end never arrives.
                              // Page turns still promote normally: turn 1 Partial, turn 2 flashes.
    assert!(matches!(
        policy.on_page_turn(page()).as_slice(),
        [RefreshCommand::Update {
            intent: RefreshIntent::Partial,
            ..
        }]
    ));
    let second = policy.on_page_turn(page());
    assert_eq!(second.len(), 2);
    assert_eq!(second[0], RefreshCommand::WaitForLast);
}

// RR3-FR10: a panel with full control but no fast mode scrolls with Partial (not Fast),
// and emits no EnterFastMode advisory.
#[test]
fn no_fast_mode_scroll_uses_partial() {
    let caps = DeviceCapabilities {
        fast_mode: false,
        ..DeviceCapabilities::supernote_full()
    };
    let mut policy = EinkRefreshPolicy::new(caps, screen());
    assert_eq!(policy.on_scroll_start(), Vec::new());
    let dirty = Rect::new(0, 100, 1404, 400);
    assert_eq!(
        policy.on_scroll_update(dirty),
        vec![RefreshCommand::Update {
            rect: dirty,
            intent: RefreshIntent::Partial,
            dither: false,
        }]
    );
    let settle = Rect::new(0, 0, 1404, 1872);
    // No fast region was entered, so no LeaveFastMode — just the Partial settle.
    assert_eq!(
        policy.on_scroll_end(settle),
        vec![RefreshCommand::Update {
            rect: settle,
            intent: RefreshIntent::Partial,
            dither: false,
        }]
    );
}

// RR3-FR10: a fast panel that can't do regional updates coalesces the dirty rect to a
// full-screen Fast update (the Rockchip full-screen quirk, RR2-FR4).
#[test]
fn no_regional_coalesces_scroll_to_screen() {
    let caps = DeviceCapabilities {
        regional_update: false,
        ..DeviceCapabilities::supernote_full()
    };
    let mut policy = EinkRefreshPolicy::new(caps, screen());
    policy.on_scroll_start();
    assert_eq!(
        policy.on_scroll_update(Rect::new(0, 100, 700, 400)),
        vec![RefreshCommand::Update {
            rect: screen(),
            intent: RefreshIntent::Fast,
            dither: false,
        }]
    );
}

// RR3-FR10 / RR3-AC3: on a basic panel, scroll emits nothing mid-motion and settles full.
#[test]
fn basic_panel_scroll_settles_full_screen() {
    let caps = DeviceCapabilities::supernote_baseline();
    let mut policy = EinkRefreshPolicy::new(caps, screen());
    assert_eq!(policy.on_scroll_start(), Vec::new());
    assert_eq!(
        policy.on_scroll_update(Rect::new(0, 0, 700, 400)),
        Vec::new()
    );
    assert_eq!(
        policy.on_scroll_end(Rect::new(0, 0, 700, 400)),
        vec![RefreshCommand::Update {
            rect: screen(),
            intent: RefreshIntent::Full,
            dither: false,
        }]
    );
}

// RR3-FR7 / RR3-AC4: with avoid_flashing on, the ghost-clear promotion is a Partial (no
// WaitForLast, no Full) but the cadence is preserved (the counter still resets).
#[test]
fn avoid_flashing_downgrades_promotion_to_partial() {
    let caps = DeviceCapabilities::supernote_full();
    let mut policy = EinkRefreshPolicy::with_interval(caps, screen(), 3).with_avoid_flashing(true);
    policy.on_page_turn(page());
    policy.on_page_turn(page());
    let third = policy.on_page_turn(page());
    assert_eq!(
        third,
        vec![RefreshCommand::Update {
            rect: page(),
            intent: RefreshIntent::Partial,
            dither: false,
        }]
    );
    // Cadence preserved: the counter reset even though no flash was emitted.
    assert_eq!(policy.partial_count(), 0);
}

// RR3-FR7: avoid_flashing also downgrades the menu-close FlashUi to a plain Ui.
#[test]
fn avoid_flashing_downgrades_menu_close_to_ui() {
    let caps = DeviceCapabilities::supernote_full();
    let mut policy = EinkRefreshPolicy::new(caps, screen()).with_avoid_flashing(true);
    let region = Rect::new(0, 0, 1404, 200);
    assert!(matches!(
        policy.on_menu(false, region).as_slice(),
        [RefreshCommand::Update {
            intent: RefreshIntent::Ui,
            ..
        }]
    ));
}

// RR3-FR5 / RR3-AC5: opening then closing a menu repaints with Ui then FlashUi and never
// touches the page-turn flash counter.
#[test]
fn menu_open_close_leaves_flash_counter_unchanged() {
    let caps = DeviceCapabilities::supernote_full();
    let mut policy = EinkRefreshPolicy::with_interval(caps, screen(), 6);
    // Advance the page-turn counter.
    policy.on_page_turn(page());
    policy.on_page_turn(page());
    assert_eq!(policy.partial_count(), 2);

    let region = Rect::new(0, 0, 1404, 200);
    // Open: a light Ui.
    assert!(matches!(
        policy.on_menu(true, region).as_slice(),
        [RefreshCommand::Update {
            intent: RefreshIntent::Ui,
            ..
        }]
    ));
    // Close: a FlashUi that leaves no chrome ghost.
    assert!(matches!(
        policy.on_menu(false, region).as_slice(),
        [RefreshCommand::Update {
            intent: RefreshIntent::FlashUi,
            ..
        }]
    ));
    // The page-turn flash counter is unchanged by menu activity.
    assert_eq!(policy.partial_count(), 2);
}

// RR3-FR6: night mode keeps an independent flash counter + interval; entering it flushes
// with a Full, resets the night counter, and leaves the day counter untouched.
#[test]
fn night_mode_uses_independent_counter() {
    let caps = DeviceCapabilities::supernote_full();
    let mut policy = EinkRefreshPolicy::with_interval(caps, screen(), 6).with_night_interval(3);

    // Day mode: 5 turns, below the day interval (6) — no flash.
    for _ in 0..5 {
        policy.on_page_turn(page());
    }
    assert_eq!(policy.partial_count(), 5);
    assert!(!policy.is_night());

    // Enter night mode: a Full flush; night counter fresh; day counter preserved.
    assert_eq!(
        policy.on_night_mode(true),
        vec![RefreshCommand::Update {
            rect: screen(),
            intent: RefreshIntent::Full,
            dither: false,
        }]
    );
    assert!(policy.is_night());
    assert_eq!(policy.partial_count(), 5);
    assert_eq!(policy.night_partial_count(), 0);

    // Night mode: turns 1,2 Partial; turn 3 promotes on the night interval (3).
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
    assert_eq!(policy.night_partial_count(), 0);
    // The day counter was never disturbed by the night-mode cadence.
    assert_eq!(policy.partial_count(), 5);
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
