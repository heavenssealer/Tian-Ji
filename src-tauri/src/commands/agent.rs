//! Agent prompting. The prompt kicks off an orchestrator turn on a background task; updates
//! stream back over `agent://delta` and `agent://approval_request`. The command returns
//! immediately so the UI stays responsive while the turn runs.

use std::sync::atomic::Ordering;

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

/// Standalone mode: run the agent autonomously toward `goal` until it finishes, gets stuck, or
/// hits a safety rail. Same streaming contract as `agent_prompt`; returns immediately.
#[tauri::command]
pub async fn agent_run_goal(
    app: AppHandle,
    state: State<'_, AppState>,
    goal: String,
) -> AppResult<()> {
    let cw = state.current()?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentUpdate>();

    let app_fwd = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(update) = rx.recv().await {
            emit_update(&app_fwd, update);
        }
    });

    tauri::async_runtime::spawn(async move {
        if let Err(e) = cw.orchestrator.run_goal(&cw.store, tx, &goal).await {
            let _ = app.emit(events::AGENT_DELTA, serde_json::json!({ "type": "error", "text": e.to_string() }));
        }
    });

    Ok(())
}

#[tauri::command]
pub async fn agent_set_free_mode(state: State<'_, AppState>, enabled: bool) -> AppResult<()> {
    state.free_mode.store(enabled, Ordering::SeqCst);
    Ok(())
}

#[tauri::command]
pub async fn agent_set_autonomous(state: State<'_, AppState>, enabled: bool) -> AppResult<()> {
    state.autonomous.store(enabled, Ordering::SeqCst);
    Ok(())
}

/// Set the cumulative token budget cap (0 = unlimited). Persisted so it survives restarts.
#[tauri::command]
pub async fn agent_set_token_budget(state: State<'_, AppState>, tokens: u64) -> AppResult<()> {
    state.token_budget.store(tokens, Ordering::SeqCst);
    state.app.set_setting("token_budget", &tokens.to_string())?;
    Ok(())
}

/// A profile fact for the UI. `scope` is "global" (operator habit) or "workspace" (this engagement).
#[derive(serde::Serialize)]
pub struct ProfileFactDto {
    pub id: i64,
    pub text: String,
    pub pinned: bool,
    pub scope: String,
}

/// Distill durable facts from recent activity into the profile (the "learn the operator" pass).
/// Runs on the sub-agent model — free if it's local. Global facts go to the app store; workspace
/// facts are written by the orchestrator into the workspace store.
#[tauri::command]
pub async fn agent_distill_profile(state: State<'_, AppState>) -> AppResult<()> {
    let cw = state.current()?;
    let global = cw.orchestrator.distill_profile(&cw.store).await;
    for text in &global {
        let _ = state.app.add_global_fact(text);
    }
    let all = state.app.global_facts()?.into_iter().map(|f| f.text).collect();
    cw.orchestrator.set_global_facts(all);
    Ok(())
}

#[tauri::command]
pub async fn profile_facts_list(state: State<'_, AppState>) -> AppResult<Vec<ProfileFactDto>> {
    let mut out: Vec<ProfileFactDto> = state
        .app
        .global_facts()?
        .into_iter()
        .map(|f| ProfileFactDto { id: f.id, text: f.text, pinned: f.pinned, scope: "global".into() })
        .collect();
    if let Ok(cw) = state.current() {
        out.extend(cw.store.workspace_facts()?.into_iter().map(|f| ProfileFactDto {
            id: f.id,
            text: f.text,
            pinned: f.pinned,
            scope: "workspace".into(),
        }));
    }
    Ok(out)
}

#[tauri::command]
pub async fn profile_fact_add(
    state: State<'_, AppState>,
    text: String,
    scope: String,
) -> AppResult<()> {
    if scope == "global" {
        state.app.add_global_fact(&text)?;
        refresh_global_facts(&state)?;
    } else if let Ok(cw) = state.current() {
        cw.store.add_workspace_fact(&text)?;
    }
    Ok(())
}

#[tauri::command]
pub async fn profile_fact_remove(
    state: State<'_, AppState>,
    id: i64,
    scope: String,
) -> AppResult<()> {
    if scope == "global" {
        state.app.remove_global_fact(id)?;
        refresh_global_facts(&state)?;
    } else if let Ok(cw) = state.current() {
        cw.store.remove_workspace_fact(id)?;
    }
    Ok(())
}

/// Pin/unpin a fact. Pinned facts are kept verbatim and never auto-pruned. (Injection is
/// unaffected — all facts are injected; pinning protects them from future cleanup.)
#[tauri::command]
pub async fn profile_fact_pin(
    state: State<'_, AppState>,
    id: i64,
    scope: String,
    pinned: bool,
) -> AppResult<()> {
    if scope == "global" {
        state.app.pin_global_fact(id, pinned)?;
    } else if let Ok(cw) = state.current() {
        cw.store.pin_workspace_fact(id, pinned)?;
    }
    Ok(())
}

/// Re-sync the orchestrator's in-memory global habits with the app store after an edit.
fn refresh_global_facts(state: &AppState) -> AppResult<()> {
    if let Ok(cw) = state.current() {
        let all = state.app.global_facts()?.into_iter().map(|f| f.text).collect();
        cw.orchestrator.set_global_facts(all);
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
        AgentUpdate::GoalStarted { goal } => {
            let _ = app.emit(
                events::AGENT_DELTA,
                serde_json::json!({ "type": "goal_start", "text": goal }),
            );
        }
        AgentUpdate::GoalIteration { iteration } => {
            let _ = app.emit(
                events::AGENT_DELTA,
                serde_json::json!({ "type": "goal_iteration", "input": iteration }),
            );
        }
        AgentUpdate::GoalFinished { outcome, iterations } => {
            let _ = app.emit(
                events::AGENT_DELTA,
                serde_json::json!({ "type": "goal_end", "text": outcome, "input": iterations }),
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
