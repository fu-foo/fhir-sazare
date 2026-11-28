//! The one way a resource body is made ready to store. Every write path —
//! create, update, patch, conditional update, batch and transaction entries,
//! `$import`, the demo data — passes its body here, so a rule added here holds
//! for all of them (conditional references were resolved on three paths and
//! stored as written on the others).
//!
//! In order: conditional references (`Patient?identifier=…`) become the one
//! resource each finds, searched as the caller; then the resource is checked
//! against R4 and its profiles; then a Subscription's own rules. The caller
//! applies its compartment rules to what comes back, so they judge the
//! references as stored.

use axum::http::StatusCode;
use sazare_core::operation_outcome::IssueType;
use sazare_core::validation::{validate_resource_all_phases, ValidationResult};
use sazare_core::OperationOutcome;
use serde_json::Value;

use crate::auth::AuthUser;
use crate::AppState;

/// Why a body can't be stored, and the status to answer with.
#[derive(Debug)]
pub struct Refused {
    pub status: StatusCode,
    pub outcome: OperationOutcome,
}

impl Refused {
    /// As a handler's error: the status and the outcome.
    pub fn into_error(self) -> (StatusCode, axum::Json<Value>) {
        (self.status, axum::Json(serde_json::json!(self.outcome)))
    }

    fn new(status: StatusCode, code: IssueType, msg: impl Into<String>) -> Self {
        Refused { status, outcome: OperationOutcome::error(code, msg.into()) }
    }
}

/// Resolve `body`'s conditional references, then validate it.
pub async fn prepare(state: &AppState, auth_user: Option<&AuthUser>, body: &mut Value) -> Result<ValidationResult, Refused> {
    resolve_conditional_references(state, auth_user, body).await?;
    let result = validate_resource_all_phases(body, &state.profile_registry, &state.terminology_registry)
        .map_err(|outcome| Refused { status: StatusCode::BAD_REQUEST, outcome })?;
    if body.get("resourceType").and_then(|v| v.as_str()) == Some("Subscription") {
        crate::subscription::validate_subscription(body, &state.search_param_registry)
            .map_err(|e| Refused::new(StatusCode::BAD_REQUEST, IssueType::Invalid, e))?;
    }
    Ok(result)
}

/// Conditional references (R4 http.html#trules) replaced by the one resource
/// each search finds; none or several is a 412. The search runs as the caller
/// — whose scope must allow searching the referenced type — so a token
/// resolves only what it could have searched for.
async fn resolve_conditional_references(state: &AppState, auth_user: Option<&AuthUser>, resource: &mut Value) -> Result<(), Refused> {
    let refs = sazare_core::reference::conditional_references(resource);
    if refs.is_empty() {
        return Ok(());
    }
    let mut map = std::collections::HashMap::new();
    for r in refs {
        let (t, q) = r.split_once('?').expect("a conditional reference has a ?");
        if let Some(user) = auth_user
            && user.grant(t, 's').is_none()
        {
            return Err(Refused::new(
                StatusCode::FORBIDDEN,
                IssueType::Forbidden,
                format!("The conditional reference '{r}' searches {t}, which the token's scope doesn't allow"),
            ));
        }
        let found = match crate::conditional_create_check(state, auth_user, t, q).await {
            crate::ConditionalResult::Exists(existing) => existing.get("id").and_then(|v| v.as_str()).map(|id| format!("{t}/{id}")),
            crate::ConditionalResult::NoMatch => None,
            crate::ConditionalResult::MultipleMatches => {
                return Err(Refused::new(
                    StatusCode::PRECONDITION_FAILED,
                    IssueType::MultipleMatches,
                    format!("The conditional reference '{r}' matches more than one {t}"),
                ));
            }
            crate::ConditionalResult::SearchError(e) => {
                return Err(Refused::new(StatusCode::BAD_REQUEST, IssueType::Invalid, format!("The conditional reference '{r}': {e}")));
            }
        };
        let Some(found) = found else {
            return Err(Refused::new(StatusCode::PRECONDITION_FAILED, IssueType::NotFound, format!("The conditional reference '{r}' matches no {t}")));
        };
        map.insert(r, found);
    }
    crate::bundle::resolve_references(resource, &map);
    Ok(())
}
