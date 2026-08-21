//! The daily quotation (#195) — a small piece of character before the article listing.
//!
//! **Curated and bundled, never fetched.** Quotations scraped live are frequently misattributed,
//! and a reader has no way to tell. Shipping a vetted collection and rotating it by date keeps the
//! feature offline, deterministic, host-testable, and free of both a network dependency and an AI.
//!
//! ## The curation rule
//!
//! Every entry is a line from a **published work in the public domain**, and the work is named.
//! That is the whole defence against misattribution: "X said" is unverifiable folklore — the
//! internet is full of Einstein and Twain quips neither man wrote — whereas "X wrote it in Y" can be
//! checked against the text. Floating aphorisms, remarks, interviews and speeches are deliberately
//! excluded however famous, because their provenance cannot be settled from the quotation itself.
//!
//! Adding an entry means naming the work it comes from. If that cannot be done, it does not go in.

/// One vetted quotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quote {
    /// The quotation itself.
    pub text: &'static str,
    /// Who wrote it.
    pub author: &'static str,
    /// The published work it comes from — what makes the attribution checkable.
    pub work: &'static str,
}

/// The bundled collection.
///
/// Every work here was published before 1930 and is in the public domain in the US, which is also
/// where attribution is firmest — these lines have been printed and reprinted for a century, so the
/// wording and the author are settled rather than folklore.
///
/// Excluded on purpose, as worked examples of the rule: "A room without books is like a body
/// without a soul" (attributed to Cicero on no evidence), "I have never let my schooling interfere
/// with my education" (attributed to Twain, unfound in his work), and anything from a work still in
/// copyright however well attested.
const QUOTES: &[Quote] = &[
    Quote {
        text: "It is a truth universally acknowledged, that a single man in possession of a good \
               fortune, must be in want of a wife.",
        author: "Jane Austen",
        work: "Pride and Prejudice (1813)",
    },
    Quote {
        text: "It was the best of times, it was the worst of times.",
        author: "Charles Dickens",
        work: "A Tale of Two Cities (1859)",
    },
    Quote {
        text: "Call me Ishmael.",
        author: "Herman Melville",
        work: "Moby-Dick (1851)",
    },
    Quote {
        text: "All happy families are alike; each unhappy family is unhappy in its own way.",
        author: "Leo Tolstoy",
        work: "Anna Karenina (1878)",
    },
    Quote {
        text: "I am no bird; and no net ensnares me: I am a free human being with an independent \
               will.",
        author: "Charlotte Brontë",
        work: "Jane Eyre (1847)",
    },
    Quote {
        text: "Whatever our souls are made of, his and mine are the same.",
        author: "Emily Brontë",
        work: "Wuthering Heights (1847)",
    },
    Quote {
        text: "I took the one less traveled by, and that has made all the difference.",
        author: "Robert Frost",
        work: "The Road Not Taken (1916)",
    },
    Quote {
        text: "Hope is the thing with feathers that perches in the soul.",
        author: "Emily Dickinson",
        work: "\"Hope\" is the thing with feathers (1891)",
    },
    Quote {
        text: "The mass of men lead lives of quiet desperation.",
        author: "Henry David Thoreau",
        work: "Walden (1854)",
    },
    Quote {
        text: "Beware; for I am fearless, and therefore powerful.",
        author: "Mary Shelley",
        work: "Frankenstein (1818)",
    },
    Quote {
        text:
            "We are such stuff as dreams are made on, and our little life is rounded with a sleep.",
        author: "William Shakespeare",
        work: "The Tempest (1611)",
    },
    Quote {
        text: "There is no frigate like a book to take us lands away.",
        author: "Emily Dickinson",
        work: "There is no Frigate like a Book (1894)",
    },
];

/// The quotation for `date`, chosen deterministically from the date's own text.
///
/// Deterministic rather than random for two reasons: this crate reads no clock — the caller stamps
/// the issue date — and a reader who recompiles today's issue should get today's quote, not a new
/// one each time.
///
/// FNV-1a over the date string, so the sequence does not walk the collection in order on
/// consecutive days the way `day_of_year % len` would.
#[must_use]
pub fn quote_for(date: &str) -> Option<&'static Quote> {
    // No explicit empty check: QUOTES is a const slice, so clippy can see such a test is always
    // false. `get` below returns None on its own if the collection is ever emptied, and the caller
    // already handles that — the cover simply omits the quotation.
    const FNV_OFFSET_64: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME_64: u64 = 0x0000_0100_0000_01b3;
    let mut h = FNV_OFFSET_64;
    for b in date.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(FNV_PRIME_64);
    }
    let len = QUOTES.len() as u64;
    if len == 0 {
        return None;
    }
    QUOTES.get((h % len) as usize)
}

/// The whole collection — for tests, and for anyone auditing the attributions.
#[must_use]
pub fn all() -> &'static [Quote] {
    QUOTES
}

#[cfg(test)]
#[path = "quote_tests.rs"]
mod tests;
