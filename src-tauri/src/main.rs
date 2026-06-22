//! Tiān Jī desktop entry. This binary is **glue only** (DESIGN.md §3, SKELETON §3): it builds
//! the shared [`AppState`] and registers the IPC command/event surface. All real logic lives in
//! the `tianji-*` library crates so it stays testable and Tauri-agnostic.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod events;
mod oauth;
mod secrets;
mod state;

use state::AppState;
use tauri::Manager;
use tianji_store::AppStore;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "tianji=info".into()),
        )
        .init();

    tauri::Builder::default()
        .setup(|app| {
            // Resolve the app data dir, open the global store, and seed shared state.
            let data_dir = app.path().app_data_dir()?;
            let app_store = AppStore::open(&data_dir).map_err(|e| e.to_string())?;
            let workspaces_root = data_dir.join("workspaces");
            app.manage(AppState::new(app_store, workspaces_root));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::workspace::workspace_list,
            commands::workspace::workspace_create,
            commands::workspace::workspace_open,
            commands::workspace::workspace_set_phase,
            commands::workspace::workspace_set_scope,
            commands::workspace::workspace_rename,
            commands::workspace::workspace_delete,
            commands::terminal::terminal_spawn,
            commands::terminal::terminal_write,
            commands::terminal::terminal_resize,
            commands::terminal::terminal_close,
            commands::agent::agent_prompt,
            commands::agent::agent_run_goal,
            commands::agent::agent_set_free_mode,
            commands::agent::agent_set_autonomous,
            commands::agent::agent_set_token_budget,
            commands::agent::agent_distill_profile,
            commands::agent::profile_facts_list,
            commands::agent::profile_fact_add,
            commands::agent::profile_fact_remove,
            commands::agent::profile_fact_pin,
            commands::agent::agent_cancel,
            commands::agent::agent_new_session,
            commands::agent::agent_switch_session,
            commands::policy::policy_resolve,
            commands::policy::policy_rules_list,
            commands::policy::policy_rule_remove,
            commands::notes::notes_add,
            commands::notes::notes_delete,
            commands::notes::notes_update,
            commands::query::events_query,
            commands::query::findings_query,
            commands::auth::auth_begin,
            commands::auth::auth_complete,
            commands::auth::auth_status,
            commands::auth::auth_disconnect,
            commands::settings::settings_set_api_key,
            commands::settings::settings_has_api_key,
            commands::settings::settings_set_sudo_password,
            commands::settings::settings_has_sudo_password,
            commands::settings::settings_list_models,
            commands::settings::settings_get_model,
            commands::settings::settings_set_model,
            commands::settings::settings_get_ollama_host,
            commands::settings::settings_set_ollama_host,
            commands::settings::settings_get_ollama_num_ctx,
            commands::settings::settings_set_ollama_num_ctx,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tiān Jī");
}
