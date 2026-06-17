//! WAL error type.

use crate::lsn::Lsn;

/// Errors returned by the WAL layer.
#[derive(Debug, thiserror::Error)]
pub enum WalError {
    /// Underlying I/O failed.
    #[error("WAL io error: {0}")]
    Io(#[from] std::io::Error),

    /// A storage-layer error escaped through the WAL surface (typically a
    /// page-header read failure when walking the page LSN).
    #[error("storage error: {0}")]
    Storage(#[from] picklejar_storage::StorageError),

    /// The record's `length` field is shorter than the minimum header
    /// size. Indicates either a malformed write or on-disk corruption.
    #[error("malformed WAL record: length {length} is below minimum {minimum}")]
    RecordTooShort {
        /// Length the record claimed.
        length: u32,
        /// Minimum bytes required for a valid record.
        minimum: u32,
    },

    /// The trailer CRC32 does not match the record bytes.
    #[error("WAL record checksum mismatch")]
    ChecksumMismatch,

    /// The record's `type` discriminant is not a known variant.
    #[error("WAL record has unknown type byte: {0:#x}")]
    UnknownRecordType(u8),

    /// An `Update` record's variable-length sub-field claims more bytes
    /// than the record contains.
    #[error("WAL record payload truncated: expected {expected} bytes, only {available} present")]
    PayloadTruncated {
        /// Bytes the field length prefix claimed.
        expected: usize,
        /// Bytes actually available in the record's payload region.
        available: usize,
    },

    /// The WAL file ended mid-record. Distinguished from clean EOF
    /// (between records), which the reader returns as `None`.
    #[error("WAL file ended mid-record at lsn {lsn}")]
    TruncatedTail {
        /// Last successfully-read LSN before truncation, or `Lsn::INVALID`
        /// if no records were read at all.
        lsn: Lsn,
    },
}

/// Convenience alias for results returned by the WAL layer.
pub type Result<T> = std::result::Result<T, WalError>;
