//! Shared application state held by Tauri's `manage`. Owns the global [`AppStore`] and the
//! currently-open [`WorkspaceStore`]. The sync rusqlite access happens directly for v0.1
//! (SQLite is local + fast); moving it behind `spawn_blocking` is a later optimization
//! (DESIGN.md §9.6).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tianji_agent::Orchestrator;
use tianji_llm::ClaudeProvider;
use tianji_pty::PtyManager;
use tianji_store::{AppStore, WorkspaceMeta, WorkspaceStore};

/// Serializable error returned across the IPC boundary. `src-tauri` is the only place library
/// errors get flattened (DESIGN.md §9.5).
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    Message(String),
    #[error("no workspace is open")]
    NoWorkspace,
}

impl serde::Serialize for AppError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        AppError::Message(e.to_string())
    }
}

impl From<tianji_store::StoreError> for AppError {
    fn from(e: tianji_store::StoreError) -> Self {
        AppError::Message(e.to_string())
    }
}

impl From<tianji_pty::PtyError> for AppError {
    fn from(e: tianji_pty::PtyError) -> Self {
        AppError::Message(e.to_string())
    }
}

pub type AppResult<T> = Result<T, AppError>;

/// Process-wide state.
pub struct AppState {
    pub app: AppStore,
    /// Root under which per-engagement workspace directories are created.
    pub workspaces_root: PathBuf,
    pub pty: PtyManager,
    /// The open workspace, behind an `Arc` so a command can clone it out and run a long async
    /// turn without holding the lock (or a `!Send` guard) across `.await`.
    pub current: Mutex<Option<Arc<CurrentWorkspace>>>,
}

impl AppState {
    pub fn new(app: AppStore, workspaces_root: PathBuf) -> Self {
        Self {
            app,
            workspaces_root,
            pty: PtyManager::new(),
            current: Mutex::new(None),
        }
    }

    /// Clone out the current workspace (lock held only briefly).
    pub fn current(&self) -> AppResult<Arc<CurrentWorkspace>> {
        self.current.lock().unwrap().clone().ok_or(AppError::NoWorkspace)
    }

    /// The selected Claude model (persisted in app settings; defaults to Opus).
    pub fn model(&self) -> String {
        self.app
            .get_setting("model")
            .ok()
            .flatten()
            .unwrap_or_else(|| "claude-opus-4-8".to_string())
    }
}

/// The workspace currently in focus. Switching swaps this whole bundle (store, scope, agent).
pub struct CurrentWorkspace {
    pub meta: WorkspaceMeta,
    pub store: WorkspaceStore,
    pub orchestrator: Orchestrator,
}

impl CurrentWorkspace {
    /// Assemble the bundle, wiring the Claude provider with the keychain-stored API key and the
    /// selected model (absent key is allowed — the provider errors only when a turn runs).
    pub fn build(meta: WorkspaceMeta, store: WorkspaceStore, model: &str) -> Self {
        let key = crate::secrets::get_api_key("anthropic").ok().flatten().unwrap_or_default();
        let provider = ClaudeProvider::new(key).with_model(model);
        let orchestrator = Orchestrator::new(Arc::new(provider));
        Self { meta, store, orchestrator }
    }
}
