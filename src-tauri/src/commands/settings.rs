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

/// Rebuild the open workspace's orchestrator (after a key, subscription, or model change).
pub(crate) fn rebuild_current(state: &AppState) -> AppResult<()> {
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
pub async fn settings_list_models(state: State<'_, AppState>) -> AppResult<Vec<String>> {
    let mut out: Vec<String> = MODELS.iter().map(|s| s.to_string()).collect();

    // Merge in whatever models the configured Ollama host currently has pulled, so `ollama pull`
    // is enough to make a model selectable — no recompile. Best-effort: if Ollama isn't reachable
    // we just return the static list.
    let host = crate::state::ollama_host(&state.app);
    if let Ok(models) = tianji_llm::list_ollama_models(&host).await {
        for name in models {
            let id = format!("ollama:{name}");
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
    Ok(out)
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

/// The configured Ollama endpoint (defaults to `http://localhost:11434`).
#[tauri::command]
pub async fn settings_get_ollama_host(state: State<'_, AppState>) -> AppResult<String> {
    Ok(crate::state::ollama_host(&state.app))
}

/// Point the local-model backend at a specific Ollama host. An empty value resets to the default.
/// Rebuilds the open workspace so a running `ollama:` model picks up the new host immediately.
#[tauri::command]
pub async fn settings_set_ollama_host(state: State<'_, AppState>, host: String) -> AppResult<()> {
    let host = host.trim().trim_end_matches('/');
    state
        .app
        .set_setting("ollama_host", if host.is_empty() { crate::state::DEFAULT_OLLAMA_HOST } else { host })?;
    rebuild_current(&state)
}

/// The configured Ollama context window (`num_ctx`).
#[tauri::command]
pub async fn settings_get_ollama_num_ctx(state: State<'_, AppState>) -> AppResult<u32> {
    Ok(crate::state::ollama_num_ctx(&state.app))
}

/// Set the Ollama context window. Clamped to a usable minimum, and both the value sent to Ollama
/// and our own history budget follow it. Rebuilds the open workspace to take effect immediately.
#[tauri::command]
pub async fn settings_set_ollama_num_ctx(state: State<'_, AppState>, num_ctx: u32) -> AppResult<()> {
    let n = num_ctx.max(crate::state::MIN_OLLAMA_NUM_CTX);
    state.app.set_setting("ollama_num_ctx", &n.to_string())?;
    rebuild_current(&state)
}
