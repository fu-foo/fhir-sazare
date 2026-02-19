use serde::{Deserialize, Serialize};

/// FHIR OperationOutcome resource for error reporting
/// See: https://www.hl7.org/fhir/operationoutcome.html
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationOutcome {
    pub resource_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub issue: Vec<OperationOutcomeIssue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationOutcomeIssue {
    pub severity: IssueSeverity,
    pub code: IssueType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<CodeableConcept>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expression: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IssueSeverity {
    Fatal,
    Error,
    Warning,
    Information,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum IssueType {
    Invalid,
    Structure,
    Required,
    Value,
    Invariant,
    Security,
    Login,
    Unknown,
    Expired,
    Forbidden,
    Suppressed,
    Processing,
    NotSupported,
    Duplicate,
    MultipleMatches,
    NotFound,
    Deleted,
    TooLong,
    CodeInvalid,
    Extension,
    TooCostly,
    BusinessRule,
    Conflict,
    Transient,
    LockError,
    NoStore,
    Exception,
    Timeout,
    Incomplete,
    Throttled,
    Informational,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeableConcept {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coding: Option<Vec<Coding>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Coding {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
}

impl OperationOutcome {
    /// Create a new OperationOutcome with a single issue
    pub fn new(severity: IssueSeverity, code: IssueType, diagnostics: impl Into<String>) -> Self {
        Self {
            resource_type: "OperationOutcome".to_string(),
            id: None,
            issue: vec![OperationOutcomeIssue {
                severity,
                code,
                diagnostics: Some(diagnostics.into()),
                details: None,
                expression: None,
            }],
        }
    }

    /// Create a success OperationOutcome (validation passed)
    pub fn success() -> Self {
        Self::new(IssueSeverity::Information, IssueType::Informational, "All OK")
    }

    /// Create an error OperationOutcome
    pub fn error(code: IssueType, diagnostics: impl Into<String>) -> Self {
        Self::new(IssueSeverity::Error, code, diagnostics)
    }

    /// Create a not found error
    pub fn not_found(resource_type: &str, id: &str) -> Self {
        Self::error(
            IssueType::NotFound,
            format!("Resource not found: {}/{}", resource_type, id),
        )
    }

    /// Create an invalid resource error
    pub fn invalid_resource(diagnostics: impl Into<String>) -> Self {
        Self::error(IssueType::Invalid, diagnostics)
    }

    /// Create an unauthorized error
    pub fn unauthorized(diagnostics: impl Into<String>) -> Self {
        Self::error(IssueType::Login, diagnostics)
    }

    /// Create a forbidden error
    pub fn forbidden(diagnostics: impl Into<String>) -> Self {
        Self::error(IssueType::Forbidden, diagnostics)
    }

    /// Create a validation error
    pub fn validation_error(diagnostics: impl Into<String>) -> Self {
        Self::error(IssueType::Value, diagnostics)
    }

    /// Create a storage error
    pub fn storage_error(diagnostics: impl Into<String>) -> Self {
        Self::error(IssueType::Exception, diagnostics)
    }

    /// Add an issue to this OperationOutcome
    pub fn add_issue(&mut self, issue: OperationOutcomeIssue) {
        self.issue.push(issue);
    }

    /// Add an issue with expression (path to the problematic element)
    pub fn with_expression(mut self, expression: Vec<String>) -> Self {
        if let Some(issue) = self.issue.last_mut() {
            issue.expression = Some(expression);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_operation_outcome_serialization() {
        let outcome = OperationOutcome::error(IssueType::NotFound, "Patient not found");
        let json = serde_json::to_string(&outcome).unwrap();
        assert!(json.contains("OperationOutcome"));
        assert!(json.contains("error"));
        assert!(json.contains("not-found"));
    }

    #[test]
    fn test_not_found_helper() {
        let outcome = OperationOutcome::not_found("Patient", "123");
        assert_eq!(outcome.issue.len(), 1);
        assert_eq!(outcome.issue[0].severity, IssueSeverity::Error);
        assert_eq!(outcome.issue[0].code, IssueType::NotFound);
        assert!(outcome.issue[0]
            .diagnostics
            .as_ref()
            .unwrap()
            .contains("Patient/123"));
    }

    #[test]
    fn test_unauthorized_helper() {
        let outcome = OperationOutcome::unauthorized("Invalid API key");
        assert_eq!(outcome.issue.len(), 1);
        assert_eq!(outcome.issue[0].severity, IssueSeverity::Error);
        assert_eq!(outcome.issue[0].code, IssueType::Login);
    }

    #[test]
    fn test_success_helper() {
        let outcome = OperationOutcome::success();
        assert_eq!(outcome.issue.len(), 1);
        assert_eq!(outcome.issue[0].severity, IssueSeverity::Information);
        assert_eq!(outcome.issue[0].code, IssueType::Informational);
        assert_eq!(
            outcome.issue[0].diagnostics.as_ref().unwrap(),
            "All OK"
        );
    }

    #[test]
    fn test_with_expression() {
        let outcome = OperationOutcome::validation_error("Invalid name")
            .with_expression(vec!["Patient.name[0]".to_string()]);
        assert_eq!(outcome.issue[0].expression.as_ref().unwrap().len(), 1);
        assert_eq!(
            outcome.issue[0].expression.as_ref().unwrap()[0],
            "Patient.name[0]"
        );
    }
}
