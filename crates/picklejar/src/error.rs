//! The engine's error type: one enum over every layer's failure.

use thiserror::Error;

/// An error from any layer, surfaced to the caller of [`Database::execute`].
///
/// [`Database::execute`]: crate::Database::execute
#[derive(Debug, Error)]
pub enum DbError {
    /// SQL did not lex or parse.
    #[error(transparent)]
    Parse(#[from] picklejar_sql::SqlError),
    /// Binding or planning failed (unknown name, unsupported statement).
    #[error(transparent)]
    Plan(#[from] picklejar_planner::PlanError),
    /// A transaction-layer failure.
    #[error(transparent)]
    Txn(#[from] picklejar_txn::TxnError),
    /// A row codec or execution failure.
    #[error(transparent)]
    Exec(#[from] picklejar_executor::ExecError),
    /// A storage-layer failure.
    #[error(transparent)]
    Storage(#[from] picklejar_storage::StorageError),
    /// A write-ahead-log failure.
    #[error(transparent)]
    Wal(#[from] picklejar_wal::WalError),
    /// An I/O failure reading or writing the catalog sidecar.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// A statement named a table the catalog does not have.
    #[error("unknown table: {0}")]
    UnknownTable(String),
    /// A statement named a column the table does not have.
    #[error("unknown column {column} in table {table}")]
    UnknownColumn {
        /// The table named.
        table: String,
        /// The column that was not found.
        column: String,
    },
    /// An `INSERT` row had a different value count than columns named.
    #[error("INSERT row has {got} values but {expected} columns")]
    ValueCount {
        /// Columns named (or table arity when no column list is given).
        expected: usize,
        /// Values supplied in the row.
        got: usize,
    },
    /// A statement or expression form the engine does not handle yet.
    #[error("{0} is not supported yet")]
    Unsupported(String),
    /// A column constraint (NOT NULL, UNIQUE, or PRIMARY KEY) was violated.
    #[error("constraint violation: {0}")]
    Constraint(String),
    /// The current role lacks the privilege a statement needs, or a role
    /// management statement was rejected.
    #[error("permission denied: {0}")]
    PermissionDenied(String),
}

/// Engine result alias.
pub type Result<T> = std::result::Result<T, DbError>;
