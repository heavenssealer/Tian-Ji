//! Strongly-typed identifiers. Newtypes prevent mixing up, e.g., a `WorkspaceId` and an
//! `EventId` at a call site.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! uuid_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

uuid_id!(WorkspaceId);
uuid_id!(EventId);
uuid_id!(TerminalId);

/// An agent (or human) actor. String rather than UUID so the orchestrator and sub-agents read
/// legibly in the audit log: `"human"`, `"orchestrator"`, `"recon"`, `"exploit"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentId(pub String);

impl AgentId {
    pub const HUMAN: &'static str = "human";

    pub fn human() -> Self {
        Self(Self::HUMAN.to_string())
    }
}

impl std::fmt::Display for AgentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
