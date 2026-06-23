//! # tianji-store — event log + read-models (DESIGN.md §5, §9.1)
//!
//! Two stores, both here:
//! - [`AppStore`]   — global, one per install: workspace registry, global allow-rules, settings.
//! - [`WorkspaceStore`] — one per engagement: the append-only event log + read-models + scope
//!   + workspace allow-rules + meta (current phase, this workspace's id).
//!
//! Backed by `rusqlite` (sync). Each store wraps a `Mutex<Connection>` so it is `Send + Sync`
//! and can live in Tauri's shared state; the async boundary (`spawn_blocking`) lives in the
//! Tauri layer (DESIGN.md §9.6). The append-only log is the source of truth; `findings` is a
//! cached projection kept current from it.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use tianji_policy::AllowRule;
use tianji_types::uuid::Uuid;
use tianji_types::{
    AgentId, Author, Event, EventId, EventKind, Finding, Phase, ScopeRules, WorkspaceId,
};

mod schema;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("workspace not found: {0}")]
    NotFound(WorkspaceId),
    #[error("datetime error: {0}")]
    DateTime(String),
    #[error("parse error: {0}")]
    Parse(String),
}

type Result<T> = std::result::Result<T, StoreError>;

// ---- small encode/decode helpers ------------------------------------------------------------

/// JSON-encode a scalar/enum for a text column (e.g. `Phase::Recon` -> `"recon"`).
fn enc<T: Serialize>(v: &T) -> Result<String> {
    Ok(serde_json::to_string(v)?)
}

fn dec<T: DeserializeOwned>(s: &str) -> Result<T> {
    Ok(serde_json::from_str(s)?)
}

fn fmt_ts(ts: &OffsetDateTime) -> Result<String> {
    ts.format(&Rfc3339).map_err(|e| StoreError::DateTime(e.to_string()))
}

fn parse_ts(s: &str) -> Result<OffsetDateTime> {
    OffsetDateTime::parse(s, &Rfc3339).map_err(|e| StoreError::DateTime(e.to_string()))
}

fn parse_uuid(s: &str) -> Result<Uuid> {
    Uuid::parse_str(s).map_err(|e| StoreError::Parse(e.to_string()))
}

// =============================================================================================
// AppStore
// =============================================================================================

/// Registry row in the [`AppStore`].
#[derive(Debug, Clone)]
pub struct WorkspaceMeta {
    pub id: WorkspaceId,
    pub name: String,
    pub root_path: String,
}

/// A distilled, always-injected profile fact (an operator habit or an engagement detail). `id`
/// is the row id within its store; `pinned` facts are kept verbatim and never auto-pruned.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileFact {
    pub id: i64,
    pub text: String,
    pub pinned: bool,
}

/// Global, install-wide store. Holds *which* workspaces exist — nothing engagement-specific.
pub struct AppStore {
    conn: Mutex<Connection>,
}

impl AppStore {
    pub fn open(app_data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(app_data_dir).ok();
        let conn = Connection::open(app_data_dir.join("app.sqlite"))?;
        schema::init_app_db(&conn)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        schema::init_app_db(&conn)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn register_workspace(&self, meta: &WorkspaceMeta) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO workspaces (id, name, root_path, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            (
                meta.id.0.to_string(),
                &meta.name,
                &meta.root_path,
                fmt_ts(&OffsetDateTime::now_utc())?,
            ),
        )?;
        Ok(())
    }

    pub fn list_workspaces(&self) -> Result<Vec<WorkspaceMeta>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT id, name, root_path FROM workspaces ORDER BY created_at DESC")?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|(id, name, root_path)| {
                Ok(WorkspaceMeta { id: WorkspaceId(parse_uuid(&id)?), name, root_path })
            })
            .collect()
    }

    pub fn rename_workspace(&self, id: WorkspaceId, name: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE workspaces SET name = ?1 WHERE id = ?2",
            (name, id.to_string()),
        )?;
        Ok(())
    }

    pub fn remove_workspace(&self, id: WorkspaceId) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM workspaces WHERE id = ?1", [id.to_string()])?;
        Ok(())
    }

    pub fn global_rules(&self) -> Result<Vec<AllowRule>> {
        let conn = self.conn.lock().unwrap();
        read_rules(&conn, "global_allow_rules")
    }

    pub fn remove_global_rule(&self, rule_json: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM global_allow_rules WHERE rule_json = ?1",
            [rule_json],
        )?;
        Ok(())
    }

    /// Promote a workspace-scoped rule to global (DESIGN.md §4.3).
    pub fn add_global_rule(&self, rule: &AllowRule) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO global_allow_rules (rule_json) VALUES (?1)",
            [enc(rule)?],
        )?;
        Ok(())
    }

    // ---- global profile facts (the operator's enduring, cross-engagement habits) -----------

    /// Add a global habit. De-duplicates on case-insensitive text; returns the (existing or new) id.
    pub fn add_global_fact(&self, text: &str) -> Result<i64> {
        fact_add(&self.conn.lock().unwrap(), "global_facts", text)
    }

    pub fn global_facts(&self) -> Result<Vec<ProfileFact>> {
        fact_list(&self.conn.lock().unwrap(), "global_facts")
    }

    pub fn remove_global_fact(&self, id: i64) -> Result<()> {
        fact_remove(&self.conn.lock().unwrap(), "global_facts", id)
    }

    pub fn pin_global_fact(&self, id: i64, pinned: bool) -> Result<()> {
        fact_set_pinned(&self.conn.lock().unwrap(), "global_facts", id, pinned)
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            (key, value),
        )?;
        Ok(())
    }
}

// =============================================================================================
// WorkspaceStore
// =============================================================================================

/// Per-engagement store. The append-only log lives here.
pub struct WorkspaceStore {
    conn: Mutex<Connection>,
    workspace_id: WorkspaceId,
}

impl WorkspaceStore {
    /// Open or create the workspace DB at `<root>/workspace.sqlite`. A fresh DB is assigned a
    /// new [`WorkspaceId`], persisted in `meta`; an existing one keeps its id.
    pub fn open(workspace_root: &Path) -> Result<Self> {
        std::fs::create_dir_all(workspace_root).ok();
        let conn = Connection::open(workspace_root.join("workspace.sqlite"))?;
        Self::from_conn(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::from_conn(Connection::open_in_memory()?)
    }

    fn from_conn(conn: Connection) -> Result<Self> {
        schema::init_workspace_db(&conn)?;
        let workspace_id = match get_meta(&conn, "workspace_id")? {
            Some(s) => WorkspaceId(parse_uuid(&s)?),
            None => {
                let id = WorkspaceId::new();
                set_meta(&conn, "workspace_id", &id.0.to_string())?;
                id
            }
        };
        Ok(Self { conn: Mutex::new(conn), workspace_id })
    }

    pub fn workspace_id(&self) -> WorkspaceId {
        self.workspace_id
    }

    // ---- the append-only log -------------------------------------------------------------

    /// Append a fact. **Never updates** — corrections are new events. On a `Finding` event the
    /// findings read-model is updated as a side effect; a malformed finding payload is logged
    /// and skipped (the log remains the source of truth).
    pub fn append(&self, event: Event) -> Result<EventId> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO events
                (id, workspace_id, phase, kind, actor, author, parent_id, payload, ts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            (
                event.id.0.to_string(),
                event.workspace_id.0.to_string(),
                enc(&event.phase)?,
                enc(&event.kind)?,
                &event.actor.0,
                enc(&event.author)?,
                event.parent_id.map(|p| p.0.to_string()),
                serde_json::to_string(&event.payload)?,
                fmt_ts(&event.ts)?,
            ),
        )?;

        if matches!(event.kind, EventKind::Finding) {
            if let Err(e) = project_finding(&conn, &event) {
                tracing::warn!(event_id = %event.id, error = %e, "skipping finding projection");
            }
        }
        Ok(event.id)
    }

    pub fn events_in_phase(&self, phase: Phase) -> Result<Vec<Event>> {
        let conn = self.conn.lock().unwrap();
        query_events(
            &conn,
            "SELECT id, workspace_id, phase, kind, actor, author, parent_id, payload, ts
             FROM events WHERE phase = ?1 ORDER BY ts ASC",
            rusqlite::params![enc(&phase)?],
        )
    }

    pub fn recent_events(&self, limit: usize) -> Result<Vec<Event>> {
        let conn = self.conn.lock().unwrap();
        query_events(
            &conn,
            "SELECT id, workspace_id, phase, kind, actor, author, parent_id, payload, ts
             FROM events ORDER BY ts DESC LIMIT ?1",
            rusqlite::params![limit as i64],
        )
    }

    /// All user-authored notes, newest first — injected into agent context every turn.
    pub fn notes(&self, limit: usize) -> Result<Vec<Event>> {
        let conn = self.conn.lock().unwrap();
        query_events(
            &conn,
            "SELECT id, workspace_id, phase, kind, actor, author, parent_id, payload, ts
             FROM events WHERE kind = ?1 AND author = ?2 ORDER BY ts ASC LIMIT ?3",
            rusqlite::params![enc(&EventKind::Note)?, enc(&Author::User)?, limit as i64],
        )
    }

    /// The most recent traced attempts (newest first), used to remind the agent what it has
    /// already tried so it doesn't loop on dead ends.
    pub fn attempts(&self, limit: usize) -> Result<Vec<Event>> {
        let conn = self.conn.lock().unwrap();
        query_events(
            &conn,
            "SELECT id, workspace_id, phase, kind, actor, author, parent_id, payload, ts
             FROM events WHERE kind = ?1 ORDER BY ts DESC LIMIT ?2",
            rusqlite::params![enc(&EventKind::Attempt)?, limit as i64],
        )
    }

    /// v0.1 recall: keyword over note / finding / agent-msg payloads. Vector recall is v0.2.
    pub fn keyword_recall(&self, query: &str, k: usize) -> Result<Vec<Event>> {
        let conn = self.conn.lock().unwrap();
        let like = format!("%{query}%");
        query_events(
            &conn,
            "SELECT id, workspace_id, phase, kind, actor, author, parent_id, payload, ts
             FROM events
             WHERE kind IN (?1, ?2, ?3) AND payload LIKE ?4
             ORDER BY ts DESC LIMIT ?5",
            rusqlite::params![
                enc(&EventKind::Note)?,
                enc(&EventKind::Finding)?,
                enc(&EventKind::AgentMsg)?,
                like,
                k as i64
            ],
        )
    }

    /// Full-text recall for the agent's `recall` tool: search the richer event kinds (tool outputs,
    /// findings, notes, agent messages, attempts) and return the FULL stored payloads (not the
    /// summarized/compacted form) so a dropped detail can be pulled back into context on demand.
    pub fn search_events(&self, query: &str, limit: usize) -> Result<Vec<Event>> {
        let conn = self.conn.lock().unwrap();
        let like = format!("%{query}%");
        query_events(
            &conn,
            "SELECT id, workspace_id, phase, kind, actor, author, parent_id, payload, ts
             FROM events
             WHERE kind IN (?1, ?2, ?3, ?4, ?5) AND payload LIKE ?6
             ORDER BY ts DESC LIMIT ?7",
            rusqlite::params![
                enc(&EventKind::ToolOutput)?,
                enc(&EventKind::Finding)?,
                enc(&EventKind::Note)?,
                enc(&EventKind::AgentMsg)?,
                enc(&EventKind::Attempt)?,
                like,
                limit as i64
            ],
        )
    }

    /// Hard-delete an event by id. Used for note/auto-note dismissal (user correcting records).
    pub fn event_delete(&self, id: EventId) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM events WHERE id = ?1", [id.0.to_string()])?;
        Ok(())
    }

    /// Update the text payload of a user note in place. Errors if the id is not a `note` event.
    pub fn note_update(&self, id: EventId, text: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let payload = serde_json::to_string(&serde_json::json!({ "text": text }))?;
        conn.execute(
            "UPDATE events SET payload = ?1 WHERE id = ?2 AND kind = ?3",
            rusqlite::params![payload, id.0.to_string(), enc(&EventKind::Note)?],
        )?;
        Ok(())
    }

    // ---- conversation persistence --------------------------------------------------------

    /// Persist a session's serialized conversation (a JSON `Vec<Message>`) so it survives app
    /// restarts and orchestrator rebuilds. Stored in the `meta` kv under `conv:<session_id>`.
    /// The event log remains the audit source of truth; this is a runtime cache of the projection.
    pub fn save_conversation(&self, session_id: &str, messages_json: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        set_meta(&conn, &format!("conv:{session_id}"), messages_json)
    }

    /// Load every persisted conversation as `(session_id, messages_json)` pairs.
    pub fn load_conversations(&self) -> Result<Vec<(String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT key, value FROM meta WHERE key LIKE 'conv:%'")?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .map(|(k, v)| (k.trim_start_matches("conv:").to_string(), v))
            .collect())
    }

    // ---- per-workspace profile facts (this engagement's state — never leaves this DB) ------

    pub fn add_workspace_fact(&self, text: &str) -> Result<i64> {
        fact_add(&self.conn.lock().unwrap(), "workspace_facts", text)
    }

    pub fn workspace_facts(&self) -> Result<Vec<ProfileFact>> {
        fact_list(&self.conn.lock().unwrap(), "workspace_facts")
    }

    pub fn remove_workspace_fact(&self, id: i64) -> Result<()> {
        fact_remove(&self.conn.lock().unwrap(), "workspace_facts", id)
    }

    pub fn pin_workspace_fact(&self, id: i64, pinned: bool) -> Result<()> {
        fact_set_pinned(&self.conn.lock().unwrap(), "workspace_facts", id, pinned)
    }

    // ---- read-models & config ------------------------------------------------------------

    pub fn findings(&self) -> Result<Vec<Finding>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, workspace_id, severity, target, summary, evidence_event_ids FROM findings",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|(id, ws, severity, target, summary, evidence)| {
                Ok(Finding {
                    id: EventId(parse_uuid(&id)?),
                    workspace_id: WorkspaceId(parse_uuid(&ws)?),
                    severity,
                    target,
                    summary,
                    evidence_event_ids: dec(&evidence)?,
                })
            })
            .collect()
    }

    pub fn current_phase(&self) -> Result<Phase> {
        let conn = self.conn.lock().unwrap();
        match get_meta(&conn, "current_phase")? {
            Some(s) => dec(&s),
            None => Ok(Phase::default()),
        }
    }

    pub fn set_phase(&self, phase: Phase) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        set_meta(&conn, "current_phase", &enc(&phase)?)
    }

    pub fn scope(&self) -> Result<ScopeRules> {
        let conn = self.conn.lock().unwrap();
        match get_meta(&conn, "scope")? {
            Some(s) => dec(&s),
            None => Ok(ScopeRules::default()),
        }
    }

    pub fn set_scope(&self, scope: &ScopeRules) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        set_meta(&conn, "scope", &enc(scope)?)
    }

    pub fn allow_rules(&self) -> Result<Vec<AllowRule>> {
        let conn = self.conn.lock().unwrap();
        read_rules(&conn, "workspace_allow_rules")
    }

    pub fn add_allow_rule(&self, rule: &AllowRule) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO workspace_allow_rules (rule_json) VALUES (?1)",
            [enc(rule)?],
        )?;
        Ok(())
    }

    pub fn remove_allow_rule(&self, rule_json: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM workspace_allow_rules WHERE rule_json = ?1",
            [rule_json],
        )?;
        Ok(())
    }
}

// ---- shared row helpers ---------------------------------------------------------------------

#[derive(Deserialize)]
struct FindingPayload {
    severity: String,
    target: String,
    summary: String,
    #[serde(default)]
    evidence_event_ids: Vec<EventId>,
}

fn project_finding(conn: &Connection, event: &Event) -> Result<()> {
    let f: FindingPayload = serde_json::from_value(event.payload.clone())?;
    conn.execute(
        "INSERT OR REPLACE INTO findings
            (id, workspace_id, severity, target, summary, evidence_event_ids)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        (
            event.id.0.to_string(),
            event.workspace_id.0.to_string(),
            f.severity,
            f.target,
            f.summary,
            enc(&f.evidence_event_ids)?,
        ),
    )?;
    Ok(())
}

fn query_events(conn: &Connection, sql: &str, params: &[&dyn rusqlite::ToSql]) -> Result<Vec<Event>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map(params, |r| {
            Ok(RawEvent {
                id: r.get(0)?,
                workspace_id: r.get(1)?,
                phase: r.get(2)?,
                kind: r.get(3)?,
                actor: r.get(4)?,
                author: r.get(5)?,
                parent_id: r.get(6)?,
                payload: r.get(7)?,
                ts: r.get(8)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(RawEvent::into_event).collect()
}

struct RawEvent {
    id: String,
    workspace_id: String,
    phase: String,
    kind: String,
    actor: String,
    author: String,
    parent_id: Option<String>,
    payload: String,
    ts: String,
}

impl RawEvent {
    fn into_event(self) -> Result<Event> {
        Ok(Event {
            id: EventId(parse_uuid(&self.id)?),
            workspace_id: WorkspaceId(parse_uuid(&self.workspace_id)?),
            phase: dec(&self.phase)?,
            kind: dec(&self.kind)?,
            actor: AgentId(self.actor),
            author: dec(&self.author)?,
            parent_id: match self.parent_id {
                Some(p) => Some(EventId(parse_uuid(&p)?)),
                None => None,
            },
            payload: serde_json::from_str(&self.payload)?,
            ts: parse_ts(&self.ts)?,
        })
    }
}

fn read_rules(conn: &Connection, table: &str) -> Result<Vec<AllowRule>> {
    let mut stmt = conn.prepare(&format!("SELECT rule_json FROM {table} ORDER BY id ASC"))?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.iter().map(|s| dec(s)).collect()
}

// ---- profile-fact helpers, shared by both stores (tables share a shape) --------------------
// `table` is always a hard-coded constant ("global_facts" / "workspace_facts"), never user input.

fn fact_add(conn: &Connection, table: &str, text: &str) -> Result<i64> {
    let text = text.trim();
    let existing: Option<i64> = conn
        .query_row(
            &format!("SELECT id FROM {table} WHERE lower(text) = lower(?1) LIMIT 1"),
            [text],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        return Ok(id);
    }
    conn.execute(
        &format!("INSERT INTO {table} (text, pinned, created_at) VALUES (?1, 0, ?2)"),
        (text, fmt_ts(&OffsetDateTime::now_utc())?),
    )?;
    Ok(conn.last_insert_rowid())
}

fn fact_list(conn: &Connection, table: &str) -> Result<Vec<ProfileFact>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT id, text, pinned FROM {table} ORDER BY pinned DESC, id ASC"
    ))?;
    let rows = stmt
        .query_map([], |r| {
            Ok(ProfileFact { id: r.get(0)?, text: r.get(1)?, pinned: r.get::<_, i64>(2)? != 0 })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn fact_remove(conn: &Connection, table: &str, id: i64) -> Result<()> {
    conn.execute(&format!("DELETE FROM {table} WHERE id = ?1"), [id])?;
    Ok(())
}

fn fact_set_pinned(conn: &Connection, table: &str, id: i64, pinned: bool) -> Result<()> {
    conn.execute(
        &format!("UPDATE {table} SET pinned = ?1 WHERE id = ?2"),
        rusqlite::params![pinned as i64, id],
    )?;
    Ok(())
}

fn get_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .optional()?)
}

fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
        (key, value),
    )?;
    Ok(())
}

// =============================================================================================
// tests
// =============================================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tianji_policy::{AllowGranularity, AllowRule};
    use tianji_types::Author;

    fn note_event(ws: WorkspaceId, text: &str) -> Event {
        Event::new(
            ws,
            Phase::Recon,
            EventKind::Note,
            AgentId::human(),
            Author::User,
            json!({ "text": text }),
        )
    }

    #[test]
    fn append_and_read_back_roundtrips() {
        let store = WorkspaceStore::open_in_memory().unwrap();
        let ws = store.workspace_id();
        let id = store.append(note_event(ws, "admin form looks custom")).unwrap();

        let recent = store.recent_events(10).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].id, id);
        assert_eq!(recent[0].payload["text"], "admin form looks custom");
    }

    #[test]
    fn profile_facts_crud_dedup_and_pin_ordering() {
        let app = AppStore::open_in_memory().unwrap();
        let id = app.add_global_fact("prefers ffuf").unwrap();
        // Case-insensitive dedup returns the same row, not a duplicate.
        assert_eq!(app.add_global_fact("Prefers ffuf").unwrap(), id);
        app.add_global_fact("checks robots.txt early").unwrap();

        // Pinned facts sort first.
        app.pin_global_fact(app.global_facts().unwrap()[1].id, true).unwrap();
        let facts = app.global_facts().unwrap();
        assert_eq!(facts.len(), 2);
        assert!(facts[0].pinned);

        app.remove_global_fact(id).unwrap();
        assert_eq!(app.global_facts().unwrap().len(), 1);
    }

    #[test]
    fn global_and_workspace_facts_are_separate_stores() {
        let app = AppStore::open_in_memory().unwrap();
        let store = WorkspaceStore::open_in_memory().unwrap();
        app.add_global_fact("operator habit").unwrap();
        store.add_workspace_fact("port 8080 runs Tomcat").unwrap();

        // The engagement detail must not appear in the cross-engagement global store.
        assert!(app.global_facts().unwrap().iter().all(|f| !f.text.contains("Tomcat")));
        assert_eq!(store.workspace_facts().unwrap().len(), 1);
    }

    #[test]
    fn conversation_blob_roundtrips() {
        let store = WorkspaceStore::open_in_memory().unwrap();
        store.save_conversation("default", "[{\"role\":\"user\"}]").unwrap();
        let convs = store.load_conversations().unwrap();
        assert_eq!(convs, vec![("default".to_string(), "[{\"role\":\"user\"}]".to_string())]);
    }

    #[test]
    fn keyword_recall_finds_matching_note() {
        let store = WorkspaceStore::open_in_memory().unwrap();
        let ws = store.workspace_id();
        store.append(note_event(ws, "outdated Apache on .5")).unwrap();
        store.append(note_event(ws, "unrelated thought")).unwrap();

        let hits = store.keyword_recall("Apache", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].payload["text"].as_str().unwrap().contains("Apache"));
    }

    #[test]
    fn finding_event_projects_into_read_model() {
        let store = WorkspaceStore::open_in_memory().unwrap();
        let ws = store.workspace_id();
        let ev = Event::new(
            ws,
            Phase::Hypothesis,
            EventKind::Finding,
            AgentId::human(),
            Author::Agent,
            json!({ "severity": "high", "target": "10.0.0.5", "summary": "outdated Apache" }),
        );
        store.append(ev).unwrap();

        let findings = store.findings().unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, "high");
        assert_eq!(findings[0].target, "10.0.0.5");
    }

    #[test]
    fn phase_and_scope_roundtrip() {
        let store = WorkspaceStore::open_in_memory().unwrap();
        assert_eq!(store.current_phase().unwrap(), Phase::Recon);
        store.set_phase(Phase::Exploit).unwrap();
        assert_eq!(store.current_phase().unwrap(), Phase::Exploit);

        let scope = ScopeRules { cidrs: vec!["10.0.0.0/24".into()], ..Default::default() };
        store.set_scope(&scope).unwrap();
        assert_eq!(store.scope().unwrap().cidrs, vec!["10.0.0.0/24".to_string()]);
    }

    #[test]
    fn allow_rules_persist() {
        let store = WorkspaceStore::open_in_memory().unwrap();
        let rule = AllowRule {
            tool: "nmap".into(),
            granularity: AllowGranularity::ToolFlagShape,
            fingerprint: vec!["-sV".into()],
        };
        store.add_allow_rule(&rule).unwrap();
        let rules = store.allow_rules().unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].tool, "nmap");
    }

    #[test]
    fn app_store_registers_and_lists_workspaces() {
        let app = AppStore::open_in_memory().unwrap();
        let meta = WorkspaceMeta {
            id: WorkspaceId::new(),
            name: "htb-lab".into(),
            root_path: "/tmp/htb-lab".into(),
        };
        app.register_workspace(&meta).unwrap();
        let list = app.list_workspaces().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "htb-lab");
    }
}
