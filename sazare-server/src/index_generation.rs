//! Search-index generations: rebuilding the index beside the live one, then
//! switching to it, without stopping searches or writes.
//! (`design/search-index-v2.md` §4.5.)
//!
//! - Generation 0 is `search_index.sqlite` (with `.fts.sqlite`); generation N is
//!   `search_index.gN.sqlite`. A generation counts once its `index_meta` says
//!   `ready = 1`; on start the newest ready one is used and unfinished ones are
//!   deleted (the build restarts). Nothing is renamed or deleted while open.
//! - Build: the writer starts journaling which resources it writes; the store is
//!   read type by type in keyset pages into the new generation.
//! - Drain: the journal is taken (under the write gate, so no write is between
//!   its index and store steps) and replayed from the store, repeatedly; once
//!   little is left, the final drain and the switch happen under the gate.
//! - The old generation's files are deleted when its last reader is done.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sazare_store::{IndexReaders, SearchIndex};
use serde_json::{json, Value};

use crate::AppState;

const READY_KEY: &str = "ready";
/// Resources read from the store per batch.
const BATCH: usize = 1000;
/// Journal entries below which the final drain (under the gate) is taken.
const FINAL_DRAIN: usize = 500;

/// Generation `generation` of the index at `base` (`.../search_index.sqlite`).
pub fn generation_path(base: &Path, generation: u32) -> PathBuf {
    if generation == 0 {
        return base.to_path_buf();
    }
    let stem = base.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "search_index".into());
    base.with_file_name(format!("{stem}.g{generation}.sqlite"))
}

/// Generation 0's path for an index file of any generation
/// (`.../search_index.g3.sqlite` → `.../search_index.sqlite`).
pub fn base_of(path: &Path) -> PathBuf {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if let Some(stem) = name.strip_suffix(".sqlite")
        && let Some((head, generation)) = stem.rsplit_once(".g")
        && !generation.is_empty()
        && generation.chars().all(|c| c.is_ascii_digit())
    {
        return path.with_file_name(format!("{head}.sqlite"));
    }
    path.to_path_buf()
}

fn generation_of(base: &Path, path: &Path) -> u32 {
    let stem = base.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    path.file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix(&format!("{stem}.g")))
        .and_then(|n| n.strip_suffix(".sqlite"))
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

fn is_ready(path: &Path) -> bool {
    SearchIndex::open_read_only(path)
        .ok()
        .and_then(|i| i.get_meta(READY_KEY).ok().flatten())
        .is_some_and(|v| v == "1")
}

/// The index generation to open: the newest ready one, or generation 0.
/// With `clean_up`, every other generation is deleted: unfinished ones (a
/// crashed build) and older ones a crash right after a switch left behind
/// (generation 0 included once a later one is in use).
pub fn current(base: &Path, clean_up: bool) -> PathBuf {
    let mut gens: Vec<(u32, PathBuf)> = std::fs::read_dir(base.parent().unwrap_or(Path::new(".")))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let g = generation_of(base, p);
            g > 0 && generation_path(base, g) == *p
        })
        .map(|p| (generation_of(base, &p), p))
        .collect();
    gens.sort_by_key(|(g, _)| std::cmp::Reverse(*g));
    let mut chosen = None;
    for (_, p) in gens {
        if chosen.is_none() && is_ready(&p) {
            chosen = Some(p);
        } else if clean_up {
            if chosen.is_none() {
                tracing::warn!("Removing unfinished index generation {}", p.display());
            } else {
                tracing::info!("Removing old index generation {}", p.display());
            }
            delete_files(&p);
        }
    }
    if clean_up && chosen.is_some() && base.exists() {
        tracing::info!("Removing old index generation {}", base.display());
        delete_files(base);
    }
    chosen.unwrap_or_else(|| base.to_path_buf())
}

/// An index file and everything that belongs to it.
pub fn delete_files(path: &Path) {
    let fts = path.with_extension("fts.sqlite");
    for p in [path.to_path_buf(), fts] {
        for suffix in ["", "-wal", "-shm"] {
            let f = PathBuf::from(format!("{}{suffix}", p.display()));
            let _ = std::fs::remove_file(f);
        }
    }
}

/// Mark an index as a complete generation.
pub fn mark_ready(index: &SearchIndex) {
    if let Err(e) = index.set_meta(READY_KEY, "1") {
        tracing::warn!("could not mark the index ready: {e}");
    }
}

/// Progress of a background rebuild, for `/health`, `/metrics` and `$reindex`.
#[derive(Default)]
pub struct RebuildStatus {
    inner: Mutex<Option<Progress>>,
    /// Set when the server shuts down: a build in progress stops at its next
    /// page instead of keeping the process alive until it is done (the
    /// runtime waits for blocking tasks on exit).
    cancelled: std::sync::atomic::AtomicBool,
}

/// The error a rebuild ends with when the server shuts down under it.
const CANCELLED: &str = "the server is shutting down";

#[derive(Clone)]
struct Progress {
    done: u64,
    total: u64,
    started: Instant,
    reason: String,
}

impl RebuildStatus {
    /// Stop any build in progress (the partial generation is deleted; the
    /// next start builds again). Further rebuilds don't start.
    pub fn cancel(&self) {
        self.cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn cancelled(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn begin(&self, total: u64, reason: &str) -> bool {
        if self.cancelled() {
            return false;
        }
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if g.is_some() {
            return false;
        }
        *g = Some(Progress { done: 0, total, started: Instant::now(), reason: reason.to_string() });
        true
    }
    fn set_total(&self, total: u64) {
        if let Some(p) = self.inner.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            p.total = total;
        }
    }
    fn add(&self, n: u64) {
        if let Some(p) = self.inner.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            p.done += n;
        }
    }
    fn end(&self) {
        *self.inner.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
    pub fn is_running(&self) -> bool {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }
    /// `(done, total)` while a rebuild runs.
    pub fn progress(&self) -> Option<(u64, u64)> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|p| (p.done, p.total))
    }
    /// For `/health`.
    pub fn to_json(&self) -> Value {
        match self.inner.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            None => json!({"state": "ready"}),
            Some(p) => json!({
                "state": "rebuilding",
                "reason": p.reason,
                "done": p.done,
                "total": p.total,
                "seconds": p.started.elapsed().as_secs(),
            }),
        }
    }
}

/// Re-index `ids` in `index` from the store (removing those that are gone).
fn replay(state: &AppState, index: &SearchIndex, ids: Vec<(String, String)>) {
    let mut updates = Vec::new();
    for (rt, id) in ids {
        match state.store.get(&rt, &id).ok().flatten().and_then(|d| serde_json::from_slice::<Value>(&d).ok()) {
            Some(resource) => updates.push(crate::handlers::index_update(&state.search_param_registry, &rt, &id, &resource)),
            None => {
                let _ = index.remove_index(&rt, &id);
            }
        }
        if updates.len() >= BATCH
            && let Err(e) = index.replace_many(&std::mem::take(&mut updates))
        {
            tracing::warn!("index replay failed: {e}");
        }
    }
    if !updates.is_empty()
        && let Err(e) = index.replace_many(&updates)
    {
        tracing::warn!("index replay failed: {e}");
    }
}

/// Fill `index` from every current resource in the store (blocking).
fn fill(state: &AppState, index: &SearchIndex) -> Result<u64, String> {
    let mut n = 0u64;
    for (rt, _) in state.store.count_by_type().map_err(|e| e.to_string())? {
        let mut after = String::new();
        loop {
            if state.rebuild.cancelled() {
                // Logged here: on shutdown the async caller doesn't run again.
                tracing::info!("Index rebuild stopped after {n} resources: {CANCELLED}; the partial index is deleted");
                return Err(CANCELLED.to_string());
            }
            let page = state.store.list_page(&rt, &after, BATCH).map_err(|e| e.to_string())?;
            let Some((last, _)) = page.last() else { break };
            after = last.clone();
            let updates: Vec<_> = page
                .iter()
                .filter_map(|(id, data)| {
                    let resource: Value = serde_json::from_slice(data).ok()?;
                    Some(crate::handlers::index_update(&state.search_param_registry, &rt, id, &resource))
                })
                .collect();
            index.replace_many(&updates).map_err(|e| e.to_string())?;
            n += page.len() as u64;
            state.rebuild.add(page.len() as u64);
            // Leave the disk to live traffic between batches.
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    Ok(n)
}

/// Build a new index generation from the store and switch to it, while the
/// server keeps serving from the current one. `Err` if a rebuild already runs.
pub async fn rebuild(state: Arc<AppState>, reason: &str) -> Result<(), String> {
    // Registered first: counting the store can take a while on a large one,
    // and /health must say "rebuilding" from the start.
    if !state.rebuild.begin(0, reason) {
        return Err("a rebuild is already running".into());
    }
    run_begun(state).await
}

/// Start a rebuild in the background; `false` when one is already running.
/// Registered before returning, so two callers can't both start one and an
/// immediate `/health` shows it.
pub fn spawn(state: Arc<AppState>, reason: &str) -> bool {
    if !state.rebuild.begin(0, reason) {
        return false;
    }
    tokio::spawn(async move {
        let _ = run_begun(state).await;
    });
    true
}

/// The rebuild after [`RebuildStatus::begin`] succeeded.
async fn run_begun(state: Arc<AppState>) -> Result<(), String> {
    let st = state.clone();
    let total = tokio::task::spawn_blocking(move || st.store.count_by_type())
        .await
        .ok()
        .and_then(|r| r.ok())
        .map(|counts| counts.iter().map(|(_, n)| *n as u64).sum())
        .unwrap_or(0);
    state.rebuild.set_total(total);
    let result = rebuild_inner(&state).await;
    state.rebuild.end();
    if let Err(e) = &result {
        if e != CANCELLED {
            tracing::error!("Index rebuild failed: {e}");
        }
        state.index.lock().await.stop_journal();
    }
    result
}

async fn rebuild_inner(state: &Arc<AppState>) -> Result<(), String> {
    // Next to the index in use (whatever the configuration says).
    let old_path = state.index_path();
    let base = base_of(&old_path);
    let generation = generation_of(&base, &old_path) + 1;
    let new_path = generation_path(&base, generation);
    delete_files(&new_path);
    let result = build_and_switch(state, old_path, new_path.clone(), generation).await;
    if result.is_err() {
        // Nothing switched: the partial generation is only disk space.
        delete_files(&new_path);
    }
    result
}

async fn build_and_switch(state: &Arc<AppState>, old_path: PathBuf, new_path: PathBuf, generation: u32) -> Result<(), String> {
    tracing::info!("Building search index generation {generation} ({}) in the background", new_path.display());
    let started = Instant::now();

    // Record writes from now on, before reading the store. Under the gate:
    // a write in flight (index written, store not yet) would otherwise be
    // in neither the journal nor, if the store page was already read, the
    // new index.
    {
        let _gate = state.write_gate.write().await;
        state.index.lock().await.start_journal();
    }

    let st = state.clone();
    let np = new_path.clone();
    let (new_index, n) = tokio::task::spawn_blocking(move || -> Result<(SearchIndex, u64), String> {
        let index = SearchIndex::open(&np).map_err(|e| e.to_string())?;
        match fill(&st, &index) {
            Ok(n) => Ok((index, n)),
            Err(e) => {
                // Here, not in the async caller: on shutdown the runtime waits
                // for this blocking task but drops async tasks unfinished.
                drop(index);
                delete_files(&np);
                Err(e)
            }
        }
    })
    .await
    .map_err(|e| e.to_string())??;
    tracing::info!("Index generation {generation}: {n} resources indexed in {:?}; catching up", started.elapsed());

    // Drain the writes that happened meanwhile, without stopping them. The
    // journal is taken under the gate (so every journaled write has reached
    // the store), replayed outside it.
    let new_index = Arc::new(Mutex::new(new_index));
    loop {
        let ids = {
            let _gate = state.write_gate.write().await;
            state.index.lock().await.take_journal()
        };
        if ids.len() < FINAL_DRAIN {
            let st = state.clone();
            let ni = new_index.clone();
            tokio::task::spawn_blocking(move || replay(&st, &ni.lock().unwrap_or_else(|e| e.into_inner()), ids))
                .await
                .map_err(|e| e.to_string())?;
            break;
        }
        let st = state.clone();
        let ni = new_index.clone();
        tokio::task::spawn_blocking(move || replay(&st, &ni.lock().unwrap_or_else(|e| e.into_inner()), ids))
            .await
            .map_err(|e| e.to_string())?;
    }

    // Switch: no write in flight, the last journal entries replayed.
    let fingerprint = crate::handlers::reindex::index_fingerprint(&state.search_param_registry);
    let gate = state.write_gate.write().await;
    let mut writer = state.index.lock().await;
    let rest = writer.take_journal();
    let new_index = Arc::try_unwrap(new_index)
        .map_err(|_| "index still shared".to_string())?
        .into_inner()
        .unwrap_or_else(|e| e.into_inner());
    // Off the async workers, still under the gate (the last few writes).
    let st = state.clone();
    let new_index = tokio::task::spawn_blocking(move || {
        replay(&st, &new_index, rest);
        new_index
    })
    .await
    .map_err(|e| e.to_string())?;
    new_index
        .set_meta(crate::handlers::reindex::FINGERPRINT_KEY, &fingerprint)
        .map_err(|e| e.to_string())?;
    mark_ready(&new_index);
    let readers = IndexReaders::open(&new_path, state.index_readers.capacity()).map_err(|e| e.to_string())?;
    let old_writer = std::mem::replace(&mut *writer, new_index);
    drop(writer);
    let old_readers = state.index_readers.replace(readers);
    state.set_index_path(new_path.clone());
    drop(gate);
    drop(old_writer);
    tracing::info!("Switched to search index generation {generation} after {:?}", started.elapsed());

    // The old generation goes once its last search has finished.
    tokio::task::spawn_blocking(move || {
        while Arc::strong_count(&old_readers) > 1 {
            std::thread::sleep(Duration::from_millis(100));
        }
        drop(old_readers);
        delete_files(&old_path);
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(path: &Path, ready: bool) {
        let index = SearchIndex::open(path).unwrap();
        if ready {
            mark_ready(&index);
        }
    }

    #[test]
    fn current_keeps_only_the_newest_ready_generation() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("search_index.sqlite");
        make(&base, false);
        make(&generation_path(&base, 1), true);
        make(&generation_path(&base, 2), true);
        make(&generation_path(&base, 3), false);

        // Without clean-up (backup): nothing deleted.
        assert_eq!(current(&base, false), generation_path(&base, 2));
        assert!(base.exists() && generation_path(&base, 1).exists() && generation_path(&base, 3).exists());

        // Startup: the unfinished g3 and the older g1 and g0 go, with their full text.
        assert_eq!(current(&base, true), generation_path(&base, 2));
        let left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| !n.ends_with("-wal") && !n.ends_with("-shm"))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        assert_eq!(left, ["search_index.g2.fts.sqlite", "search_index.g2.sqlite"]);
    }

    #[test]
    fn current_falls_back_to_generation_zero() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("search_index.sqlite");
        make(&base, false);
        make(&generation_path(&base, 1), false);
        assert_eq!(current(&base, true), base);
        assert!(base.exists() && !generation_path(&base, 1).exists());
    }
}
