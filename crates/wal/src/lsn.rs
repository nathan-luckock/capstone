//! Log sequence number and transaction id newtypes.

/// Monotonically increasing identifier for a WAL record.
///
/// LSNs are assigned by the [`WalWriter`](crate::WalWriter) starting at 1.
/// `Lsn(0)` is a valid encoded value but is never assigned in practice;
/// the writer reserves it. The dedicated "no prior LSN" sentinel is
/// [`Lsn::INVALID`] (`u64::MAX`).
///
/// Picking `u64::MAX` rather than `0` as the sentinel keeps `0`-init bugs
/// (default-constructed structs, zeroed pages) from quietly looking like
/// valid LSN values.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct Lsn(pub u64);

impl Lsn {
    /// Sentinel meaning "no prior LSN". Used in a record's `prev_lsn`
    /// field when the record is the first one in its transaction.
    pub const INVALID: Self = Self(u64::MAX);

    /// Construct an `Lsn` from a raw u64.
    #[must_use]
    pub const fn new(v: u64) -> Self {
        Self(v)
    }

    /// The raw u64 value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// True for the [`Lsn::INVALID`] sentinel.
    #[must_use]
    pub const fn is_invalid(self) -> bool {
        self.0 == u64::MAX
    }
}

impl std::fmt::Display for Lsn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_invalid() {
            write!(f, "lsn:INVALID")
        } else {
            write!(f, "lsn:{}", self.0)
        }
    }
}

/// Transaction identifier. Assigned by the transaction manager (Sprint 5);
/// for now the WAL accepts any `u64`.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct TxnId(pub u64);

impl TxnId {
    /// Construct a `TxnId` from a raw u64.
    #[must_use]
    pub const fn new(v: u64) -> Self {
        Self(v)
    }

    /// The raw u64 value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for TxnId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "txn:{}", self.0)
    }
}
