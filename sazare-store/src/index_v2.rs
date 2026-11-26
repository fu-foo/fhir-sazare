//! Search index v2 (design/search-index-v2.md §4): integer keys, one table per
//! value kind, set operations in SQLite.
//!
//! Every query is one SQL statement driven by its most selective criterion,
//! the others checked per candidate (`EXISTS` probes on the by-resource
//! indexes). Totals are counted on their own; sorted pages over large sets
//! walk the sort key's index. Checked against a reference model of the FHIR
//! search rules by `differential_tests`.
//!
//! Full text (`_text`, `_content`) is an FTS5 table in a file of its own
//! (`<index>.fts.sqlite`), attached as `fts`, its rowid the resource's `rid`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;

use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use sazare_core::{ChainParameter, HasParameter, SearchParamType, SearchParameter, SearchQuery, SortKey};

use crate::sqlite_index::{fhir_date_range, split_text_terms, IndexUpdate, ResourceText};
use crate::{Result, StoreError};

const TEXT_SCHEMA: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS fts.resource_text USING fts5(narrative, content, tokenize = 'trigram');
"#;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS rtype (tid INTEGER PRIMARY KEY, name TEXT UNIQUE NOT NULL);
CREATE TABLE IF NOT EXISTS param (pid INTEGER PRIMARY KEY, tid INT NOT NULL, name TEXT NOT NULL,
    kind TEXT NOT NULL, multi INT NOT NULL DEFAULT 0, max_span INT NOT NULL DEFAULT 0, UNIQUE (tid, name));
CREATE TABLE IF NOT EXISTS csystem (sid INTEGER PRIMARY KEY, uri TEXT UNIQUE NOT NULL);
CREATE TABLE IF NOT EXISTS rkey (rid INTEGER PRIMARY KEY AUTOINCREMENT, tid INT NOT NULL, id TEXT NOT NULL,
    present INT NOT NULL, UNIQUE (tid, id));
CREATE INDEX IF NOT EXISTS rkey_live ON rkey (tid, rid) WHERE present = 1;
CREATE INDEX IF NOT EXISTS rkey_id ON rkey (id);
CREATE TABLE IF NOT EXISTS type_count (tid INTEGER PRIMARY KEY, n INT NOT NULL);

CREATE TABLE IF NOT EXISTS v_token (pid INT, code TEXT, sid INT, rid INT, PRIMARY KEY (pid, code, sid, rid)) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS v_token_r ON v_token (rid, pid, code, sid);
CREATE TABLE IF NOT EXISTS v_string (pid INT, norm TEXT, rid INT, exact TEXT, PRIMARY KEY (pid, norm, rid, exact)) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS v_string_r ON v_string (rid, pid);
CREATE TABLE IF NOT EXISTS v_date (pid INT, lo INT, hi INT, rid INT, PRIMARY KEY (pid, lo, hi, rid)) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS v_date_h ON v_date (pid, hi, lo);
CREATE INDEX IF NOT EXISTS v_date_r ON v_date (rid, pid);
CREATE TABLE IF NOT EXISTS v_number (pid INT, val REAL, rid INT, PRIMARY KEY (pid, val, rid)) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS v_number_r ON v_number (rid, pid);
CREATE TABLE IF NOT EXISTS v_quantity (pid INT, val REAL, rid INT, sid INT, code TEXT, unit TEXT,
    PRIMARY KEY (pid, val, rid, sid, code, unit)) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS v_quantity_r ON v_quantity (rid, pid);
CREATE TABLE IF NOT EXISTS v_ref (pid INT, target INT, rid INT, PRIMARY KEY (pid, target, rid)) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS v_ref_r ON v_ref (rid, pid);
CREATE INDEX IF NOT EXISTS v_ref_t ON v_ref (target, pid, rid);
CREATE TABLE IF NOT EXISTS v_ref_text (pid INT, value TEXT, rid INT, PRIMARY KEY (pid, value, rid)) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS v_ref_text_r ON v_ref_text (rid, pid);
CREATE TABLE IF NOT EXISTS index_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
"#;

const VALUE_TABLES: [&str; 7] = ["v_token", "v_string", "v_date", "v_number", "v_quantity", "v_ref", "v_ref_text"];

/// `index_meta` key: the format the index was written in.
const FORMAT_KEY: &str = "index_format";

/// A probe count at or above this means "large": not worth counting further.
#[cfg(not(test))]
const PROBE_CAP: i64 = 10_001;
/// Small in tests, so the differential tests reach the large-set paths.
#[cfg(test)]
const PROBE_CAP: i64 = 6;

/// Cap when sizing large sets to choose between probing and intersecting.
const SIZE_CAP: i64 = 4_000_001;

/// A sorted search over at most this many matches gathers their keys and
/// sorts; above it, the sort key's index is walked in order.
#[cfg(not(test))]
const GATHER_MAX: i64 = 5_000;
/// Small in tests, so the differential tests exercise both paths.
#[cfg(test)]
const GATHER_MAX: i64 = 8;

/// A value bound into SQL.
#[derive(Clone, Debug)]
enum Arg {
    Int(i64),
    Real(f64),
    Text(String),
}

impl rusqlite::ToSql for Arg {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        match self {
            Arg::Int(i) => i.to_sql(),
            Arg::Real(f) => f.to_sql(),
            Arg::Text(s) => s.to_sql(),
        }
    }
}

/// SQL with its arguments, `?` placeholders in order.
#[derive(Clone, Debug, Default)]
struct Sql {
    text: String,
    args: Vec<Arg>,
}

impl Sql {
    fn new(text: impl Into<String>, args: Vec<Arg>) -> Self {
        Self { text: text.into(), args }
    }
    /// `self` with every `?X?` replaced by `rid_expr` (the candidate's rid).
    fn with_rid(&self, rid_expr: &str) -> Sql {
        Sql { text: self.text.replace("?X?", rid_expr), args: self.args.clone() }
    }
}

/// One criterion of a query, compiled: the set of rids it matches (`set`), a
/// per-candidate check (`probe`, with `?X?` for the candidate's rid), and
/// whether `set` can drive (seekable through an index).
struct Criterion {
    set: Sql,
    probe: Sql,
    seekable: bool,
    rank: u8,
    /// `set` is an equality seek (token code, reference, `:exact`): a
    /// contiguous index range, cheap to merge. Ranges (dates, prefixes) aren't.
    equality: bool,
    /// `set` yields each rid at most once (so a query it drives needs no
    /// `DISTINCT`): a single condition on a parameter no resource has two
    /// values for, a `UNION`, or a set taken from the key table.
    unique: bool,
}

/// A compiled query: rids (possibly repeated unless `distinct` is false).
struct Compiled {
    sql: Sql,
    /// The rows can repeat a rid; select `DISTINCT`.
    distinct: bool,
    /// The query has no criteria (every resource of the type).
    unfiltered: bool,
    /// Every criterion as one per-candidate check (`?X?` for the rid): a
    /// sort walk tests each row of the sort key's index with it.
    probe: Option<Sql>,
    /// Distinct matches for counting, when several criteria are each large:
    /// their sets intersected (a merge, not a probe per row), the rest probed.
    count_sql: Option<Sql>,
}

impl Compiled {
    fn select(&self) -> Sql {
        if self.distinct {
            Sql::new(format!("SELECT DISTINCT rid FROM ({})", self.sql.text), self.sql.args.clone())
        } else {
            self.sql.clone()
        }
    }
}

pub struct IndexV2 {
    conn: Connection,
    tids: RefCell<HashMap<String, i64>>,
    pids: RefCell<HashMap<(i64, String), (i64, String)>>,
    sids: RefCell<HashMap<String, i64>>,
    /// `index_format`, read once: a file's format never changes once written.
    format: u32,
}

impl IndexV2 {
    /// Open (creating if needed) the index at `path` and its full text beside
    /// it (`search_index.sqlite` → `search_index.fts.sqlite`; `:memory:`
    /// stays in memory).
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(crate::WAL_PRAGMAS)?;
        conn.execute_batch(SCHEMA)?;
        Self::attach_text(&conn, path)?;
        conn.execute_batch("PRAGMA fts.journal_mode=WAL; PRAGMA fts.journal_size_limit=67108864;")?;
        conn.execute_batch(TEXT_SCHEMA)?;
        let index = Self::with(conn)?;
        // A new (empty) index is in this release's format; one without the
        // record was written by 0.16 (format 3).
        if index.get_meta(FORMAT_KEY)?.is_none() && index.is_empty_of_keys()? {
            index.set_meta(FORMAT_KEY, &crate::INDEX_FORMAT.to_string())?;
            return Ok(Self { format: crate::INDEX_FORMAT, ..index });
        }
        Ok(index)
    }

    fn is_empty_of_keys(&self) -> Result<bool> {
        let any: bool = self.conn.query_row("SELECT EXISTS (SELECT 1 FROM rkey)", [], |r| r.get(0))?;
        Ok(!any)
    }

    /// The index format the file was written in (`INDEX_FORMAT`; 3 for an
    /// index from 0.16, which didn't record it). An older format answers
    /// searches while its replacement is built, without what it lacks.
    pub fn format(&self) -> u32 {
        self.format
    }

    /// Open an existing index read-only, for searches beside the writer.
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self> {
        use rusqlite::OpenFlags;
        let path = path.as_ref();
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // Attached files open with the main connection's flags: read-only too.
        Self::attach_text(&conn, path)?;
        Self::with(conn)
    }

    fn attach_text(conn: &Connection, path: &Path) -> Result<()> {
        let text = if path.as_os_str() == ":memory:" {
            ":memory:".to_string()
        } else {
            path.with_extension("fts.sqlite")
                .to_str()
                .ok_or_else(|| StoreError::Other(format!("index path is not UTF-8: {}", path.display())))?
                .to_string()
        };
        conn.execute("ATTACH DATABASE ?1 AS fts", params![text])?;
        Ok(())
    }

    fn with(conn: Connection) -> Result<Self> {
        // Room for every write statement (rusqlite keeps 16 by default).
        conn.set_prepared_statement_cache_capacity(64);
        let mut index = Self { conn, tids: Default::default(), pids: Default::default(), sids: Default::default(), format: 0 };
        index.format = index.get_meta(FORMAT_KEY)?.and_then(|v| v.parse().ok()).unwrap_or(3);
        Ok(index)
    }

    /// Forget cached dictionary keys (after a failed write rolled some back).
    fn forget(&self) {
        self.tids.borrow_mut().clear();
        self.pids.borrow_mut().clear();
        self.sids.borrow_mut().clear();
    }

    // ---- metadata -----------------------------------------------------------

    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT value FROM index_meta WHERE key = ?1", params![key], |r| r.get(0)).optional()?)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO index_meta (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// No resource indexed (an existence check).
    pub fn is_empty(&self) -> Result<bool> {
        let any: bool = self.conn.query_row("SELECT EXISTS (SELECT 1 FROM rkey WHERE present = 1)", [], |r| r.get(0))?;
        Ok(!any)
    }

    /// Some resource has full text (an existence check).
    pub fn has_text(&self) -> Result<bool> {
        Ok(self.conn.query_row("SELECT EXISTS (SELECT 1 FROM fts.resource_text)", [], |r| r.get(0))?)
    }

    // ---- dictionaries -------------------------------------------------------

    fn tid(&self, name: &str, create: bool) -> Result<Option<i64>> {
        if let Some(t) = self.tids.borrow().get(name) {
            return Ok(Some(*t));
        }
        if create {
            self.conn.execute("INSERT OR IGNORE INTO rtype (name) VALUES (?1)", params![name])?;
        }
        let t: Option<i64> =
            self.conn.query_row("SELECT tid FROM rtype WHERE name = ?1", params![name], |r| r.get(0)).optional()?;
        if let Some(t) = t {
            self.tids.borrow_mut().insert(name.to_string(), t);
        }
        Ok(t)
    }

    /// `(pid, kind)` of a parameter of a type.
    fn pid(&self, tid: i64, name: &str, kind: Option<&str>) -> Result<Option<(i64, String)>> {
        if let Some(p) = self.pids.borrow().get(&(tid, name.to_string())) {
            return Ok(Some(p.clone()));
        }
        if let Some(kind) = kind {
            self.conn.execute(
                "INSERT OR IGNORE INTO param (tid, name, kind) VALUES (?1, ?2, ?3)",
                params![tid, name, kind],
            )?;
        }
        let p: Option<(i64, String)> = self
            .conn
            .query_row("SELECT pid, kind FROM param WHERE tid = ?1 AND name = ?2", params![tid, name], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        if let Some(p) = &p {
            self.pids.borrow_mut().insert((tid, name.to_string()), p.clone());
        }
        Ok(p)
    }

    fn sid(&self, uri: Option<&str>, create: bool) -> Result<Option<i64>> {
        let Some(uri) = uri else { return Ok(Some(0)) };
        if let Some(s) = self.sids.borrow().get(uri) {
            return Ok(Some(*s));
        }
        if create {
            self.conn.execute("INSERT OR IGNORE INTO csystem (uri) VALUES (?1)", params![uri])?;
        }
        let s: Option<i64> =
            self.conn.query_row("SELECT sid FROM csystem WHERE uri = ?1", params![uri], |r| r.get(0)).optional()?;
        if let Some(s) = s {
            self.sids.borrow_mut().insert(uri.to_string(), s);
        }
        Ok(s)
    }

    /// The rid of `type/id`, creating the key; `present` marks a stored
    /// resource. Also whether it was stored before: one that wasn't has no
    /// rows (`remove` clears them before unmarking).
    fn rid(&self, tid: i64, id: &str, present: bool) -> Result<(i64, bool)> {
        self.conn
            .prepare_cached("INSERT OR IGNORE INTO rkey (tid, id, present) VALUES (?1, ?2, 0)")?
            .execute(params![tid, id])?;
        let (rid, was): (i64, i64) = self
            .conn
            .prepare_cached("SELECT rid, present FROM rkey WHERE tid = ?1 AND id = ?2")?
            .query_row(params![tid, id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        if present && was == 0 {
            self.conn.prepare_cached("UPDATE rkey SET present = 1 WHERE rid = ?1")?.execute(params![rid])?;
            self.conn
                .prepare_cached("INSERT INTO type_count (tid, n) VALUES (?1, 1) ON CONFLICT (tid) DO UPDATE SET n = n + 1")?
                .execute(params![tid])?;
        }
        Ok((rid, was != 0))
    }

    // ---- writes -------------------------------------------------------------

    /// Replace the index rows of each resource (one transaction).
    pub fn replace_many(&self, items: &[IndexUpdate]) -> Result<()> {
        let r = self.write_many(items);
        if r.is_err() {
            self.forget();
        }
        r
    }

    /// Add one value to a resource's rows (keeping the others).
    pub fn add_value(&self, resource_type: &str, id: &str, name: &str, kind: &str, value: &str, system: Option<&str>) -> Result<()> {
        let r = self.add_value_inner(resource_type, id, name, kind, value, system);
        if r.is_err() {
            self.forget();
        }
        r
    }

    fn add_value_inner(&self, resource_type: &str, id: &str, name: &str, kind: &str, value: &str, system: Option<&str>) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let tid = self.tid(resource_type, true)?.expect("created");
        let (rid, _) = self.rid(tid, id, true)?;
        let (pid, _) = self.pid(tid, name, Some(kind))?.expect("created");
        let has = self.has_value(pid, kind).with_rid(&rid.to_string());
        let had: bool = self.conn.query_row(&format!("SELECT {}", has.text), params_from_iter(has.args.iter()), |r| r.get(0))?;
        if self.write_value(pid, kind, value, system, rid)? && had {
            self.conn.execute("UPDATE param SET multi = 1 WHERE pid = ?1 AND multi = 0", params![pid])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// One resource's rows and full text.
    pub fn replace_one(&self, resource_type: &str, id: &str, entries: &[(String, String, String, Option<String>)], text: &ResourceText) -> Result<()> {
        self.replace_many(&[IndexUpdate {
            resource_type: resource_type.to_string(),
            resource_id: id.to_string(),
            entries: entries.to_vec(),
            text: text.clone(),
        }])
    }

    fn write_many(&self, items: &[IndexUpdate]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for u in items {
            let tid = self.tid(&u.resource_type, true)?.expect("created");
            let (rid, stored) = self.rid(tid, &u.resource_id, true)?;
            // A new resource has no rows to clear (all of them, in a rebuild).
            if stored {
                self.clear_rows(rid)?;
            }
            // Extraction adds a bare-id copy after each reference ("Patient/1"
            // → "1"). For a local reference the key table answers bare-id
            // searches; for an absolute URL the copy is kept as text.
            let local_ids: std::collections::HashSet<(&str, &str)> = u
                .entries
                .iter()
                .filter(|e| e.1 == "reference")
                .filter_map(|e| local_reference(&e.2).map(|(_, id)| (e.0.as_str(), id)))
                .collect();
            let mut per_pid: HashMap<i64, usize> = HashMap::new();
            for (name, kind, value, system) in &u.entries {
                let (pid, _) = self.pid(tid, name, Some(kind))?.expect("created");
                let bare_of_local = kind == "reference" && local_ids.contains(&(name.as_str(), value.as_str()));
                if !bare_of_local && self.write_value(pid, kind, value, system.as_deref(), rid)? {
                    *per_pid.entry(pid).or_default() += 1;
                }
            }
            for (pid, n) in per_pid {
                if n > 1 {
                    self.conn.execute("UPDATE param SET multi = 1 WHERE pid = ?1 AND multi = 0", params![pid])?;
                }
            }
            self.conn.prepare_cached("INSERT INTO fts.resource_text (rowid, narrative, content) VALUES (?1, ?2, ?3)")?.execute(params![rid, u.text.narrative, u.text.content])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Write one value; `false` when it isn't stored (an unparsable date or
    /// number).
    fn write_value(&self, pid: i64, kind: &str, value: &str, system: Option<&str>, rid: i64) -> Result<bool> {
        match kind {
            "string" => {
                self.conn.prepare_cached("INSERT OR IGNORE INTO v_string (pid, norm, rid, exact) VALUES (?1, ?2, ?3, ?4)")?.execute(params![pid, value.to_lowercase(), rid, value])?;
            }
            "date" => {
                let (lo, hi) = match value.split_once('/') {
                    Some((a, b)) => (
                        fhir_date_range(a).map(|r| r.0),
                        if b.is_empty() { Some(i64::MAX) } else { fhir_date_range(b).map(|r| r.1) },
                    ),
                    None => match fhir_date_range(value) {
                        Some((a, b)) => (Some(a), Some(b)),
                        None => (None, None),
                    },
                };
                let (Some(lo), Some(hi)) = (lo, hi) else { return Ok(false) };
                self.conn.prepare_cached("INSERT OR IGNORE INTO v_date (pid, lo, hi, rid) VALUES (?1, ?2, ?3, ?4)")?.execute(params![pid, lo, hi, rid])?;
                if hi != i64::MAX {
                    self.conn.prepare_cached("UPDATE param SET max_span = ?2 WHERE pid = ?1 AND max_span < ?2")?.execute(params![pid, hi - lo])?;
                }
            }
            "number" => {
                let Some(v) = value.parse::<f64>().ok().filter(|v| v.is_finite()) else { return Ok(false) };
                self.conn.prepare_cached("INSERT OR IGNORE INTO v_number (pid, val, rid) VALUES (?1, ?2, ?3)")?.execute(params![pid, v, rid])?;
            }
            "quantity" => {
                // number, code, unit (IndexBuilder::quantity_row).
                let mut parts = value.splitn(3, crate::index_builder::QUANTITY_SEP);
                let Some(v) = parts.next().and_then(|n| n.parse::<f64>().ok()).filter(|v| v.is_finite()) else {
                    return Ok(false);
                };
                let (code, unit) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
                let sid = self.sid(system, true)?.expect("created");
                self.conn.prepare_cached("INSERT OR IGNORE INTO v_quantity (pid, val, rid, sid, code, unit) VALUES (?1, ?2, ?3, ?4, ?5, ?6)")?.execute(params![pid, v, rid, sid, code, unit])?;
            }
            "reference" => match local_reference(value) {
                Some((ttype, tid_text)) => {
                    let ttid = self.tid(ttype, true)?.expect("created");
                    let (target, _) = self.rid(ttid, tid_text, false)?;
                    self.conn.prepare_cached("INSERT OR IGNORE INTO v_ref (pid, target, rid) VALUES (?1, ?2, ?3)")?.execute(params![pid, target, rid])?;
                }
                // Absolute URLs, urn:, #contained, and bare ids of absolute URLs.
                None => {
                    self.conn.prepare_cached("INSERT OR IGNORE INTO v_ref_text (pid, value, rid) VALUES (?1, ?2, ?3)")?.execute(params![pid, value, rid])?;
                }
            },
            _ => {
                let sid = self.sid(system, true)?.expect("created");
                self.conn.prepare_cached("INSERT OR IGNORE INTO v_token (pid, code, sid, rid) VALUES (?1, ?2, ?3, ?4)")?.execute(params![pid, value, sid, rid])?;
            }
        }
        Ok(true)
    }

    fn clear_rows(&self, rid: i64) -> Result<()> {
        for t in VALUE_TABLES {
            self.conn.prepare_cached(&format!("DELETE FROM {t} WHERE rid = ?1"))?.execute(params![rid])?;
        }
        self.conn.prepare_cached("DELETE FROM fts.resource_text WHERE rowid = ?1")?.execute(params![rid])?;
        Ok(())
    }

    /// A resource was deleted.
    pub fn remove(&self, resource_type: &str, id: &str) -> Result<()> {
        let Some(tid) = self.tid(resource_type, false)? else { return Ok(()) };
        let r: Option<(i64, i64)> = self
            .conn
            .query_row("SELECT rid, present FROM rkey WHERE tid = ?1 AND id = ?2", params![tid, id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        if let Some((rid, present)) = r {
            let tx = self.conn.unchecked_transaction()?;
            self.clear_rows(rid)?;
            if present == 1 {
                self.conn.execute("UPDATE rkey SET present = 0 WHERE rid = ?1", params![rid])?;
                self.conn.execute("UPDATE type_count SET n = n - 1 WHERE tid = ?1", params![tid])?;
            }
            tx.commit()?;
        }
        Ok(())
    }

    // ---- search -------------------------------------------------------------

    /// Ids of `resource_type` whose reference parameter `param` points at any
    /// of `references` (`Type/id`, a bare id, or a URL) — `_revinclude`,
    /// `$everything`.
    pub fn referencing(&self, resource_type: &str, param: &str, references: &[String]) -> Result<Vec<String>> {
        let Some(tid) = self.tid(resource_type, false)? else { return Ok(Vec::new()) };
        let mut seen = std::collections::HashSet::new();
        for r in references {
            let p = SearchParameter {
                name: param.to_string(),
                value: r.clone(),
                modifier: None,
                prefix: None,
                param_type: SearchParamType::Reference,
            };
            let c = self.parameter(tid, &p)?;
            seen.extend(self.rids(&c.set)?);
        }
        let mut rids: Vec<i64> = seen.into_iter().collect();
        rids.sort_unstable();
        self.ids_for(&rids)
    }

    /// Errors a query's values can carry (`:missing` not true/false, a
    /// number that doesn't parse), as the 400 message.
    fn validate(query: &SearchQuery) -> std::result::Result<(), String> {
        for p in &query.parameters {
            if p.modifier.as_deref() == Some("missing") {
                let v = p.value.trim();
                if v != "true" && v != "false" {
                    return Err(format!(":missing expects true|false, got '{v}'"));
                }
            } else if matches!(p.param_type, SearchParamType::Number | SearchParamType::Quantity)
                && !matches!(p.name.as_str(), "_text" | "_content")
            {
                for (_, v) in sazare_core::search_param::prefixed_alternatives(p) {
                    if v.split('|').next().unwrap_or("").trim().parse::<f64>().is_err() {
                        return Err(format!("Invalid number '{}' for parameter '{}'", v, p.name));
                    }
                }
            }
        }
        Ok(())
    }

    /// Ids matching `query` (no paging), in no particular order.
    pub fn search(&self, resource_type: &str, query: &SearchQuery) -> std::result::Result<Vec<String>, String> {
        Self::validate(query)?;
        let Some(c) = self.compile(resource_type, query).map_err(|e| e.to_string())? else {
            return Ok(Vec::new());
        };
        self.ids_of(&c.select(), None)
    }

    /// The page `query` asks for (`_sort`, `_offset`, `_count`) and the total.
    pub fn search_with_total(
        &self,
        resource_type: &str,
        query: &SearchQuery,
    ) -> std::result::Result<(Vec<String>, usize), String> {
        Self::validate(query)?;
        let Some(c) = self.compile(resource_type, query).map_err(|e| e.to_string())? else {
            return Ok((Vec::new(), 0));
        };
        let tid = self.tid(resource_type, false).map_err(|e| e.to_string())?.unwrap_or(-1);
        let err = |e: crate::StoreError| e.to_string();
        let sql = c.select();
        let filter = (!c.unfiltered).then_some(&sql);
        // `_total=none`: no count. One row past the page says whether there
        // is a next one; the "total" returned is then only that bound.
        let exact = !query.omit_total || c.unfiltered;
        let total = if exact { Some(self.count(tid, c.count_sql.as_ref().or(filter)).map_err(err)?) } else { None };
        let offset = query.offset.unwrap_or(0);
        let count = query.count.map(|n| if exact { n } else { n + 1 });
        let large = match total {
            Some(t) => t as i64 > GATHER_MAX,
            None => self.probe_count(&sql).map_err(err)? > GATHER_MAX,
        };
        let mut page: Vec<i64> = if query.sort.is_empty() {
            // Unsorted: the page straight off the query, in index order (the
            // order of an unsorted search is unspecified).
            let paged = Sql::new(
                format!("{} LIMIT ? OFFSET ?", sql.text),
                [sql.args.clone(), vec![Arg::Int(count.map_or(-1, |n| n as i64)), Arg::Int(offset as i64)]].concat(),
            );
            self.rids(&paged).map_err(err)?
        } else if c.unfiltered || large {
            // Sorted over a large set: walk the first key's index in order and
            // stop at the page, instead of gathering every candidate's key.
            let want = offset.saturating_add(count.unwrap_or(usize::MAX / 2));
            match self.walk_sorted(tid, &query.sort, filter, c.probe.as_ref(), total, want).map_err(err)? {
                Some(walked) => walked.into_iter().skip(offset).collect(),
                None => self.gathered_page(tid, &query.sort, &sql, offset, count).map_err(err)?,
            }
        } else {
            self.gathered_page(tid, &query.sort, &sql, offset, count).map_err(err)?
        };
        let total = total.unwrap_or(offset + page.len());
        if let Some(n) = query.count {
            page.truncate(n);
        }
        let ids = self.ids_for(&page).map_err(err)?;
        Ok((ids, total))
    }

    /// A sorted page by gathering every match's keys and sorting them.
    fn gathered_page(&self, tid: i64, keys: &[SortKey], sql: &Sql, offset: usize, count: Option<usize>) -> Result<Vec<i64>> {
        let mut rids = self.rids(sql)?;
        self.sort_rids(tid, keys, &mut rids)?;
        let mut page: Vec<i64> = rids.into_iter().skip(offset).collect();
        if let Some(n) = count {
            page.truncate(n);
        }
        Ok(page)
    }

    /// The number of matches: `type_count` without criteria.
    fn count(&self, tid: i64, filter: Option<&Sql>) -> Result<usize> {
        let n: i64 = match filter {
            None => self
                .conn
                .query_row("SELECT coalesce((SELECT n FROM type_count WHERE tid = ?1), 0)", params![tid], |r| r.get(0))?,
            Some(f) => self.conn.query_row(
                &format!("SELECT count(*) FROM ({})", f.text),
                params_from_iter(f.args.iter()),
                |r| r.get(0),
            )?,
        };
        Ok(n as usize)
    }

    /// The first `want` rids in sort order: the first key's value index walked
    /// in order (each rid first seen at its lowest value ascending, highest
    /// descending), groups of equal first keys ordered by the remaining keys,
    /// then the rids without a value, in rid order. With `filter`, only its
    /// matches.
    ///
    /// Each row of the walk is tested against the filter one of two ways: a
    /// probe per row (`EXISTS` on the by-resource indexes; nothing to build,
    /// so the first page of a broad filter is immediate), or membership in
    /// the filter's matches, built once (cheap per row; better when the
    /// filter is narrow and the walk passes many rows to fill the page).
    /// `None`: the first key has no ordered index (references).
    fn walk_sorted(
        &self,
        tid: i64,
        keys: &[SortKey],
        filter: Option<&Sql>,
        probe: Option<&Sql>,
        total: Option<usize>,
        want: usize,
    ) -> Result<Option<Vec<i64>>> {
        let first = &keys[0];
        let mut out: Vec<i64> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        if let Some((pid, kind)) = self.pid(tid, &first.name, None)? {
            let (table, col) = match kind.as_str() {
                "date" => ("v_date", "lo"),
                "number" => ("v_number", "val"),
                "quantity" => ("v_quantity", "val"),
                "string" => ("v_string", "norm"),
                "token" => ("v_token", "code"),
                _ => return Ok(None),
            };
            // Rows the walk visits ≈ want × type size / matches; a probe
            // costs ~2.5 µs a row, building the membership ~0.4 µs a match.
            let by_probe = match (filter, total) {
                (None, _) => false,
                (Some(_), None) => true,
                (Some(_), Some(t)) => {
                    let n: i64 = self
                        .conn
                        .query_row("SELECT coalesce((SELECT n FROM type_count WHERE tid = ?1), 0)", params![tid], |r| r.get(0))?;
                    walk_by_probe(t as f64, want as f64, n as f64)
                }
            };
            let (cond, cond_args) = match (filter, probe) {
                (Some(_), Some(p)) if by_probe => {
                    let p = p.with_rid("w.rid");
                    (format!(" AND {}", p.text), p.args)
                }
                (Some(f), _) => (format!(" AND w.rid IN ({})", f.text), f.args.clone()),
                (None, _) => (String::new(), Vec::new()),
            };
            let dir = if first.descending { "DESC" } else { "ASC" };
            let sql = format!("SELECT w.rid, w.{col} FROM {table} w NOT INDEXED WHERE w.pid = ?{cond} ORDER BY w.{col} {dir}");
            let mut args = vec![Arg::Int(pid)];
            args.extend(cond_args);
            let mut stmt = self.conn.prepare(&sql)?;
            let mut rows = stmt.query(params_from_iter(args.iter()))?;
            let mut group: Vec<i64> = Vec::new();
            let mut group_key: Option<rusqlite::types::Value> = None;
            let flush = |group: &mut Vec<i64>, out: &mut Vec<i64>| -> Result<()> {
                // Equal first keys: by the remaining keys, then insertion
                // order (the walk's DESC would otherwise reverse it).
                if group.len() > 1 {
                    self.sort_rids(tid, &keys[1..], group)?;
                }
                out.append(group);
                Ok(())
            };
            while let Some(row) = rows.next()? {
                let rid: i64 = row.get(0)?;
                let key: rusqlite::types::Value = row.get(1)?;
                if !seen.insert(rid) {
                    continue;
                }
                if group_key.as_ref() != Some(&key) {
                    flush(&mut group, &mut out)?;
                    if out.len() >= want {
                        break;
                    }
                    group_key = Some(key);
                }
                group.push(rid);
            }
            if out.len() < want {
                flush(&mut group, &mut out)?;
            }
        }
        if out.len() < want {
            // Resources without a value for the first key go last (rid order).
            let sql = match filter {
                Some(f) => format!("SELECT rid FROM ({}) ORDER BY rid", f.text),
                None => "SELECT rid FROM rkey INDEXED BY rkey_live WHERE tid = ? AND present = 1 ORDER BY rid".to_string(),
            };
            let args: Vec<Arg> = match filter {
                Some(f) => f.args.clone(),
                None => vec![Arg::Int(tid)],
            };
            let mut stmt = self.conn.prepare(&sql)?;
            let mut rows = stmt.query(params_from_iter(args.iter()))?;
            let mut rest = Vec::new();
            while let Some(row) = rows.next()? {
                let rid: i64 = row.get(0)?;
                if !seen.contains(&rid) {
                    rest.push(rid);
                    if out.len() + rest.len() >= want && keys.len() == 1 {
                        break;
                    }
                }
            }
            if keys.len() > 1 {
                self.sort_rids(tid, &keys[1..], &mut rest)?;
            }
            out.extend(rest);
        }
        out.truncate(want);
        Ok(Some(out))
    }

    fn rids(&self, sql: &Sql) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare(&sql.text)?;
        let rows = stmt.query_map(params_from_iter(sql.args.iter()), |r| r.get::<_, i64>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    fn ids_of(&self, sql: &Sql, limit: Option<usize>) -> std::result::Result<Vec<String>, String> {
        let mut rids = self.rids(sql).map_err(|e| e.to_string())?;
        if let Some(l) = limit {
            rids.truncate(l);
        }
        self.ids_for(&rids).map_err(|e| e.to_string())
    }

    fn ids_for(&self, rids: &[i64]) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare_cached("SELECT id FROM rkey WHERE rid = ?1")?;
        let mut out = Vec::with_capacity(rids.len());
        for rid in rids {
            out.push(stmt.query_row(params![rid], |r| r.get::<_, String>(0))?);
        }
        Ok(out)
    }

    /// Sort `rids` by the keys (lowest value ascending, highest descending;
    /// no value last; then by rid — insertion order, the unspecified tie order).
    fn sort_rids(&self, tid: i64, keys: &[SortKey], rids: &mut [i64]) -> Result<()> {
        #[derive(PartialEq, PartialOrd, Clone)]
        enum K {
            N(f64),
            T(String),
        }
        let mut columns: Vec<(HashMap<i64, K>, bool)> = Vec::new();
        for key in keys {
            let mut m: HashMap<i64, K> = HashMap::new();
            let pid = self.pid(tid, &key.name, None)?;
            if let Some((pid, kind)) = pid {
                let (sql, numeric) = match kind.as_str() {
                    "date" => ("SELECT rid, lo FROM v_date INDEXED BY v_date_r WHERE rid = ?1 AND pid = ?2", true),
                    "number" => ("SELECT rid, val FROM v_number INDEXED BY v_number_r WHERE rid = ?1 AND pid = ?2", true),
                    "quantity" => ("SELECT rid, val FROM v_quantity INDEXED BY v_quantity_r WHERE rid = ?1 AND pid = ?2", true),
                    "string" => ("SELECT rid, norm FROM v_string INDEXED BY v_string_r WHERE rid = ?1 AND pid = ?2", false),
                    // A reference sorts by its text: "Type/id" for a local
                    // target, else as written (bare-id copies left out).
                    "reference" => (
                        "SELECT v.rid, t.name || '/' || k.id FROM v_ref v INDEXED BY v_ref_r \
                         CROSS JOIN rkey k ON k.rid = v.target CROSS JOIN rtype t ON t.tid = k.tid \
                         WHERE v.rid = ?1 AND v.pid = ?2 \
                         UNION ALL SELECT rid, value FROM v_ref_text INDEXED BY v_ref_text_r \
                         WHERE rid = ?1 AND pid = ?2 AND (value LIKE '%/%' OR value LIKE '%:%' OR value LIKE '#%')",
                        false,
                    ),
                    _ => ("SELECT rid, code FROM v_token INDEXED BY v_token_r WHERE rid = ?1 AND pid = ?2", false),
                };
                let mut stmt = self.conn.prepare_cached(sql)?;
                for rid in rids.iter() {
                    let vals = stmt.query_map(params![rid, pid], |r| {
                        Ok(if numeric { K::N(r.get::<_, f64>(1)?) } else { K::T(r.get::<_, String>(1)?) })
                    })?;
                    for v in vals {
                        let v = v?;
                        let e = m.entry(*rid).or_insert_with(|| v.clone());
                        let better = if key.descending { v > *e } else { v < *e };
                        if better {
                            *e = v;
                        }
                    }
                }
            }
            columns.push((m, key.descending));
        }
        rids.sort_by(|a, b| {
            for (m, desc) in &columns {
                let o = match (m.get(a), m.get(b)) {
                    (Some(x), Some(y)) => {
                        let o = x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal);
                        if *desc { o.reverse() } else { o }
                    }
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                };
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
            }
            a.cmp(b)
        });
        Ok(())
    }

    /// The query as one SQL statement selecting distinct rids, or `None` when
    /// it can't match anything (an unknown type).
    fn compile(&self, resource_type: &str, query: &SearchQuery) -> Result<Option<Compiled>> {
        let Some(tid) = self.tid(resource_type, false)? else { return Ok(None) };
        let mut crits: Vec<Criterion> = Vec::new();
        for p in &query.parameters {
            crits.push(self.parameter(tid, p)?);
        }
        for c in &query.chain_parameters {
            crits.push(self.chain(tid, c)?);
        }
        for h in &query.has_parameters {
            crits.push(self.has(tid, h)?);
        }
        let all_of_type = Sql::new("SELECT rid FROM rkey INDEXED BY rkey_live WHERE tid = ? AND present = 1", vec![Arg::Int(tid)]);
        if crits.is_empty() {
            return Ok(Some(Compiled { sql: all_of_type, distinct: false, unfiltered: true, probe: None, count_sql: None }));
        }
        // Drive from the smallest seekable criterion (bounded probe counts;
        // ties and saturated counts by rank); the others are checked per row.
        let mut best: Option<(usize, i64, u8)> = None;
        let mut large: Vec<usize> = Vec::new();
        for (i, c) in crits.iter().enumerate() {
            if !c.seekable {
                continue;
            }
            let n = self.probe_count(&c.set)?;
            if n >= PROBE_CAP && c.equality {
                large.push(i);
            }
            let better = match best {
                None => true,
                Some((_, bn, br)) => n < bn || (n == bn && c.rank < br),
            };
            if better {
                best = Some((i, n, c.rank));
            }
        }
        let (driver, unique, rest): (Sql, bool, Vec<&Criterion>) = match best {
            Some((i, _, _)) => (
                crits[i].set.clone(),
                crits[i].unique,
                crits.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, c)| c).collect(),
            ),
            None => (all_of_type, true, crits.iter().collect()),
        };
        let mut text = format!("SELECT d.rid AS rid FROM ({}) d", driver.text);
        let mut args = driver.args;
        for (k, c) in rest.iter().enumerate() {
            text.push_str(if k == 0 { " WHERE " } else { " AND " });
            let p = c.probe.with_rid("d.rid");
            text.push_str(&p.text);
            args.extend(p.args);
        }
        // Counting a query whose every seekable criterion is large: merging
        // the large equality sets (~0.4 µs a row) beats probing each of the
        // driver's rows against the others (~2.5 µs a probe) when the sets
        // are of similar size. Size them (capped) to decide.
        // Only when the total is wanted (`_total=none` skips it).
        let driver_large = !query.omit_total && best.is_some_and(|(_, n, _)| n >= PROBE_CAP);
        if driver_large && large.len() >= 2 {
            let mut sized: Vec<(usize, i64)> = Vec::new();
            for &i in &large {
                let sql = format!("SELECT count(*) FROM ({} LIMIT {SIZE_CAP})", crits[i].set.text);
                let n: i64 = self.conn.query_row(&sql, params_from_iter(crits[i].set.args.iter()), |r| r.get(0))?;
                sized.push((i, n));
            }
            let min = sized.iter().map(|s| s.1).min().unwrap_or(0);
            let sum: i64 = sized.iter().map(|s| s.1).sum();
            if sum > 5 * min * (sized.len() as i64 - 1) {
                large.clear();
            }
        } else {
            large.clear();
        }
        let count_sql = (large.len() >= 2).then(|| {
            let mut text = format!(
                "SELECT d.rid AS rid FROM ({}) d",
                large.iter().map(|&i| format!("SELECT rid FROM ({})", crits[i].set.text)).collect::<Vec<_>>().join(" INTERSECT ")
            );
            let mut args: Vec<Arg> = large.iter().flat_map(|&i| crits[i].set.args.clone()).collect();
            for (k, (_, c)) in crits.iter().enumerate().filter(|(i, _)| !large.contains(i)).enumerate() {
                text.push_str(if k == 0 { " WHERE " } else { " AND " });
                let p = c.probe.with_rid("d.rid");
                text.push_str(&p.text);
                args.extend(p.args);
            }
            Sql::new(text, args)
        });
        let probe = Sql::new(
            crits.iter().map(|c| c.probe.text.clone()).collect::<Vec<_>>().join(" AND "),
            crits.iter().flat_map(|c| c.probe.args.clone()).collect(),
        );
        Ok(Some(Compiled { sql: Sql::new(text, args), distinct: !unique, unfiltered: false, probe: Some(probe), count_sql }))
    }

    fn probe_count(&self, set: &Sql) -> Result<i64> {
        let sql = format!("SELECT count(*) FROM ({} LIMIT {PROBE_CAP})", set.text);
        Ok(self.conn.query_row(&sql, params_from_iter(set.args.iter()), |r| r.get(0))?)
    }

    /// "The candidate (`?X?`) has a value for `pid`": references live in two
    /// tables (local targets, and URLs / urn: / #contained as text).
    fn has_value(&self, pid: i64, kind: &str) -> Sql {
        let t = table_for(kind);
        let mut text = format!("EXISTS (SELECT 1 FROM {t} INDEXED BY {t}_r WHERE rid = ?X? AND pid = ?)");
        let mut args = vec![Arg::Int(pid)];
        if kind == "reference" {
            text = format!("({text} OR EXISTS (SELECT 1 FROM v_ref_text INDEXED BY v_ref_text_r WHERE rid = ?X? AND pid = ?))");
            args.push(Arg::Int(pid));
        }
        Sql::new(text, args)
    }

    /// One search parameter (with its modifier and comma alternatives).
    fn parameter(&self, tid: i64, p: &SearchParameter) -> Result<Criterion> {
        if matches!(p.name.as_str(), "_text" | "_content") && p.modifier.is_none() {
            return Ok(self.text(tid, p));
        }
        let pid = self.pid(tid, &p.name, None)?;
        let all = Sql::new("SELECT rid FROM rkey INDEXED BY rkey_live WHERE tid = ? AND present = 1", vec![Arg::Int(tid)]);
        let none = Criterion { set: Sql::new("SELECT 0 AS rid WHERE 0", vec![]), probe: Sql::new("0", vec![]), seekable: true, rank: 0, unique: true, equality: false };
        match p.modifier.as_deref() {
            Some("missing") => {
                let want_missing = p.value.trim() == "true";
                let has = match &pid {
                    Some((pid, kind)) => self.has_value(*pid, kind),
                    None => Sql::new("0", vec![]),
                };
                let probe = if want_missing { Sql::new(format!("NOT {}", has.text), has.args) } else { has };
                let set = Sql::new(format!("{} AND {}", all.text, probe.with_rid("rkey.rid").text), [all.args, probe.args.clone()].concat());
                return Ok(Criterion { set, probe, seekable: false, rank: 6, unique: true, equality: false });
            }
            Some("not") => {
                let mut inner = p.clone();
                inner.modifier = None;
                let c = self.parameter(tid, &inner)?;
                let probe = Sql::new(format!("NOT ({})", c.probe.text), c.probe.args);
                let set = Sql::new(format!("{} AND {}", all.text, probe.with_rid("rkey.rid").text), [all.args, probe.args.clone()].concat());
                return Ok(Criterion { set, probe, seekable: false, rank: 6, unique: true, equality: false });
            }
            _ => {}
        }
        let Some((pid, kind)) = pid else { return Ok(none) };
        let mut sets = Vec::new();
        let mut probes = Vec::new();
        let mut seekable = true;
        let mut alternatives = Vec::new();
        // Each alternative with its own prefix (`gt140,lt90`).
        for (prefix, v) in sazare_core::search_param::prefixed_alternatives(p) {
            let alt = SearchParameter { prefix, ..p.clone() };
            let v = v.as_str();
            alternatives.extend(self.condition(pid, &kind, &alt, v)?);
            if kind == "reference" && !v.contains(['/', ':', '#']) {
                // A bare id also matches the bare-id copy of an absolute URL.
                alternatives.push(("v_ref_text", "value = ?".to_string(), vec![Arg::Text(v.into())], None, true));
            }
        }
        for (t, cond, args, index, seek) in alternatives {
            let using = match index {
                Some(i) => format!("INDEXED BY {i}"),
                None => "NOT INDEXED".to_string(),
            };
            sets.push(Sql::new(format!("SELECT rid FROM {t} {using} WHERE pid = ? AND {cond}"), [vec![Arg::Int(pid)], args.clone()].concat()));
            probes.push(Sql::new(
                format!("EXISTS (SELECT 1 FROM {t} INDEXED BY {t}_r WHERE rid = ?X? AND pid = ? AND {cond})"),
                [vec![Arg::Int(pid)], args].concat(),
            ));
            seekable &= seek;
        }
        if sets.is_empty() {
            return Ok(none);
        }
        let set = Sql::new(
            sets.iter().map(|s| s.text.clone()).collect::<Vec<_>>().join(" UNION "),
            sets.iter().flat_map(|s| s.args.clone()).collect(),
        );
        let probe = Sql::new(
            format!("({})", probes.iter().map(|s| s.text.clone()).collect::<Vec<_>>().join(" OR ")),
            probes.iter().flat_map(|s| s.args.clone()).collect(),
        );
        let rank = match (p.name.as_str(), &p.param_type) {
            ("_id", _) => 0,
            (_, SearchParamType::Reference) => 1,
            ("identifier", _) => 1,
            (_, SearchParamType::Token) => 2,
            (_, SearchParamType::String) => 3,
            _ => 4,
        };
        // One value on a parameter nobody has twice, or several values
        // (a UNION, which is distinct): no repeated rids.
        let multi: i64 = self.conn.query_row("SELECT multi FROM param WHERE pid = ?1", params![pid], |r| r.get(0))?;
        let unique = sets.len() > 1 || multi == 0;
        let equality = matches!(kind.as_str(), "token" | "reference") || p.modifier.as_deref() == Some("exact");
        Ok(Criterion { set, probe, seekable, rank, unique, equality })
    }

    /// The condition on a value table for one value, its arguments, the index
    /// to seek and whether it seeks. `None`: the value can't match (unknown
    /// system, unparsable date).
    #[allow(clippy::type_complexity)]
    fn condition(
        &self,
        pid: i64,
        kind: &str,
        p: &SearchParameter,
        v: &str,
    ) -> Result<Option<(&'static str, String, Vec<Arg>, Option<&'static str>, bool)>> {
        let t = table_for(kind);
        Ok(Some(match kind {
            "string" => match p.modifier.as_deref() {
                Some("exact") => ("norm = ? AND exact = ?".into(), vec![Arg::Text(v.to_lowercase()), Arg::Text(v.into())], None, true),
                Some("contains") => (
                    "norm LIKE ? ESCAPE '\\'".into(),
                    vec![Arg::Text(format!("%{}%", escape_like(&v.to_lowercase())))],
                    None,
                    false,
                ),
                _ => {
                    let lower = v.to_lowercase();
                    (
                        "norm >= ? AND norm < ?".into(),
                        vec![Arg::Text(lower.clone()), Arg::Text(format!("{lower}\u{10FFFF}"))],
                        None,
                        true,
                    )
                }
            },
            "date" => {
                let Some((qs, qe)) = fhir_date_range(v) else { return Ok(None) };
                let span: i64 = self.conn.query_row("SELECT max_span FROM param WHERE pid = ?1", params![pid], |r| r.get(0))?;
                let (cond, args, index): (&str, Vec<i64>, Option<&'static str>) = match p.prefix.as_deref().unwrap_or("eq") {
                    "ne" => ("(lo < ? OR hi > ?)", vec![qs, qe], None),
                    "gt" => ("hi > ?", vec![qe], Some("v_date_h")),
                    "ge" => ("hi > ?", vec![qs], Some("v_date_h")),
                    "lt" => ("lo < ?", vec![qs], None),
                    "le" => ("lo < ?", vec![qe], None),
                    "sa" => ("lo >= ?", vec![qe], None),
                    "eb" => ("hi <= ?", vec![qs], Some("v_date_h")),
                    "ap" => (
                        "((lo >= ? AND lo < ? AND hi > ?) OR (hi = 9223372036854775807 AND lo < ?))",
                        vec![qs.saturating_sub(span), qe, qs, qe],
                        None,
                    ),
                    _ => ("lo >= ? AND hi <= ?", vec![qs, qe], None),
                };
                // `ne` is an OR across the whole parameter: no seek.
                let seek = p.prefix.as_deref() != Some("ne");
                (cond.into(), args.into_iter().map(Arg::Int).collect(), index, seek)
            }
            "number" => {
                let Some((cond, args, seek)) = number_condition(p.prefix.as_deref(), v) else { return Ok(None) };
                (cond, args, None, seek)
            }
            "quantity" => {
                // `number[|system|code]`; `number||code` matches the code or
                // the unit in any system.
                let mut parts = v.splitn(3, '|');
                let number = parts.next().unwrap_or("");
                let Some((mut cond, mut args, seek)) = number_condition(p.prefix.as_deref(), number) else { return Ok(None) };
                match (parts.next().unwrap_or(""), parts.next().unwrap_or("")) {
                    ("", "") => {}
                    ("", code) => {
                        cond = format!("{cond} AND (code = ? OR unit = ?)");
                        args.extend([Arg::Text(code.into()), Arg::Text(code.into())]);
                    }
                    (sys, code) => {
                        let Some(sid) = self.sid(Some(sys), false)? else { return Ok(None) };
                        cond = format!("{cond} AND sid = ?");
                        args.push(Arg::Int(sid));
                        if !code.is_empty() {
                            cond = format!("{cond} AND code = ?");
                            args.push(Arg::Text(code.into()));
                        }
                    }
                }
                (cond, args, None, seek)
            }
            "reference" => match local_reference(v) {
                Some((ttype, id)) => {
                    let Some(ttid) = self.tid(ttype, false)? else { return Ok(None) };
                    (
                        "target IN (SELECT rid FROM rkey WHERE tid = ? AND id = ?)".into(),
                        vec![Arg::Int(ttid), Arg::Text(id.into())],
                        None,
                        true,
                    )
                }
                None if !v.contains(['/', ':', '#']) => (
                    "target IN (SELECT rid FROM rkey INDEXED BY rkey_id WHERE id = ?)".into(),
                    vec![Arg::Text(v.into())],
                    None,
                    true,
                ),
                // Absolute URLs, urn:, #contained: stored as text.
                None => return Ok(Some(("v_ref_text", "value = ?".into(), vec![Arg::Text(v.into())], None, true))),
            },
            _ => match v.split_once('|') {
                Some((sys, "")) => {
                    let Some(sid) = self.sid(Some(sys), false)? else { return Ok(None) };
                    ("sid = ?".into(), vec![Arg::Int(sid)], None, false)
                }
                Some(("", code)) => ("code = ? AND sid = 0".into(), vec![Arg::Text(code.into())], None, true),
                Some((sys, code)) => {
                    let Some(sid) = self.sid(Some(sys), false)? else { return Ok(None) };
                    ("code = ? AND sid = ?".into(), vec![Arg::Text(code.into()), Arg::Int(sid)], None, true)
                }
                None => ("code = ?".into(), vec![Arg::Text(v.into())], None, true),
            },
        }))
        .map(|o| o.map(|(cond, args, index, seek): (String, Vec<Arg>, Option<&'static str>, bool)| (t, cond, args, index, seek)))
    }

    /// `_text` (narrative) / `_content` (whole resource): every term (or
    /// "quoted phrase") occurs, case-insensitively, as a substring; comma
    /// values are alternatives. Terms of 3+ characters go through the trigram
    /// index, shorter ones (a two-character Japanese surname) through LIKE.
    fn text(&self, tid: i64, p: &SearchParameter) -> Criterion {
        let column = if p.name == "_text" { "narrative" } else { "content" };
        let mut sets = Vec::new();
        let mut probes = Vec::new();
        let mut seekable = true;
        for v in p.value.split(',') {
            let terms = split_text_terms(v);
            if terms.is_empty() {
                continue;
            }
            let (long, short): (Vec<String>, Vec<String>) = terms.into_iter().partition(|t| t.chars().count() >= 3);
            let mut cond = String::new();
            let mut args = Vec::new();
            if long.is_empty() {
                seekable = false;
            } else {
                let q = long.iter().map(|t| format!("\"{}\"", t.replace('"', "\"\""))).collect::<Vec<_>>().join(" AND ");
                cond.push_str(" AND t.resource_text MATCH ?");
                args.push(Arg::Text(format!("{column} : ({q})")));
            }
            for t in short {
                cond.push_str(&format!(" AND t.{column} LIKE ? ESCAPE '\\'"));
                args.push(Arg::Text(format!("%{}%", escape_like(&t))));
            }
            sets.push(Sql::new(
                format!("SELECT k.rid AS rid FROM fts.resource_text t CROSS JOIN rkey k ON k.rid = t.rowid WHERE k.tid = ?{cond}"),
                [vec![Arg::Int(tid)], args.clone()].concat(),
            ));
            probes.push(Sql::new(format!("EXISTS (SELECT 1 FROM fts.resource_text t WHERE t.rowid = ?X?{cond})"), args));
        }
        if sets.is_empty() {
            return Criterion { set: Sql::new("SELECT 0 AS rid WHERE 0", vec![]), probe: Sql::new("0", vec![]), seekable: true, rank: 0, unique: true, equality: false };
        }
        let set = Sql::new(
            sets.iter().map(|s| s.text.clone()).collect::<Vec<_>>().join(" UNION "),
            sets.iter().flat_map(|s| s.args.clone()).collect(),
        );
        let probe = Sql::new(
            format!("({})", probes.iter().map(|s| s.text.clone()).collect::<Vec<_>>().join(" OR ")),
            probes.iter().flat_map(|s| s.args.clone()).collect(),
        );
        Criterion { set, probe, seekable, rank: 5, unique: true, equality: false }
    }

    /// A chain: rids whose reference (through each hop) reaches a target that
    /// matches the terminal parameter.
    fn chain(&self, tid: i64, c: &ChainParameter) -> Result<Criterion> {
        // Walk outward: the set of target rids at the end, then back hop by hop.
        let last = c.links.last().expect("a chain has a link");
        let ttid = self.tid(&last.target_type, false)?.unwrap_or(-1);
        let (prefix, value) = crate::search_executor::split_prefix(&c.target_param_type, &c.value);
        let terminal = SearchParameter {
            name: c.target_param.clone(),
            value,
            modifier: None,
            prefix,
            param_type: c.target_param_type.clone(),
        };
        let terminal_c = self.parameter(ttid, &terminal)?;
        let mut targets = terminal_c.set;
        // The per-candidate check, built from the end: each hop follows the
        // candidate's own references (v_ref_r) to a target checked by the
        // next hop's probe — no set of all targets is built.
        let mut probe = terminal_c.probe;
        for i in (0..c.links.len()).rev() {
            let hop_tid = if i == 0 { tid } else { self.tid(&c.links[i - 1].target_type, false)?.unwrap_or(-1) };
            let hop_pid = self.pid(hop_tid, &c.links[i].reference_param, None)?.map(|p| p.0).unwrap_or(-1);
            let inner = probe.with_rid(&format!("c{i}.target"));
            probe = Sql::new(
                format!("EXISTS (SELECT 1 FROM v_ref c{i} INDEXED BY v_ref_r WHERE c{i}.rid = ?X? AND c{i}.pid = ? AND {})", inner.text),
                [vec![Arg::Int(hop_pid)], inner.args].concat(),
            );
        }
        // Hops before the last: each set is the rids of hop i's type whose
        // reference param points into the next set.
        for i in (1..c.links.len()).rev() {
            let hop_tid = self.tid(&c.links[i - 1].target_type, false)?.unwrap_or(-1);
            let pid = self.pid(hop_tid, &c.links[i].reference_param, None)?.map(|p| p.0).unwrap_or(-1);
            targets = Sql::new(
                format!("SELECT rid FROM v_ref NOT INDEXED WHERE pid = ? AND target IN ({})", targets.text),
                [vec![Arg::Int(pid)], targets.args].concat(),
            );
        }
        let pid = self.pid(tid, &c.links[0].reference_param, None)?.map(|p| p.0).unwrap_or(-1);
        let multi: i64 = self
            .conn
            .query_row("SELECT multi FROM param WHERE pid = ?1", params![pid], |r| r.get(0))
            .optional()?
            .unwrap_or(0);
        let set = Sql::new(
            format!("SELECT rid FROM v_ref NOT INDEXED WHERE pid = ? AND target IN ({})", targets.text),
            [vec![Arg::Int(pid)], targets.args.clone()].concat(),
        );
        // Seekable: a chain whose targets are few (a name, an identifier)
        // is the best driver; the bounded probe count says so.
        Ok(Criterion { set, probe, seekable: true, rank: 5, unique: multi == 0, equality: false })
    }

    /// `_has`: rids of the searched type that a matching source resource
    /// references (existing resources of this type only).
    fn has(&self, tid: i64, h: &HasParameter) -> Result<Criterion> {
        let stid = self.tid(&h.source_type, false)?.unwrap_or(-1);
        let (prefix, value) = crate::search_executor::split_prefix(&h.target_param_type, &h.value);
        let inner = SearchParameter {
            name: h.target_param.clone(),
            value,
            modifier: None,
            prefix,
            param_type: h.target_param_type.clone(),
        };
        let inner_c = self.parameter(stid, &inner)?;
        let sources = inner_c.set;
        let pid = self.pid(stid, &h.reference_param, None)?.map(|p| p.0).unwrap_or(-1);
        // Driving: walk the matching sources, follow their references to
        // existing resources of this type, each once — a page stops early
        // instead of materializing every target first.
        let set = Sql::new(
            format!(
                "SELECT DISTINCT v.target AS rid FROM ({}) s \
                 CROSS JOIN v_ref v INDEXED BY v_ref_r ON v.rid = s.rid AND v.pid = ? \
                 CROSS JOIN rkey k ON k.rid = v.target \
                 WHERE k.tid = ? AND k.present = 1",
                sources.text
            ),
            [sources.args, vec![Arg::Int(pid), Arg::Int(tid)]].concat(),
        );
        // Per candidate: the sources referencing it (v_ref_t), each checked
        // against the inner criterion — a handful of rows, not every source.
        let check = inner_c.probe.with_rid("h.rid");
        let probe = Sql::new(
            format!("EXISTS (SELECT 1 FROM v_ref h INDEXED BY v_ref_t WHERE h.target = ?X? AND h.pid = ? AND {})", check.text),
            [vec![Arg::Int(pid)], check.args].concat(),
        );
        Ok(Criterion { set, probe, seekable: true, rank: 5, unique: true, equality: false })
    }
}

/// Whether a sort walk should test rows with probes rather than membership
/// (see [`IndexV2::walk_sorted`]): `matches` of `size`, `want` rows wanted.
#[cfg(not(test))]
fn walk_by_probe(matches: f64, want: f64, size: f64) -> bool {
    matches * matches * 0.4 > want * size * 2.5
}

/// In tests both ways run, alternating with the page size.
#[cfg(test)]
fn walk_by_probe(_matches: f64, want: f64, _size: f64) -> bool {
    want as u64 % 2 == 1
}

/// The condition on `val` for a number (`prefix`, FHIR's implicit precision:
/// `eq100` is [99.5, 100.5)), its arguments and whether it seeks.
fn number_condition(prefix: Option<&str>, v: &str) -> Option<(String, Vec<Arg>, bool)> {
    let (val, half) = number_with_precision(v.trim())?;
    let (cond, args): (&str, Vec<f64>) = match prefix.unwrap_or("eq") {
        "ne" => ("(val < ? OR val >= ?)", vec![val - half, val + half]),
        "gt" | "sa" => ("val > ?", vec![val]),
        "ge" => ("val >= ?", vec![val]),
        "lt" | "eb" => ("val < ?", vec![val]),
        "le" => ("val <= ?", vec![val]),
        "ap" => {
            let d = (val.abs() * 0.1).max(half);
            ("val >= ? AND val <= ?", vec![val - d, val + d])
        }
        _ => ("val >= ? AND val < ?", vec![val - half, val + half]),
    };
    Some((cond.to_string(), args.into_iter().map(Arg::Real).collect(), prefix != Some("ne")))
}

fn table_for(kind: &str) -> &'static str {
    match kind {
        "string" => "v_string",
        "date" => "v_date",
        "number" => "v_number",
        "quantity" => "v_quantity",
        "reference" => "v_ref",
        _ => "v_token",
    }
}

/// `Type/id` (also `Type/id/_history/n`) → (type, id); `None` for anything else.
fn local_reference(v: &str) -> Option<(&str, &str)> {
    if v.contains("://") || v.starts_with("urn:") || v.starts_with('#') {
        return None;
    }
    let mut parts = v.split('/');
    let t = parts.next()?;
    let id = parts.next()?;
    (!t.is_empty() && !id.is_empty() && t.starts_with(|c: char| c.is_ascii_uppercase())).then_some((t, id))
}

fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

fn number_with_precision(value: &str) -> Option<(f64, f64)> {
    let v: f64 = value.parse().ok().filter(|n: &f64| n.is_finite())?;
    let lower = value.to_ascii_lowercase();
    let (mantissa, exp) = match lower.split_once('e') {
        Some((m, e)) => (m, e.parse::<i32>().ok()?),
        None => (lower.as_str(), 0),
    };
    let decimals = mantissa.split_once('.').map_or(0, |(_, f)| f.len() as i32);
    Some((v, 0.5 * 10f64.powi(exp - decimals)))
}
