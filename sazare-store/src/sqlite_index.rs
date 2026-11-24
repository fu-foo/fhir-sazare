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

/// `(resource_id, numeric key, text key)` — see [`SearchIndex::sort_values`].
pub type SortValue = (String, Option<f64>, Option<String>);

/// Run a `SELECT resource_id ...` statement and return the ids de-duplicated,
/// in first-seen order.
///
/// The search queries deliberately omit SQL `DISTINCT`: with it, SQLite's
/// planner prefers `idx_resource (resource_type, resource_id)` to get rows in
/// id order for free, which degenerates into a scan of every index row of the
/// resource type (tens of ms per lookup at ~50k resources) instead of seeking
/// the `(resource_type, param_name, value...)` indexes. De-duplicating here
/// keeps the plan on the selective index without depending on ANALYZE stats.
fn collect_distinct_ids<P: rusqlite::Params>(
    stmt: &mut rusqlite::Statement<'_>,
    params: P,
) -> Result<Vec<String>> {
    let mut seen = std::collections::HashSet::new();
    let mut ids = Vec::new();
    let rows = stmt.query_map(params, |row| row.get::<_, String>(0))?;
    for row in rows {
        let id = row?;
        if seen.insert(id.clone()) {
            ids.push(id);
        }
    }
    Ok(ids)
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
        // v2 — covering lookup indexes. Every search selects `resource_id`; the
        // v1 indexes lacked it, so without ANALYZE stats the planner preferred
        // the covering UNIQUE autoindex and scanned all rows of the resource
        // type. Ending each index with `resource_id` makes the selective index
        // also covering. `idx_sp_lower` serves string (prefix/exact) searches,
        // which match on `value_string_lower` and had no index at all.
        r#"
        DROP INDEX IF EXISTS idx_type_param_string;
        DROP INDEX IF EXISTS idx_type_param_token;
        DROP INDEX IF EXISTS idx_type_param_date;
        CREATE INDEX IF NOT EXISTS idx_sp_string
            ON search_index(resource_type, param_name, value_string, resource_id);
        CREATE INDEX IF NOT EXISTS idx_sp_token
            ON search_index(resource_type, param_name, value_system, value_string, resource_id);
        CREATE INDEX IF NOT EXISTS idx_sp_lower
            ON search_index(resource_type, param_name, value_string_lower, resource_id);
        CREATE INDEX IF NOT EXISTS idx_sp_date
            ON search_index(resource_type, param_name, value_date_start, value_date_end, resource_id);
        "#,
        // v3 — number search parameters (custom SearchParameters of type
        // `number`; they were never indexed before, so nothing to backfill).
        r#"
        ALTER TABLE search_index ADD COLUMN value_number REAL;
        CREATE INDEX IF NOT EXISTS idx_sp_number
            ON search_index(resource_type, param_name, value_number, resource_id);
        "#,
    ];

    /// Open the index (create if not exists)
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let mut conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        crate::migrate::run_migrations(&mut conn, Self::MIGRATIONS)?;
        // Refresh planner statistics where they are missing or stale. Bounded by
        // analysis_limit so startup stays fast on large indexes.
        conn.execute_batch("PRAGMA analysis_limit=1000; PRAGMA optimize=0x10002;")?;
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
        let number: Option<f64> = if param_type == "number" {
            value_string.and_then(|s| s.parse::<f64>().ok()).filter(|n| n.is_finite())
        } else {
            None
        };

        self.conn.execute(
            r#"
            INSERT OR REPLACE INTO search_index
            (resource_type, resource_id, param_name, param_type,
             value_string, value_string_lower, value_system,
             value_date_start, value_date_end, value_number)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
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
                number,
            ],
        )?;

        Ok(())
    }

    /// Replace every index entry of one resource with `entries`
    /// (`(param_name, param_type, value, system)`, as produced by
    /// `IndexBuilder`) in a single transaction.
    ///
    /// One commit per resource instead of one per entry: each autocommitted
    /// statement is its own WAL commit, which dominated write/import cost.
    pub fn replace_resource_index(
        &self,
        resource_type: &str,
        resource_id: &str,
        entries: &[(String, String, String, Option<String>)],
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.remove_index(resource_type, resource_id)?;
        for (param_name, param_type, value, system) in entries {
            self.add_index(
                resource_type,
                resource_id,
                param_name,
                param_type,
                Some(value),
                system.as_deref(),
            )?;
        }
        tx.commit()?;
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
        if let Some(sys) = system {
            let mut stmt = self.conn.prepare(
                r#"
                SELECT resource_id FROM search_index
                WHERE resource_type = ?1
                  AND param_name = ?2
                  AND value_system = ?3
                  AND value_string = ?4
                "#,
            )?;
            collect_distinct_ids(&mut stmt, params![resource_type, param_name, sys, code])
        } else {
            let mut stmt = self.conn.prepare(
                r#"
                SELECT resource_id FROM search_index
                WHERE resource_type = ?1
                  AND param_name = ?2
                  AND value_string = ?3
                "#,
            )?;
            collect_distinct_ids(&mut stmt, params![resource_type, param_name, code])
        }
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
        if mode == StringMatch::Prefix {
            // A LIKE 'abc%' pattern can't seek an index (SQLite's LIKE
            // optimization needs case_sensitive_like or a NOCASE column), so
            // bound the scan with the equivalent range [abc, abc\u{10FFFF}) on
            // idx_sp_lower and keep the LIKE as an exact residual filter.
            let mut stmt = self.conn.prepare(
                "SELECT resource_id FROM search_index \
                 WHERE resource_type = ?1 AND param_name = ?2 \
                   AND value_string_lower >= ?3 AND value_string_lower < ?4 \
                   AND value_string_lower LIKE ?5 ESCAPE '\\'",
            )?;
            let upper = format!("{lower}\u{10FFFF}");
            let pattern = format!("{}%", escape_like(&lower));
            return collect_distinct_ids(
                &mut stmt,
                params![resource_type, param_name, lower, upper, pattern],
            );
        }
        let (sql, bound): (&str, String) = match mode {
            StringMatch::Exact => (
                "SELECT resource_id FROM search_index \
                 WHERE resource_type = ?1 AND param_name = ?2 AND value_string_lower = ?3",
                lower,
            ),
            StringMatch::Prefix => unreachable!("handled above"),
            StringMatch::Contains => (
                "SELECT resource_id FROM search_index \
                 WHERE resource_type = ?1 AND param_name = ?2 \
                   AND value_string_lower LIKE ?3 ESCAPE '\\'",
                format!("%{}%", escape_like(&lower)),
            ),
        };

        let mut stmt = self.conn.prepare(sql)?;
        collect_distinct_ids(&mut stmt, params![resource_type, param_name, bound])
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
            "SELECT resource_id FROM search_index \
             WHERE resource_type = ?1 AND param_name = ?2 \
               AND value_string = ?3 AND value_system IS NULL",
        )?;
        collect_distinct_ids(&mut stmt, params![resource_type, param_name, code])
    }

    /// Token search by system only (`system|`): any code within the system.
    pub fn search_token_system_only(
        &self,
        resource_type: &str,
        param_name: &str,
        system: &str,
    ) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT resource_id FROM search_index \
             WHERE resource_type = ?1 AND param_name = ?2 AND value_system = ?3",
        )?;
        collect_distinct_ids(&mut stmt, params![resource_type, param_name, system])
    }

    /// Distinct resource ids that have at least one index entry for `param_name`
    /// (used to implement the `:missing` and `:not` modifiers).
    pub fn ids_with_param(&self, resource_type: &str, param_name: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT resource_id FROM search_index \
             WHERE resource_type = ?1 AND param_name = ?2",
        )?;
        collect_distinct_ids(&mut stmt, params![resource_type, param_name])
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
            SELECT resource_id FROM search_index
            WHERE resource_type = ?1
              AND param_name = ?2
              AND value_string = ?3
              AND param_type = 'reference'
            "#,
        )?;
        collect_distinct_ids(&mut stmt, params![resource_type, param_name, reference])
    }

    /// Reference search against many targets at once: ids of `resource_type`
    /// resources whose `param_name` references any of `references`
    /// (e.g. `["Patient/p1", "Patient/p2"]`), de-duplicated. Used by chained
    /// search and `_revinclude`, which would otherwise issue one query per
    /// target.
    pub fn search_reference_any(
        &self,
        resource_type: &str,
        param_name: &str,
        references: &[String],
    ) -> Result<Vec<String>> {
        let mut seen = std::collections::HashSet::new();
        let mut ids = Vec::new();
        // Chunk the IN list to stay clear of SQLite's bound-variable limit.
        for chunk in references.chunks(500) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT resource_id FROM search_index \
                 WHERE resource_type = ? AND param_name = ? \
                 AND value_string IN ({placeholders}) AND param_type = 'reference'"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let mut args: Vec<&str> = Vec::with_capacity(chunk.len() + 2);
            args.push(resource_type);
            args.push(param_name);
            args.extend(chunk.iter().map(|s| s.as_str()));
            for id in collect_distinct_ids(&mut stmt, rusqlite::params_from_iter(args))? {
                if seen.insert(id.clone()) {
                    ids.push(id);
                }
            }
        }
        Ok(ids)
    }

    /// Resolve the references held by a set of source resources — the inverse of
    /// [`search_reference`]. Given source resources of `source_type` and their
    /// `param_name` reference parameter, return the ids of every referenced
    /// resource of `target_type`. Used to evaluate `_has` reverse chains.
    ///
    /// References are stored as `value_string` like `"Patient/123"`, so we match
    /// the `"{target_type}/"` prefix and strip it to recover the bare id.
    pub fn referenced_targets(
        &self,
        source_type: &str,
        param_name: &str,
        source_ids: &[String],
        target_type: &str,
    ) -> Result<Vec<String>> {
        if source_ids.is_empty() {
            return Ok(Vec::new());
        }
        let prefix = format!("{target_type}/");
        let mut out: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

        // Chunk the IN list to stay clear of SQLite's bound-variable limit.
        for chunk in source_ids.chunks(500) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            // No SQL DISTINCT (deduped via `seen`): see `collect_distinct_ids`.
            let sql = format!(
                "SELECT value_string FROM search_index \
                 WHERE resource_type = ? AND param_name = ? AND param_type = 'reference' \
                 AND resource_id IN ({placeholders})"
            );
            let mut stmt = self.conn.prepare(&sql)?;

            let mut args: Vec<&str> = Vec::with_capacity(chunk.len() + 2);
            args.push(source_type);
            args.push(param_name);
            args.extend(chunk.iter().map(|s| s.as_str()));

            let rows = stmt.query_map(rusqlite::params_from_iter(args), |row| {
                row.get::<_, String>(0)
            })?;
            for row in rows {
                let value = row?;
                if let Some(id) = value.strip_prefix(&prefix)
                    && seen.insert(id.to_string())
                {
                    out.push(id.to_string());
                }
            }
        }
        Ok(out)
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
            "SELECT resource_id FROM search_index
             WHERE resource_type = ?1 AND param_name = ?2
               AND value_date_start IS NOT NULL AND ({cond})"
        );
        let mut stmt = self.conn.prepare(&query)?;
        collect_distinct_ids(&mut stmt, params![resource_type, param_name, qs, qe])
    }

    /// Every indexed value of one parameter across a resource type, shaped as
    /// a sort key: `(resource_id, numeric key, text key)`. Dates sort by range
    /// start (epoch seconds), numbers by value, everything else by the
    /// lower-cased string. A resource may appear several times.
    pub fn sort_values(
        &self,
        resource_type: &str,
        param_name: &str,
    ) -> Result<Vec<SortValue>> {
        let mut stmt = self.conn.prepare(
            "SELECT resource_id, param_type, value_date_start, value_number, value_string_lower
             FROM search_index WHERE resource_type = ?1 AND param_name = ?2",
        )?;
        let rows = stmt.query_map(params![resource_type, param_name], |row| {
            let id: String = row.get(0)?;
            let ty: String = row.get(1)?;
            let date: Option<i64> = row.get(2)?;
            let number: Option<f64> = row.get(3)?;
            let text: Option<String> = row.get(4)?;
            Ok(match ty.as_str() {
                "date" => (id, date.map(|d| d as f64), None),
                "number" => (id, number, None),
                _ => (id, None, text),
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Number search with FHIR prefixes. `eq` (and `ne`) honour the query's
    /// precision: `100` matches [99.5, 100.5), `100.00` matches
    /// [99.995, 100.005). `ap` is ±10% of the value. `sa`/`eb` behave like
    /// `gt`/`lt` for a single number.
    pub fn search_number_with_prefix(
        &self,
        resource_type: &str,
        param_name: &str,
        prefix: &str,
        value: &str,
    ) -> Result<Vec<String>> {
        let Some((v, half)) = number_with_precision(value) else {
            return Ok(Vec::new());
        };
        let (lo, hi, cond) = match prefix {
            "ne" => (v - half, v + half, "(value_number < ?3 OR value_number >= ?4)"),
            "gt" | "sa" => (v, v, "value_number > ?3 AND ?4 = ?4"),
            "ge" => (v, v, "value_number >= ?3 AND ?4 = ?4"),
            "lt" | "eb" => (v, v, "value_number < ?3 AND ?4 = ?4"),
            "le" => (v, v, "value_number <= ?3 AND ?4 = ?4"),
            "ap" => {
                let d = (v.abs() * 0.1).max(half);
                (v - d, v + d, "value_number >= ?3 AND value_number <= ?4")
            }
            _ => (v - half, v + half, "value_number >= ?3 AND value_number < ?4"),
        };
        let query = format!(
            "SELECT resource_id FROM search_index
             WHERE resource_type = ?1 AND param_name = ?2
               AND value_number IS NOT NULL AND ({cond})"
        );
        let mut stmt = self.conn.prepare(&query)?;
        collect_distinct_ids(&mut stmt, params![resource_type, param_name, lo, hi])
    }
}

/// Parse a FHIR number search value into (value, half of its precision step):
/// `100` → (100, 0.5), `100.25` → (100.25, 0.005), `1e2` → (100, 50).
fn number_with_precision(value: &str) -> Option<(f64, f64)> {
    let v: f64 = value.parse().ok().filter(|n: &f64| n.is_finite())?;
    let lower = value.to_ascii_lowercase();
    let (mantissa, exp) = match lower.split_once('e') {
        Some((m, e)) => (m, e.parse::<i32>().ok()?),
        None => (lower.as_str(), 0),
    };
    let decimals = mantissa.split_once('.').map_or(0, |(_, frac)| frac.len() as i32);
    Some((v, 0.5 * 10f64.powi(exp - decimals)))
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
    fn test_referenced_targets() {
        let index = SearchIndex::open(":memory:").unwrap();
        // Two observations point at Patient/123, one at Patient/456, one elsewhere.
        index.add_index("Observation", "o1", "patient", "reference", Some("Patient/123"), None).unwrap();
        index.add_index("Observation", "o2", "patient", "reference", Some("Patient/123"), None).unwrap();
        index.add_index("Observation", "o3", "patient", "reference", Some("Patient/456"), None).unwrap();
        index.add_index("Observation", "o4", "patient", "reference", Some("Group/g1"), None).unwrap();

        // Resolve the patients referenced by o1, o3, o4 — deduped, Patient-only.
        let sources = vec!["o1".to_string(), "o3".to_string(), "o4".to_string()];
        let mut targets = index
            .referenced_targets("Observation", "patient", &sources, "Patient")
            .unwrap();
        targets.sort();
        assert_eq!(targets, vec!["123".to_string(), "456".to_string()]);

        // A source set that references nothing of the target type yields nothing.
        let empty = index
            .referenced_targets("Observation", "patient", &["o4".to_string()], "Patient")
            .unwrap();
        assert!(empty.is_empty());
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

    #[test]
    fn test_string_prefix_is_case_insensitive_and_literal() {
        let index = SearchIndex::open(":memory:").unwrap();
        for (id, v) in [("1", "Doe"), ("2", "DOE-Smith"), ("3", "Do_e"), ("4", "Dzoe")] {
            index.add_index("Patient", id, "family", "string", Some(v), None).unwrap();
        }
        let mut r = index.search_string("Patient", "family", "dO", StringMatch::Prefix).unwrap();
        r.sort();
        assert_eq!(r, vec!["1", "2", "3"]);
        // `_` is literal, not a LIKE wildcard.
        let r = index.search_string("Patient", "family", "do_", StringMatch::Prefix).unwrap();
        assert_eq!(r, vec!["3"]);
    }

    #[test]
    fn test_results_are_deduplicated() {
        let index = SearchIndex::open(":memory:").unwrap();
        // Two given names on one resource → two index rows, one result.
        index.add_index("Patient", "1", "given", "string", Some("Anna"), None).unwrap();
        index.add_index("Patient", "1", "given", "string", Some("Annabel"), None).unwrap();
        let r = index.search_string("Patient", "given", "ann", StringMatch::Prefix).unwrap();
        assert_eq!(r, vec!["1"]);
    }

    #[test]
    fn test_number_search_prefixes_and_precision() {
        let index = SearchIndex::open(":memory:").unwrap();
        for (id, v) in [("a", "0.8"), ("b", "100"), ("c", "100.4"), ("d", "105"), ("e", "-3")] {
            index.add_index("RiskAssessment", id, "probability", "number", Some(v), None).unwrap();
        }
        let q = |prefix: &str, v: &str| {
            let mut ids = index.search_number_with_prefix("RiskAssessment", "probability", prefix, v).unwrap();
            ids.sort();
            ids
        };
        // eq honours the query's precision.
        assert_eq!(q("eq", "100"), vec!["b", "c"]);
        assert_eq!(q("eq", "100.0"), vec!["b"]);
        assert_eq!(q("eq", "0.80"), vec!["a"]);
        assert_eq!(q("ne", "100"), vec!["a", "d", "e"]);
        assert_eq!(q("gt", "100"), vec!["c", "d"]);
        assert_eq!(q("ge", "100"), vec!["b", "c", "d"]);
        assert_eq!(q("lt", "0"), vec!["e"]);
        assert_eq!(q("le", "0.8"), vec!["a", "e"]);
        assert_eq!(q("ap", "100"), vec!["b", "c", "d"]);
        assert_eq!(q("eq", "1e2"), vec!["b", "c", "d"]);
        assert!(q("eq", "abc").is_empty());
    }

    #[test]
    fn test_number_with_precision() {
        assert_eq!(number_with_precision("100"), Some((100.0, 0.5)));
        assert_eq!(number_with_precision("-2.50"), Some((-2.5, 0.005)));
        assert_eq!(number_with_precision("1e2"), Some((100.0, 50.0)));
        assert_eq!(number_with_precision("NaN"), None);
    }

    #[test]
    fn test_replace_resource_index() {
        let index = SearchIndex::open(":memory:").unwrap();
        let entry = |v: &str| ("status".to_string(), "token".to_string(), v.to_string(), None);
        index.replace_resource_index("Observation", "o1", &[entry("preliminary")]).unwrap();
        index.replace_resource_index("Observation", "o1", &[entry("final")]).unwrap();
        assert!(index.search_token("Observation", "status", None, "preliminary").unwrap().is_empty());
        assert_eq!(index.search_token("Observation", "status", None, "final").unwrap(), vec!["o1"]);
    }

    #[test]
    fn test_search_reference_any() {
        let index = SearchIndex::open(":memory:").unwrap();
        for (id, r) in [("o1", "Patient/a"), ("o2", "Patient/b"), ("o3", "Patient/c")] {
            index.add_index("Observation", id, "subject", "reference", Some(r), None).unwrap();
        }
        let refs: Vec<String> = ["Patient/a", "Patient/c", "Patient/zz"].map(String::from).to_vec();
        let mut r = index.search_reference_any("Observation", "subject", &refs).unwrap();
        r.sort();
        assert_eq!(r, vec!["o1", "o3"]);
    }

    /// Without ANALYZE stats (a fresh database), each lookup must still seek
    /// its selective covering index rather than scan every row of the resource
    /// type via idx_resource / the UNIQUE autoindex — the regression that made
    /// single-patient lookups cost tens of ms at ~50k resources.
    #[test]
    fn test_lookups_use_selective_index_without_stats() {
        let index = SearchIndex::open(":memory:").unwrap();
        let plan = |sql: &str| -> String {
            let mut stmt = index.conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(3))
                .unwrap()
                .map(|r| r.unwrap())
                .collect::<Vec<_>>();
            rows.join(" | ")
        };
        let cases = [
            ("idx_sp_string", "SELECT resource_id FROM search_index WHERE resource_type='Observation' AND param_name='subject' AND value_string='Patient/1' AND param_type='reference'"),
            ("idx_sp_token", "SELECT resource_id FROM search_index WHERE resource_type='Observation' AND param_name='code' AND value_system='http://loinc.org' AND value_string='1'"),
            ("idx_sp_string", "SELECT resource_id FROM search_index WHERE resource_type='Observation' AND param_name='code' AND value_string='1'"),
            ("idx_sp_lower", "SELECT resource_id FROM search_index WHERE resource_type='Patient' AND param_name='family' AND value_string_lower >= 'do' AND value_string_lower < 'do\u{10FFFF}' AND value_string_lower LIKE 'do%' ESCAPE '\\'"),
            ("idx_sp_date", "SELECT resource_id FROM search_index WHERE resource_type='Observation' AND param_name='date' AND value_date_start IS NOT NULL AND value_date_end > 0"),
        ];
        for (want, sql) in cases {
            let p = plan(sql);
            assert!(p.contains(want), "expected {want} in plan for {sql}\n got: {p}");
        }
    }
}
