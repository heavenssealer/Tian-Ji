//! Anthropic subscription login — connect a Claude Pro/Max account so turns bill the subscription
//! instead of API credits. Thin wrappers over [`crate::oauth`] (PKCE + token exchange); tokens are
//! stored in the OS keychain and a connected subscription takes precedence over the API key.

use tauri::State;

use crate::commands::settings::rebuild_current;
use crate::state::{AppError, AppResult, AppState};

/// Start a login. Returns the browser URL to open; the operator authorizes, then pastes the code
/// back into `auth_complete`. The PKCE verifier is stashed in `AppState` until then.
#[tauri::command]
pub async fn auth_begin(state: State<'_, AppState>) -> AppResult<String> {
    let (url, pending) = crate::oauth::begin();
    *state.pending_oauth.lock().unwrap() = Some(pending);
    Ok(url)
}

/// Finish a login with the pasted authorization code. Exchanges it for tokens, stores them, and
/// rebuilds the open workspace so the subscription takes effect immediately.
#[tauri::command]
pub async fn auth_complete(state: State<'_, AppState>, code: String) -> AppResult<()> {
    let pending = state
        .pending_oauth
        .lock()
        .unwrap()
        .take()
        .ok_or_else(|| AppError::Message("no login in progress — start again".to_string()))?;

    let tokens = crate::oauth::exchange_code(&code, &pending)
        .await
        .map_err(AppError::Message)?;
    crate::oauth::store_tokens(&tokens)?;
    rebuild_current(&state)
}

/// Whether an Anthropic subscription is currently connected.
#[tauri::command]
pub async fn auth_status(_state: State<'_, AppState>) -> AppResult<bool> {
    Ok(crate::oauth::load_tokens()?.is_some())
}

/// Disconnect the subscription. Clears the stored tokens and rebuilds the workspace so Claude
/// requests fall back to the API key (if one is set).
#[tauri::command]
pub async fn auth_disconnect(state: State<'_, AppState>) -> AppResult<()> {
    crate::oauth::clear_tokens()?;
    rebuild_current(&state)
}
