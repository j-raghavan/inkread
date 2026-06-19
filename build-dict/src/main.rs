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
use inkread_dict::stardict::{definition_text, parse_idx, parse_ifo, parse_syn};
use inkread_dict::Dict;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!("usage: build-dict <stardict-dir> <out-dict.db> <lang>");
        return ExitCode::FAILURE;
    }
    match run(Path::new(&args[1]), Path::new(&args[2]), &args[3]) {
        Ok(n) => {
            println!("imported {n} entries ({}) → {}", args[3], args[2]);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("build-dict: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(dir: &Path, out_db: &Path, lang: &str) -> Result<usize, String> {
    let ifo_path = find(dir, "ifo")?;
    let idx_path = find(dir, "idx")?;
    let ifo = parse_ifo(&read_text(&ifo_path)?);
    let idx = parse_idx(
        &fs::read(&idx_path).map_err(io(&idx_path))?,
        ifo.offset_bits,
    );
    let dict_bytes = read_dict(dir)?;

    let db = Dict::open(out_db).map_err(|e| e.to_string())?;
    let mut imported = 0usize;
    for entry in &idx {
        let (start, end) = (
            entry.offset as usize,
            entry.offset as usize + entry.size as usize,
        );
        let Some(block) = dict_bytes.get(start..end) else {
            continue; // a bad offset/size just skips that entry
        };
        let defn = definition_text(block, ifo.same_type_sequence);
        if defn.is_empty() {
            continue;
        }
        if db.put_entry(lang, &entry.word, &defn).is_ok() {
            imported += 1;
        }
    }

    // Alternate spellings (.syn) become alias entries pointing at the same definition, so a lookup
    // of either spelling resolves. (Thesaurus synonyms come from a separate source — D5.)
    if let Ok(syn_path) = find(dir, "syn") {
        for s in parse_syn(&fs::read(&syn_path).map_err(io(&syn_path))?) {
            if let Some(target) = idx.get(s.index as usize) {
                let (start, end) = (
                    target.offset as usize,
                    target.offset as usize + target.size as usize,
                );
                if let Some(block) = dict_bytes.get(start..end) {
                    let defn = definition_text(block, ifo.same_type_sequence);
                    if !defn.is_empty() && db.put_entry(lang, &s.word, &defn).is_ok() {
                        imported += 1;
                    }
                }
            }
        }
    }
    Ok(imported)
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
