//! Read-side queries over the event log — projections the UI renders (notes feed, history, findings).

use serde::Serialize;
use tauri::State;

use crate::state::{AppResult, AppState};
use tianji_types::{Author, Event, EventKind};

/// A flattened event for the frontend. `text` is a best-effort display string pulled from the
/// kind-specific payload.
#[derive(Debug, Clone, Serialize)]
pub struct EventDto {
    pub id: String,
    pub kind: String,
    pub author: String,
    pub phase: String,
    pub text: String,
    pub ts: String,
}

#[tauri::command]
pub async fn events_query(state: State<'_, AppState>, limit: usize) -> AppResult<Vec<EventDto>> {
    let cw = state.current()?;
    let events = cw.store.recent_events(limit.clamp(1, 500))?;
    Ok(events.iter().map(to_dto).collect())
}

fn to_dto(e: &Event) -> EventDto {
    EventDto {
        id: e.id.to_string(),
        kind: kind_str(e.kind).to_string(),
        author: match e.author {
            Author::User => "user",
            Author::Agent => "agent",
        }
        .to_string(),
        phase: format!("{:?}", e.phase).to_lowercase(),
        text: display_text(e),
        ts: e.ts.to_string(),
    }
}

fn kind_str(k: EventKind) -> &'static str {
    match k {
        EventKind::UserPrompt => "user_prompt",
        EventKind::AgentMsg => "agent_msg",
        EventKind::ToolProposed => "tool_proposed",
        EventKind::ToolApproved => "tool_approved",
        EventKind::ToolDenied => "tool_denied",
        EventKind::ToolOutput => "tool_output",
        EventKind::Note => "note",
        EventKind::PhaseChange => "phase_change",
        EventKind::Finding => "finding",
    }
}

/// Pull a human-readable string out of the payload, trying the common fields in order.
/// A finding row for the findings panel.
#[derive(Debug, Clone, Serialize)]
pub struct FindingDto {
    pub id: String,
    pub severity: String,
    pub target: String,
    pub summary: String,
}

#[tauri::command]
pub async fn findings_query(state: State<'_, AppState>) -> AppResult<Vec<FindingDto>> {
    let cw = state.current()?;
    let findings = cw.store.findings()?;
    Ok(findings
        .into_iter()
        .map(|f| FindingDto {
            id: f.id.to_string(),
            severity: f.severity,
            target: f.target,
            summary: f.summary,
        })
        .collect())
}

fn display_text(e: &Event) -> String {
    let p = &e.payload;
    for key in ["text", "summary", "output", "reason", "phase"] {
        if let Some(s) = p.get(key).and_then(|v| v.as_str()) {
            return s.to_string();
        }
    }
    // Tool events: render "tool argv".
    if let (Some(tool), Some(argv)) = (p.get("tool").and_then(|v| v.as_str()), p.get("argv")) {
        let args = argv
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(" "))
            .unwrap_or_default();
        return format!("{tool} {args}").trim().to_string();
    }
    p.to_string()
}
