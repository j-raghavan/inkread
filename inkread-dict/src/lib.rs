//! `inkread-dict` — the offline-first dictionary + thesaurus engine (RR12 / ADR-INKREAD-0009 D2).
//!
//! A [`Dict`] resolves a tapped/highlighted word to a [`Definition`] (senses + synonyms) over an
//! **indexed SQLite corpus** (`dict.db`, pre-built offline from StarDict + WordNet — D2 build step).
//! Lookup is layered for the Kindle-like "finds the base word" feel: exact → normalized →
//! stem-stripped, across the document's likely languages. A miss may fall through to an
//! **online source** (the shell supplies the network impl via [`OnlineSource`], so the core stays
//! offline-testable, IR-4); online hits are **cached** back into the DB so a repeat is instant.
//!
//! This crate is pure logic + SQLite; it names no vendor and is fully host-tested against an
//! in-memory fixture.

mod engine;

pub use engine::Dict;

/// A resolved dictionary entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    /// The display headword that matched (may differ from the query after stemming).
    pub headword: String,
    /// BCP-47-ish language code of the matched entry (`en`, `it`, `fr`, `da`, …).
    pub lang: String,
    /// One or more sense lines.
    pub senses: Vec<String>,
    /// Thesaurus synonyms for the headword (may be empty).
    pub synonyms: Vec<String>,
}

/// A network dictionary source the shell wires up (Wiktionary, etc.). Kept as a port so the engine
/// — and its tests — never touch the network (RR19-FR9: online is user-configured + opt-in).
pub trait OnlineSource: Send + Sync {
    /// Look `word` up online, or `None` on a miss/error. Implementations must be non-blocking-safe
    /// for the caller's thread model (the shell calls this off the UI thread).
    fn lookup(&self, word: &str) -> Option<OnlineEntry>;
}

/// A definition fetched from an [`OnlineSource`], ready to cache + return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnlineEntry {
    /// Language code of the result.
    pub lang: String,
    /// The headword as the source spells it.
    pub headword: String,
    /// Sense lines.
    pub senses: Vec<String>,
}

/// The dictionary result alias.
pub type DictResult<T> = Result<T, DictError>;

/// The dictionary error surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DictError {
    /// Opening or preparing the corpus failed.
    Backend(String),
}

impl std::fmt::Display for DictError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DictError::Backend(m) => write!(f, "dictionary backend error: {m}"),
        }
    }
}

impl std::error::Error for DictError {}
