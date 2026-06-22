//! IPC inbound surface (SKELETON §5). Each submodule groups `#[tauri::command]` handlers by
//! domain. Handlers are thin: validate, call into the library crates, marshal the result.

pub mod agent;
pub mod auth;
pub mod notes;
pub mod policy;
pub mod query;
pub mod settings;
pub mod terminal;
pub mod workspace;
