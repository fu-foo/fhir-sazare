//! SQLite-based search index
//!
//! Single file with tables per resource type for performance.

use crate::error::Result;
use rusqlite::{params, Connection};
use std::path::Path;

/// SQLite-backed search index
pub struct SearchIndex {
    conn: Connection,
}

#[allow(clippy::result_large_err)]
impl SearchIndex {
    /// Open the index (create if not exists)
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        let index = Self { conn };
        index.initialize()?;
        Ok(index)
    }

    /// Initialize tables
    fn initialize(&self) -> Result<()> {
        // Generic search index table
        self.conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS search_index (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                resource_type TEXT NOT NULL,
                resource_id TEXT NOT NULL,
                param_name TEXT NOT NULL,
                param_type TEXT NOT NULL,
                value_string TEXT,
                value_string_lower TEXT,
                value_system TEXT,
                value_date_start INTEGER,
                value_date_end INTEGER,
                UNIQUE(resource_type, resource_id, param_name, value_string, value_system)
            )
            "#,
            [],
        )?;

        // Create indexes
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_type_param_string
             ON search_index(resource_type, param_name, value_string)",
            [],
        )?;

        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_type_param_token
             ON search_index(resource_type, param_name, value_system, value_string)",
            [],
        )?;

        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_type_param_date
             ON search_index(resource_type, param_name, value_date_start, value_date_end)",
            [],
        )?;

        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_resource
             ON search_index(resource_type, resource_id)",
            [],
        )?;

        Ok(())
    }

    /// Add an index entry
    pub fn add_index(
        &self,
        resource_type: &str,
        resource_id: &str,
        param_name: &str,
        param_type: &str,
        value_string: Option<&str>,
        value_system: Option<&str>,
    ) -> Result<()> {
        let value_string_lower = value_string.map(|s| s.to_lowercase());

        self.conn.execute(
            r#"
            INSERT OR REPLACE INTO search_index
            (resource_type, resource_id, param_name, param_type,
             value_string, value_string_lower, value_system)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
            params![
                resource_type,
                resource_id,
                param_name,
                param_type,
                value_string,
                value_string_lower,
                value_system,
            ],
        )?;

        Ok(())
    }

    /// Remove all index entries for a resource
    pub fn remove_index(&self, resource_type: &str, resource_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM search_index WHERE resource_type = ?1 AND resource_id = ?2",
            params![resource_type, resource_id],
        )?;
        Ok(())
    }

    /// Token search (code, identifier, etc.)
    pub fn search_token(
        &self,
        resource_type: &str,
        param_name: &str,
        system: Option<&str>,
        code: &str,
    ) -> Result<Vec<String>> {
        let mut ids = Vec::new();

        if let Some(sys) = system {
            let mut stmt = self.conn.prepare(
                r#"
                SELECT DISTINCT resource_id FROM search_index
                WHERE resource_type = ?1
                  AND param_name = ?2
                  AND value_system = ?3
                  AND value_string = ?4
                "#,
            )?;
            let rows = stmt.query_map(params![resource_type, param_name, sys, code], |row| {
                row.get(0)
            })?;
            for row in rows {
                ids.push(row?);
            }
        } else {
            let mut stmt = self.conn.prepare(
                r#"
                SELECT DISTINCT resource_id FROM search_index
                WHERE resource_type = ?1
                  AND param_name = ?2
                  AND value_string = ?3
                "#,
            )?;
            let rows = stmt.query_map(params![resource_type, param_name, code], |row| {
                row.get(0)
            })?;
            for row in rows {
                ids.push(row?);
            }
        }

        Ok(ids)
    }

    /// String search (name, etc., prefix match)
    pub fn search_string(
        &self,
        resource_type: &str,
        param_name: &str,
        value: &str,
        exact: bool,
    ) -> Result<Vec<String>> {
        let query = if exact {
            r#"
            SELECT DISTINCT resource_id FROM search_index
            WHERE resource_type = ?1
              AND param_name = ?2
              AND value_string_lower = ?3
            "#
        } else {
            r#"
            SELECT DISTINCT resource_id FROM search_index
            WHERE resource_type = ?1
              AND param_name = ?2
              AND value_string_lower LIKE ?3
            "#
        };

        let search_value = if exact {
            value.to_lowercase()
        } else {
            format!("{}%", value.to_lowercase())
        };

        let mut stmt = self.conn.prepare(query)?;
        let rows = stmt.query_map(params![resource_type, param_name, search_value], |row| {
            row.get(0)
        })?;

        let mut ids = Vec::new();
        for row in rows {
            ids.push(row?);
        }

        Ok(ids)
    }

    /// Reference search (subject, patient, etc.)
    pub fn search_reference(
        &self,
        resource_type: &str,
        param_name: &str,
        reference: &str,
    ) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT DISTINCT resource_id FROM search_index
            WHERE resource_type = ?1
              AND param_name = ?2
              AND value_string = ?3
              AND param_type = 'reference'
            "#,
        )?;
        let rows = stmt.query_map(params![resource_type, param_name, reference], |row| {
            row.get(0)
        })?;

        let mut ids = Vec::new();
        for row in rows {
            ids.push(row?);
        }

        Ok(ids)
    }

    /// Date search (with prefix: eq, ge, le, gt, lt)
    pub fn search_date_with_prefix(
        &self,
        resource_type: &str,
        param_name: &str,
        prefix: &str,
        value: &str,
    ) -> Result<Vec<String>> {
        let (_op, query) = match prefix {
            "eq" => ("=", r#"
                SELECT DISTINCT resource_id FROM search_index
                WHERE resource_type = ?1 AND param_name = ?2 AND value_string = ?3
            "#),
            "ge" => (">=", r#"
                SELECT DISTINCT resource_id FROM search_index
                WHERE resource_type = ?1 AND param_name = ?2 AND value_string >= ?3
            "#),
            "le" => ("<=", r#"
                SELECT DISTINCT resource_id FROM search_index
                WHERE resource_type = ?1 AND param_name = ?2 AND value_string <= ?3
            "#),
            "gt" => (">", r#"
                SELECT DISTINCT resource_id FROM search_index
                WHERE resource_type = ?1 AND param_name = ?2 AND value_string > ?3
            "#),
            "lt" => ("<", r#"
                SELECT DISTINCT resource_id FROM search_index
                WHERE resource_type = ?1 AND param_name = ?2 AND value_string < ?3
            "#),
            _ => ("=", r#"
                SELECT DISTINCT resource_id FROM search_index
                WHERE resource_type = ?1 AND param_name = ?2 AND value_string = ?3
            "#),
        };
        let mut stmt = self.conn.prepare(query)?;
        let rows = stmt.query_map(params![resource_type, param_name, value], |row| {
            row.get(0)
        })?;

        let mut ids = Vec::new();
        for row in rows {
            ids.push(row?);
        }

        Ok(ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_search() {
        let index = SearchIndex::open(":memory:").unwrap();

        index
            .add_index(
                "Patient",
                "123",
                "identifier",
                "token",
                Some("12345678"),
                Some("urn:oid:1.2.392.100495.20.3.51"),
            )
            .unwrap();

        let results = index
            .search_token(
                "Patient",
                "identifier",
                Some("urn:oid:1.2.392.100495.20.3.51"),
                "12345678",
            )
            .unwrap();

        assert_eq!(results, vec!["123"]);
    }

    #[test]
    fn test_string_search() {
        let index = SearchIndex::open(":memory:").unwrap();

        index
            .add_index("Patient", "123", "family", "string", Some("Doe"), None)
            .unwrap();

        index
            .add_index("Patient", "456", "family", "string", Some("Donovan"), None)
            .unwrap();

        // Prefix match search
        let results = index
            .search_string("Patient", "family", "yama", false)
            .unwrap();

        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_reference_search() {
        let index = SearchIndex::open(":memory:").unwrap();

        index
            .add_index("Observation", "o1", "subject", "reference", Some("Patient/123"), None)
            .unwrap();

        let results = index
            .search_reference("Observation", "subject", "Patient/123")
            .unwrap();

        assert_eq!(results, vec!["o1"]);
    }

    #[test]
    fn test_date_search() {
        let index = SearchIndex::open(":memory:").unwrap();

        index
            .add_index("Patient", "p1", "birthdate", "date", Some("1990-01-01"), None)
            .unwrap();
        index
            .add_index("Patient", "p2", "birthdate", "date", Some("2000-06-15"), None)
            .unwrap();

        let results = index
            .search_date_with_prefix("Patient", "birthdate", "ge", "1995-01-01")
            .unwrap();

        assert_eq!(results, vec!["p2"]);
    }
}
