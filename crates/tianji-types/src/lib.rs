//! # tianji-types
//!
//! Shared domain types for Tiān Jī. The **leaf crate** — every other crate depends on this,
//! and this depends on nothing internal. No logic lives here; only data shapes.
//!
//! Provider-neutral LLM types ([`Message`], [`ToolSpec`], [`AgentEvent`]) live here so that
//! **no SDK types ever leak into core logic** (DESIGN.md §7.1). The Anthropic/OpenAI specifics
//! stay confined to `tianji-llm`.

use time::OffsetDateTime;
use uuid::Uuid;

pub mod ids;
pub mod event;
pub mod scope;
pub mod policy;
pub mod llm;

pub use event::{Author, Event, EventKind, Finding, Phase};
pub use ids::{AgentId, EventId, TerminalId, WorkspaceId};
pub use llm::{AgentEvent, Content, Message, Role, ToolCall, ToolSpec};
pub use policy::{Classification, Decision};
pub use scope::{ScopeRules, Target};

/// Re-exported so downstream crates share one `time`/`uuid`/`serde_json`.
pub use {serde_json, time, uuid};

/// Convenience alias used across the workspace.
pub type Timestamp = OffsetDateTime;

#[doc(hidden)]
pub fn _new_uuid() -> Uuid {
    Uuid::new_v4()
}
