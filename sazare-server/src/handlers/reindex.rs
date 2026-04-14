use axum::{extract::State, http::StatusCode, response::Json};
use sazare_core::SearchParamRegistry;
use sazare_store::{IndexBuilder, SearchIndex, SqliteStore};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::AppState;

pub struct ReindexSummary {
    pub resources_indexed: usize,
    pub entries_written: usize,
}

/// Rebuild the search index from the resource store in-place.
/// Clears existing entries, then re-extracts indices for every resource.
pub fn perform_reindex(
    store: &SqliteStore,
    index: &SearchIndex,
    registry: &SearchParamRegistry,
) -> Result<ReindexSummary, String> {
    index.clear_all().map_err(|e| format!("clear index: {}", e))?;

    let all = store.list_all(None).map_err(|e| format!("list resources: {}", e))?;
    let mut resources_indexed = 0usize;
    let mut entries_written = 0usize;

    for (resource_type, id, bytes) in all {
        let resource: Value = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("Skipping {}/{}: parse error: {}", resource_type, id, e);
                continue;
            }
        };
        let indices = IndexBuilder::extract_indices_with_registry(registry, &resource_type, &resource);
        for (param_name, param_type, value, system) in &indices {
            if let Err(e) = index.add_index(
                &resource_type,
                &id,
                param_name,
                param_type,
                Some(value),
                system.as_deref(),
            ) {
                tracing::warn!("add_index {}/{} {}: {}", resource_type, id, param_name, e);
            }
        }
        entries_written += indices.len();
        resources_indexed += 1;
    }

    Ok(ReindexSummary { resources_indexed, entries_written })
}

/// POST /$reindex — admin endpoint to rebuild the search index.
pub async fn reindex(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let index = state.index.lock().await;
    let summary = perform_reindex(&state.store, &index, &state.search_param_registry)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"resourceType": "OperationOutcome",
                    "issue": [{"severity": "error", "code": "exception", "diagnostics": e}]})),
            )
        })?;

    tracing::info!(
        "Reindex complete: {} resources, {} index entries",
        summary.resources_indexed,
        summary.entries_written
    );

    Ok(Json(json!({
        "resourceType": "Parameters",
        "parameter": [
            {"name": "resourcesIndexed", "valueInteger": summary.resources_indexed},
            {"name": "entriesWritten", "valueInteger": summary.entries_written},
        ]
    })))
}
