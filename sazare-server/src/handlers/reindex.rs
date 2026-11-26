use axum::{extract::State, http::StatusCode, response::Json};
use sazare_core::SearchParamRegistry;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::AppState;

/// `index_meta` key holding [`index_fingerprint`].
pub const FINGERPRINT_KEY: &str = "param_fingerprint";

/// What an index built now would be made from: the index format plus every
/// search parameter definition. A stored fingerprint that differs means the
/// index is stale (e.g. new built-in parameters after an upgrade, or a
/// changed `searchparameters/` directory).
pub fn index_fingerprint(registry: &SearchParamRegistry) -> String {
    format!("format={}\n{}", sazare_store::INDEX_FORMAT, registry.fingerprint())
}

/// POST /$reindex — rebuild the search index in the background: the server
/// keeps serving from the current index and switches when the new one is
/// complete. 202 with the progress location; 409 while a rebuild runs.
pub async fn reindex(
    State(state): State<Arc<AppState>>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    if !crate::index_generation::spawn(state.clone(), "requested with $reindex") {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({"resourceType": "OperationOutcome", "issue": [{"severity": "error", "code": "conflict",
                "diagnostics": "A rebuild of the search index is already running (GET /health shows its progress)"}]})),
        ));
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"resourceType": "OperationOutcome", "issue": [{"severity": "information", "code": "informational",
            "diagnostics": "Rebuilding the search index in the background; searches keep using the current index until it is done (progress: GET /health)"}]})),
    ))
}
