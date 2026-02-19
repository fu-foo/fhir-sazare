pub mod error;
pub mod redb_store;
pub mod sqlite_store;
pub mod sqlite_index;
pub mod sqlite_audit;
pub mod search_executor;
pub mod index_builder;

pub use error::{Result, StoreError};
pub use redb_store::RedbStore;
pub use sqlite_store::SqliteStore;
pub use sqlite_index::SearchIndex;
pub use sqlite_audit::{AuditLog, Operation};
pub use search_executor::SearchExecutor;
pub use index_builder::IndexBuilder;
