//! Manual notebook authoring. Notes are `note`-type events with `author = User`; the agent can
//! read them too, so the notebook doubles as a way to steer the agent (DESIGN.md §8.1).

use serde_json::json;
use tauri::{AppHandle, Emitter, State};

use crate::events;
use crate::state::{AppError, AppResult, AppState};
use tianji_types::{uuid::Uuid, AgentId, Author, Event, EventId, EventKind};

#[tauri::command]
pub async fn notes_add(app: AppHandle, state: State<'_, AppState>, markdown: String) -> AppResult<()> {
    let cw = state.current()?;

    cw.store.append(Event::new(
        cw.meta.id,
        cw.store.current_phase()?,
        EventKind::Note,
        AgentId::human(),
        Author::User,
        json!({ "text": markdown }),
    ))?;

    let _ = app.emit(events::NOTES_UPDATED, ());
    Ok(())
}

#[tauri::command]
pub async fn notes_delete(app: AppHandle, state: State<'_, AppState>, id: String) -> AppResult<()> {
    let cw = state.current()?;
    let eid = EventId(Uuid::parse_str(&id).map_err(|e| AppError::Message(e.to_string()))?);
    cw.store.event_delete(eid)?;
    let _ = app.emit(events::NOTES_UPDATED, ());
    Ok(())
}

#[tauri::command]
pub async fn notes_update(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    text: String,
) -> AppResult<()> {
    let cw = state.current()?;
    let eid = EventId(Uuid::parse_str(&id).map_err(|e| AppError::Message(e.to_string()))?);
    cw.store.note_update(eid, &text)?;
    let _ = app.emit(events::NOTES_UPDATED, ());
    Ok(())
}
