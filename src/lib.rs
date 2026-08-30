//! Durable continuity for long-running coding-agent tasks.

mod agent;
pub mod cli;
pub mod codex_child;
mod codex_source;
mod goal_change_store;
mod inspect_state;
pub mod intent_history;
mod ledger;
pub mod projection;
pub mod run_store;
mod semantic_resolver;
pub mod session_bundle;
pub mod verification;
pub mod workspace;

pub use ledger::{ClosureError, Ledger, LedgerError, RequirementStatus, Snapshot};
