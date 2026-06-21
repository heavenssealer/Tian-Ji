//! Outbound events (Rust → frontend). The push half of the IPC contract (SKELETON §5). Channel
//! names are kept stable; the frontend mirrors them in `src/lib/events.ts`.

#![allow(dead_code)]

use serde::Serialize;
use tauri::{AppHandle, Emitter};

pub const PTY_OUTPUT: &str = "pty://output";
pub const AGENT_DELTA: &str = "agent://delta";
pub const AGENT_APPROVAL_REQUEST: &str = "agent://approval_request";
pub const NOTES_UPDATED: &str = "notes://updated";
pub const EVENT_APPENDED: &str = "event://appended";

pub fn emit<T: Serialize + Clone>(app: &AppHandle, channel: &str, payload: T) {
    if let Err(e) = app.emit(channel, payload) {
        tracing::warn!(%channel, error = %e, "failed to emit event");
    }
}
