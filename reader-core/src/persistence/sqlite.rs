//! `SqliteStore` — the rusqlite adapter for [`ReaderStore`] (ADR Decision 4).
//!
//! WAL journaling; **parameterized SQL only** (never string-formatted — RR21 boundary rule).
//! The schema evolves through an ordered, idempotent migration list keyed by
//! `PRAGMA user_version` (RR23-FR3). The `rusqlite::Connection` is wrapped in a `Mutex` so the
//! store is `Sync` (a bare `Connection` is `Send` but not `Sync`); the engine drives it from one
//! worker thread (RR21), the lock just keeps `Arc`-sharing sound.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection};

use super::{BookId, ReaderStore, ReadingPosition};
use crate::error::{CoreError, CoreResult};
use crate::settings::{Scope, SettingKey, SettingValue, SettingsSnapshot};

/// Ordered schema migrations (ADR Decision 4 / RR23-FR3). Index `i` is schema version `i + 1`;
/// each runs once, in order, and bumps `user_version`. Append-only — never edit a shipped one.
const MIGRATIONS: &[&str] = &[
    // v1 — reading position (RR12-FR3).
    "CREATE TABLE reading_position (
        book_id     TEXT PRIMARY KEY,
        page_index  INTEGER NOT NULL,
        total       INTEGER NOT NULL,
        resume_blob BLOB,
        updated_at  INTEGER NOT NULL
     );",
    // v2 — settings (RR23): one typed value per (scope, key), plus a version counter.
    "CREATE TABLE settings (
        scope TEXT NOT NULL,
        key   TEXT NOT NULL,
        value TEXT NOT NULL,
        kind  INTEGER NOT NULL,
        PRIMARY KEY (scope, key)
     );
     CREATE TABLE settings_meta (id INTEGER PRIMARY KEY CHECK (id = 0), version INTEGER NOT NULL);
     INSERT INTO settings_meta (id, version) VALUES (0, 0);",
];

/// The SQLite-backed reading store.
pub struct SqliteStore {
    conn: Mutex<Connection>,
}

impl SqliteStore {
    /// Open (or create) a store at `path`, applying WAL + migrations.
    pub fn open(path: &Path) -> CoreResult<Self> {
        Self::init(Connection::open(path).map_err(db_err)?)
    }

    /// An in-memory store (host tests) — same schema/migrations, no file.
    pub fn open_in_memory() -> CoreResult<Self> {
        Self::init(Connection::open_in_memory().map_err(db_err)?)
    }

    fn init(conn: Connection) -> CoreResult<Self> {
        // WAL for crash-safety (RR12 DoD); `pragma_update` with journal_mode returns a row,
        // so use a query that tolerates that.
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(db_err)?;
        migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl ReaderStore for SqliteStore {
    fn load_position(&self, book: &BookId) -> CoreResult<Option<ReadingPosition>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT page_index, total, resume_blob FROM reading_position WHERE book_id = ?1",
            )
            .map_err(db_err)?;
        let row = stmt.query_row(params![book.as_str()], |r| {
            Ok(ReadingPosition {
                page_index: r.get::<_, i64>(0)? as usize,
                total: r.get::<_, i64>(1)? as usize,
                resume_blob: r.get::<_, Option<Vec<u8>>>(2)?,
            })
        });
        match row {
            Ok(p) => Ok(Some(p)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(db_err(e)),
        }
    }

    fn save_position(&self, book: &BookId, pos: &ReadingPosition) -> CoreResult<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO reading_position (book_id, page_index, total, resume_blob, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(book_id) DO UPDATE SET
                 page_index = ?2, total = ?3, resume_blob = ?4, updated_at = ?5",
            params![
                book.as_str(),
                pos.page_index as i64,
                pos.total as i64,
                pos.resume_blob.as_deref(),
                now_secs(),
            ],
        )
        .map_err(db_err)?;
        Ok(())
    }

    fn schema_version(&self) -> CoreResult<u32> {
        let conn = self.lock();
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .map(|v| v as u32)
            .map_err(db_err)
    }

    fn load_settings(&self) -> CoreResult<SettingsSnapshot> {
        let conn = self.lock();
        let version: i64 = conn
            .query_row("SELECT version FROM settings_meta WHERE id = 0", [], |r| {
                r.get(0)
            })
            .map_err(db_err)?;
        let mut stmt = conn
            .prepare("SELECT scope, key, value, kind FROM settings")
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })
            .map_err(db_err)?;
        let mut values = Vec::new();
        for row in rows {
            let (scope_s, key_s, value_s, kind) = row.map_err(db_err)?;
            // An unknown scope/key/value is skipped (defaulted on read) — RR23-FR3, never crash.
            let (Some(scope), Some(key), Some(val)) = (
                parse_scope(&scope_s),
                SettingKey::from_name(&key_s),
                SettingValue::from_storage(kind, &value_s),
            ) else {
                continue;
            };
            values.push((scope, key, val));
        }
        Ok(SettingsSnapshot::from_values(version as u32, values))
    }

    fn put_setting(&self, scope: Scope, key: SettingKey, value: SettingValue) -> CoreResult<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO settings (scope, key, value, kind) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(scope, key) DO UPDATE SET value = ?3, kind = ?4",
            params![
                scope_to_str(&scope),
                key.as_str(),
                value.to_storage(),
                value.kind_code(),
            ],
        )
        .map_err(db_err)?;
        conn.execute(
            "UPDATE settings_meta SET version = version + 1 WHERE id = 0",
            [],
        )
        .map_err(db_err)?;
        Ok(())
    }
}

/// `Scope` → storage string: `"global"` or `"book:<id>"`.
fn scope_to_str(scope: &Scope) -> String {
    match scope {
        Scope::Global => "global".to_string(),
        Scope::Book(b) => format!("book:{}", b.as_str()),
    }
}

/// Parse a stored scope string; `None` for an unrecognized/invalid scope (skipped on read).
fn parse_scope(s: &str) -> Option<Scope> {
    if s == "global" {
        return Some(Scope::Global);
    }
    s.strip_prefix("book:")
        .and_then(|id| BookId::new(id).ok())
        .map(Scope::Book)
}

/// Run any migrations past the connection's `user_version`, bumping it after each.
fn migrate(conn: &Connection) -> CoreResult<()> {
    let current: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(db_err)?;
    for (i, sql) in MIGRATIONS.iter().enumerate() {
        let version = (i + 1) as i64;
        if current < version {
            conn.execute_batch(sql).map_err(db_err)?;
            // user_version is an identifier slot, not a bind parameter — format the integer in.
            conn.pragma_update(None, "user_version", version)
                .map_err(db_err)?;
        }
    }
    Ok(())
}

fn db_err(e: rusqlite::Error) -> CoreError {
    CoreError::Persistence(e.to_string())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_sets_version_and_is_idempotent() {
        let store = SqliteStore::open_in_memory().unwrap();
        assert_eq!(store.schema_version().unwrap(), MIGRATIONS.len() as u32);
        // Re-running migrations is a no-op (version unchanged, no error).
        {
            let conn = store.lock();
            migrate(&conn).unwrap();
        }
        assert_eq!(store.schema_version().unwrap(), MIGRATIONS.len() as u32);
    }

    #[test]
    fn position_round_trips_and_upserts() {
        let store = SqliteStore::open_in_memory().unwrap();
        let book = BookId::new("book-1").unwrap();
        assert_eq!(store.load_position(&book).unwrap(), None);

        store
            .save_position(&book, &ReadingPosition::new(4, 20))
            .unwrap();
        let loaded = store.load_position(&book).unwrap().unwrap();
        assert_eq!(loaded.page_index, 4);
        assert_eq!(loaded.total, 20);
        assert_eq!(loaded.resume_blob, None);

        // Re-saving the same book upserts in place (no duplicate row).
        store
            .save_position(&book, &ReadingPosition::new(7, 20))
            .unwrap();
        assert_eq!(store.load_position(&book).unwrap().unwrap().page_index, 7);
    }

    #[test]
    fn distinct_books_keep_distinct_positions() {
        let store = SqliteStore::open_in_memory().unwrap();
        let a = BookId::new("a").unwrap();
        let b = BookId::new("b").unwrap();
        store
            .save_position(&a, &ReadingPosition::new(1, 10))
            .unwrap();
        store
            .save_position(&b, &ReadingPosition::new(5, 10))
            .unwrap();
        assert_eq!(store.load_position(&a).unwrap().unwrap().page_index, 1);
        assert_eq!(store.load_position(&b).unwrap().unwrap().page_index, 5);
    }

    // A resume_blob round-trips intact (the reserved M2 slot is wired end-to-end now).
    #[test]
    fn resume_blob_round_trips() {
        let store = SqliteStore::open_in_memory().unwrap();
        let book = BookId::new("b").unwrap();
        let mut pos = ReadingPosition::new(0, 1);
        pos.resume_blob = Some(vec![1, 2, 3, 4]);
        store.save_position(&book, &pos).unwrap();
        assert_eq!(
            store.load_position(&book).unwrap().unwrap().resume_blob,
            Some(vec![1, 2, 3, 4])
        );
    }

    // RR23-AC1/AC2: settings persist with per-book override and the version bumps on each write.
    #[test]
    fn settings_round_trip_and_version_bumps() {
        let store = SqliteStore::open_in_memory().unwrap();
        let snap0 = store.load_settings().unwrap();
        assert_eq!(snap0.version(), 0);
        assert_eq!(snap0.get_int(SettingKey::FlashInterval, None), 6); // built-in default

        store
            .put_setting(
                Scope::Global,
                SettingKey::FlashInterval,
                SettingValue::Int(9),
            )
            .unwrap();
        let book = BookId::new("b").unwrap();
        store
            .put_setting(
                Scope::Book(book.clone()),
                SettingKey::FlashInterval,
                SettingValue::Int(3),
            )
            .unwrap();

        let snap = store.load_settings().unwrap();
        assert_eq!(snap.version(), 2);
        assert_eq!(snap.get_int(SettingKey::FlashInterval, None), 9);
        assert_eq!(snap.get_int(SettingKey::FlashInterval, Some(&book)), 3);

        // Upsert replaces and bumps again.
        store
            .put_setting(
                Scope::Global,
                SettingKey::FlashInterval,
                SettingValue::Int(5),
            )
            .unwrap();
        let snap2 = store.load_settings().unwrap();
        assert_eq!(snap2.get_int(SettingKey::FlashInterval, None), 5);
        assert_eq!(snap2.version(), 3);
    }

    // RR23-FR3: an unknown key persisted by a newer build is skipped (defaulted), not a crash.
    #[test]
    fn unknown_setting_key_is_skipped_on_load() {
        let store = SqliteStore::open_in_memory().unwrap();
        {
            let conn = store.lock();
            conn.execute(
                "INSERT INTO settings (scope, key, value, kind) VALUES ('global', 'future_key', '1', 1)",
                [],
            )
            .unwrap();
        }
        let snap = store.load_settings().unwrap();
        assert_eq!(snap.get_int(SettingKey::FlashInterval, None), 6);
    }
}
