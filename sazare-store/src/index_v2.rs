//! Search index v2 (design/search-index-v2.md §4): integer keys, one table per
//! value kind, set operations in SQLite.
//!
//! This first cut is about correctness against the reference model
//! (`differential_tests`): every query is one SQL statement driven by its most
//! selective criterion, the others checked per candidate. The ordered-walk
//! optimizations for `_sort` (§4.3) come next; sorting here gathers the
//! candidates' keys and sorts them.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;

use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use sazare_core::{ChainParameter, HasParameter, SearchParamType, SearchParameter, SearchQuery, SortKey};

use crate::sqlite_index::{fhir_date_range, IndexUpdate};
use crate::Result;

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
CREATE TABLE IF NOT EXISTS v_ref (pid INT, target INT, rid INT, PRIMARY KEY (pid, target, rid)) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS v_ref_r ON v_ref (rid, pid);
CREATE INDEX IF NOT EXISTS v_ref_t ON v_ref (target, pid, rid);
CREATE TABLE IF NOT EXISTS v_ref_text (pid INT, value TEXT, rid INT, PRIMARY KEY (pid, value, rid)) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS v_ref_text_r ON v_ref_text (rid, pid);
"#;

const VALUE_TABLES: [&str; 6] = ["v_token", "v_string", "v_date", "v_number", "v_ref", "v_ref_text"];

/// A probe count at or above this means "large": not worth counting further.
const PROBE_CAP: i64 = 10_001;

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
}

pub struct IndexV2 {
    conn: Connection,
    tids: RefCell<HashMap<String, i64>>,
    pids: RefCell<HashMap<(i64, String), (i64, String)>>,
    sids: RefCell<HashMap<String, i64>>,
}

impl IndexV2 {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn, tids: Default::default(), pids: Default::default(), sids: Default::default() })
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

    /// The rid of `type/id`, creating the key; `present` marks a stored resource.
    fn rid(&self, tid: i64, id: &str, present: bool) -> Result<i64> {
        self.conn.execute(
            "INSERT OR IGNORE INTO rkey (tid, id, present) VALUES (?1, ?2, 0)",
            params![tid, id],
        )?;
        let (rid, was): (i64, i64) =
            self.conn.query_row("SELECT rid, present FROM rkey WHERE tid = ?1 AND id = ?2", params![tid, id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
        if present && was == 0 {
            self.conn.execute("UPDATE rkey SET present = 1 WHERE rid = ?1", params![rid])?;
            self.conn.execute(
                "INSERT INTO type_count (tid, n) VALUES (?1, 1) ON CONFLICT (tid) DO UPDATE SET n = n + 1",
                params![tid],
            )?;
        }
        Ok(rid)
    }

    // ---- writes -------------------------------------------------------------

    /// Replace the index rows of each resource (one transaction).
    pub fn replace_many(&self, items: &[IndexUpdate]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for u in items {
            let tid = self.tid(&u.resource_type, true)?.expect("created");
            let rid = self.rid(tid, &u.resource_id, true)?;
            self.clear_rows(rid)?;
            let mut per_pid: HashMap<i64, usize> = HashMap::new();
            for (name, kind, value, system) in &u.entries {
                let (pid, _) = self.pid(tid, name, Some(kind))?.expect("created");
                if self.write_value(pid, kind, value, system.as_deref(), rid)? {
                    *per_pid.entry(pid).or_default() += 1;
                }
            }
            for (pid, n) in per_pid {
                if n > 1 {
                    self.conn.execute("UPDATE param SET multi = 1 WHERE pid = ?1 AND multi = 0", params![pid])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Write one value; `false` when it isn't stored (e.g. the bare-id copy of
    /// a reference, which v2 resolves through the key table instead).
    fn write_value(&self, pid: i64, kind: &str, value: &str, system: Option<&str>, rid: i64) -> Result<bool> {
        match kind {
            "string" => {
                self.conn.execute(
                    "INSERT OR IGNORE INTO v_string (pid, norm, rid, exact) VALUES (?1, ?2, ?3, ?4)",
                    params![pid, value.to_lowercase(), rid, value],
                )?;
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
                self.conn.execute(
                    "INSERT OR IGNORE INTO v_date (pid, lo, hi, rid) VALUES (?1, ?2, ?3, ?4)",
                    params![pid, lo, hi, rid],
                )?;
                if hi != i64::MAX {
                    self.conn.execute(
                        "UPDATE param SET max_span = ?2 WHERE pid = ?1 AND max_span < ?2",
                        params![pid, hi - lo],
                    )?;
                }
            }
            "number" => {
                let Some(v) = value.parse::<f64>().ok().filter(|v| v.is_finite()) else { return Ok(false) };
                self.conn.execute(
                    "INSERT OR IGNORE INTO v_number (pid, val, rid) VALUES (?1, ?2, ?3)",
                    params![pid, v, rid],
                )?;
            }
            "reference" => match local_reference(value) {
                Some((ttype, tid_text)) => {
                    let ttid = self.tid(ttype, true)?.expect("created");
                    let target = self.rid(ttid, tid_text, false)?;
                    self.conn.execute(
                        "INSERT OR IGNORE INTO v_ref (pid, target, rid) VALUES (?1, ?2, ?3)",
                        params![pid, target, rid],
                    )?;
                }
                // The bare-id copy v1 extraction adds next to "Type/id".
                None if !value.contains(['/', ':', '#']) => return Ok(false),
                None => {
                    self.conn.execute(
                        "INSERT OR IGNORE INTO v_ref_text (pid, value, rid) VALUES (?1, ?2, ?3)",
                        params![pid, value, rid],
                    )?;
                }
            },
            _ => {
                let sid = self.sid(system, true)?.expect("created");
                self.conn.execute(
                    "INSERT OR IGNORE INTO v_token (pid, code, sid, rid) VALUES (?1, ?2, ?3, ?4)",
                    params![pid, value, sid, rid],
                )?;
            }
        }
        Ok(true)
    }

    fn clear_rows(&self, rid: i64) -> Result<()> {
        for t in VALUE_TABLES {
            self.conn.execute(&format!("DELETE FROM {t} WHERE rid = ?1"), params![rid])?;
        }
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

    /// Ids matching `query` (no paging), in no particular order.
    pub fn search(&self, resource_type: &str, query: &SearchQuery) -> std::result::Result<Vec<String>, String> {
        let Some(sql) = self.compile(resource_type, query).map_err(|e| e.to_string())? else {
            return Ok(Vec::new());
        };
        self.ids_of(&sql, None)
    }

    /// The page `query` asks for (`_sort`, `_offset`, `_count`) and the total.
    pub fn search_with_total(
        &self,
        resource_type: &str,
        query: &SearchQuery,
    ) -> std::result::Result<(Vec<String>, usize), String> {
        let Some(sql) = self.compile(resource_type, query).map_err(|e| e.to_string())? else {
            return Ok((Vec::new(), 0));
        };
        let tid = self.tid(resource_type, false).map_err(|e| e.to_string())?.unwrap_or(-1);
        let filtered = !(query.parameters.is_empty() && query.chain_parameters.is_empty() && query.has_parameters.is_empty());
        // Sorted over a large set: walk the first key's index in order and stop
        // at the page, instead of gathering every candidate's key.
        if !query.sort.is_empty() {
            let large = !filtered || self.probe_count(&sql).map_err(|e| e.to_string())? > GATHER_MAX;
            if large {
                let want = query.offset.unwrap_or(0) + query.count.unwrap_or(usize::MAX / 2);
                let ordered = self
                    .walk_sorted(tid, &query.sort, filtered.then_some(&sql), want)
                    .map_err(|e| e.to_string())?;
                let total = self.count(tid, filtered.then_some(&sql)).map_err(|e| e.to_string())?;
                let page: Vec<i64> = ordered.into_iter().skip(query.offset.unwrap_or(0)).collect();
                let ids = self.ids_for(&page).map_err(|e| e.to_string())?;
                return Ok((ids, total));
            }
        }
        let mut rids = self.rids(&sql).map_err(|e| e.to_string())?;
        let total = rids.len();
        if !query.sort.is_empty() {
            self.sort_rids(tid, &query.sort, &mut rids).map_err(|e| e.to_string())?;
        }
        let offset = query.offset.unwrap_or(0).min(rids.len());
        let mut page: Vec<i64> = rids.drain(offset..).collect();
        if let Some(c) = query.count {
            page.truncate(c);
        }
        let ids = self.ids_for(&page).map_err(|e| e.to_string())?;
        Ok((ids, total))
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
    fn walk_sorted(&self, tid: i64, keys: &[SortKey], filter: Option<&Sql>, want: usize) -> Result<Vec<i64>> {
        let first = &keys[0];
        let member = filter.map(|f| (format!(" AND rid IN ({})", f.text), f.args.clone())).unwrap_or_default();
        let mut out: Vec<i64> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        if let Some((pid, kind)) = self.pid(tid, &first.name, None)? {
            let (table, col) = match kind.as_str() {
                "date" => ("v_date", "lo"),
                "number" => ("v_number", "val"),
                "string" => ("v_string", "norm"),
                _ => ("v_token", "code"),
            };
            let dir = if first.descending { "DESC" } else { "ASC" };
            let sql = format!("SELECT rid, {col} FROM {table} NOT INDEXED WHERE pid = ?{} ORDER BY {col} {dir}", member.0);
            let mut args = vec![Arg::Int(pid)];
            args.extend(member.1.clone());
            let mut stmt = self.conn.prepare(&sql)?;
            let mut rows = stmt.query(params_from_iter(args.iter()))?;
            let mut group: Vec<i64> = Vec::new();
            let mut group_key: Option<rusqlite::types::Value> = None;
            let flush = |group: &mut Vec<i64>, out: &mut Vec<i64>| -> Result<()> {
                if group.len() > 1 && keys.len() > 1 {
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
        Ok(out)
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
                    "string" => ("SELECT rid, norm FROM v_string INDEXED BY v_string_r WHERE rid = ?1 AND pid = ?2", false),
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
    fn compile(&self, resource_type: &str, query: &SearchQuery) -> Result<Option<Sql>> {
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
            return Ok(Some(all_of_type));
        }
        // Drive from the smallest seekable criterion (bounded probe counts;
        // ties and saturated counts by rank); the others are checked per row.
        let mut best: Option<(usize, i64, u8)> = None;
        for (i, c) in crits.iter().enumerate() {
            if !c.seekable {
                continue;
            }
            let n = self.probe_count(&c.set)?;
            let better = match best {
                None => true,
                Some((_, bn, br)) => n < bn || (n == bn && c.rank < br),
            };
            if better {
                best = Some((i, n, c.rank));
            }
        }
        let (driver, rest): (Sql, Vec<&Criterion>) = match best {
            Some((i, _, _)) => (crits[i].set.clone(), crits.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, c)| c).collect()),
            None => (all_of_type, crits.iter().collect()),
        };
        let mut text = format!("SELECT DISTINCT d.rid FROM ({}) d", driver.text);
        let mut args = driver.args;
        for (k, c) in rest.iter().enumerate() {
            text.push_str(if k == 0 { " WHERE " } else { " AND " });
            let p = c.probe.with_rid("d.rid");
            text.push_str(&p.text);
            args.extend(p.args);
        }
        Ok(Some(Sql::new(text, args)))
    }

    fn probe_count(&self, set: &Sql) -> Result<i64> {
        let sql = format!("SELECT count(*) FROM ({} LIMIT {PROBE_CAP})", set.text);
        Ok(self.conn.query_row(&sql, params_from_iter(set.args.iter()), |r| r.get(0))?)
    }

    /// One search parameter (with its modifier and comma alternatives).
    fn parameter(&self, tid: i64, p: &SearchParameter) -> Result<Criterion> {
        let pid = self.pid(tid, &p.name, None)?;
        let all = Sql::new("SELECT rid FROM rkey INDEXED BY rkey_live WHERE tid = ? AND present = 1", vec![Arg::Int(tid)]);
        let none = Criterion { set: Sql::new("SELECT 0 WHERE 0", vec![]), probe: Sql::new("0", vec![]), seekable: true, rank: 0 };
        match p.modifier.as_deref() {
            Some("missing") => {
                let want_missing = p.value.trim() == "true";
                let has = match &pid {
                    Some((pid, kind)) => {
                        let t = table_for(kind);
                        Sql::new(format!("EXISTS (SELECT 1 FROM {t} INDEXED BY {t}_r WHERE rid = ?X? AND pid = ?)"), vec![Arg::Int(*pid)])
                    }
                    None => Sql::new("0", vec![]),
                };
                let probe = if want_missing { Sql::new(format!("NOT {}", has.text), has.args) } else { has };
                let set = Sql::new(format!("{} AND {}", all.text, probe.with_rid("rkey.rid").text), [all.args, probe.args.clone()].concat());
                return Ok(Criterion { set, probe, seekable: false, rank: 6 });
            }
            Some("not") => {
                let mut inner = p.clone();
                inner.modifier = None;
                let c = self.parameter(tid, &inner)?;
                let probe = Sql::new(format!("NOT ({})", c.probe.text), c.probe.args);
                let set = Sql::new(format!("{} AND {}", all.text, probe.with_rid("rkey.rid").text), [all.args, probe.args.clone()].concat());
                return Ok(Criterion { set, probe, seekable: false, rank: 6 });
            }
            _ => {}
        }
        let Some((pid, kind)) = pid else { return Ok(none) };
        let values: Vec<&str> = p.value.split(',').map(str::trim).filter(|v| !v.is_empty()).collect();
        let mut sets = Vec::new();
        let mut probes = Vec::new();
        let mut seekable = true;
        for v in values {
            let Some((t, cond, args, index, seek)) = self.condition(pid, &kind, p, v)? else { continue };
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
        Ok(Criterion { set, probe, seekable, rank })
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
                (cond.into(), args.into_iter().map(Arg::Int).collect(), index, true)
            }
            "number" => {
                let Some((val, half)) = number_with_precision(v) else { return Ok(None) };
                let (cond, lo, hi) = match p.prefix.as_deref().unwrap_or("eq") {
                    "ne" => ("(val < ? OR val >= ?)", val - half, val + half),
                    "gt" | "sa" => ("val > ? AND ? = ?", val, val),
                    "ge" => ("val >= ? AND ? = ?", val, val),
                    "lt" | "eb" => ("val < ? AND ? = ?", val, val),
                    "le" => ("val <= ? AND ? = ?", val, val),
                    "ap" => {
                        let d = (val.abs() * 0.1).max(half);
                        ("val >= ? AND val <= ?", val - d, val + d)
                    }
                    _ => ("val >= ? AND val < ?", val - half, val + half),
                };
                let args = if cond.contains("? = ?") {
                    vec![Arg::Real(lo), Arg::Real(hi), Arg::Real(hi)]
                } else {
                    vec![Arg::Real(lo), Arg::Real(hi)]
                };
                (cond.into(), args, None, true)
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

    /// A chain: rids whose reference (through each hop) reaches a target that
    /// matches the terminal parameter.
    fn chain(&self, tid: i64, c: &ChainParameter) -> Result<Criterion> {
        // Walk outward: the set of target rids at the end, then back hop by hop.
        let last = c.links.last().expect("a chain has a link");
        let ttid = self.tid(&last.target_type, false)?.unwrap_or(-1);
        let terminal = SearchParameter {
            name: c.target_param.clone(),
            value: c.value.clone(),
            modifier: None,
            prefix: None,
            param_type: c.target_param_type.clone(),
        };
        let mut targets = self.parameter(ttid, &terminal)?.set;
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
        let set = Sql::new(
            format!("SELECT rid FROM v_ref NOT INDEXED WHERE pid = ? AND target IN ({})", targets.text),
            [vec![Arg::Int(pid)], targets.args.clone()].concat(),
        );
        let probe = Sql::new(
            format!("EXISTS (SELECT 1 FROM v_ref INDEXED BY v_ref_r WHERE rid = ?X? AND pid = ? AND target IN ({}))", targets.text),
            [vec![Arg::Int(pid)], targets.args].concat(),
        );
        Ok(Criterion { set, probe, seekable: false, rank: 5 })
    }

    /// `_has`: rids of the searched type that a matching source resource
    /// references (existing resources of this type only).
    fn has(&self, tid: i64, h: &HasParameter) -> Result<Criterion> {
        let stid = self.tid(&h.source_type, false)?.unwrap_or(-1);
        let inner = SearchParameter {
            name: h.target_param.clone(),
            value: h.value.clone(),
            modifier: None,
            prefix: None,
            param_type: h.target_param_type.clone(),
        };
        let sources = self.parameter(stid, &inner)?.set;
        let pid = self.pid(stid, &h.reference_param, None)?.map(|p| p.0).unwrap_or(-1);
        let targets = Sql::new(
            format!("SELECT target FROM v_ref INDEXED BY v_ref_r WHERE pid = ? AND rid IN ({})", sources.text),
            [vec![Arg::Int(pid)], sources.args].concat(),
        );
        let set = Sql::new(
            format!(
                "SELECT rid FROM rkey WHERE tid = ? AND present = 1 AND rid IN ({})",
                targets.text
            ),
            [vec![Arg::Int(tid)], targets.args.clone()].concat(),
        );
        let probe = Sql::new(format!("?X? IN ({})", targets.text), targets.args);
        Ok(Criterion { set, probe, seekable: false, rank: 5 })
    }
}

fn table_for(kind: &str) -> &'static str {
    match kind {
        "string" => "v_string",
        "date" => "v_date",
        "number" => "v_number",
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
