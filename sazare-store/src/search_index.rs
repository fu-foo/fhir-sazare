//! The search index the server uses: index v2 ([`IndexV2`]), or — for an
//! index written by an older version — the v1 index ([`V1Index`]), which keeps
//! serving until its v2 replacement, built in the background as a new index
//! generation, takes over. Which one a file holds is read from its tables.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::Path;

use crate::index_v2::IndexV2;
use crate::sqlite_index::{IndexUpdate, ResourceText, V1Index};
use crate::Result;

pub(crate) enum Engine {
    V1(V1Index),
    V2(IndexV2),
}

pub struct SearchIndex {
    engine: Engine,
    /// While set, the resources whose index rows were written, so a new index
    /// generation being built beside this one can replay them
    /// ([`SearchIndex::start_journal`]).
    journal: RefCell<Option<HashSet<(String, String)>>>,
}

/// Whether the index file at `path` is a v1 index (its `search_index` table).
fn is_v1(path: &Path) -> Result<bool> {
    if path.as_os_str() == ":memory:" || !path.exists() {
        return Ok(false);
    }
    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'search_index')",
        [],
        |r| r.get(0),
    )?)
}

#[allow(clippy::result_large_err)]
impl SearchIndex {
    /// Open the index at `path`, creating a v2 index if there is none.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let engine = if is_v1(path)? { Engine::V1(V1Index::open(path)?) } else { Engine::V2(IndexV2::open(path)?) };
        Ok(Self { engine, journal: RefCell::new(None) })
    }

    /// Open an existing index read-only, for searches beside the writer.
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let engine = if is_v1(path)? {
            Engine::V1(V1Index::open_read_only(path)?)
        } else {
            Engine::V2(IndexV2::open_read_only(path)?)
        };
        Ok(Self { engine, journal: RefCell::new(None) })
    }

    pub(crate) fn engine(&self) -> &Engine {
        &self.engine
    }

    /// A v1 index (written by sazare 0.15 or before): to be rebuilt as v2.
    pub fn is_v1(&self) -> bool {
        matches!(self.engine, Engine::V1(_))
    }

    /// Start recording which resources this index writes (see `journal`).
    pub fn start_journal(&self) {
        *self.journal.borrow_mut() = Some(Default::default());
    }

    /// The resources written since the journal started or was last taken.
    pub fn take_journal(&self) -> Vec<(String, String)> {
        match self.journal.borrow_mut().as_mut() {
            Some(set) => set.drain().collect(),
            None => Vec::new(),
        }
    }

    pub fn stop_journal(&self) {
        *self.journal.borrow_mut() = None;
    }

    fn note(&self, resource_type: &str, resource_id: &str) {
        if let Some(set) = self.journal.borrow_mut().as_mut() {
            set.insert((resource_type.to_string(), resource_id.to_string()));
        }
    }

    /// Replace one resource's index rows and full text.
    pub fn replace_resource_index(
        &self,
        resource_type: &str,
        resource_id: &str,
        entries: &[(String, String, String, Option<String>)],
        text: &ResourceText,
    ) -> Result<()> {
        self.note(resource_type, resource_id);
        match &self.engine {
            Engine::V1(i) => i.replace_resource_index(resource_type, resource_id, entries, text),
            Engine::V2(i) => i.replace_one(resource_type, resource_id, entries, text),
        }
    }

    /// Replace many resources' rows in one transaction (bulk writes).
    pub fn replace_many(&self, items: &[IndexUpdate]) -> Result<()> {
        for u in items {
            self.note(&u.resource_type, &u.resource_id);
        }
        match &self.engine {
            Engine::V1(i) => i.replace_many(items),
            Engine::V2(i) => i.replace_many(items),
        }
    }

    /// Add one value to a resource's index rows (keeping its others).
    pub fn add_index(
        &self,
        resource_type: &str,
        resource_id: &str,
        param_name: &str,
        param_type: &str,
        value: Option<&str>,
        system: Option<&str>,
    ) -> Result<()> {
        self.note(resource_type, resource_id);
        match &self.engine {
            Engine::V1(i) => i.add_index(resource_type, resource_id, param_name, param_type, value, system),
            Engine::V2(i) => match value {
                Some(v) => i.add_value(resource_type, resource_id, param_name, param_type, v, system),
                None => Ok(()),
            },
        }
    }

    /// A resource was deleted.
    pub fn remove_index(&self, resource_type: &str, resource_id: &str) -> Result<()> {
        self.note(resource_type, resource_id);
        match &self.engine {
            Engine::V1(i) => i.remove_index(resource_type, resource_id),
            Engine::V2(i) => i.remove(resource_type, resource_id),
        }
    }

    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        match &self.engine {
            Engine::V1(i) => i.get_meta(key),
            Engine::V2(i) => i.get_meta(key),
        }
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        match &self.engine {
            Engine::V1(i) => i.set_meta(key, value),
            Engine::V2(i) => i.set_meta(key, value),
        }
    }

    /// No resource indexed (an existence check).
    pub fn is_empty(&self) -> Result<bool> {
        match &self.engine {
            Engine::V1(i) => i.is_empty(),
            Engine::V2(i) => i.is_empty(),
        }
    }

    /// Some resource has full text (an existence check).
    pub fn has_text(&self) -> Result<bool> {
        match &self.engine {
            Engine::V1(i) => i.has_text(),
            Engine::V2(i) => i.has_text(),
        }
    }

    /// Ids of `resource_type` whose reference parameter `param_name` points
    /// at `reference`.
    pub fn search_reference(&self, resource_type: &str, param_name: &str, reference: &str) -> Result<Vec<String>> {
        self.search_reference_any(resource_type, param_name, &[reference.to_string()])
    }

    /// Ids of `resource_type` whose reference parameter `param_name` points
    /// at any of `references`, each once.
    pub fn search_reference_any(&self, resource_type: &str, param_name: &str, references: &[String]) -> Result<Vec<String>> {
        match &self.engine {
            Engine::V1(i) => i.search_reference_any(resource_type, param_name, references),
            Engine::V2(i) => i.referencing(resource_type, param_name, references),
        }
    }
}

/// A pool of read-only [`SearchIndex`] connections for searches, so a slow
/// search (an exact total over millions of matches) doesn't hold the writer
/// — or other searches — up. Empty for an in-memory index (callers then use
/// the writer).
pub struct IndexReaders {
    pool: Vec<std::sync::Mutex<SearchIndex>>,
    next: std::sync::atomic::AtomicUsize,
}

impl IndexReaders {
    /// `n` read-only connections to the index at `path` (which must exist:
    /// open the writer first).
    pub fn open(path: impl AsRef<Path>, n: usize) -> Result<Self> {
        let path = path.as_ref();
        let mut pool = Vec::new();
        if path.as_os_str() != ":memory:" {
            for _ in 0..n {
                pool.push(std::sync::Mutex::new(SearchIndex::open_read_only(path)?));
            }
        }
        Ok(Self { pool, next: std::sync::atomic::AtomicUsize::new(0) })
    }

    /// No readers: every search uses the writer.
    pub fn none() -> Self {
        Self { pool: Vec::new(), next: std::sync::atomic::AtomicUsize::new(0) }
    }

    /// A reader to use now: the first free one, else (all busy) wait for the
    /// next in turn. `None` when the pool is empty.
    pub fn get(&self) -> Option<std::sync::MutexGuard<'_, SearchIndex>> {
        if self.pool.is_empty() {
            return None;
        }
        for r in &self.pool {
            if let Ok(g) = r.try_lock() {
                return Some(g);
            }
        }
        let i = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % self.pool.len();
        Some(self.pool[i].lock().unwrap_or_else(|e| e.into_inner()))
    }

    pub fn len(&self) -> usize {
        self.pool.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pool.is_empty()
    }
}

