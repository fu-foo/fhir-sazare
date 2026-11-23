//! Online backup of a SQLite database file.

use crate::error::Result;
use rusqlite::{Connection, OpenFlags};
use std::path::Path;

/// Write a consistent, compacted copy of the database at `src` to `dest`
/// using `VACUUM INTO`.
///
/// Safe while the server is running: the copy is taken inside a single read
/// transaction, so it reflects one committed state (WAL contents included)
/// and doesn't block writers. A plain file copy of a WAL-mode database can
/// miss the `-wal` file or tear mid-write. `dest` must not exist.
pub fn backup_database(src: &Path, dest: &Path) -> Result<()> {
    let conn = Connection::open_with_flags(src, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.busy_timeout(std::time::Duration::from_secs(30))?;
    let dest = dest.to_str().ok_or_else(|| {
        rusqlite::Error::InvalidPath(dest.to_path_buf())
    })?;
    conn.execute("VACUUM INTO ?1", [dest])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SqliteStore;

    #[test]
    fn backup_captures_committed_wal_state() {
        let dir = std::env::temp_dir().join(format!("sazare-backup-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("resources.sqlite");
        let dest = dir.join("backup.sqlite");
        let _ = std::fs::remove_file(&dest);

        // Keep the store open so the write still sits in the WAL file.
        let store = SqliteStore::open(&src).unwrap();
        store.put("Patient", "p1", br#"{"resourceType":"Patient","id":"p1"}"#).unwrap();

        backup_database(&src, &dest).unwrap();
        // Refuses to overwrite an existing backup.
        assert!(backup_database(&src, &dest).is_err());

        let restored = SqliteStore::open(&dest).unwrap();
        assert!(restored.get("Patient", "p1").unwrap().is_some());

        drop((store, restored));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
