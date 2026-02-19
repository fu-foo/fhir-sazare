//! SQLite-based resource storage
//!
//! Schema:
//!   - resources: Current version only (resource_type, id)
//!   - resource_history: Version history (resource_type, id, version_id)

use crate::error::Result;
use rusqlite::{params, Connection, Transaction};
use std::ops::Deref;
use std::path::Path;
use std::sync::Mutex;

/// SQLite-based resource store
pub struct SqliteStore {
    conn: Mutex<Connection>,
}

#[allow(clippy::result_large_err)]
impl SqliteStore {
    /// Open the store (create if not exists)
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;

        // Enable WAL mode for read-write concurrency
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;

        // Current version table
        conn.execute(
            "CREATE TABLE IF NOT EXISTS resources (
                resource_type TEXT NOT NULL,
                id TEXT NOT NULL,
                value TEXT NOT NULL,
                PRIMARY KEY (resource_type, id)
            )",
            [],
        )?;

        // History table
        conn.execute(
            "CREATE TABLE IF NOT EXISTS resource_history (
                resource_type TEXT NOT NULL,
                id TEXT NOT NULL,
                version_id TEXT NOT NULL,
                value TEXT NOT NULL,
                PRIMARY KEY (resource_type, id, version_id)
            )",
            [],
        )?;

        // Create indexes
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_resources_type ON resources(resource_type)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_history_type ON resource_history(resource_type)",
            [],
        )?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Get a resource
    pub fn get(&self, resource_type: &str, id: &str) -> Result<Option<Vec<u8>>> {
        let conn = self.conn.lock().unwrap();

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
        let conn = self.conn.lock().unwrap();

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
    ) -> Result<()> {
        let value = std::str::from_utf8(data)
            .map_err(|e| crate::error::StoreError::Other(format!("Invalid UTF-8: {}", e)))?;

        let conn = self.conn.lock().unwrap();

        // Save current version
        conn.execute(
            "INSERT OR REPLACE INTO resources (resource_type, id, value) VALUES (?, ?, ?)",
            params![resource_type, id, value],
        )?;

        // Save to history
        conn.execute(
            "INSERT OR REPLACE INTO resource_history (resource_type, id, version_id, value) VALUES (?, ?, ?, ?)",
            params![resource_type, id, version_id, value],
        )?;

        Ok(())
    }

    /// Get a specific version
    pub fn get_version(
        &self,
        resource_type: &str,
        id: &str,
        version_id: &str,
    ) -> Result<Option<Vec<u8>>> {
        let conn = self.conn.lock().unwrap();

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

    /// Delete a resource (current version only, history is preserved)
    pub fn delete(&self, resource_type: &str, id: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();

        let rows = conn.execute(
            "DELETE FROM resources WHERE resource_type = ? AND id = ?",
            params![resource_type, id]
        )?;
        Ok(rows > 0)
    }

    /// List version history (list of version_ids)
    pub fn list_versions(&self, resource_type: &str, id: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();

        let mut stmt = conn.prepare(
            "SELECT version_id FROM resource_history WHERE resource_type = ? AND id = ? ORDER BY version_id"
        )?;
        let rows = stmt.query_map(params![resource_type, id], |row| row.get::<_, String>(0))?;

        let mut versions = Vec::new();
        for version_id in rows {
            versions.push(version_id?);
        }

        Ok(versions)
    }

    /// Get resource counts by type
    pub fn count_by_type(&self) -> Result<Vec<(String, i64)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT resource_type, COUNT(*) FROM resources GROUP BY resource_type ORDER BY resource_type",
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
        let conn = self.conn.lock().unwrap();

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

    /// Execute multiple operations atomically within an SQLite transaction
    pub fn in_transaction<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&TransactionOps<'_>) -> Result<T>,
    {
        let mut conn = self.conn.lock().unwrap();
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
    ) -> Result<()> {
        let value = std::str::from_utf8(data)
            .map_err(|e| crate::error::StoreError::Other(format!("Invalid UTF-8: {}", e)))?;
        let conn = self.tx.deref();

        conn.execute(
            "INSERT OR REPLACE INTO resources (resource_type, id, value) VALUES (?, ?, ?)",
            params![resource_type, id, value],
        )?;
        conn.execute(
            "INSERT OR REPLACE INTO resource_history (resource_type, id, version_id, value) VALUES (?, ?, ?, ?)",
            params![resource_type, id, version_id, value],
        )?;
        Ok(())
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

    /// Delete a resource (current version only, history is preserved)
    pub fn delete(&self, resource_type: &str, id: &str) -> Result<bool> {
        let conn = self.tx.deref();
        let rows = conn.execute(
            "DELETE FROM resources WHERE resource_type = ? AND id = ?",
            params![resource_type, id],
        )?;
        Ok(rows > 0)
    }
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

    #[test]
    fn test_versioning() {
        let store = SqliteStore::open(":memory:").unwrap();

        let v1 = br#"{"resourceType":"Patient","id":"123","meta":{"versionId":"1"}}"#;
        let v2 = br#"{"resourceType":"Patient","id":"123","meta":{"versionId":"2"}}"#;

        store.put_with_version("Patient", "123", "1", v1).unwrap();
        store.put_with_version("Patient", "123", "2", v2).unwrap();

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
            ops.put_with_version("Patient", "p1", "1", d1)?;
            ops.put_with_version("Observation", "o1", "1", d2)?;
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
            ops.put_with_version("Patient", "p1", "1", d1)?;
            // Force an error after the put
            Err(crate::error::StoreError::Other("forced error".into()))
        });

        assert!(result.is_err());
        // Nothing should be saved due to rollback
        assert!(store.get("Patient", "p1").unwrap().is_none());
    }
}
