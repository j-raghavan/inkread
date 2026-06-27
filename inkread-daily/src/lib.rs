//! `inkread-daily` (#66): turn followed feed/article sources into a self-contained **daily-issue
//! EPUB** that the inkread reader opens like any book — calm, offline, e-ink-first reading.
//!
//! This crate is the *assembly* half: it takes already-extracted [`Article`]s, composes them into an
//! [`Issue`], and serializes that to an EPUB ([`assemble_epub`]). It is **pure and host-testable** —
//! no network and no clock. Fetching feeds and extracting readable text live at the edges (the
//! Android shell does the network; a later slice does HTML→clean extraction), keeping the core
//! vendor- and IO-free (RR1-AC3 / IR-7). Delivery as EPUB means the whole existing reader — reflow,
//! font-size/spacing controls, reflow-stable resume — is reused with no new rendering code.

mod epub;
mod model;

pub use epub::assemble_epub;
pub use model::{Article, Issue, Source};
