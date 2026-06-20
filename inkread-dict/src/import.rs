//! On-device StarDict import (ADR-INKREAD-0009 D2).
//!
//! The file IO + `.dict.dz` (dictzip / gzip-compatible) decompression that used to live only in the
//! host `build-dict` tool, lifted into the library so the **shell can install user dictionaries on
//! the device** (KOReader-style) over JNI — not just at build time. The parsing primitives in
//! [`crate::stardict`] stay pure; this module is the thin IO shell around them.
//!
//! Gated behind the `import` feature so the device cdylib only pulls `flate2` in when this is built
//! (the runtime lookup path needs neither). `flate2`'s default (pure-Rust `miniz_oxide`) backend
//! cross-compiles to `aarch64-android` with no system zlib.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;

use crate::stardict::{
    definition_text, extract_inline_synonyms, parse_idx, parse_ifo, parse_syn, parse_synonym_list,
    IdxEntry,
};
use crate::{Dict, DictError, DictResult};

/// Import the StarDict bundle in `dir` into `db` under the language/source tag `lang`.
///
/// A bundle is the StarDict trio (`*.ifo`, `*.idx`, `*.dict` or `*.dict.dz`) with an optional
/// `*.syn`. When `syn` is true the bundle is treated as a Moby-style **thesaurus** (its bodies are
/// synonym lists) and written to the synonyms table; otherwise **definitions** are written — plus
/// any inline WordNet `[syn: {…}]` synonyms and `.syn` alternate-spelling aliases. Returns the
/// number of records imported. Never panics: bad offsets / truncation are skipped, IO errors map to
/// [`DictError::Backend`].
pub fn import_stardict(dir: &Path, db: &Dict, lang: &str, syn: bool) -> DictResult<usize> {
    let idx_path = find(dir, "idx")?;
    let ifo = parse_ifo(&read_text(&find(dir, "ifo")?)?);
    let idx = parse_idx(&read_bytes(&idx_path)?, ifo.offset_bits);
    let dict_bytes = read_dict(dir)?;

    // The plain-text definition block for an idx entry, or None on a bad offset/size.
    let block_of = |e: &IdxEntry| -> Option<String> {
        let (start, end) = (e.offset as usize, e.offset as usize + e.size as usize);
        dict_bytes
            .get(start..end)
            .map(|b| definition_text(b, ifo.same_type_sequence))
    };

    if syn {
        // Thesaurus: each entry's body is a synonym list (Moby) → synonyms table.
        let mut items: Vec<(String, Vec<String>)> = Vec::new();
        for e in &idx {
            if let Some(defn) = block_of(e) {
                let syns = parse_synonym_list(&defn);
                if !syns.is_empty() {
                    items.push((e.word.clone(), syns));
                }
            }
        }
        return db.import_synonyms(lang, &items);
    }

    // Definitions, plus tight synonyms harvested from any inline `[syn: {…}]` markup (WordNet).
    let mut items: Vec<(String, String)> = Vec::new();
    let mut syns: Vec<(String, Vec<String>)> = Vec::new();
    for e in &idx {
        if let Some(defn) = block_of(e) {
            if !defn.is_empty() {
                let mut s = extract_inline_synonyms(&defn);
                s.retain(|x| !x.eq_ignore_ascii_case(&e.word)); // drop the headword itself
                if !s.is_empty() {
                    syns.push((e.word.clone(), s));
                }
                items.push((e.word.clone(), defn));
            }
        }
    }
    // Alternate spellings (.syn) → alias entries pointing at the same definition.
    if let Ok(syn_path) = find(dir, "syn") {
        for s in parse_syn(&read_bytes(&syn_path)?) {
            if let Some(target) = idx.get(s.index as usize) {
                if let Some(defn) = block_of(target) {
                    if !defn.is_empty() {
                        items.push((s.word, defn));
                    }
                }
            }
        }
    }
    let n = db.import_entries(lang, &items)?;
    db.import_synonyms(lang, &syns)?;
    Ok(n)
}

/// True when `dir` holds a StarDict bundle (an `*.ifo` paired with an `*.idx`) — a cheap pre-check
/// the shell can use to list installable folders without attempting a full import.
#[must_use]
pub fn is_stardict_dir(dir: &Path) -> bool {
    find(dir, "ifo").is_ok() && find(dir, "idx").is_ok()
}

/// The `.dict` data, decompressing `.dict.dz` (dictzip, gzip-compatible) when that's the only form.
fn read_dict(dir: &Path) -> DictResult<Vec<u8>> {
    if let Ok(plain) = find(dir, "dict") {
        return read_bytes(&plain);
    }
    let dz = find(dir, "dict.dz")?;
    let raw = read_bytes(&dz)?;
    let mut out = Vec::new();
    GzDecoder::new(raw.as_slice())
        .read_to_end(&mut out)
        .map_err(|e| DictError::Backend(format!("decompress {}: {e}", dz.display())))?;
    Ok(out)
}

/// Find the single file in `dir` ending with `.<ext>` (the extension is matched case-sensitively).
fn find(dir: &Path, ext: &str) -> DictResult<PathBuf> {
    let suffix = format!(".{ext}");
    fs::read_dir(dir)
        .map_err(be(dir))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(&suffix))
        })
        .ok_or_else(|| DictError::Backend(format!("no *{suffix} file in {}", dir.display())))
}

fn read_bytes(path: &Path) -> DictResult<Vec<u8>> {
    fs::read(path).map_err(be(path))
}

fn read_text(path: &Path) -> DictResult<String> {
    fs::read_to_string(path).map_err(be(path))
}

/// Map an IO error against `path` into a [`DictError::Backend`] carrying the path for diagnostics.
fn be(path: &Path) -> impl Fn(std::io::Error) -> DictError + '_ {
    move |e| DictError::Backend(format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;

    /// A unique scratch dir under the system temp root (no `tempfile` dep; cleaned at test end).
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "inkread-import-{tag}-{}",
            std::process::id() as u64 + tag.len() as u64
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Build a 32-bit `.idx` record: `word\0` + BE u32 offset + BE u32 size.
    fn idx_rec(word: &str, offset: u32, size: u32) -> Vec<u8> {
        let mut v = word.as_bytes().to_vec();
        v.push(0);
        v.extend_from_slice(&offset.to_be_bytes());
        v.extend_from_slice(&size.to_be_bytes());
        v
    }

    fn write_bundle(dir: &Path, dict_name: &str, dict_bytes: &[u8]) {
        let body0 = "to move quickly [syn: {sprint}]";
        let body1 = "a small fruit";
        fs::write(
            dir.join("test.ifo"),
            "StarDict's dict ifo file\nsametypesequence=m\nwordcount=2\nidxoffsetbits=32\n",
        )
        .unwrap();
        let mut idx = idx_rec("run", 0, body0.len() as u32);
        idx.extend(idx_rec("apple", body0.len() as u32, body1.len() as u32));
        fs::write(dir.join("test.idx"), idx).unwrap();
        fs::write(dir.join(dict_name), dict_bytes).unwrap();
    }

    #[test]
    fn import_plain_dict_round_trips_definitions_and_inline_synonyms() {
        let dir = scratch("plain");
        let body = b"to move quickly [syn: {sprint}]a small fruit";
        write_bundle(&dir, "test.dict", body);

        let db = Dict::open(dir.join("out.db")).unwrap();
        let n = import_stardict(&dir, &db, "en", false).unwrap();
        assert_eq!(n, 2, "two headwords imported");

        let run = db.lookup("run", &["en"], None).unwrap();
        assert!(run.senses.iter().any(|s| s.contains("to move quickly")));
        assert!(
            run.synonyms.iter().any(|s| s == "sprint"),
            "inline [syn: {{…}}] harvested: {:?}",
            run.synonyms
        );
        assert!(db.lookup("apple", &["en"], None).is_some());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_decompresses_dict_dz() {
        let dir = scratch("dz");
        let body = b"to move quickly [syn: {sprint}]a small fruit";
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(body).unwrap();
        let gz = enc.finish().unwrap();
        write_bundle(&dir, "test.dict.dz", &gz);

        let db = Dict::open(dir.join("out.db")).unwrap();
        let n = import_stardict(&dir, &db, "en", false).unwrap();
        assert_eq!(n, 2, "the .dict.dz bundle decompresses and imports");
        assert!(db.lookup("run", &["en"], None).is_some());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_missing_files_errors_without_panic() {
        let dir = scratch("empty");
        let db = Dict::open(dir.join("out.db")).unwrap();
        assert!(matches!(
            import_stardict(&dir, &db, "en", false),
            Err(DictError::Backend(_))
        ));
        assert!(!is_stardict_dir(&dir));
        let _ = fs::remove_dir_all(&dir);
    }
}
