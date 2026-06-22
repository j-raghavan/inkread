//! The reader-core refresh policy (RR3) — the engine-side implementation of the
//! [`RefreshPolicy`](device_eink::RefreshPolicy) contract.
//!
//! M0 implements the page-turn `Partial` + ghost-clear `Full` core (RR3-FR3) and the
//! `!eink_full` collapse-to-full degradation (RR3-FR10); the richer scroll/menu/night
//! behaviour is M1 (out of M0 scope).

mod eink_policy;

pub use eink_policy::{EinkRefreshPolicy, DEFAULT_GHOST_CLEAR_INTERVAL};
