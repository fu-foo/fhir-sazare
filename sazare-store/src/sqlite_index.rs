//! SQLite-based search index
//!
//! Single file with tables per resource type for performance.

use crate::error::Result;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use rusqlite::{params, Connection};
use std::path::Path;

/// Parse a FHIR date/dateTime string into a half-open epoch-**microsecond**
/// range `[start, end)` that reflects the value's precision: a year spans a
/// year, a date a day, a whole-second dateTime one second, and a dateTime with
/// fractional seconds one microsecond (effectively exact, so e.g. `_lastUpdated`
/// instants that differ only in sub-second digits do not collide). Returns
/// `None` if the value cannot be parsed.
pub(crate) fn fhir_date_range(value: &str) -> Option<(i64, i64)> {
    let v = value.trim();
    if v.contains('T') {
        // dateTime — require a timezone per FHIR; try RFC3339, then assume UTC.
        // Minute precision (`YYYY-MM-DDThh:mm`, no seconds) is a valid FHIR
        // dateTime and spans one minute.
        if let Ok(dt) = DateTime::parse_from_rfc3339(v) {
            let start = dt.with_timezone(&Utc).timestamp_micros();
            let span = if v.contains('.') { 1 } else { 1_000_000 };
            return Some((start, start + span));
        }
        if let Ok(n) = chrono::NaiveDateTime::parse_from_str(v, "%Y-%m-%dT%H:%M:%S") {
            let start = Utc.from_utc_datetime(&n).timestamp_micros();
            return Some((start, start + 1_000_000));
        }
        // Minute precision, with or without a trailing `Z` (offset forms at
        // minute precision are uncommon and left unparsed).
        let naive = v.trim_end_matches('Z');
        if let Ok(n) = chrono::NaiveDateTime::parse_from_str(naive, "%Y-%m-%dT%H:%M") {
            let start = Utc.from_utc_datetime(&n).timestamp_micros();
            return Some((start, start + 60_000_000));
        }
        return None;
    }
    match v.len() {
        4 => {
            let year: i32 = v.parse().ok()?;
            let start = Utc.with_ymd_and_hms(year, 1, 1, 0, 0, 0).single()?;
            let end = Utc.with_ymd_and_hms(year + 1, 1, 1, 0, 0, 0).single()?;
            Some((start.timestamp_micros(), end.timestamp_micros()))
        }
        7 => {
            let year: i32 = v[0..4].parse().ok()?;
            let month: u32 = v[5..7].parse().ok()?;
            let start = Utc.with_ymd_and_hms(year, month, 1, 0, 0, 0).single()?;
            let (ny, nm) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
            let end = Utc.with_ymd_and_hms(ny, nm, 1, 0, 0, 0).single()?;
            Some((start.timestamp_micros(), end.timestamp_micros()))
        }
        10 => {
            let d = NaiveDate::parse_from_str(v, "%Y-%m-%d").ok()?;
            let start = Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0)?);
            let end = start + chrono::Duration::days(1);
            Some((start.timestamp_micros(), end.timestamp_micros()))
        }
        _ => None,
    }
}

/// String search matching mode.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StringMatch {
    /// Default FHIR string search: case-insensitive prefix match.
    Prefix,
    /// `:exact` — exact (case-insensitive on the stored lowercased value) match.
    Exact,
    /// `:contains` — case-insensitive substring match.
    Contains,
}

/// Escape SQL LIKE metacharacters (`%`, `_`, and the `\` escape char itself) in
/// a user-supplied value so they are matched literally under `ESCAPE '\'`.
fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// SQLite-backed search index
pub struct SearchIndex {
    conn: Connection,
}

#[allow(clippy::result_large_err)]
impl SearchIndex {
    /// Ordered schema migrations (see `crate::migrate`). Append-only.
    const MIGRATIONS: &'static [&'static str] = &[
        // v1 — initial schema.
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
        );
        CREATE INDEX IF NOT EXISTS idx_type_param_string
            ON search_index(resource_type, param_name, value_string);
        CREATE INDEX IF NOT EXISTS idx_type_param_token
            ON search_index(resource_type, param_name, value_system, value_string);
        CREATE INDEX IF NOT EXISTS idx_type_param_date
            ON search_index(resource_type, param_name, value_date_start, value_date_end);
        CREATE INDEX IF NOT EXISTS idx_resource
            ON search_index(resource_type, resource_id);
        "#,
    ];

    /// Open the index (create if not exists)
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let mut conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        crate::migrate::run_migrations(&mut conn, Self::MIGRATIONS)?;
        Ok(Self { conn })
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

        // For date params, derive a [start, end) epoch-second range so searches
        // can apply FHIR range semantics. A Period is encoded by the extractor as
        // "start/end"; a plain date/dateTime spans a single precision window.
        let (date_start, date_end): (Option<i64>, Option<i64>) = if param_type == "date" {
            match value_string {
                Some(s) => {
                    if let Some((lo, hi)) = s.split_once('/') {
                        let start = fhir_date_range(lo).map(|(a, _)| a);
                        let end = if hi.is_empty() {
                            Some(i64::MAX)
                        } else {
                            fhir_date_range(hi).map(|(_, b)| b)
                        };
                        (start, end)
                    } else {
                        match fhir_date_range(s) {
                            Some((a, b)) => (Some(a), Some(b)),
                            None => (None, None),
                        }
                    }
                }
                None => (None, None),
            }
        } else {
            (None, None)
        };

        self.conn.execute(
            r#"
            INSERT OR REPLACE INTO search_index
            (resource_type, resource_id, param_name, param_type,
             value_string, value_string_lower, value_system,
             value_date_start, value_date_end)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            "#,
            params![
                resource_type,
                resource_id,
                param_name,
                param_type,
                value_string,
                value_string_lower,
                value_system,
                date_start,
                date_end,
            ],
        )?;

        Ok(())
    }

    /// Count total entries in the search index (for reindex decisions)
    pub fn row_count(&self) -> Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM search_index",
            [],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    /// Drop all entries from the search index
    pub fn clear_all(&self) -> Result<()> {
        self.conn.execute("DELETE FROM search_index", [])?;
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

    /// String search (name, etc.) honoring the `:exact` / `:contains` modifiers.
    pub fn search_string(
        &self,
        resource_type: &str,
        param_name: &str,
        value: &str,
        mode: StringMatch,
    ) -> Result<Vec<String>> {
        let lower = value.to_lowercase();
        let (sql, bound): (&str, String) = match mode {
            StringMatch::Exact => (
                "SELECT DISTINCT resource_id FROM search_index \
                 WHERE resource_type = ?1 AND param_name = ?2 AND value_string_lower = ?3",
                lower,
            ),
            StringMatch::Prefix => (
                "SELECT DISTINCT resource_id FROM search_index \
                 WHERE resource_type = ?1 AND param_name = ?2 \
                   AND value_string_lower LIKE ?3 ESCAPE '\\'",
                format!("{}%", escape_like(&lower)),
            ),
            StringMatch::Contains => (
                "SELECT DISTINCT resource_id FROM search_index \
                 WHERE resource_type = ?1 AND param_name = ?2 \
                   AND value_string_lower LIKE ?3 ESCAPE '\\'",
                format!("%{}%", escape_like(&lower)),
            ),
        };

        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![resource_type, param_name, bound], |row| row.get(0))?;

        let mut ids = Vec::new();
        for row in rows {
            ids.push(row?);
        }
        Ok(ids)
    }

    /// Token search with NO system (`|code`): the resource's value must carry
    /// the code and have been indexed without a system.
    pub fn search_token_no_system(
        &self,
        resource_type: &str,
        param_name: &str,
        code: &str,
    ) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT resource_id FROM search_index \
             WHERE resource_type = ?1 AND param_name = ?2 \
               AND value_string = ?3 AND value_system IS NULL",
        )?;
        let rows = stmt.query_map(params![resource_type, param_name, code], |row| row.get(0))?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row?);
        }
        Ok(ids)
    }

    /// Token search by system only (`system|`): any code within the system.
    pub fn search_token_system_only(
        &self,
        resource_type: &str,
        param_name: &str,
        system: &str,
    ) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT resource_id FROM search_index \
             WHERE resource_type = ?1 AND param_name = ?2 AND value_system = ?3",
        )?;
        let rows = stmt.query_map(params![resource_type, param_name, system], |row| row.get(0))?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row?);
        }
        Ok(ids)
    }

    /// Distinct resource ids that have at least one index entry for `param_name`
    /// (used to implement the `:missing` and `:not` modifiers).
    pub fn ids_with_param(&self, resource_type: &str, param_name: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT resource_id FROM search_index \
             WHERE resource_type = ?1 AND param_name = ?2",
        )?;
        let rows = stmt.query_map(params![resource_type, param_name], |row| row.get(0))?;
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

    /// Date search (with prefix: eq, ne, ge, le, gt, lt).
    ///
    /// Indexed values are half-open epoch ranges `[start, end)` reflecting their
    /// precision (and the full span of a Period). The query value is likewise
    /// expanded to a range `[qs, qe)`, then compared per FHIR date semantics so
    /// that, e.g., an instant `eq` search does not match a wider Period.
    pub fn search_date_with_prefix(
        &self,
        resource_type: &str,
        param_name: &str,
        prefix: &str,
        value: &str,
    ) -> Result<Vec<String>> {
        let Some((qs, qe)) = fhir_date_range(value) else {
            return Ok(Vec::new());
        };

        // Each arm compares the resource range [start, end) to the query [qs, qe).
        // qs is bound to ?3 and qe to ?4; the trailing `?n = ?n` tautologies keep
        // both placeholders referenced so the bound parameter count always matches.
        let cond = match prefix {
            "eq" => "value_date_start >= ?3 AND value_date_end <= ?4",
            "ne" => "(value_date_start < ?3 OR value_date_end > ?4)",
            "gt" => "value_date_end > ?4 AND ?3 = ?3",
            "ge" => "value_date_end > ?3 AND ?4 = ?4",
            "lt" => "value_date_start < ?3 AND ?4 = ?4",
            "le" => "value_date_start < ?4 AND ?3 = ?3",
            // `sa` (starts after): the resource range begins at/after the query
            // range end. `eb` (ends before): the resource range ends at/before
            // the query range start.
            "sa" => "value_date_start >= ?4",
            "eb" => "value_date_end <= ?3 AND ?4 = ?4",
            // `ap` (approximately): any overlap with the query range.
            "ap" => "value_date_start < ?4 AND value_date_end > ?3",
            _ => "value_date_start >= ?3 AND value_date_end <= ?4",
        };
        let query = format!(
            "SELECT DISTINCT resource_id FROM search_index
             WHERE resource_type = ?1 AND param_name = ?2
               AND value_date_start IS NOT NULL AND ({cond})"
        );
        let mut stmt = self.conn.prepare(&query)?;
        let rows = stmt.query_map(params![resource_type, param_name, qs, qe], |row| {
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
            .search_string("Patient", "family", "do", StringMatch::Prefix)
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

    #[test]
    fn test_date_range_period_vs_instant() {
        let index = SearchIndex::open(":memory:").unwrap();
        // A point dateTime Observation and a Period-valued Observation sharing a start.
        index
            .add_index("Observation", "point", "date", "date", Some("2025-12-01T09:15:00-05:00"), None)
            .unwrap();
        index
            .add_index(
                "Observation",
                "period",
                "date",
                "date",
                Some("2025-12-01T09:15:00-05:00/2025-12-01T09:30:00-05:00"),
                None,
            )
            .unwrap();

        // eq on the instant matches the point but NOT the wider Period.
        let eq = index
            .search_date_with_prefix("Observation", "date", "eq", "2025-12-01T09:15:00-05:00")
            .unwrap();
        assert_eq!(eq, vec!["point"]);

        // Comparators that the avg-bp Inferno test uses must return the Period.
        for (prefix, val) in [
            ("gt", "2025-11-30T09:15:00-05:00"),
            ("ge", "2025-11-30T09:15:00-05:00"),
            ("lt", "2025-12-02T09:15:00-05:00"),
            ("le", "2025-12-02T09:15:00-05:00"),
        ] {
            let ids = index
                .search_date_with_prefix("Observation", "date", prefix, val)
                .unwrap();
            assert!(ids.contains(&"period".to_string()), "{prefix} should match the Period");
        }
    }

    #[test]
    fn test_token_system_edge_forms() {
        let index = SearchIndex::open(":memory:").unwrap();
        index.add_index("Obs", "withsys", "code", "token", Some("1234-5"), Some("http://loinc.org")).unwrap();
        index.add_index("Obs", "nosys", "code", "token", Some("1234-5"), None).unwrap();

        // `|code` → only the entry indexed without a system.
        assert_eq!(index.search_token_no_system("Obs", "code", "1234-5").unwrap(), vec!["nosys"]);
        // `system|` → any code within the system.
        assert_eq!(index.search_token_system_only("Obs", "code", "http://loinc.org").unwrap(), vec!["withsys"]);
        // bare code → both (any system).
        assert_eq!(index.search_token("Obs", "code", None, "1234-5").unwrap().len(), 2);
    }

    #[test]
    fn test_string_contains_and_like_escaping() {
        let index = SearchIndex::open(":memory:").unwrap();
        index.add_index("Patient", "p1", "name", "string", Some("Yamada"), None).unwrap();
        index.add_index("Patient", "p2", "name", "string", Some("50%off"), None).unwrap();

        // :contains substring
        assert_eq!(index.search_string("Patient", "name", "mad", StringMatch::Contains).unwrap(), vec!["p1"]);
        // A literal % must not act as a wildcard.
        let r = index.search_string("Patient", "name", "50%", StringMatch::Prefix).unwrap();
        assert_eq!(r, vec!["p2"]);
        // and a non-matching literal % search returns nothing (no wildcard blowup)
        assert!(index.search_string("Patient", "name", "z%", StringMatch::Prefix).unwrap().is_empty());
    }

    #[test]
    fn test_date_sa_eb_prefixes() {
        let index = SearchIndex::open(":memory:").unwrap();
        index.add_index("Obs", "early", "date", "date", Some("2020-01-01"), None).unwrap();
        index.add_index("Obs", "late", "date", "date", Some("2025-01-01"), None).unwrap();

        // sa (starts after) the whole of 2022
        assert_eq!(index.search_date_with_prefix("Obs", "date", "sa", "2022").unwrap(), vec!["late"]);
        // eb (ends before) the whole of 2022
        assert_eq!(index.search_date_with_prefix("Obs", "date", "eb", "2022").unwrap(), vec!["early"]);
    }

    #[test]
    fn test_minute_precision_datetime_parses() {
        // `YYYY-MM-DDThh:mm` is a valid FHIR dateTime (one-minute window),
        // with or without a trailing Z.
        for v in ["2024-03-04T09:30Z", "2024-03-04T09:30"] {
            let r = fhir_date_range(v);
            assert!(r.is_some(), "minute-precision dateTime {v} must parse");
            let (s, e) = r.unwrap();
            assert_eq!(e - s, 60_000_000, "{v} should span one minute");
        }
    }

    #[test]
    fn test_date_subsecond_eq_does_not_collide() {
        // Two instants in the same second but different microseconds (e.g. two
        // resources' _lastUpdated) must not match each other under eq.
        let index = SearchIndex::open(":memory:").unwrap();
        index
            .add_index("Observation", "a", "_lastUpdated", "date", Some("2026-05-30T09:49:12.133115+00:00"), None)
            .unwrap();
        index
            .add_index("Observation", "b", "_lastUpdated", "date", Some("2026-05-30T09:49:12.133164+00:00"), None)
            .unwrap();

        let eq = index
            .search_date_with_prefix("Observation", "_lastUpdated", "eq", "2026-05-30T09:49:12.133164+00:00")
            .unwrap();
        assert_eq!(eq, vec!["b"]);
    }
}
