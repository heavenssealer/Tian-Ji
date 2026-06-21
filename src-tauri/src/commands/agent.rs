//! Agent prompting. The prompt kicks off an orchestrator turn on a background task; updates
//! stream back over `agent://delta` and `agent://approval_request`. The command returns
//! immediately so the UI stays responsive while the turn runs.

use tauri::{AppHandle, Emitter, State};

use crate::events;
use crate::state::{AppResult, AppState};
use tianji_agent::AgentUpdate;

#[tauri::command]
pub async fn agent_prompt(
    app: AppHandle,
    state: State<'_, AppState>,
    prompt: String,
) -> AppResult<()> {
    let cw = state.current()?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentUpdate>();

    // Forwarder: map orchestrator updates onto frontend events.
    let app_fwd = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(update) = rx.recv().await {
            emit_update(&app_fwd, update);
        }
    });

    // The turn itself.
    tauri::async_runtime::spawn(async move {
        if let Err(e) = cw.orchestrator.handle_prompt(&cw.store, tx, &prompt).await {
            let _ = app.emit(events::AGENT_DELTA, serde_json::json!({ "type": "error", "text": e.to_string() }));
        }
    });

    Ok(())
}

#[tauri::command]
pub async fn agent_set_free_mode(state: State<'_, AppState>, enabled: bool) -> AppResult<()> {
    if let Ok(cw) = state.current() {
        cw.orchestrator.set_free_mode(enabled);
    }
    Ok(())
}

#[tauri::command]
pub async fn agent_set_autonomous(state: State<'_, AppState>, enabled: bool) -> AppResult<()> {
    if let Ok(cw) = state.current() {
        cw.orchestrator.set_autonomous(enabled);
    }
    Ok(())
}

#[tauri::command]
pub async fn agent_cancel(state: State<'_, AppState>) -> AppResult<()> {
    if let Ok(cw) = state.current() {
        cw.orchestrator.cancel();
    }
    Ok(())
}

#[tauri::command]
pub async fn agent_new_session(
    state: State<'_, AppState>,
    session_id: String,
) -> AppResult<()> {
    let cw = state.current()?;
    cw.orchestrator.new_session(&session_id);
    Ok(())
}

#[tauri::command]
pub async fn agent_switch_session(
    state: State<'_, AppState>,
    session_id: String,
) -> AppResult<()> {
    let cw = state.current()?;
    cw.orchestrator.switch_session(&session_id);
    Ok(())
}

fn emit_update(app: &AppHandle, update: AgentUpdate) {
    match update {
        AgentUpdate::Text(text) => {
            let _ = app.emit(events::AGENT_DELTA, serde_json::json!({ "type": "text_delta", "text": text }));
        }
        AgentUpdate::ToolStarted { tool, argv } => {
            let _ = app.emit(
                events::AGENT_DELTA,
                serde_json::json!({ "type": "tool_call", "text": format!("{tool} {}", argv.join(" ")) }),
            );
        }
        AgentUpdate::ToolOutput { text } => {
            let _ = app.emit(events::AGENT_DELTA, serde_json::json!({ "type": "tool_output", "text": text }));
        }
        AgentUpdate::ApprovalRequest { token, call } => {
            let _ = app.emit(
                events::AGENT_APPROVAL_REQUEST,
                serde_json::json!({
                    "token": token.0.to_string(),
                    "tool": call.tool,
                    "argv": call.argv,
                    "targets": call.targets.iter().map(target_str).collect::<Vec<_>>(),
                    "classification": classification_str(call.classification),
                }),
            );
        }
        AgentUpdate::Denied { reason } => {
            let _ = app.emit(events::AGENT_DELTA, serde_json::json!({ "type": "denied", "text": reason }));
        }
        AgentUpdate::FindingRecorded { severity, target, summary } => {
            let _ = app.emit(events::NOTES_UPDATED, ());
            let _ = app.emit(
                events::AGENT_DELTA,
                serde_json::json!({ "type": "finding", "text": format!("[{severity}] {target} — {summary}") }),
            );
        }
        AgentUpdate::TokensUsed { input, output } => {
            let _ = app.emit(
                events::AGENT_DELTA,
                serde_json::json!({ "type": "token_usage", "input": input, "output": output }),
            );
        }
        AgentUpdate::SubAgentStarted { name, objective } => {
            let _ = app.emit(
                events::AGENT_DELTA,
                serde_json::json!({ "type": "subagent_start", "agentName": name, "objective": objective }),
            );
        }
        AgentUpdate::SubAgentText { name, text } => {
            let _ = app.emit(
                events::AGENT_DELTA,
                serde_json::json!({ "type": "subagent_text", "agentName": name, "text": text }),
            );
        }
        AgentUpdate::SubAgentFinished { name, summary } => {
            let _ = app.emit(
                events::AGENT_DELTA,
                serde_json::json!({ "type": "subagent_end", "agentName": name, "text": summary }),
            );
        }
        AgentUpdate::TurnEnded => {
            let _ = app.emit(events::AGENT_DELTA, serde_json::json!({ "type": "turn_end" }));
        }
        AgentUpdate::Error(message) => {
            let _ = app.emit(events::AGENT_DELTA, serde_json::json!({ "type": "error", "text": message }));
        }
    }
}

fn target_str(t: &tianji_types::Target) -> String {
    use tianji_types::Target::*;
    match t {
        Ip(s) | Cidr(s) | Hostname(s) | Url(s) => s.clone(),
    }
}

fn classification_str(c: tianji_types::Classification) -> &'static str {
    use tianji_types::Classification::*;
    match c {
        ReadOnly => "read_only",
        Mutating => "mutating",
        Exploit => "exploit",
        Unknown => "unknown",
    }
}
