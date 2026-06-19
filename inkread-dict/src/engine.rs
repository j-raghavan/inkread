//! The [`Dict`] lookup engine over the SQLite corpus (ADR-INKREAD-0009 D2).

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use rusqlite::{params, Connection, OptionalExtension};

use crate::{Definition, DictError, DictResult, OnlineSource};

/// The corpus schema (idempotent — a pre-built `dict.db` already has it). `key` is the normalized
/// lowercase headword used for lookup; `headword` is the display form.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS entries (
  lang     TEXT NOT NULL,
  key      TEXT NOT NULL,
  headword TEXT NOT NULL,
  defn     TEXT NOT NULL,
  PRIMARY KEY (lang, key)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS idx_entries_key ON entries(key);
CREATE TABLE IF NOT EXISTS synonyms (
  lang TEXT NOT NULL,
  key  TEXT NOT NULL,
  syn  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_syn_key ON synonyms(lang, key);
";

/// The dictionary corpus + lookup engine. `Send + Sync` via an interior `Mutex` so the engine can
/// be shared (`Arc`) across threads, mirroring `reader-core`'s `SqliteStore`.
pub struct Dict {
    conn: Mutex<Connection>,
}

impl Dict {
    /// Open (or create) a corpus at `path`, ensuring the schema.
    pub fn open(path: impl AsRef<Path>) -> DictResult<Self> {
        let conn = Connection::open(path).map_err(be)?;
        conn.execute_batch(SCHEMA).map_err(be)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// An in-memory corpus (tests, and the transient online cache).
    pub fn open_in_memory() -> DictResult<Self> {
        let conn = Connection::open_in_memory().map_err(be)?;
        conn.execute_batch(SCHEMA).map_err(be)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Insert/replace a dictionary entry (the StarDict importer + the online cache use this).
    pub fn put_entry(&self, lang: &str, headword: &str, defn: &str) -> DictResult<()> {
        let key = normalize(headword);
        self.lock()
            .execute(
                "INSERT OR REPLACE INTO entries (lang, key, headword, defn) VALUES (?1, ?2, ?3, ?4)",
                params![lang, key, headword, defn],
            )
            .map_err(be)?;
        Ok(())
    }

    /// Add a thesaurus synonym for a headword.
    pub fn put_synonym(&self, lang: &str, headword: &str, syn: &str) -> DictResult<()> {
        let key = normalize(headword);
        self.lock()
            .execute(
                "INSERT INTO synonyms (lang, key, syn) VALUES (?1, ?2, ?3)",
                params![lang, key, syn],
            )
            .map_err(be)?;
        Ok(())
    }

    /// Resolve `query` to a [`Definition`] (RR12): try `lang_hints` first then any language, then a
    /// stem fallback for inflected forms (`running`→`run`). On a full on-device miss, fall through
    /// to `online` (if supplied) and cache the result. `None` if nothing matches.
    pub fn lookup(
        &self,
        query: &str,
        lang_hints: &[&str],
        online: Option<&dyn OnlineSource>,
    ) -> Option<Definition> {
        let key = normalize(query);
        if key.is_empty() {
            return None;
        }
        let mut candidates = vec![key.clone()];
        candidates.extend(stem_candidates(&key));
        candidates.dedup();

        for cand in &candidates {
            for lang in lang_hints {
                if let Some(d) = self.exact(lang, cand) {
                    return Some(d);
                }
            }
            if let Some(d) = self.exact_any(cand) {
                return Some(d);
            }
        }

        if let Some(src) = online {
            if let Some(e) = src.lookup(query) {
                let _ = self.put_entry(&e.lang, &e.headword, &e.senses.join("\n"));
                return Some(Definition {
                    headword: e.headword,
                    lang: e.lang,
                    senses: e.senses,
                    synonyms: Vec::new(),
                });
            }
        }
        None
    }

    fn exact(&self, lang: &str, key: &str) -> Option<Definition> {
        let (headword, defn) = self
            .lock()
            .query_row(
                "SELECT headword, defn FROM entries WHERE lang = ?1 AND key = ?2",
                params![lang, key],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()
            .ok()??;
        Some(self.build(lang, key, headword, defn))
    }

    fn exact_any(&self, key: &str) -> Option<Definition> {
        let (lang, headword, defn) = self
            .lock()
            .query_row(
                "SELECT lang, headword, defn FROM entries WHERE key = ?1 LIMIT 1",
                params![key],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .ok()??;
        Some(self.build(&lang, key, headword, defn))
    }

    fn build(&self, lang: &str, key: &str, headword: String, defn: String) -> Definition {
        let senses = defn
            .split('\n')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        Definition {
            headword,
            lang: lang.to_string(),
            senses,
            synonyms: self.synonyms(lang, key),
        }
    }

    fn synonyms(&self, lang: &str, key: &str) -> Vec<String> {
        let conn = self.lock();
        let mut stmt = match conn.prepare("SELECT syn FROM synonyms WHERE lang = ?1 AND key = ?2") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        // Bind `out` first and confine the borrowing iterator to this block so it ends before the
        // guard (`conn`) drops at return.
        let mut out = Vec::new();
        if let Ok(rows) = stmt.query_map(params![lang, key], |r| r.get::<_, String>(0)) {
            for s in rows.flatten() {
                out.push(s);
            }
        }
        out
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn be(e: rusqlite::Error) -> DictError {
    DictError::Backend(e.to_string())
}

/// Normalize a word for the lookup key: trimmed + lowercased.
fn normalize(word: &str) -> String {
    word.trim().to_lowercase()
}

/// Candidate base forms for an inflected word (heuristic; a real lemmatizer/forms-table is D5).
/// Ordered most-to-least likely; the caller tries each. Suffixes are ASCII, so the byte slices
/// always land on a char boundary even for accented (it/fr/da) words.
fn stem_candidates(key: &str) -> Vec<String> {
    let mut out = Vec::new();
    let len = key.chars().count();
    let mut push = |s: String| {
        if s.chars().count() >= 2 {
            out.push(s);
        }
    };

    if key.ends_with("ies") && len > 4 {
        push(format!("{}y", &key[..key.len() - 3])); // berries -> berry
    }
    if key.ends_with("ing") && len > 5 {
        let base = &key[..key.len() - 3];
        push(base.to_string()); // singing -> sing
        push(undouble(base)); // running -> runn -> run
        push(format!("{base}e")); // making -> mak -> make
    }
    if key.ends_with("ed") && len > 4 {
        let base = &key[..key.len() - 2];
        push(base.to_string()); // walked -> walk
        push(undouble(base)); // stopped -> stopp -> stop
        push(format!("{base}e")); // baked -> bak -> bake
    }
    if key.ends_with("es") && len > 4 {
        push(key[..key.len() - 2].to_string()); // boxes -> box
    }
    for suf in ["s", "ly", "er", "est"] {
        if key.ends_with(suf) && len > suf.len() + 2 {
            push(key[..key.len() - suf.len()].to_string());
        }
    }
    out.dedup();
    out
}

/// Drop a trailing doubled consonant (`runn` -> `run`).
fn undouble(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() >= 2 && chars[chars.len() - 1] == chars[chars.len() - 2] {
        chars[..chars.len() - 1].iter().collect()
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OnlineEntry;

    fn fixture() -> Dict {
        let d = Dict::open_in_memory().unwrap();
        d.put_entry("en", "run", "to move quickly on foot\nto operate or manage")
            .unwrap();
        d.put_synonym("en", "run", "sprint").unwrap();
        d.put_synonym("en", "run", "dash").unwrap();
        d.put_entry("en", "make", "to create or produce").unwrap();
        d.put_entry("en", "stop", "to cease moving").unwrap();
        d.put_entry("en", "berry", "a small juicy fruit").unwrap();
        d.put_entry("it", "casa", "house; home").unwrap();
        d
    }

    #[test]
    fn exact_lookup_returns_senses_and_synonyms() {
        let d = fixture();
        let def = d.lookup("run", &["en"], None).unwrap();
        assert_eq!(def.headword, "run");
        assert_eq!(def.lang, "en");
        assert_eq!(def.senses.len(), 2);
        assert_eq!(def.synonyms, vec!["sprint", "dash"]);
    }

    #[test]
    fn lookup_is_trimmed_and_case_insensitive() {
        let d = fixture();
        assert_eq!(d.lookup("  RUN  ", &["en"], None).unwrap().headword, "run");
    }

    #[test]
    fn stemming_finds_the_base_word() {
        let d = fixture();
        for inflected in ["running", "runs"] {
            assert_eq!(
                d.lookup(inflected, &["en"], None).map(|x| x.headword),
                Some("run".to_string()),
                "{inflected}",
            );
        }
        assert_eq!(d.lookup("making", &["en"], None).unwrap().headword, "make");
        assert_eq!(d.lookup("stopped", &["en"], None).unwrap().headword, "stop");
        assert_eq!(
            d.lookup("berries", &["en"], None).unwrap().headword,
            "berry"
        );
    }

    #[test]
    fn language_routing_prefers_hints_then_any() {
        let d = fixture();
        // 'casa' isn't English; with an en-only hint it still resolves via exact_any (it).
        assert_eq!(d.lookup("casa", &["en"], None).unwrap().lang, "it");
        assert_eq!(d.lookup("casa", &["it", "en"], None).unwrap().lang, "it");
    }

    #[test]
    fn miss_returns_none() {
        let d = fixture();
        assert!(d.lookup("zxqwv", &["en"], None).is_none());
        assert!(d.lookup("   ", &["en"], None).is_none());
    }

    struct FakeOnline;
    impl OnlineSource for FakeOnline {
        fn lookup(&self, word: &str) -> Option<OnlineEntry> {
            if word == "neologism" {
                Some(OnlineEntry {
                    lang: "en".into(),
                    headword: "neologism".into(),
                    senses: vec!["a newly coined word".into()],
                })
            } else {
                None
            }
        }
    }

    #[test]
    fn online_fallback_returns_and_caches() {
        let d = fixture();
        // miss on device → online resolves it
        let def = d.lookup("neologism", &["en"], Some(&FakeOnline)).unwrap();
        assert_eq!(def.senses, vec!["a newly coined word"]);
        // cached: a second lookup with NO online source still hits
        assert_eq!(
            d.lookup("neologism", &["en"], None).unwrap().headword,
            "neologism"
        );
    }
}
