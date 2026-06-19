//! Pure StarDict-format parsing for the offline corpus importer (ADR-INKREAD-0009 D2).
//!
//! StarDict ships three core files: `.ifo` (text metadata), `.idx` (a sorted index of
//! `word\0 offset size`), and `.dict[.dz]` (concatenated definition blocks). An optional `.syn`
//! maps alternate spellings to index entries. This module parses **already-decompressed bytes** so
//! it stays dependency-free and fully host-tested; the `build-dict` tool does the file IO and the
//! `.dz` (gzip) decompression and calls [`crate::Dict::put_entry`].

/// The `.ifo` fields the importer needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ifo {
    /// Single type char applied to every entry (`m` = plain text, `h` = HTML, …), or `None` when
    /// each definition block is type-prefixed.
    pub same_type_sequence: Option<char>,
    /// Index offset width — `32` (default) or `64`.
    pub offset_bits: u32,
    /// Declared headword count (advisory).
    pub word_count: u64,
}

/// One `.idx` record: a headword and where its definition sits in `.dict`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdxEntry {
    /// The headword.
    pub word: String,
    /// Byte offset of the definition block in `.dict`.
    pub offset: u64,
    /// Byte length of the definition block.
    pub size: u32,
}

/// One `.syn` record: an alternate spelling and the `.idx` entry index it resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SynEntry {
    /// The alternate spelling.
    pub word: String,
    /// Zero-based index into the `.idx` entries.
    pub index: u32,
}

/// Parse `.ifo` text (lenient: unknown keys ignored, missing keys defaulted).
#[must_use]
pub fn parse_ifo(text: &str) -> Ifo {
    let mut same = None;
    let mut bits = 32u32;
    let mut word_count = 0u64;
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            let v = v.trim();
            match k.trim() {
                "sametypesequence" => same = v.chars().next(),
                "idxoffsetbits" => bits = if v == "64" { 64 } else { 32 },
                "wordcount" => word_count = v.parse().unwrap_or(0),
                _ => {}
            }
        }
    }
    Ifo {
        same_type_sequence: same,
        offset_bits: bits,
        word_count,
    }
}

/// Parse `.idx` bytes into entries. Records are `word\0` + big-endian offset (`offset_bits/8`
/// bytes) + big-endian `u32` size. Truncated trailing bytes are ignored (never panics).
#[must_use]
pub fn parse_idx(bytes: &[u8], offset_bits: u32) -> Vec<IdxEntry> {
    let off_len = if offset_bits == 64 { 8 } else { 4 };
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        while i < bytes.len() && bytes[i] != 0 {
            i += 1;
        }
        if i >= bytes.len() {
            break; // no terminator → truncated
        }
        let word = String::from_utf8_lossy(&bytes[start..i]).into_owned();
        i += 1; // skip the NUL
        if i + off_len + 4 > bytes.len() {
            break;
        }
        let offset = read_be(&bytes[i..i + off_len]);
        i += off_len;
        let size = read_be(&bytes[i..i + 4]) as u32;
        i += 4;
        if !word.is_empty() {
            out.push(IdxEntry { word, offset, size });
        }
    }
    out
}

/// Parse `.syn` bytes: `word\0` + big-endian `u32` index.
#[must_use]
pub fn parse_syn(bytes: &[u8]) -> Vec<SynEntry> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        while i < bytes.len() && bytes[i] != 0 {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let word = String::from_utf8_lossy(&bytes[start..i]).into_owned();
        i += 1;
        if i + 4 > bytes.len() {
            break;
        }
        let index = read_be(&bytes[i..i + 4]) as u32;
        i += 4;
        if !word.is_empty() {
            out.push(SynEntry { word, index });
        }
    }
    out
}

/// Extract the plain-text definition for an entry's `.dict` block. With `same_type` set, the whole
/// block is that type's content; without it, the block is type-prefixed (first byte = type char).
/// HTML (`h`) is crudely stripped to text; other text types pass through (lossy UTF-8).
#[must_use]
pub fn definition_text(block: &[u8], same_type: Option<char>) -> String {
    match same_type {
        Some('h') => strip_html(&String::from_utf8_lossy(block)),
        Some(_) => String::from_utf8_lossy(block).trim().to_string(),
        None => {
            if block.is_empty() {
                return String::new();
            }
            let t = block[0] as char;
            let content = &block[1..];
            if t == 'h' {
                strip_html(&String::from_utf8_lossy(content))
            } else {
                String::from_utf8_lossy(content).trim().to_string()
            }
        }
    }
}

/// Parse a thesaurus entry's body into synonyms. Moby entries are a header line
/// (`"N Moby Thesaurus words for "x":"`) followed by a comma-separated list; we take everything
/// after the first colon and split on commas. Over-long tokens (junk) are dropped.
#[must_use]
pub fn parse_synonym_list(defn: &str) -> Vec<String> {
    let body = defn.split_once(':').map_or(defn, |(_, tail)| tail);
    let mut out: Vec<String> = body
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.chars().count() <= 60)
        .map(str::to_string)
        .collect();
    out.dedup();
    out
}

/// Extract tight synonyms from WordNet-style `[syn: {a}, {b}]` markup inside a definition: each
/// `{token}` within a `[syn: …]` span is a synonym (true synset members, far sharper + smaller than
/// a broad thesaurus). De-duplicated; the caller drops the headword itself. A no-op for dictionaries
/// without that markup.
#[must_use]
pub fn extract_inline_synonyms(defn: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut search = defn;
    while let Some(start) = search.find("[syn:") {
        let after = &search[start + 5..];
        let end = after.find(']').unwrap_or(after.len());
        let mut rest = &after[..end];
        while let Some(b) = rest.find('{') {
            let tail = &rest[b + 1..];
            match tail.find('}') {
                Some(e) => {
                    let tok = tail[..e].trim();
                    if !tok.is_empty() {
                        out.push(tok.to_string());
                    }
                    rest = &tail[e + 1..];
                }
                None => break,
            }
        }
        search = &after[end..];
    }
    out.sort();
    out.dedup();
    out
}

fn read_be(b: &[u8]) -> u64 {
    b.iter().fold(0u64, |acc, &x| (acc << 8) | u64::from(x))
}

/// Strip HTML tags + unescape the common entities — enough to turn a dictionary HTML body into
/// readable definition text.
fn strip_html(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ifo_reads_the_fields() {
        let ifo = parse_ifo(
            "StarDict's dict ifo file\nversion=2.4.2\nwordcount=42\nsametypesequence=m\nidxoffsetbits=32\n",
        );
        assert_eq!(ifo.same_type_sequence, Some('m'));
        assert_eq!(ifo.offset_bits, 32);
        assert_eq!(ifo.word_count, 42);
        // 64-bit + missing sametypesequence
        let ifo2 = parse_ifo("idxoffsetbits=64\n");
        assert_eq!(ifo2.offset_bits, 64);
        assert_eq!(ifo2.same_type_sequence, None);
    }

    /// Build a 32-bit `.idx` record.
    fn idx_rec(word: &str, offset: u32, size: u32) -> Vec<u8> {
        let mut v = word.as_bytes().to_vec();
        v.push(0);
        v.extend_from_slice(&offset.to_be_bytes());
        v.extend_from_slice(&size.to_be_bytes());
        v
    }

    #[test]
    fn parse_idx_reads_records_in_order() {
        let mut bytes = idx_rec("run", 0, 7);
        bytes.extend(idx_rec("café", 7, 5)); // accented headword survives
        let idx = parse_idx(&bytes, 32);
        assert_eq!(idx.len(), 2);
        assert_eq!(
            idx[0],
            IdxEntry {
                word: "run".into(),
                offset: 0,
                size: 7
            }
        );
        assert_eq!(idx[1].word, "café");
        assert_eq!(idx[1].offset, 7);
    }

    #[test]
    fn parse_idx_tolerates_truncation() {
        let mut bytes = idx_rec("run", 0, 7);
        bytes.extend_from_slice(b"trunc\0\x00\x00"); // a word with a chopped offset/size
        let idx = parse_idx(&bytes, 32);
        assert_eq!(
            idx.len(),
            1,
            "the complete record parses; the truncated tail is dropped"
        );
    }

    #[test]
    fn definition_text_plain_and_html() {
        assert_eq!(
            definition_text(b"to move quickly", Some('m')),
            "to move quickly"
        );
        assert_eq!(
            definition_text(b"<b>a</b> small <i>fruit</i>", Some('h')),
            "a small fruit"
        );
        // type-prefixed (no sametypesequence): first byte is the type
        assert_eq!(definition_text(b"mhello", None), "hello");
    }

    #[test]
    fn parse_synonym_list_extracts_moby_synonyms() {
        let body =
            "12 Moby Thesaurus words for \"happy\":\n  cheerful, glad, joyful,\n  joyous, pleased";
        let syns = parse_synonym_list(body);
        assert_eq!(
            syns,
            vec!["cheerful", "glad", "joyful", "joyous", "pleased"]
        );
        // no header colon → split the whole thing
        assert_eq!(parse_synonym_list("a, b, c"), vec!["a", "b", "c"]);
    }

    #[test]
    fn extract_inline_synonyms_from_wordnet_markup() {
        let defn = "n 1: a score in baseball [syn: {run}, {tally}]\nn 2: foot race [syn: {run}, {running}]";
        assert_eq!(
            extract_inline_synonyms(defn),
            vec!["run", "running", "tally"]
        );
        // no markup → empty
        assert!(extract_inline_synonyms("plain definition").is_empty());
    }

    #[test]
    fn parse_syn_maps_spelling_to_index() {
        let mut bytes = b"colour".to_vec();
        bytes.push(0);
        bytes.extend_from_slice(&5u32.to_be_bytes());
        let syn = parse_syn(&bytes);
        assert_eq!(
            syn,
            vec![SynEntry {
                word: "colour".into(),
                index: 5
            }]
        );
    }
}
