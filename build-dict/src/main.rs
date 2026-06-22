//! `build-dict` — import StarDict dictionaries into the `dict.db` corpus (ADR-INKREAD-0009 D2).
//!
//! Offline, host-only CLI over [`inkread_dict::import::import_stardict`] — the *same* import code
//! the device runs (KOReader-style on-device install), exercised here at build time to populate the
//! pre-built SQLite the app ships read-only:
//!
//! ```text
//! build-dict <stardict-dir> <out-dict.db> <lang> [--syn]
//! ```
//!
//! `<stardict-dir>` holds a StarDict set (`*.ifo`, `*.idx`, `*.dict` or `*.dict.dz`, optional
//! `*.syn`); `<lang>` is the code stored with every entry (`en`, `it`, `fr`, `da`, …). Re-running
//! with another language adds to the same `out-dict.db`.

use std::path::Path;
use std::process::ExitCode;

use inkread_dict::import::import_stardict;
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
    let db = Dict::open(out_db).map_err(|e| e.to_string())?;
    import_stardict(dir, &db, lang, syn).map_err(|e| e.to_string())
}
