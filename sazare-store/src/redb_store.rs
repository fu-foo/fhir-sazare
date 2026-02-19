//! ReDB-based resource storage
//!
//! Key format:
//!   - Current: {resource_type}/{id}
//!   - History: {resource_type}/{id}/_ver/{version_id}

use crate::error::Result;
use redb::{Database, TableDefinition};
use std::path::Path;

const RESOURCES: TableDefinition<&str, &[u8]> = TableDefinition::new("resources");

/// ReDB-backed resource store
pub struct RedbStore {
    db: Database,
}

#[allow(clippy::result_large_err)]
impl RedbStore {
    /// Open the store (create if not exists)
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let db = Database::create(path)?;

        // Initialize table
        let write_txn = db.begin_write()?;
        {
            let _ = write_txn.open_table(RESOURCES)?;
        }
        write_txn.commit()?;

        Ok(Self { db })
    }

    /// Get a resource
    pub fn get(&self, resource_type: &str, id: &str) -> Result<Option<Vec<u8>>> {
        let key = format!("{}/{}", resource_type, id);
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(RESOURCES)?;

        match table.get(key.as_str())? {
            Some(value) => Ok(Some(value.value().to_vec())),
            None => Ok(None),
        }
    }

    /// Save a resource (current version)
    pub fn put(&self, resource_type: &str, id: &str, data: &[u8]) -> Result<()> {
        let key = format!("{}/{}", resource_type, id);
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(RESOURCES)?;
            table.insert(key.as_str(), data)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Save a resource with versioned history
    pub fn put_with_version(
        &self,
        resource_type: &str,
        id: &str,
        version_id: &str,
        data: &[u8],
    ) -> Result<()> {
        let current_key = format!("{}/{}", resource_type, id);
        let history_key = format!("{}/{}/_ver/{}", resource_type, id, version_id);

        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(RESOURCES)?;
            // Save current version
            table.insert(current_key.as_str(), data)?;
            // Save history version
            table.insert(history_key.as_str(), data)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Get a specific version
    pub fn get_version(
        &self,
        resource_type: &str,
        id: &str,
        version_id: &str,
    ) -> Result<Option<Vec<u8>>> {
        let key = format!("{}/{}/_ver/{}", resource_type, id, version_id);
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(RESOURCES)?;

        match table.get(key.as_str())? {
            Some(value) => Ok(Some(value.value().to_vec())),
            None => Ok(None),
        }
    }

    /// Delete a resource (current version only, history is preserved)
    pub fn delete(&self, resource_type: &str, id: &str) -> Result<bool> {
        let key = format!("{}/{}", resource_type, id);
        let write_txn = self.db.begin_write()?;
        let removed = {
            let mut table = write_txn.open_table(RESOURCES)?;
            table.remove(key.as_str())?.is_some()
        };
        write_txn.commit()?;
        Ok(removed)
    }

    /// List all versions (returns version_id list)
    pub fn list_versions(&self, resource_type: &str, id: &str) -> Result<Vec<String>> {
        let prefix = format!("{}/{}/_ver/", resource_type, id);
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(RESOURCES)?;

        let mut versions = Vec::new();
        let range = table.range::<&str>(..)?;

        for entry in range {
            let (key, _) = entry?;
            let key_str = key.value();
            if key_str.starts_with(&prefix)
                && let Some(ver) = key_str.strip_prefix(&prefix)
            {
                versions.push(ver.to_string());
            }
        }

        Ok(versions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_db_path(test_name: &str) -> String {
        format!("/tmp/test_redb_{}_{}.db", std::process::id(), test_name)
    }

    #[test]
    fn test_put_and_get() {
        let path = temp_db_path("put_get");
        let store = RedbStore::open(&path).unwrap();

        let data = br#"{"resourceType":"Patient","id":"123"}"#;
        store.put("Patient", "123", data).unwrap();

        let retrieved = store.get("Patient", "123").unwrap();
        assert_eq!(retrieved, Some(data.to_vec()));

        fs::remove_file(&path).ok();
    }

    #[test]
    fn test_versioning() {
        let path = temp_db_path("versioning");
        let store = RedbStore::open(&path).unwrap();

        let v1 = br#"{"resourceType":"Patient","id":"123","meta":{"versionId":"1"}}"#;
        let v2 = br#"{"resourceType":"Patient","id":"123","meta":{"versionId":"2"}}"#;

        store.put_with_version("Patient", "123", "1", v1).unwrap();
        store.put_with_version("Patient", "123", "2", v2).unwrap();

        // Current version should be v2
        let current = store.get("Patient", "123").unwrap();
        assert_eq!(current, Some(v2.to_vec()));

        // v1 should still be retrievable
        let history = store.get_version("Patient", "123", "1").unwrap();
        assert_eq!(history, Some(v1.to_vec()));

        fs::remove_file(&path).ok();
    }
}
