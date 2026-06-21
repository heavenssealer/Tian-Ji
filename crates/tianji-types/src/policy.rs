//! Policy result types. The *logic* lives in `tianji-policy`; only the shapes live here so
//! other crates (store, agent, the Tauri layer) can speak about decisions without depending on
//! the engine.

use serde::{Deserialize, Serialize};

/// How dangerous a proposed command is judged to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Classification {
    ReadOnly,
    Mutating,
    Exploit,
    /// Unmatched by allow/deny lists — the engine fails closed on this.
    Unknown,
}

/// What the supervisor will do with a proposed command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Decision {
    AutoRun,
    NeedsApproval,
    Deny { reason: String },
}
