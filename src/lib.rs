//! Durable continuity for long-running coding-agent tasks.

mod agent;
pub mod cli;
mod codex_source;
pub mod intent_history;
mod ledger;

pub use ledger::{ClosureError, Ledger, LedgerError, RequirementStatus, Snapshot};
