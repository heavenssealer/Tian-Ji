//! Workspace lifecycle: create / open / list / set-phase. Switching swaps the entire context
//! (store, scope, agent, terminals). Backed by the global [`AppStore`] registry and a
//! per-engagement [`WorkspaceStore`].

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::State;

use crate::state::{AppError, AppResult, AppState, CurrentWorkspace};
use tianji_store::{WorkspaceMeta, WorkspaceStore};
use tianji_types::{AgentId, Author, Event, EventKind, Phase, ScopeRules, WorkspaceId};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceInfo {
    pub id: String,
    pub name: String,
    pub phase: String,
    pub scope_cidrs: Vec<String>,
    pub scope_hostnames: Vec<String>,
    pub scope_url_domains: Vec<String>,
}

fn phase_str(p: Phase) -> &'static str {
    match p {
        Phase::Recon => "recon",
        Phase::Hypothesis => "hypothesis",
        Phase::Poc => "poc",
        Phase::Exploit => "exploit",
        Phase::Report => "report",
    }
}

fn parse_phase(s: &str) -> AppResult<Phase> {
    Ok(match s {
        "recon" => Phase::Recon,
        "hypothesis" => Phase::Hypothesis,
        "poc" => Phase::Poc,
        "exploit" => Phase::Exploit,
        "report" => Phase::Report,
        other => return Err(AppError::Message(format!("unknown phase: {other}"))),
    })
}

/// Filesystem-safe slug for the workspace directory name.
fn slug(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

#[tauri::command]
pub async fn workspace_list(state: State<'_, AppState>) -> AppResult<Vec<WorkspaceInfo>> {
    let metas = state.app.list_workspaces()?;
    let mut out = Vec::with_capacity(metas.len());
    for m in metas {
        // Read the persisted phase from each workspace's own DB.
        let phase = WorkspaceStore::open(std::path::Path::new(&m.root_path))
            .and_then(|s| s.current_phase())
            .unwrap_or(Phase::Recon);
        let scope = WorkspaceStore::open(std::path::Path::new(&m.root_path))
            .and_then(|s| s.scope())
            .unwrap_or_default();
        out.push(WorkspaceInfo {
            id: m.id.to_string(),
            name: m.name,
            phase: phase_str(phase).into(),
            scope_cidrs: scope.cidrs,
            scope_hostnames: scope.hostnames,
            scope_url_domains: scope.url_domains,
        });
    }
    Ok(out)
}

#[tauri::command]
pub async fn workspace_create(
    state: State<'_, AppState>,
    name: String,
    scope_cidrs: Vec<String>,
) -> AppResult<WorkspaceInfo> {
    let dir = state.workspaces_root.join(slug(&name));
    let store = WorkspaceStore::open(&dir)?;
    store.set_scope(&ScopeRules { cidrs: scope_cidrs.clone(), ..Default::default() })?;

    let meta = WorkspaceMeta {
        id: store.workspace_id(),
        name: name.clone(),
        root_path: dir.to_string_lossy().into_owned(),
    };
    state.app.register_workspace(&meta)?;

    let info = WorkspaceInfo {
        id: meta.id.to_string(),
        name,
        phase: "recon".into(),
        scope_cidrs: scope_cidrs.clone(),
        scope_hostnames: vec![],
        scope_url_domains: vec![],
    };
    let model = state.model();
    *state.current.lock().unwrap() = Some(Arc::new(CurrentWorkspace::build(
        meta, store, &model, state.autonomous.clone(), state.free_mode.clone(),
    )));
    Ok(info)
}

#[tauri::command]
pub async fn workspace_open(state: State<'_, AppState>, id: String) -> AppResult<WorkspaceInfo> {
    let meta = state
        .app
        .list_workspaces()?
        .into_iter()
        .find(|m| m.id.to_string() == id)
        .ok_or_else(|| AppError::Message(format!("workspace {id} not found")))?;

    let store = WorkspaceStore::open(std::path::Path::new(&meta.root_path))?;
    let scope = store.scope().unwrap_or_default();
    let info = WorkspaceInfo {
        id: meta.id.to_string(),
        name: meta.name.clone(),
        phase: phase_str(store.current_phase()?).into(),
        scope_cidrs: scope.cidrs,
        scope_hostnames: scope.hostnames,
        scope_url_domains: scope.url_domains,
    };
    let model = state.model();
    *state.current.lock().unwrap() = Some(Arc::new(CurrentWorkspace::build(
        meta, store, &model, state.autonomous.clone(), state.free_mode.clone(),
    )));
    Ok(info)
}

#[tauri::command]
pub async fn workspace_set_scope(
    state: State<'_, AppState>,
    cidrs: Vec<String>,
    hostnames: Vec<String>,
    url_domains: Vec<String>,
) -> AppResult<()> {
    let cw = state.current()?;
    cw.store.set_scope(&ScopeRules { cidrs, hostnames, url_domains })?;
    Ok(())
}

#[tauri::command]
pub async fn workspace_rename(
    state: State<'_, AppState>,
    id: String,
    name: String,
) -> AppResult<()> {
    let uuid = tianji_types::uuid::Uuid::parse_str(&id)
        .map_err(|e| AppError::Message(e.to_string()))?;
    state.app.rename_workspace(WorkspaceId(uuid), &name)?;
    Ok(())
}

#[tauri::command]
pub async fn workspace_delete(
    state: State<'_, AppState>,
    id: String,
) -> AppResult<()> {
    let uuid = tianji_types::uuid::Uuid::parse_str(&id)
        .map_err(|e| AppError::Message(e.to_string()))?;
    let ws_id = WorkspaceId(uuid);
    state.app.remove_workspace(ws_id)?;
    // Clear current if this was the open workspace.
    let mut current = state.current.lock().unwrap();
    if current.as_ref().map(|cw| cw.meta.id == ws_id).unwrap_or(false) {
        *current = None;
    }
    Ok(())
}

#[tauri::command]
pub async fn workspace_set_phase(state: State<'_, AppState>, phase: String) -> AppResult<()> {
    let phase = parse_phase(&phase)?;
    let cw = state.current()?;

    cw.store.set_phase(phase)?;
    cw.store.append(Event::new(
        cw.meta.id,
        phase,
        EventKind::PhaseChange,
        AgentId::human(),
        Author::User,
        json!({ "phase": phase_str(phase) }),
    ))?;
    Ok(())
}
