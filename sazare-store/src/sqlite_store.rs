//! SQLite-based resource storage
//!
//! Schema:
//!   - resources: Current version only (resource_type, id)
//!   - resource_history: Version history (resource_type, id, version_id)

use crate::error::Result;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Transaction};
use std::ops::Deref;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

/// Number of read-only connections to open for a file-backed store. With WAL,
/// these read concurrently with each other and with the single writer.
const READ_POOL_SIZE: usize = 4;

/// SQLite-based resource store.
///
/// All writes go through a single connection (`conn`); SQLite allows only one
/// writer anyway. Reads use a small pool of read-only connections so that
/// concurrent GET/search requests aren't serialized behind each other or behind
/// a write (WAL gives readers a consistent snapshot of the last commit).
pub struct SqliteStore {
    conn: Mutex<Connection>,
    read_pool: Vec<Mutex<Connection>>,
    next_reader: AtomicUsize,
}

#[allow(clippy::result_large_err)]
impl SqliteStore {
    /// Ordered schema migrations (see `crate::migrate`). Append-only.
    const MIGRATIONS: &'static [&'static str] = &[
        // v1 — initial schema.
        r#"
        CREATE TABLE IF NOT EXISTS resources (
            resource_type TEXT NOT NULL,
            id TEXT NOT NULL,
            value TEXT NOT NULL,
            PRIMARY KEY (resource_type, id)
        );
        CREATE TABLE IF NOT EXISTS resource_history (
            resource_type TEXT NOT NULL,
            id TEXT NOT NULL,
            version_id TEXT NOT NULL,
            value TEXT NOT NULL,
            PRIMARY KEY (resource_type, id, version_id)
        );
        CREATE INDEX IF NOT EXISTS idx_resources_type ON resources(resource_type);
        CREATE INDEX IF NOT EXISTS idx_history_type ON resource_history(resource_type);
        "#,
        // v2 — every change to the current resources, numbered, for the
        // search index to catch up with after a crash (see `changed_since`).
        // Triggers, so no write path can miss it.
        r#"
        CREATE TABLE changes (
            seq INTEGER PRIMARY KEY AUTOINCREMENT,
            resource_type TEXT NOT NULL,
            id TEXT NOT NULL
        );
        CREATE TRIGGER changes_insert AFTER INSERT ON resources BEGIN
            INSERT INTO changes (resource_type, id) VALUES (NEW.resource_type, NEW.id);
        END;
        CREATE TRIGGER changes_update AFTER UPDATE ON resources BEGIN
            INSERT INTO changes (resource_type, id) VALUES (NEW.resource_type, NEW.id);
        END;
        CREATE TRIGGER changes_delete AFTER DELETE ON resources BEGIN
            INSERT INTO changes (resource_type, id) VALUES (OLD.resource_type, OLD.id);
        END;
        "#,
        // v3 — a delete is a version of its own in the history (`deleted`
        // = 1; its value is the content it deleted, with the new versionId).
        r#"
        ALTER TABLE resource_history ADD COLUMN deleted INTEGER NOT NULL DEFAULT 0;
        "#,
        // v4 — resources per type, kept by triggers: the dashboard (every 5 s)
        // and /metrics (every scrape) counted the whole table, seconds at
        // 100M. Counted once here. Writes are INSERT OR REPLACE, whose
        // replaced row fires no DELETE trigger: an insert counts only when
        // the id is new (checked before the insert).
        r#"
        CREATE TABLE resource_counts (
            resource_type TEXT PRIMARY KEY,
            n INTEGER NOT NULL
        ) WITHOUT ROWID;
        INSERT INTO resource_counts (resource_type, n)
            SELECT resource_type, COUNT(*) FROM resources GROUP BY resource_type;
        CREATE TRIGGER counts_insert BEFORE INSERT ON resources
            WHEN NOT EXISTS (SELECT 1 FROM resources WHERE resource_type = NEW.resource_type AND id = NEW.id)
        BEGIN
            INSERT INTO resource_counts (resource_type, n) VALUES (NEW.resource_type, 1)
                ON CONFLICT (resource_type) DO UPDATE SET n = n + 1;
        END;
        CREATE TRIGGER counts_delete AFTER DELETE ON resources BEGIN
            UPDATE resource_counts SET n = n - 1 WHERE resource_type = OLD.resource_type;
        END;
        -- The primary keys start with resource_type: these two served nothing
        -- and cost GBs and a write each at 100M.
        DROP INDEX IF EXISTS idx_resources_type;
        DROP INDEX IF EXISTS idx_history_type;
        "#,
        // v5 — the HTTP method that made each version (POST, PUT, PATCH), for
        // the history's request.method: a create by PUT showed as POST.
        // NULL for versions before it (and $import's): inferred as before.
        r#"
        ALTER TABLE resource_history ADD COLUMN method TEXT;
        "#,
    ];

    /// Open the store (create if not exists)
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let mut conn = Connection::open(path)?;
        // Wait up to 5 s for a lock (e.g. a WAL checkpoint, --backup) instead of
        // failing with SQLITE_BUSY. rusqlite's default too; stated so it's not lost.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;

        // Enable WAL mode for read-write concurrency
        conn.execute_batch(crate::WAL_PRAGMAS)?;

        crate::migrate::run_migrations(&mut conn, Self::MIGRATIONS)?;

        // A read pool only makes sense for a real file: separate connections to
        // ":memory:" would each get their own empty database. In-memory stores
        // (tests) fall back to the single connection for reads.
        let is_memory = path
            .to_str()
            .map(|p| p.is_empty() || p.contains(":memory:"))
            .unwrap_or(false);
        let read_pool = if is_memory {
            Vec::new()
        } else {
            let mut pool = Vec::with_capacity(READ_POOL_SIZE);
            for _ in 0..READ_POOL_SIZE {
                let rc = Connection::open_with_flags(
                    path,
                    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )?;
                rc.busy_timeout(std::time::Duration::from_secs(5))?;
                pool.push(Mutex::new(rc));
            }
            pool
        };

        Ok(Self {
            conn: Mutex::new(conn),
            read_pool,
            next_reader: AtomicUsize::new(0),
        })
    }

    /// Lock the write connection, recovering from a poisoned mutex.
    ///
    /// A poisoned mutex means a previous operation panicked while holding the
    /// lock. The `Connection` itself stays usable (rusqlite calls are discrete
    /// and don't leave it half-mutated), so recover the guard instead of
    /// propagating the panic — otherwise a single panic would take down every
    /// subsequent request to the store.
    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Whether any current resource of `resource_type` exists.
    pub fn has_type(&self, resource_type: &str) -> Result<bool> {
        let conn = self.reader();
        Ok(conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM resources WHERE resource_type = ?1)",
            params![resource_type],
            |r| r.get(0),
        )?)
    }

    /// The number of the last change to the current resources (0: none yet).
    /// Read on the write connection: the latest commit.
    pub fn change_seq(&self) -> Result<i64> {
        let conn = self.conn();
        let seq: Option<i64> = conn
            .query_row("SELECT seq FROM sqlite_sequence WHERE name = 'changes'", [], |r| r.get(0))
            .optional()?;
        Ok(seq.unwrap_or(0))
    }

    /// The resources changed after change `seq` (each once).
    pub fn changed_since(&self, seq: i64) -> Result<Vec<(String, String)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT DISTINCT resource_type, id FROM changes WHERE seq > ?1")?;
        let rows = stmt.query_map(params![seq], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Drop the changes up to `seq` (the search index has them).
    pub fn forget_changes(&self, seq: i64) -> Result<()> {
        self.conn().execute("DELETE FROM changes WHERE seq <= ?1", params![seq])?;
        Ok(())
    }

    /// Pick a connection for a read query: round-robin over the read pool, or
    /// the write connection when there is no pool (in-memory store).
    fn reader(&self) -> MutexGuard<'_, Connection> {
        if self.read_pool.is_empty() {
            return self.conn();
        }
        let i = self.next_reader.fetch_add(1, Ordering::Relaxed) % self.read_pool.len();
        self.read_pool[i]
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The version to give `resource_type/id` when it is (re)created: one past
    /// its history, so a re-create after a delete continues the history.
    pub fn next_version(&self, resource_type: &str, id: &str) -> Result<String> {
        next_version_in(&self.conn(), resource_type, id)
    }

    /// Get a resource
    pub fn get(&self, resource_type: &str, id: &str) -> Result<Option<Vec<u8>>> {
        let conn = self.reader();

        let mut stmt = conn.prepare(
            "SELECT value FROM resources WHERE resource_type = ? AND id = ?"
        )?;
        let result = stmt.query_row(params![resource_type, id], |row| row.get::<_, String>(0));

        match result {
            Ok(value) => Ok(Some(value.into_bytes())),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Store a resource (current version)
    pub fn put(&self, resource_type: &str, id: &str, data: &[u8]) -> Result<()> {
        let value = std::str::from_utf8(data)
            .map_err(|e| crate::error::StoreError::Other(format!("Invalid UTF-8: {}", e)))?;
        let conn = self.conn();

        conn.execute(
            "INSERT OR REPLACE INTO resources (resource_type, id, value) VALUES (?, ?, ?)",
            params![resource_type, id, value],
        )?;

        Ok(())
    }

    /// Store a resource with version history
    pub fn put_with_version(
        &self,
        resource_type: &str,
        id: &str,
        version_id: &str,
        data: &[u8],
        method: &str,
    ) -> Result<()> {
        let value = std::str::from_utf8(data)
            .map_err(|e| crate::error::StoreError::Other(format!("Invalid UTF-8: {}", e)))?;

        let conn = self.conn();

        // Write the current-version row and the history row atomically — a crash
        // (or error) between them must never leave a current resource whose
        // version has no history entry (which would 404 on a vread of the live
        // version). The single writer mutex makes the unchecked transaction safe.
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "INSERT OR REPLACE INTO resources (resource_type, id, value) VALUES (?, ?, ?)",
            params![resource_type, id, value],
        )?;
        tx.execute(
            "INSERT INTO resource_history (resource_type, id, version_id, value, method) VALUES (?, ?, ?, ?, ?)",
            params![resource_type, id, version_id, value, method],
        )?;
        tx.commit()?;

        Ok(())
    }

    /// Several versions in one transaction — one commit (one sync) for all,
    /// where `put_with_version` syncs each: `$import` at the disk's sync rate
    /// was ~170 resources/s on an SSD. Each row is all or nothing (its
    /// current row and history row together); a failing row is reported and
    /// the others are kept.
    #[allow(clippy::type_complexity)]
    pub fn put_versions(&self, rows: &[(&str, &str, &str, &[u8])]) -> Result<Vec<Result<()>>> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let mut out = Vec::with_capacity(rows.len());
        for (resource_type, id, version_id, data) in rows {
            let row = (|| -> Result<()> {
                let value = std::str::from_utf8(data)
                    .map_err(|e| crate::error::StoreError::Other(format!("Invalid UTF-8: {e}")))?;
                tx.execute_batch("SAVEPOINT row")?;
                let written = tx
                    .prepare_cached(
                        "INSERT INTO resource_history (resource_type, id, version_id, value) VALUES (?, ?, ?, ?)",
                    )
                    .and_then(|mut s| s.execute(params![resource_type, id, version_id, value]))
                    .and_then(|_| {
                        tx.prepare_cached("INSERT OR REPLACE INTO resources (resource_type, id, value) VALUES (?, ?, ?)")
                            .and_then(|mut s| s.execute(params![resource_type, id, value]))
                    });
                match written {
                    Ok(_) => {
                        tx.execute_batch("RELEASE row")?;
                        Ok(())
                    }
                    Err(e) => {
                        tx.execute_batch("ROLLBACK TO row; RELEASE row")?;
                        Err(e.into())
                    }
                }
            })();
            out.push(row);
        }
        tx.commit()?;
        Ok(out)
    }

    /// Compare-and-swap write: persist `data` as `new_version` only if the
    /// resource's current stored version still matches `expected_current`
    /// (`Some(v)` for an update of a resource last seen at version `v`, `None`
    /// for a create that requires the resource to be absent). Returns `false`
    /// without writing when the precondition fails — the caller maps that to a
    /// 409/412. The read-compare-write happens under the single writer lock, so
    /// two concurrent updates can no longer both bump to the same version and
    /// clobber each other's history (a lost update).
    pub fn put_with_version_cas(
        &self,
        resource_type: &str,
        id: &str,
        expected_current: Option<&str>,
        new_version: &str,
        data: &[u8],
        method: &str,
    ) -> Result<bool> {
        let value = std::str::from_utf8(data)
            .map_err(|e| crate::error::StoreError::Other(format!("Invalid UTF-8: {}", e)))?;

        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;

        let current: Option<String> = tx
            .query_row(
                "SELECT json_extract(value, '$.meta.versionId') FROM resources \
                 WHERE resource_type = ? AND id = ?",
                params![resource_type, id],
                |row| row.get::<_, Option<String>>(0),
            )
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;

        let ok = match (expected_current, current.as_deref()) {
            (None, None) => true,                       // create, still absent
            (None, Some(_)) => false,                   // create, but now exists
            (Some(exp), Some(cur)) => exp == cur,       // update, version unchanged
            (Some(_), None) => false,                   // update, but vanished
        };
        if !ok {
            return Ok(false);
        }

        tx.execute(
            "INSERT OR REPLACE INTO resources (resource_type, id, value) VALUES (?, ?, ?)",
            params![resource_type, id, value],
        )?;
        tx.execute(
            "INSERT INTO resource_history (resource_type, id, version_id, value, method) VALUES (?, ?, ?, ?, ?)",
            params![resource_type, id, new_version, value, method],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Get a specific version
    pub fn get_version(
        &self,
        resource_type: &str,
        id: &str,
        version_id: &str,
    ) -> Result<Option<Vec<u8>>> {
        let conn = self.reader();

        let mut stmt = conn.prepare(
            "SELECT value FROM resource_history WHERE resource_type = ? AND id = ? AND version_id = ?"
        )?;
        let result = stmt.query_row(params![resource_type, id, version_id], |row| row.get::<_, String>(0));

        match result {
            Ok(value) => Ok(Some(value.into_bytes())),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// True if the resource has no current row but does have history — i.e. it
    /// was deleted (a tombstone). Used to answer reads of deleted resources with
    /// 410 Gone rather than 404 Not Found.
    pub fn is_deleted(&self, resource_type: &str, id: &str) -> Result<bool> {
        let conn = self.reader();
        let current: i64 = conn.query_row(
            "SELECT COUNT(*) FROM resources WHERE resource_type = ? AND id = ?",
            params![resource_type, id],
            |row| row.get(0),
        )?;
        if current > 0 {
            return Ok(false);
        }
        let history: i64 = conn.query_row(
            "SELECT COUNT(*) FROM resource_history WHERE resource_type = ? AND id = ?",
            params![resource_type, id],
            |row| row.get(0),
        )?;
        Ok(history > 0)
    }

    /// Delete a resource: the history gains a version that records the
    /// delete; `false` when there was nothing to delete.
    pub fn delete(&self, resource_type: &str, id: &str) -> Result<bool> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let deleted = delete_in(&tx, resource_type, id)?;
        tx.commit()?;
        Ok(deleted)
    }

    /// The newest version in the history (for a deleted resource: the record
    /// of its delete, which holds what it was).
    pub fn latest_version(&self, resource_type: &str, id: &str) -> Result<Option<Vec<u8>>> {
        let conn = self.reader();
        let row = conn
            .query_row(
                "SELECT value FROM resource_history WHERE resource_type = ?1 AND id = ?2 \
                 ORDER BY CAST(version_id AS INTEGER) DESC LIMIT 1",
                params![resource_type, id],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        Ok(row.map(String::into_bytes))
    }

    /// A version from the history, and whether it is the record of a delete.
    /// A version's content, whether it is a delete, and the HTTP method that
    /// made it (`None`: from before store v5, or `$import`) — one read.
    #[allow(clippy::type_complexity)]
    pub fn history_entry(&self, resource_type: &str, id: &str, version_id: &str) -> Result<Option<(Vec<u8>, bool, Option<String>)>> {
        let conn = self.reader();
        let row = conn
            .query_row(
                "SELECT value, deleted, method FROM resource_history WHERE resource_type = ? AND id = ? AND version_id = ?",
                params![resource_type, id, version_id],
                |r| Ok((r.get::<_, String>(0)?.into_bytes(), r.get::<_, i64>(1)? != 0, r.get::<_, Option<String>>(2)?)),
            )
            .optional()?;
        Ok(row)
    }

    pub fn get_history_version(&self, resource_type: &str, id: &str, version_id: &str) -> Result<Option<(Vec<u8>, bool)>> {
        let conn = self.reader();
        let row = conn
            .query_row(
                "SELECT value, deleted FROM resource_history WHERE resource_type = ? AND id = ? AND version_id = ?",
                params![resource_type, id, version_id],
                |r| Ok((r.get::<_, String>(0)?.into_bytes(), r.get::<_, i64>(1)? != 0)),
            )
            .optional()?;
        Ok(row)
    }

    /// List version history (list of version_ids)
    pub fn list_versions(&self, resource_type: &str, id: &str) -> Result<Vec<String>> {
        let conn = self.reader();

        // Order numerically: version_id is TEXT, so a lexical sort would put
        // "10" before "2". CAST to integer for the common monotonic-integer
        // versioning scheme (descending = newest first).
        let mut stmt = conn.prepare(
            "SELECT version_id FROM resource_history WHERE resource_type = ? AND id = ? \
             ORDER BY CAST(version_id AS INTEGER) DESC"
        )?;
        let rows = stmt.query_map(params![resource_type, id], |row| row.get::<_, String>(0))?;

        let mut versions = Vec::new();
        for version_id in rows {
            versions.push(version_id?);
        }

        Ok(versions)
    }

    /// True if the store holds at least one resource (row), without loading any.
    pub fn has_any(&self) -> Result<bool> {
        let conn = self.reader();
        let exists: bool =
            conn.query_row("SELECT EXISTS(SELECT 1 FROM resources)", [], |row| row.get(0))?;
        Ok(exists)
    }

    /// Resources per type (from `resource_counts`: a read of a few rows, not
    /// a count of the table)
    pub fn count_by_type(&self) -> Result<Vec<(String, i64)>> {
        let conn = self.reader();
        let mut stmt = conn.prepare(
            "SELECT resource_type, n FROM resource_counts WHERE n > 0 ORDER BY resource_type",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut counts = Vec::new();
        for row in rows {
            counts.push(row?);
        }
        Ok(counts)
    }

    /// List all resources (optionally filtered by resource type)
    pub fn list_all(&self, resource_type: Option<&str>) -> Result<Vec<(String, String, Vec<u8>)>> {
        let conn = self.reader();

        let mut results = Vec::new();

        if let Some(rt) = resource_type {
            let mut stmt = conn.prepare(
                "SELECT resource_type, id, value FROM resources WHERE resource_type = ? ORDER BY id",
            )?;
            let rows = stmt.query_map(params![rt], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (rt, id, val) = row?;
                results.push((rt, id, val.into_bytes()));
            }
        } else {
            let mut stmt = conn.prepare(
                "SELECT resource_type, id, value FROM resources ORDER BY resource_type, id",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (rt, id, val) = row?;
                results.push((rt, id, val.into_bytes()));
            }
        }

        Ok(results)
    }

    /// One page of a type's current resources in id order, after `after_id`
    /// (empty for the first page): a pass over millions of resources in
    /// bounded memory.
    pub fn list_page(&self, resource_type: &str, after_id: &str, limit: usize) -> Result<Vec<(String, Vec<u8>)>> {
        let conn = self.reader();
        let mut stmt = conn.prepare_cached(
            "SELECT id, value FROM resources WHERE resource_type = ?1 AND id > ?2 ORDER BY id LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![resource_type, after_id, limit as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut page = Vec::new();
        for row in rows {
            let (id, val) = row?;
            page.push((id, val.into_bytes()));
        }
        Ok(page)
    }

    /// List resource IDs of a type (id column only).
    ///
    /// Used by search when no parameters are given: avoids loading every
    /// resource body into memory just to extract IDs (the caller fetches only
    /// the page it needs afterwards).
    pub fn list_ids(&self, resource_type: &str) -> Result<Vec<String>> {
        let conn = self.reader();
        let mut stmt = conn
            .prepare("SELECT id FROM resources WHERE resource_type = ? ORDER BY id")?;
        let rows = stmt.query_map(params![resource_type], |row| row.get::<_, String>(0))?;
        let mut ids = Vec::new();
        for id in rows {
            ids.push(id?);
        }
        Ok(ids)
    }

    /// Execute multiple operations atomically within an SQLite transaction
    pub fn in_transaction<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&TransactionOps<'_>) -> Result<T>,
    {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let ops = TransactionOps { tx: &tx };
        let result = f(&ops)?;
        tx.commit()?;
        Ok(result)
    }
}

/// Operations available within a transaction
pub struct TransactionOps<'a> {
    tx: &'a Transaction<'a>,
}

#[allow(clippy::result_large_err)]
impl<'a> TransactionOps<'a> {
    /// Store a resource with version history
    pub fn put_with_version(
        &self,
        resource_type: &str,
        id: &str,
        version_id: &str,
        data: &[u8],
        method: &str,
    ) -> Result<()> {
        let value = std::str::from_utf8(data)
            .map_err(|e| crate::error::StoreError::Other(format!("Invalid UTF-8: {}", e)))?;
        let conn = self.tx.deref();

        conn.execute(
            "INSERT OR REPLACE INTO resources (resource_type, id, value) VALUES (?, ?, ?)",
            params![resource_type, id, value],
        )?;
        conn.execute(
            "INSERT INTO resource_history (resource_type, id, version_id, value, method) VALUES (?, ?, ?, ?, ?)",
            params![resource_type, id, version_id, value, method],
        )?;
        Ok(())
    }

    /// [`SqliteStore::next_version`] inside the transaction.
    pub fn next_version(&self, resource_type: &str, id: &str) -> Result<String> {
        next_version_in(self.tx.deref(), resource_type, id)
    }

    /// Get a resource
    pub fn get(&self, resource_type: &str, id: &str) -> Result<Option<Vec<u8>>> {
        let conn = self.tx.deref();
        let mut stmt = conn.prepare(
            "SELECT value FROM resources WHERE resource_type = ? AND id = ?",
        )?;
        let result = stmt.query_row(params![resource_type, id], |row| row.get::<_, String>(0));
        match result {
            Ok(value) => Ok(Some(value.into_bytes())),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Delete a resource (see [`SqliteStore::delete`]).
    pub fn delete(&self, resource_type: &str, id: &str) -> Result<bool> {
        delete_in(self.tx.deref(), resource_type, id)
    }
}

/// Delete `resource_type/id` inside the caller's transaction: record the
/// delete as the next version (the deleted content, with that versionId and
/// the time), then remove the current resource.
fn delete_in(conn: &Connection, resource_type: &str, id: &str) -> Result<bool> {
    let current: Option<String> = conn
        .query_row(
            "SELECT value FROM resources WHERE resource_type = ?1 AND id = ?2",
            params![resource_type, id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(current) = current else { return Ok(false) };
    let version = next_version_in(conn, resource_type, id)?;
    let mut record: serde_json::Value = serde_json::from_str(&current).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(obj) = record.as_object_mut() {
        let meta = obj.entry("meta").or_insert_with(|| serde_json::json!({}));
        if let Some(meta) = meta.as_object_mut() {
            meta.insert("versionId".into(), serde_json::json!(version));
            meta.insert("lastUpdated".into(), serde_json::json!(chrono::Utc::now().to_rfc3339()));
        }
    }
    conn.execute(
        "INSERT INTO resource_history (resource_type, id, version_id, value, deleted) VALUES (?1, ?2, ?3, ?4, 1)",
        params![resource_type, id, version, record.to_string()],
    )?;
    conn.execute("DELETE FROM resources WHERE resource_type = ?1 AND id = ?2", params![resource_type, id])?;
    Ok(true)
}

/// The version a write of an absent resource starts at: one past the highest
/// version in its history (a deleted resource keeps its history, and a
/// re-create must continue it, not overwrite version 1), or "1".
fn next_version_in(conn: &Connection, resource_type: &str, id: &str) -> Result<String> {
    let max: Option<i64> = conn.query_row(
        "SELECT MAX(CAST(version_id AS INTEGER)) FROM resource_history WHERE resource_type = ?1 AND id = ?2",
        params![resource_type, id],
        |row| row.get(0),
    )?;
    Ok((max.unwrap_or(0) + 1).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_put_and_get() {
        let store = SqliteStore::open(":memory:").unwrap();

        let data = br#"{"resourceType":"Patient","id":"123"}"#;
        store.put("Patient", "123", data).unwrap();

        let retrieved = store.get("Patient", "123").unwrap();
        assert_eq!(retrieved, Some(data.to_vec()));
    }

    /// The kept counts match a count of the table through creates, updates
    /// (INSERT OR REPLACE), deletes and re-creates.
    #[test]
    fn counts_follow_writes() {
        let store = SqliteStore::open(":memory:").unwrap();
        let counted = |store: &SqliteStore| -> Vec<(String, i64)> {
            let conn = store.conn();
            let mut st = conn
                .prepare("SELECT resource_type, COUNT(*) FROM resources GROUP BY resource_type ORDER BY resource_type")
                .unwrap();
            st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        let p = |id: &str| format!(r#"{{"resourceType":"Patient","id":"{id}"}}"#);
        for id in ["1", "2", "3"] {
            store.put("Patient", id, p(id).as_bytes()).unwrap();
        }
        store.put("Patient", "2", p("2").as_bytes()).unwrap(); // update
        store.put("Observation", "o", br#"{"resourceType":"Observation","id":"o"}"#).unwrap();
        assert!(store.delete("Patient", "1").unwrap());
        assert!(!store.delete("Patient", "1").unwrap()); // already gone
        store.put("Patient", "1", p("1").as_bytes()).unwrap(); // re-create
        assert!(store.delete("Observation", "o").unwrap());
        assert_eq!(store.count_by_type().unwrap(), counted(&store));
        assert_eq!(store.count_by_type().unwrap(), vec![("Patient".to_string(), 3)]);
    }

    /// A store from before v4 gets its counts on upgrade.
    #[test]
    fn counts_are_taken_on_upgrade() {
        let path = std::env::temp_dir().join(format!("sazare-counts-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let mut conn = Connection::open(&path).unwrap();
            crate::migrate::run_migrations(&mut conn, &SqliteStore::MIGRATIONS[..3]).unwrap();
            for id in ["a", "b"] {
                conn.execute("INSERT INTO resources VALUES ('Patient', ?1, '{}')", [id]).unwrap();
            }
        }
        let store = SqliteStore::open(&path).unwrap();
        assert_eq!(store.count_by_type().unwrap(), vec![("Patient".to_string(), 2)]);
        drop(store);
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
        }
    }

    #[test]
    fn test_has_any() {
        let store = SqliteStore::open(":memory:").unwrap();
        assert!(!store.has_any().unwrap());
        store.put("Patient", "1", br#"{"resourceType":"Patient","id":"1"}"#).unwrap();
        assert!(store.has_any().unwrap());
    }

    #[test]
    fn test_cas_prevents_lost_update() {
        let store = SqliteStore::open(":memory:").unwrap();
        let v1 = br#"{"resourceType":"Patient","id":"1","meta":{"versionId":"1"}}"#;
        store.put_with_version("Patient", "1", "1", v1, "PUT").unwrap();

        // Two writers both read version "1" and try to write "2".
        let a = br#"{"resourceType":"Patient","id":"1","meta":{"versionId":"2"},"x":"A"}"#;
        let b = br#"{"resourceType":"Patient","id":"1","meta":{"versionId":"2"},"x":"B"}"#;

        // First CAS against expected current "1" succeeds.
        assert!(store.put_with_version_cas("Patient", "1", Some("1"), "2", a, "PUT").unwrap());
        // Second CAS still expecting "1" must fail — current is now "2".
        assert!(!store.put_with_version_cas("Patient", "1", Some("1"), "2", b, "PUT").unwrap());

        // The winner's content survived.
        let cur = store.get("Patient", "1").unwrap().unwrap();
        assert!(String::from_utf8(cur).unwrap().contains("\"A\""));
    }

    #[test]
    fn test_cas_create_requires_absent() {
        let store = SqliteStore::open(":memory:").unwrap();
        let v1 = br#"{"resourceType":"Patient","id":"1","meta":{"versionId":"1"}}"#;
        // create (expected absent) on empty store → ok
        assert!(store.put_with_version_cas("Patient", "1", None, "1", v1, "PUT").unwrap());
        // create again with expected-absent → refused (already exists)
        assert!(!store.put_with_version_cas("Patient", "1", None, "1", v1, "PUT").unwrap());
    }

    #[test]
    fn test_versioning() {
        let store = SqliteStore::open(":memory:").unwrap();

        let v1 = br#"{"resourceType":"Patient","id":"123","meta":{"versionId":"1"}}"#;
        let v2 = br#"{"resourceType":"Patient","id":"123","meta":{"versionId":"2"}}"#;

        store.put_with_version("Patient", "123", "1", v1, "PUT").unwrap();
        store.put_with_version("Patient", "123", "2", v2, "PUT").unwrap();

        // Current version should be v2
        let current = store.get("Patient", "123").unwrap();
        assert_eq!(current, Some(v2.to_vec()));

        // v1 is still accessible
        let history = store.get_version("Patient", "123", "1").unwrap();
        assert_eq!(history, Some(v1.to_vec()));
    }

    #[test]
    fn test_delete() {
        let store = SqliteStore::open(":memory:").unwrap();

        let data = br#"{"resourceType":"Patient","id":"123"}"#;
        store.put("Patient", "123", data).unwrap();

        assert!(store.delete("Patient", "123").unwrap());
        assert_eq!(store.get("Patient", "123").unwrap(), None);
    }

    #[test]
    fn test_list_all() {
        let store = SqliteStore::open(":memory:").unwrap();

        store.put("Patient", "p1", br#"{"resourceType":"Patient","id":"p1"}"#).unwrap();
        store.put("Patient", "p2", br#"{"resourceType":"Patient","id":"p2"}"#).unwrap();
        store.put("Observation", "o1", br#"{"resourceType":"Observation","id":"o1"}"#).unwrap();

        // All resources
        let all = store.list_all(None).unwrap();
        assert_eq!(all.len(), 3);

        // Filtered by type
        let patients = store.list_all(Some("Patient")).unwrap();
        assert_eq!(patients.len(), 2);

        let obs = store.list_all(Some("Observation")).unwrap();
        assert_eq!(obs.len(), 1);

        let empty = store.list_all(Some("Encounter")).unwrap();
        assert_eq!(empty.len(), 0);
    }

    #[test]
    fn test_in_transaction_commit() {
        let store = SqliteStore::open(":memory:").unwrap();

        let d1 = br#"{"resourceType":"Patient","id":"p1","meta":{"versionId":"1"}}"#;
        let d2 = br#"{"resourceType":"Observation","id":"o1","meta":{"versionId":"1"}}"#;

        store.in_transaction(|ops| {
            ops.put_with_version("Patient", "p1", "1", d1, "PUT")?;
            ops.put_with_version("Observation", "o1", "1", d2, "PUT")?;
            Ok(())
        }).unwrap();

        assert!(store.get("Patient", "p1").unwrap().is_some());
        assert!(store.get("Observation", "o1").unwrap().is_some());
    }

    #[test]
    fn test_in_transaction_rollback() {
        let store = SqliteStore::open(":memory:").unwrap();

        let d1 = br#"{"resourceType":"Patient","id":"p1","meta":{"versionId":"1"}}"#;

        let result: Result<()> = store.in_transaction(|ops| {
            ops.put_with_version("Patient", "p1", "1", d1, "PUT")?;
            // Force an error after the put
            Err(crate::error::StoreError::Other("forced error".into()))
        });

        assert!(result.is_err());
        // Nothing should be saved due to rollback
        assert!(store.get("Patient", "p1").unwrap().is_none());
    }

    #[test]
    fn test_recovers_from_poisoned_lock() {
        let store = SqliteStore::open(":memory:").unwrap();
        store
            .put("Patient", "p1", br#"{"resourceType":"Patient","id":"p1"}"#)
            .unwrap();

        // Poison the connection mutex: panic while the lock guard is held.
        // (Silence the panic hook so the expected backtrace doesn't clutter
        // test output.)
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: Result<()> = store.in_transaction(|_ops| {
                panic!("boom while holding the connection lock");
            });
        }));
        std::panic::set_hook(prev);
        assert!(caught.is_err(), "the panic should have been caught");

        // Despite the now-poisoned mutex, the store must keep working rather
        // than panicking on every subsequent lock.
        assert!(store.get("Patient", "p1").unwrap().is_some());
        store
            .put("Patient", "p2", br#"{"resourceType":"Patient","id":"p2"}"#)
            .unwrap();
        assert!(store.get("Patient", "p2").unwrap().is_some());
    }

    #[test]
    fn test_read_pool_sees_writes() {
        // A file-backed store gets a read pool; every pooled read connection
        // must observe committed writes (WAL snapshot at read start).
        let path = std::env::temp_dir().join(format!("sazare-rp-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let store = SqliteStore::open(&path).unwrap();
        assert_eq!(
            store.read_pool.len(),
            READ_POOL_SIZE,
            "file-backed store should have a read pool"
        );

        store
            .put("Patient", "p1", br#"{"resourceType":"Patient","id":"p1"}"#)
            .unwrap();
        // Read more times than the pool size so the round-robin visits every
        // read connection.
        for _ in 0..(READ_POOL_SIZE * 2 + 1) {
            assert!(store.get("Patient", "p1").unwrap().is_some());
        }

        drop(store);
        for ext in ["sqlite", "sqlite-wal", "sqlite-shm"] {
            let _ = std::fs::remove_file(path.with_extension(ext));
        }
    }
}
