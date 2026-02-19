use axum::extract::{ConnectInfo, Request};
use sazare_store::{AuditLog, Operation};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::auth::AuthUser;

/// Audit context extracted from HTTP request
#[derive(Debug, Clone)]
pub struct AuditContext {
    pub user_id: Option<String>,
    pub client_ip: String,
}

impl AuditContext {
    /// Create audit context without connection info (for testing)
    pub fn new(user_id: Option<String>, client_ip: String) -> Self {
        Self { user_id, client_ip }
    }

    /// Extract audit context from an Axum request
    pub fn from_request(request: &Request) -> Self {
        let client_ip = request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ci| ci.0.ip().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let user_id = request
            .extensions()
            .get::<AuthUser>()
            .map(|u| u.user_id.clone());

        Self { user_id, client_ip }
    }
}

/// Map operation string to AuditLog Operation enum
fn parse_operation(op: &str) -> Operation {
    match op.to_uppercase().as_str() {
        "CREATE" => Operation::Create,
        "READ" | "READ_VERSION" | "VALIDATE" => Operation::Read,
        "UPDATE" => Operation::Update,
        "DELETE" => Operation::Delete,
        "SEARCH" | "HISTORY" => Operation::Search,
        "TRANSACTION" | "BATCH" | "IMPORT" => Operation::Create,
        "EXPORT" => Operation::Read,
        _ => Operation::Read, // default fallback
    }
}

/// Log a successful operation
pub fn log_operation_success(
    context: &AuditContext,
    operation: &str,
    resource_type: &str,
    resource_id: &str,
    audit_log: &Arc<Mutex<AuditLog>>,
) {
    tracing::info!(
        user_id = context.user_id.as_deref().unwrap_or("anonymous"),
        client_ip = %context.client_ip,
        operation = operation,
        resource_type = resource_type,
        resource_id = resource_id,
        status = "success",
        "Audit: {} {} {}/{}",
        operation,
        resource_type,
        resource_type,
        resource_id
    );

    // Write to database asynchronously in a spawned task
    let op = parse_operation(operation);
    let context = context.clone();
    let resource_type = resource_type.to_string();
    let resource_id = resource_id.to_string();
    let audit_log = Arc::clone(audit_log);

    tokio::spawn(async move {
        let audit = audit_log.lock().await;
        if let Err(e) = audit.log_success(
            op,
            &resource_type,
            &resource_id,
            context.user_id.as_deref(),
            Some(&context.client_ip),
        ) {
            tracing::error!("Failed to write audit log to database: {}", e);
        }
    });
}

/// Log a failed operation
pub fn log_operation_error(
    context: &AuditContext,
    operation: &str,
    resource_type: &str,
    resource_id: Option<&str>,
    error: &str,
    audit_log: &Arc<Mutex<AuditLog>>,
) {
    tracing::warn!(
        user_id = context.user_id.as_deref().unwrap_or("anonymous"),
        client_ip = %context.client_ip,
        operation = operation,
        resource_type = resource_type,
        resource_id = resource_id.unwrap_or("N/A"),
        status = "error",
        error = error,
        "Audit: {} {} failed: {}",
        operation,
        resource_type,
        error
    );

    // Write to database asynchronously in a spawned task
    let op = parse_operation(operation);
    let context = context.clone();
    let resource_type = resource_type.to_string();
    let resource_id = resource_id.map(|s| s.to_string());
    let error = error.to_string();
    let audit_log = Arc::clone(audit_log);

    tokio::spawn(async move {
        let audit = audit_log.lock().await;
        if let Err(e) = audit.log_error(
            op,
            Some(&resource_type),
            resource_id.as_deref(),
            context.user_id.as_deref(),
            Some(&context.client_ip),
            &error,
        ) {
            tracing::error!("Failed to write audit log to database: {}", e);
        }
    });
}

/// Log an authentication attempt
pub fn log_auth_attempt(client_ip: &str, user_id: Option<&str>, success: bool) {
    if success {
        tracing::info!(
            user_id = user_id.unwrap_or("unknown"),
            client_ip = %client_ip,
            status = "success",
            "Audit: Authentication successful"
        );
    } else {
        tracing::warn!(
            client_ip = %client_ip,
            status = "failed",
            "Audit: Authentication failed"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audit_context_creation() {
        let context = AuditContext::new(Some("user123".to_string()), "192.168.1.1".to_string());
        assert_eq!(context.user_id, Some("user123".to_string()));
        assert_eq!(context.client_ip, "192.168.1.1");
    }

    #[test]
    fn test_audit_context_anonymous() {
        let context = AuditContext::new(None, "127.0.0.1".to_string());
        assert_eq!(context.user_id, None);
        assert_eq!(context.client_ip, "127.0.0.1");
    }
}
