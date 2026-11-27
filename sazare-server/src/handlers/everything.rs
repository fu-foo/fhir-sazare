use axum::{
    extract::{Path, Request, State},
    http::StatusCode,
    response::{Json, Response},
};
use sazare_core::{
    operation_outcome::IssueType,
    OperationOutcome,
};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::audit::{self, AuditContext};
use crate::auth::AuthUser;
use crate::compartment_check::{check_compartment_access, filter_mixed_by_compartment};
use crate::AppState;

/// Patient $everything (GET /Patient/{id}/$everything)
///
/// Returns a searchset Bundle containing the Patient and all resources
/// in the Patient's compartment.
pub async fn patient_everything(
    State(state): State<Arc<AppState>>,
    Path((resource_type, patient_id)): Path<(String, String)>,
    request: Request,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let audit_ctx = AuditContext::from_request(&request);
    let auth_user = request.extensions().get::<AuthUser>().cloned();

    if resource_type != "Patient" {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!(OperationOutcome::error(
                IssueType::NotSupported,
                format!("$everything is only supported for Patient, not {}", resource_type)
            ))),
        ));
    }

    // Load the Patient resource
    let patient_data = state.store.get("Patient", &patient_id).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!(OperationOutcome::storage_error(e.to_string()))),
        )
    })?;

    let patient_data = patient_data.ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(json!(OperationOutcome::not_found("Patient", &patient_id))),
        )
    })?;

    let patient: Value = serde_json::from_slice(&patient_data).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!(OperationOutcome::storage_error(e.to_string()))),
        )
    })?;

    // Compartment check: patient-scoped users can only access their own data
    check_compartment_access(auth_user.as_ref(), &state.compartment_def, "Patient", &patient)?;

    // Paging and `_type`; anything else ($everything's start/end/_since) is
    // a 400, not ignored.
    let mut count = 100usize;
    let mut offset = 0usize;
    let mut types: Option<Vec<String>> = None;
    let params: Vec<(String, String)> = serde_urlencoded::from_str(request.uri().query().unwrap_or("")).map_err(|e| {
        (StatusCode::BAD_REQUEST, Json(json!(OperationOutcome::error(IssueType::Invalid, format!("query: {e}")))))
    })?;
    for (k, v) in params {
        let bad = |what: &str| (StatusCode::BAD_REQUEST, Json(json!(OperationOutcome::error(IssueType::Invalid, what.to_string()))));
        match k.as_ref() {
            "_count" => count = v.trim().parse::<usize>().map_err(|_| bad("_count: expected a number"))?.min(1000),
            "_offset" => offset = v.trim().parse().map_err(|_| bad("_offset: expected a number"))?,
            "_type" => types = Some(v.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect()),
            other => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(json!(OperationOutcome::error(
                        IssueType::NotSupported,
                        format!("$everything: '{other}' is not supported (supported: _count, _offset, _type)")
                    ))),
                ));
            }
        }
    }
    let wanted = |t: &str| types.as_ref().is_none_or(|ts| ts.iter().any(|x| x == t));

    // Every resource of the compartment, as type and id only (the page's are
    // loaded below): the Patient first, then each compartment type the
    // caller's scopes let them read, found through the fields that link it
    // to the Patient.
    let (au, pid) = (auth_user.clone(), patient_id.clone());
    let types_wanted: Option<Vec<String>> = types.clone();
    let mut keys: Vec<(String, String)> = Vec::new();
    if wanted("Patient") {
        keys.push(("Patient".to_string(), patient_id.clone()));
    }
    keys.extend(
        crate::with_index_reader(&state, move |state, index| {
            let patient_ref = format!("Patient/{}", pid);
            let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
            let mut keys = Vec::new();
            for comp_type in state.compartment_def.resource_types() {
                if comp_type == "Patient" || types_wanted.as_ref().is_some_and(|ts| !ts.iter().any(|t| t == comp_type)) {
                    continue;
                }
                if au.as_ref().is_some_and(|u| u.grant(comp_type, 'r').is_none()) {
                    continue;
                }
                let Some(ref_fields) = state.compartment_def.get_reference_fields(comp_type) else {
                    continue;
                };
                for field in ref_fields {
                    for id in index.search_reference(comp_type, field, &patient_ref).unwrap_or_default() {
                        // Each resource once (e.g. Task may match on both "for" and "owner").
                        let key = (comp_type.to_string(), id);
                        if seen.insert(key.clone()) {
                            keys.push(key);
                        }
                    }
                }
            }
            keys
        })
        .await,
    );
    let total = keys.len();

    // The page.
    let page: Vec<Value> = keys
        .iter()
        .skip(offset)
        .take(count)
        .filter_map(|(t, id)| {
            if t == "Patient" && id == &patient_id {
                return Some(patient.clone());
            }
            let data = state.store.get(t, id).ok().flatten()?;
            serde_json::from_slice::<Value>(&data).ok()
        })
        .collect();
    let base = super::base_url_from_headers(request.headers());
    // Same rules as `_include`: scopes and the patient compartment.
    let entries: Vec<Value> = filter_mixed_by_compartment(auth_user.as_ref(), &state.compartment_def, page)
        .into_iter()
        .map(|resource| {
            let full_url = format!(
                "{base}/{}/{}",
                resource.get("resourceType").and_then(|v| v.as_str()).unwrap_or(""),
                resource.get("id").and_then(|v| v.as_str()).unwrap_or("")
            );
            json!({"fullUrl": full_url, "resource": resource, "search": {"mode": "match"}})
        })
        .collect();

    audit::log_operation_success(
        &audit_ctx,
        "$everything",
        "Patient",
        &format!("{}: {} resources", patient_id, total),
        &state.audit,
    );

    let page_url = |offset: usize| {
        let mut url = format!("{base}/Patient/{patient_id}/$everything?_count={count}&_offset={offset}");
        if let Some(ts) = &types {
            url.push_str(&format!("&_type={}", ts.join(",")));
        }
        url
    };
    let mut links = vec![json!({"relation": "self", "url": page_url(offset)})];
    if offset + count < total {
        links.push(json!({"relation": "next", "url": page_url(offset + count)}));
    }
    let mut bundle = json!({
        "resourceType": "Bundle",
        "type": "searchset",
        "total": total,
        "link": links
    });
    if !entries.is_empty() {
        bundle["entry"] = json!(entries);
    }
    Ok(super::fhir_json(StatusCode::OK, bundle))
}
