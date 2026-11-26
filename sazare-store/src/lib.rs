pub mod backup;
pub mod error;
mod migrate;
pub mod sqlite_store;
pub mod sqlite_index;
pub mod search_index;
pub mod sqlite_audit;
pub mod search_executor;
pub mod index_builder;
pub mod index_v2;
#[cfg(test)]
mod differential_tests;

pub use backup::backup_database;
pub use error::{Result, StoreError};
pub use sqlite_store::SqliteStore;
pub use search_index::{IndexReaders, SearchIndex};
pub use sqlite_index::{IndexUpdate, ResourceText, V1Index};
pub use sqlite_audit::{AuditLog, Operation};
pub use search_executor::{SearchExecutor, V1Executor};
pub use index_builder::{resource_text, IndexBuilder, COMPARTMENT_PARAM, INDEX_FORMAT};
