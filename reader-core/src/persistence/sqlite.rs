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
}
