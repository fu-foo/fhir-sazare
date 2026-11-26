pub mod backup;
pub mod error;
mod migrate;
pub mod sqlite_store;
pub mod sqlite_index;
pub mod sqlite_audit;
pub mod search_executor;
pub mod index_builder;
#[cfg(test)]
mod differential_tests;

pub use backup::backup_database;
pub use error::{Result, StoreError};
pub use sqlite_store::SqliteStore;
pub use sqlite_index::{IndexReaders, IndexUpdate, ResourceText, SearchIndex};
pub use sqlite_audit::{AuditLog, Operation};
pub use search_executor::SearchExecutor;
pub use index_builder::{resource_text, IndexBuilder, INDEX_FORMAT};
