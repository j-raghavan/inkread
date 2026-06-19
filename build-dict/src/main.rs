//! `build-dict` — import StarDict dictionaries into the `dict.db` corpus (ADR-INKREAD-0009 D2).
//!
//! Offline, host-only tool (never shipped to the device). Run once per language to populate the
//! pre-built SQLite the app opens read-only at runtime:
//!
//! ```text
//! build-dict <stardict-dir> <out-dict.db> <lang>
//! ```
//!
//! `<stardict-dir>` holds a StarDict set (`*.ifo`, `*.idx`, `*.dict` or `*.dict.dz`, optional
//! `*.syn`); `<lang>` is the code stored with every entry (`en`, `it`, `fr`, `da`, …). Re-running
//! with another language adds to the same `out-dict.db`.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use flate2::read::GzDecoder;
use inkread_dict::stardict::{
    definition_text, extract_inline_synonyms, parse_idx, parse_ifo, parse_syn, parse_synonym_list,
    IdxEntry,
};
use inkread_dict::Dict;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let syn = args.iter().any(|a| a == "--syn");
    let pos: Vec<&String> = args
        .iter()
        .skip(1)
        .filter(|a| !a.starts_with("--"))
        .collect();
    if pos.len() != 3 {
        eprintln!("usage: build-dict <stardict-dir> <out-dict.db> <lang> [--syn]");
        return ExitCode::FAILURE;
    }
    match run(Path::new(pos[0]), Path::new(pos[1]), pos[2], syn) {
        Ok(n) => {
            let kind = if syn { "synonyms" } else { "entries" };
            println!("imported {n} {kind} ({}) → {}", pos[2], pos[1]);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("build-dict: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(dir: &Path, out_db: &Path, lang: &str, syn: bool) -> Result<usize, String> {
    let idx_path = find(dir, "idx")?;
    let ifo = parse_ifo(&read_text(&find(dir, "ifo")?)?);
    let idx = parse_idx(
        &fs::read(&idx_path).map_err(io(&idx_path))?,
        ifo.offset_bits,
    );
    let dict_bytes = read_dict(dir)?;
    let db = Dict::open(out_db).map_err(|e| e.to_string())?;

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
        return db.import_synonyms(lang, &items).map_err(|e| e.to_string());
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
        for s in parse_syn(&fs::read(&syn_path).map_err(io(&syn_path))?) {
            if let Some(target) = idx.get(s.index as usize) {
                if let Some(defn) = block_of(target) {
                    if !defn.is_empty() {
                        items.push((s.word, defn));
                    }
                }
            }
        }
    }
    let n = db.import_entries(lang, &items).map_err(|e| e.to_string())?;
    db.import_synonyms(lang, &syns).map_err(|e| e.to_string())?;
    Ok(n)
}

/// The `.dict` data, decompressing `.dict.dz` (gzip-compatible) when that's the only form present.
fn read_dict(dir: &Path) -> Result<Vec<u8>, String> {
    if let Ok(plain) = find(dir, "dict") {
        return fs::read(&plain).map_err(io(&plain));
    }
    let dz = find(dir, "dict.dz")?;
    let raw = fs::read(&dz).map_err(io(&dz))?;
    let mut out = Vec::new();
    GzDecoder::new(raw.as_slice())
        .read_to_end(&mut out)
        .map_err(|e| format!("decompress {}: {e}", dz.display()))?;
    Ok(out)
}

/// Find the single file in `dir` ending with `.<ext>` (case-sensitive on the extension).
fn find(dir: &Path, ext: &str) -> Result<PathBuf, String> {
    let suffix = format!(".{ext}");
    fs::read_dir(dir)
        .map_err(io(dir))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(&suffix))
        })
        .ok_or_else(|| format!("no *{suffix} file in {}", dir.display()))
}

fn read_text(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(io(path))
}

fn io(path: &Path) -> impl Fn(std::io::Error) -> String + '_ {
    move |e| format!("{}: {e}", path.display())
}
