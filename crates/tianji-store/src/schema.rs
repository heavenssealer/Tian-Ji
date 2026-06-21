//! Schema migrations. Versioned via SQLite's `user_version` pragma — each `init_*` applies any
//! migrations newer than the DB's current version, so opening an existing DB is idempotent.

use rusqlite::Connection;

use crate::StoreError;

/// Shared connection setup: WAL for append-heavy workloads, FK enforcement, a busy timeout.
/// `journal_mode` returns a row, so it must go through `execute_batch`, not `pragma_update`.
/// (WAL is silently ignored for in-memory DBs, which is fine.)
pub fn configure(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(())
}

fn user_version(conn: &Connection) -> Result<i64, StoreError> {
    Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

fn set_user_version(conn: &Connection, v: i64) -> Result<(), StoreError> {
    conn.pragma_update(None, "user_version", v)?;
    Ok(())
}

/// Global, install-wide DB: the workspace registry, global allow-rules, settings.
pub fn init_app_db(conn: &Connection) -> Result<(), StoreError> {
    configure(conn)?;
    if user_version(conn)? < 1 {
        conn.execute_batch(APP_V1)?;
        set_user_version(conn, 1)?;
    }
    Ok(())
}

/// Per-engagement DB: the append-only event log + read-models + scope + allow-rules + meta.
pub fn init_workspace_db(conn: &Connection) -> Result<(), StoreError> {
    configure(conn)?;
    if user_version(conn)? < 1 {
        conn.execute_batch(WORKSPACE_V1)?;
        set_user_version(conn, 1)?;
    }
    Ok(())
}

const APP_V1: &str = "
CREATE TABLE workspaces (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    root_path  TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE TABLE global_allow_rules (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    rule_json TEXT NOT NULL
);
CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

const WORKSPACE_V1: &str = "
CREATE TABLE events (
    id           TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    phase        TEXT NOT NULL,
    kind         TEXT NOT NULL,
    actor        TEXT NOT NULL,
    author       TEXT NOT NULL,
    parent_id    TEXT,
    payload      TEXT NOT NULL,
    ts           TEXT NOT NULL
);
CREATE INDEX idx_events_ts    ON events(ts);
CREATE INDEX idx_events_phase ON events(phase);
CREATE INDEX idx_events_kind  ON events(kind);

CREATE TABLE findings (
    id                 TEXT PRIMARY KEY,
    workspace_id       TEXT NOT NULL,
    severity           TEXT NOT NULL,
    target             TEXT NOT NULL,
    summary            TEXT NOT NULL,
    evidence_event_ids TEXT NOT NULL
);

CREATE TABLE workspace_allow_rules (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    rule_json TEXT NOT NULL
);

CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";
