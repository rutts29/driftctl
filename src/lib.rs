//! Durable continuity for long-running coding-agent tasks.

mod ledger;

pub use ledger::{ClosureError, Ledger, LedgerError, Snapshot};
