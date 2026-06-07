use thiserror::Error;

#[derive(Error, Debug)]
pub enum SazareError {
    #[error("Resource not found: {resource_type}/{id}")]
    NotFound {
        resource_type: String,
        id: String,
    },

    #[error("Validation failed: {message}")]
    Validation { message: String },

    #[error("Storage issue: {0}")]
    Storage(String),

    #[error("Invalid JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, SazareError>;
