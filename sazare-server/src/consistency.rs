//! Keeping the search index in step with the store across crashes.
//!
//! The store and the index are separate SQLite databases: a write commits to
//! one, then the other, and a crash (or a power loss) between the two leaves
//! them apart — a stored resource the index doesn't find, or index rows for a
//! write that never reached the store. Two records close that gap:
//!
//! - the store numbers every change to its current resources (`changes`,
//!   written by triggers in the same transaction as the change);
//! - the index notes every resource it writes (`recent`), in the same
//!   transaction as the rows.
//!
//! Every minute and at shutdown, with no write in flight (the write gate held
//! exclusively), the index records the store's last change number as the
//! point it matches ([`mark`]); the notes and the older changes are dropped.
//! At startup, [`catch_up`] indexes again, from the store, the resources
//! changed after that point and those the index noted since: whatever a crash
//! cut in two is put back to what is stored.

use std::sync::Arc;
use std::time::Duration;

use crate::AppState;

/// How often the index's matching point moves forward.
const MARK_EVERY: Duration = Duration::from_secs(60);

/// Index again, from the store, every resource a crash may have left
/// different in the two (before serving). Returns how many were looked at.
pub async fn catch_up(state: &Arc<AppState>) -> Result<usize, String> {
    let (synced, recent) = {
        let index = state.index.lock().await;
        if index.is_v1() {
            return Ok(0);
        }
        (index.synced().map_err(|e| e.to_string())?, index.recent().map_err(|e| e.to_string())?)
    };
    let Some(synced) = synced else {
        // First start with these records (an index from before them, or a
        // new one): nothing to go by. From here on, there is.
        mark(state).await?;
        return Ok(0);
    };
    let mut keys = state.store.changed_since(synced).map_err(|e| e.to_string())?;
    keys.extend(recent);
    keys.sort();
    keys.dedup();
    for (resource_type, id) in &keys {
        crate::handlers::crud::restore_index(state, resource_type, id).await;
    }
    if !keys.is_empty() {
        tracing::warn!(
            "Search index: {} resources written just before the last stop were indexed again from the store \
             (the server had not stopped cleanly)",
            keys.len()
        );
    }
    mark(state).await?;
    Ok(keys.len())
}

/// Record that the index matches the store as of now. Takes the write gate
/// exclusively for a moment (no write in flight).
pub async fn mark(state: &Arc<AppState>) -> Result<(), String> {
    let seq = {
        let _gate = state.write_gate.write().await;
        let index = state.index.lock().await;
        if index.is_v1() {
            return Ok(());
        }
        let seq = state.store.change_seq().map_err(|e| e.to_string())?;
        let moved = index.synced().map_err(|e| e.to_string())? != Some(seq);
        if moved || index.has_recent().map_err(|e| e.to_string())? {
            index.mark_synced(seq).map_err(|e| e.to_string())?;
        }
        seq
    };
    // Outside the gate: later changes are numbered higher and stay. (Also
    // when a rebuild's switch moved the point: the changes before it go.)
    state.store.forget_changes(seq).map_err(|e| e.to_string())
}

/// Move the matching point forward every [`MARK_EVERY`] for as long as the
/// server runs.
pub fn spawn_marker(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut every = tokio::time::interval(MARK_EVERY);
        every.tick().await;
        loop {
            every.tick().await;
            if let Err(e) = mark(&state).await {
                tracing::warn!("Search index: recording the point it matches the store failed: {e}");
            }
        }
    });
}
