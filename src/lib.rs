//! Durable continuity for long-running coding-agent tasks.

mod agent;
pub mod cli;
mod codex_source;
pub mod intent_history;
mod ledger;
pub mod projection;
pub mod session_bundle;

pub use ledger::{ClosureError, Ledger, LedgerError, RequirementStatus, Snapshot};
