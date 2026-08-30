//! The reader-core refresh policy (RR3) — the engine-side implementation of the
//! [`RefreshPolicy`](device_eink::RefreshPolicy) contract.
//!
//! Implements the page-turn `Partial` + ghost-clear `Full` core (RR3-FR3), the `!eink_full`
//! collapse-to-full degradation (RR3-FR10), scroll/fling suppression so a long scroll never
//! mid-flashes (RR3-FR4), and a separate night-mode flash cadence (RR3-FR6).

mod eink_policy;

pub use eink_policy::{EinkRefreshPolicy, DEFAULT_GHOST_CLEAR_INTERVAL};
