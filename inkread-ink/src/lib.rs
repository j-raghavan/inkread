//! `inkread-ink` — the vector-ink domain (RR6 / ADR-INKREAD-0004).
//!
//! A pure, dependency-free model of handwritten ink: a [`Stroke`] is an ordered list of
//! [`InkPoint`] samples with a [`Tool`], an [`InkColor`] (preserved even on grayscale panels —
//! RR6-FR6), and a nominal width. An [`InkLayer`] holds all strokes on one page and owns the
//! undo/redo history and eraser hit-testing (RR6-FR3).
//!
//! Coordinates are **normalized to the page** — `[0,1]` on both axes, top-left origin — exactly
//! like `reader-core`'s `PageLink`, so ink is resolution-independent and survives a viewport
//! change or a re-render at a different size. Pressure is normalized `[0,1]`.
//!
//! This crate is **host-only and names no vendor** (IR-7). It owns its own [`InkError`]; the
//! `reader-core` boundary maps that to its `CoreError`. Time is never read here (no clock) — a
//! stroke's `created_at_ms` is supplied by the caller so the model stays deterministic and
//! host-testable.
//!
//! The on-disk `.inkbin` encoding will live in a `codec` module (added next), mirroring
//! `reader-core`'s versioned, little-endian, saturating wire codecs.

pub mod model;

pub use model::{BBox, InkColor, InkError, InkLayer, InkPoint, Stroke, StrokeId, Tool};
