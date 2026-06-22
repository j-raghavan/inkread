//! The vendor-neutral refresh vocabulary the policy emits (RR2-FR1).
//!
//! The core never names a waveform: it returns [`RefreshCommand`]s as plain data and
//! the Kotlin adapter maps each [`RefreshIntent`] to the panel's mechanism (RR2-FR3).

use crate::geometry::Rect;

/// What a refresh is *for*, independent of any vendor waveform (RR2-FR1).
///
/// The discriminant values are part of the JNI wire contract (`wire.rs`); keep this
/// order stable and in sync with the Kotlin side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum RefreshIntent {
    /// High-fidelity flashing refresh; clears accumulated ghosting.
    Full = 0,
    /// Anti-ghost content refresh; counts toward the page-turn flash promotion.
    Partial = 1,
    /// Light UI-element update (chrome, status overlay).
    Ui = 2,
    /// 1-bit fast path, for scroll/fling/keyboard.
    Fast = 3,
    /// Flashing UI refresh (e.g. menu close); does NOT count toward promotion.
    FlashUi = 4,
    /// Flashing partial refresh; does NOT count toward promotion.
    FlashPartial = 5,
}

/// A device-agnostic instruction the policy emits for the adapter to execute (RR2-FR1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshCommand {
    /// Refresh `rect` with `intent`; `dither` requests dithering (honored only if the
    /// device advertises `hw_dither`, else the renderer dithers in software, RR2-FR5).
    Update {
        /// The region to refresh.
        rect: Rect,
        /// Why this refresh happens (maps to a waveform in the adapter).
        intent: RefreshIntent,
        /// Whether to dither this update.
        dither: bool,
    },
    /// Sync barrier: block until the prior update's marker completes (RR3-FR8).
    WaitForLast,
    /// Advisory: the adapter MAY pin a persistent fast region (RR2-FR1).
    EnterFastMode,
    /// Leave the advisory fast region.
    LeaveFastMode,
}
