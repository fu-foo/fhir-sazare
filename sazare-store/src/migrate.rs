//! Minimal schema migrations using SQLite's built-in `PRAGMA user_version`.
//!
//! Each database (resources, search index, audit) declares an ordered list of
//! migration scripts. `migrations[i]` upgrades the schema from version `i` to
//! `i + 1`, so the target version is `migrations.len()`. On open, any pending
//! migrations are applied in order, each in its own transaction; already-applied
//! ones are skipped. Existing pre-versioning databases sit at version 0 and are
//! brought forward by the (idempotent, `IF NOT EXISTS`) initial migration
//! without data loss. New schema changes are made by appending a script.

use crate::error::Result;
use rusqlite::Connection;

pub(crate) fn run_migrations(conn: &mut Connection, migrations: &[&str]) -> Result<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let target = migrations.len() as i64;
    for version in current..target {
        let tx = conn.transaction()?;
        tx.execute_batch(migrations[version as usize])?;
        // user_version is stored in the db header and is part of the transaction.
        tx.execute_batch(&format!("PRAGMA user_version = {};", version + 1))?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_applies_and_is_idempotent() {
        let mut conn = Connection::open_in_memory().unwrap();
        let migrations = ["CREATE TABLE t (id INTEGER);"];

        run_migrations(&mut conn, &migrations).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 1);

        // Insert a row, then re-run: migration must not re-create/clobber.
        conn.execute("INSERT INTO t (id) VALUES (1)", []).unwrap();
        run_migrations(&mut conn, &migrations).unwrap();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1, "re-running migrations must not drop data");
    }

    #[test]
    fn test_applies_only_pending() {
        let mut conn = Connection::open_in_memory().unwrap();
        run_migrations(&mut conn, &["CREATE TABLE a (id INTEGER);"]).unwrap();
        // Add a second migration; only it should run.
        run_migrations(
            &mut conn,
            &["CREATE TABLE a (id INTEGER);", "CREATE TABLE b (id INTEGER);"],
        )
        .unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 2);
        // Both tables exist.
        for t in ["a", "b"] {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?",
                    [t],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "table {} should exist", t);
        }
    }
}
