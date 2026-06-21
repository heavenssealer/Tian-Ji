//! Terminal lifecycle + user keystrokes. Output streams back over the `pty://output` event.

use tauri::{AppHandle, Emitter, State};

use crate::events;
use crate::state::{AppError, AppResult, AppState};
use tianji_types::{uuid::Uuid, TerminalId};

fn parse_tid(s: &str) -> AppResult<TerminalId> {
    Ok(TerminalId(
        Uuid::parse_str(s).map_err(|e| AppError::Message(e.to_string()))?,
    ))
}

#[tauri::command]
pub async fn terminal_spawn(
    app: AppHandle,
    state: State<'_, AppState>,
    title: String,
) -> AppResult<String> {
    let id = state.pty.spawn(&title)?;
    let mut rx = state.pty.subscribe(id)?;

    // Forward this terminal's output to the matching xterm pane.
    tauri::async_runtime::spawn(async move {
        while let Ok(chunk) = rx.recv().await {
            let _ = app.emit(
                events::PTY_OUTPUT,
                serde_json::json!({
                    "terminal_id": chunk.terminal_id.to_string(),
                    "chunk": chunk.bytes,
                }),
            );
        }
    });

    Ok(id.to_string())
}

#[tauri::command]
pub async fn terminal_write(state: State<'_, AppState>, id: String, data: String) -> AppResult<()> {
    state.pty.write(parse_tid(&id)?, data.as_bytes())?;
    Ok(())
}

#[tauri::command]
pub async fn terminal_resize(
    state: State<'_, AppState>,
    id: String,
    cols: u16,
    rows: u16,
) -> AppResult<()> {
    state.pty.resize(parse_tid(&id)?, rows, cols)?;
    Ok(())
}

#[tauri::command]
pub async fn terminal_close(state: State<'_, AppState>, id: String) -> AppResult<()> {
    state.pty.close(parse_tid(&id)?)?;
    Ok(())
}
