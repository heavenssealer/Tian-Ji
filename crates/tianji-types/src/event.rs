//! The event — the single unit of the append-only log that is the system's source of truth
//! (DESIGN.md §5). Notes, memory, phases, and the audit trail are all projections of these.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::ids::{AgentId, EventId, WorkspaceId};
use crate::Timestamp;

/// Engagement phase. Drives the active agent system-prompt/toolset and stamps every event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Recon,
    Hypothesis,
    Poc,
    Exploit,
    Report,
}

impl Default for Phase {
    fn default() -> Self {
        Phase::Recon
    }
}

/// Who authored the event — distinguishes the user's deliberate notebook from agent auto-notes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Author {
    User,
    Agent,
}

/// The kind of thing that happened. `payload` carries kind-specific data as JSON so the schema
/// stays append-friendly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    UserPrompt,
    AgentMsg,
    ToolProposed,
    ToolApproved,
    ToolDenied,
    ToolOutput,
    Note,
    PhaseChange,
    Finding,
}

/// An immutable fact in the log. Never updated in place — corrections are new events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: EventId,
    pub workspace_id: WorkspaceId,
    pub phase: Phase,
    pub kind: EventKind,
    /// Which agent/human produced it — reserves multi-agent (DESIGN.md §7.2).
    pub actor: AgentId,
    /// User vs Agent authorship — manual notebook vs auto-notes.
    pub author: Author,
    /// Causal / delegation tree link — reserves multi-agent.
    pub parent_id: Option<EventId>,
    /// Kind-specific payload (command argv, output text, note markdown, …).
    pub payload: serde_json::Value,
    pub ts: Timestamp,
}

impl Event {
    /// Build a fresh event with a new id and the current UTC time. `parent_id` defaults to
    /// `None`; chain [`Event::with_parent`] for delegation/causal links.
    pub fn new(
        workspace_id: WorkspaceId,
        phase: Phase,
        kind: EventKind,
        actor: AgentId,
        author: Author,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            id: EventId::new(),
            workspace_id,
            phase,
            kind,
            actor,
            author,
            parent_id: None,
            payload,
            ts: OffsetDateTime::now_utc(),
        }
    }

    pub fn with_parent(mut self, parent: EventId) -> Self {
        self.parent_id = Some(parent);
        self
    }
}

/// A read-model row (projection of the log), not a primary event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub id: EventId,
    pub workspace_id: WorkspaceId,
    pub severity: String,
    pub target: String,
    pub summary: String,
    /// Events that substantiate this finding.
    pub evidence_event_ids: Vec<EventId>,
}
