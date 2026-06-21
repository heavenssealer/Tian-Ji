//! App settings — Anthropic API key (OS keychain) + selected model (app settings DB). Changing
//! either rebuilds the open workspace's orchestrator so it takes effect immediately.

use std::sync::Arc;

use tauri::State;

use crate::secrets;
use crate::state::{AppResult, AppState, CurrentWorkspace};
use tianji_store::WorkspaceStore;

/// Models offered in the UI picker. `ollama:<name>` entries run locally (free, no API key) via a
/// running Ollama instance; the listed ones are tool-calling-capable. Pull them first with
/// `ollama pull <name>`.
const MODELS: &[&str] = &[
    "claude-opus-4-8",
    "claude-sonnet-4-6",
    "claude-haiku-4-5-20251001",
    "ollama:llama3.1",
    "ollama:qwen2.5-coder",
    "ollama:mistral-nemo",
];

/// Rebuild the open workspace's orchestrator (after a key or model change).
fn rebuild_current(state: &AppState) -> AppResult<()> {
    let open = { state.current.lock().unwrap().as_ref().map(|cw| cw.meta.clone()) };
    if let Some(meta) = open {
        let store = WorkspaceStore::open(std::path::Path::new(&meta.root_path))?;
        let model = state.model();
        *state.current.lock().unwrap() = Some(Arc::new(CurrentWorkspace::build(
            meta, store, &model, state.autonomous.clone(), state.free_mode.clone(),
            state.tokens_spent.clone(), state.token_budget.clone(), &state.app,
        )));
    }
    Ok(())
}

#[tauri::command]
pub async fn settings_set_api_key(state: State<'_, AppState>, key: String) -> AppResult<()> {
    secrets::set_api_key("anthropic", &key)?;
    rebuild_current(&state)
}

#[tauri::command]
pub async fn settings_has_api_key(_state: State<'_, AppState>) -> AppResult<bool> {
    Ok(secrets::get_api_key("anthropic")?
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false))
}

#[tauri::command]
pub async fn settings_set_sudo_password(state: State<'_, AppState>, password: String) -> AppResult<()> {
    secrets::set_api_key("sudo", &password)?;
    rebuild_current(&state)
}

#[tauri::command]
pub async fn settings_has_sudo_password(_state: State<'_, AppState>) -> AppResult<bool> {
    Ok(secrets::get_api_key("sudo")?
        .map(|p| !p.trim().is_empty())
        .unwrap_or(false))
}

#[tauri::command]
pub async fn settings_list_models(_state: State<'_, AppState>) -> AppResult<Vec<String>> {
    Ok(MODELS.iter().map(|s| s.to_string()).collect())
}

#[tauri::command]
pub async fn settings_get_model(state: State<'_, AppState>) -> AppResult<String> {
    Ok(state.model())
}

#[tauri::command]
pub async fn settings_set_model(state: State<'_, AppState>, model: String) -> AppResult<()> {
    state.app.set_setting("model", &model)?;
    rebuild_current(&state)
}
