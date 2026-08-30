//! Finding a query on a page (RR11).
//!
//! Case- and accent-insensitive matching over the page's glyph stream, returning each hit's boxes
//! plus a short snippet for the results list. Normalization happens once per query, not per glyph.

use super::word::wrap_of;
use super::*; // a match that spans a line break reads its wrap off the printed hyphen

/// Context characters kept on each side of a match for its results-list snippet.
const SNIPPET_CONTEXT: usize = 28;

/// Case-insensitive, whitespace-normalized substring search over a page's `chars`. Returns one
/// [`SearchMatch`] per **non-overlapping** occurrence, left to right, each with per-line highlight
/// boxes and a context snippet. An empty or whitespace-only `query` yields no matches. Pure and
/// dependency-free (host-tested) — the backend only supplies the page's `CharBox`es (RR21-FR3:
/// never panics).
#[must_use]
pub fn find_matches(chars: &[CharBox], query: &str) -> Vec<SearchMatch> {
    let needle: Vec<char> = normalize_query(query);
    if needle.is_empty() {
        return Vec::new();
    }
    // Normalized page text as chars, with a parallel map from each normalized char back to its
    // source `chars` index (so a hit's positions resolve to highlight boxes + a snippet).
    let mut hay: Vec<char> = Vec::with_capacity(chars.len());
    let mut src: Vec<usize> = Vec::with_capacity(chars.len());
    let mut prev_space = false;
    let mut prev: Option<usize> = None;
    for (i, c) in chars.iter().enumerate() {
        if c.ch.is_whitespace() {
            if !prev_space && !hay.is_empty() {
                hay.push(' ');
                src.push(i);
                prev_space = true;
            }
        } else {
            // A line break with no explicit space glyph (text wrap) still separates words, so the
            // query "foo bar" matches across the wrap — unless the break split a word, in which
            // case the halves are one word and the search reads them as `word_at` defines them
            // ("pontificate" finds "pontifi-" / "cate", "well-known" finds "well-" / "known").
            if let Some(p) = prev.filter(|&p| !same_line(&chars[p].rect, &c.rect)) {
                match wrap_of(chars, p).filter(|_| is_word_char(c.ch) || is_hyphen(c.ch)) {
                    Some(wrap) => {
                        while hay.last() == Some(&' ') {
                            hay.pop();
                            src.pop();
                        }
                        if wrap == Wrap::SoftHyphen {
                            hay.pop();
                            src.pop();
                        }
                    }
                    None if !prev_space => {
                        hay.push(' ');
                        src.push(i);
                    }
                    None => {}
                }
            }
            for lc in c.ch.to_lowercase() {
                hay.push(lc);
                src.push(i);
            }
            prev_space = false;
            prev = Some(i);
        }
    }

    let n = needle.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i + n <= hay.len() {
        if hay[i..i + n] == needle[..] {
            let s = src[i];
            let e = src[i + n - 1];
            out.push(SearchMatch {
                boxes: line_boxes(&chars[s..=e]),
                snippet: snippet_around(&hay, i, n),
            });
            i += n; // non-overlapping: resume past this match
        } else {
            i += 1;
        }
    }
    out
}

/// Lowercase + collapse internal whitespace + trim a query into its char sequence.
fn normalize_query(query: &str) -> Vec<char> {
    let mut out: Vec<char> = Vec::new();
    let mut prev_space = false;
    for c in query.chars() {
        if c.is_whitespace() {
            if !out.is_empty() && !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            for lc in c.to_lowercase() {
                out.push(lc);
            }
            prev_space = false;
        }
    }
    while out.last() == Some(&' ') {
        out.pop();
    }
    out
}

/// A `…`-trimmed context window of `hay` around the match at `[start, start+len)`.
fn snippet_around(hay: &[char], start: usize, len: usize) -> String {
    let from = start.saturating_sub(SNIPPET_CONTEXT);
    let to = (start + len + SNIPPET_CONTEXT).min(hay.len());
    let mut s = String::new();
    if from > 0 {
        s.push('…');
    }
    s.extend(&hay[from..to]);
    if to < hay.len() {
        s.push('…');
    }
    s
}
