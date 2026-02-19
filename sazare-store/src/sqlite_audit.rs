//! SQLite-based audit log
//!
//! Separate file for easy management and rotation.

use crate::error::Result;
use rusqlite::{params, Connection};
use std::path::Path;

/// Operation type
#[derive(Debug, Clone, Copy)]
pub enum Operation {
    Create,
    Read,
    Update,
    Delete,
    Search,
}

impl Operation {
    pub fn as_str(&self) -> &'static str {
        match self {
            Operation::Create => "create",
            Operation::Read => "read",
            Operation::Update => "update",
            Operation::Delete => "delete",
            Operation::Search => "search",
        }
    }
}

/// Audit log
pub struct AuditLog {
    conn: Connection,
}

#[allow(clippy::result_large_err)]
impl AuditLog {
    /// Open the audit log (create if not exists)
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        let audit = Self { conn };
        audit.initialize()?;
        Ok(audit)
    }

    /// Initialize tables
    fn initialize(&self) -> Result<()> {
        self.conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS audit_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL DEFAULT (datetime('now')),
                operation TEXT NOT NULL,
                resource_type TEXT,
                resource_id TEXT,
                version_id TEXT,
                query_string TEXT,
                user_id TEXT,
                client_ip TEXT,
                result TEXT NOT NULL,
                error_message TEXT
            )
            "#,
            [],
        )?;

        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_audit_timestamp ON audit_log(timestamp)",
            [],
        )?;

        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_audit_resource ON audit_log(resource_type, resource_id)",
            [],
        )?;

        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_audit_user ON audit_log(user_id)",
            [],
        )?;

        Ok(())
    }

    /// Record an audit log entry
    #[allow(clippy::too_many_arguments)]
    pub fn log(
        &self,
        operation: Operation,
        resource_type: Option<&str>,
        resource_id: Option<&str>,
        version_id: Option<&str>,
        query_string: Option<&str>,
        user_id: Option<&str>,
        client_ip: Option<&str>,
        success: bool,
        error_message: Option<&str>,
    ) -> Result<()> {
        let result = if success { "success" } else { "error" };

        self.conn.execute(
            r#"
            INSERT INTO audit_log
            (operation, resource_type, resource_id, version_id,
             query_string, user_id, client_ip, result, error_message)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            "#,
            params![
                operation.as_str(),
                resource_type,
                resource_id,
                version_id,
                query_string,
                user_id,
                client_ip,
                result,
                error_message,
            ],
        )?;

        Ok(())
    }

    /// Record a success log entry (helper)
    pub fn log_success(
        &self,
        operation: Operation,
        resource_type: &str,
        resource_id: &str,
        user_id: Option<&str>,
        client_ip: Option<&str>,
    ) -> Result<()> {
        self.log(
            operation,
            Some(resource_type),
            Some(resource_id),
            None,
            None,
            user_id,
            client_ip,
            true,
            None,
        )
    }

    /// Record an error log entry (helper)
    pub fn log_error(
        &self,
        operation: Operation,
        resource_type: Option<&str>,
        resource_id: Option<&str>,
        user_id: Option<&str>,
        client_ip: Option<&str>,
        error: &str,
    ) -> Result<()> {
        self.log(
            operation,
            resource_type,
            resource_id,
            None,
            None,
            user_id,
            client_ip,
            false,
            Some(error),
        )
    }

    /// Get recent audit log entries
    #[allow(clippy::type_complexity)]
    pub fn recent_entries(
        &self,
        limit: usize,
    ) -> Result<Vec<(String, String, Option<String>, Option<String>, String)>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT timestamp, operation, resource_type, resource_id, result
            FROM audit_log
            ORDER BY id DESC
            LIMIT ?1
            "#,
        )?;

        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;

        let mut entries = Vec::new();
        for row in rows {
            entries.push(row?);
        }

        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audit_log() {
        let audit = AuditLog::open(":memory:").unwrap();

        audit
            .log_success(
                Operation::Create,
                "Patient",
                "123",
                Some("admin"),
                Some("127.0.0.1"),
            )
            .unwrap();

        audit
            .log_error(
                Operation::Read,
                Some("Patient"),
                Some("999"),
                Some("user1"),
                Some("192.168.1.1"),
                "Resource not found",
            )
            .unwrap();

        // Verify logs were recorded
        let count: i32 = audit
            .conn
            .query_row("SELECT COUNT(*) FROM audit_log", [], |row| row.get(0))
            .unwrap();

        assert_eq!(count, 2);
    }

    #[test]
    fn test_recent_entries() {
        let audit = AuditLog::open(":memory:").unwrap();

        audit.log_success(Operation::Create, "Patient", "p1", None, None).unwrap();
        audit.log_success(Operation::Read, "Patient", "p1", None, None).unwrap();
        audit.log_error(Operation::Update, Some("Patient"), Some("p2"), None, None, "not found").unwrap();

        let entries = audit.recent_entries(10).unwrap();
        assert_eq!(entries.len(), 3);
        // Most recent first
        assert_eq!(entries[0].1, "update");
        assert_eq!(entries[0].4, "error");
    }
}
