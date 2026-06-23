//! Shared application state held by Tauri's `manage`. Owns the global [`AppStore`] and the
//! currently-open [`WorkspaceStore`]. The sync rusqlite access happens directly for v0.1
//! (SQLite is local + fast); moving it behind `spawn_blocking` is a later optimization
//! (DESIGN.md §9.6).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tianji_agent::Orchestrator;
use tianji_llm::{ClaudeAuth, ClaudeProvider, DeepSeekProvider, LlmProvider, OllamaProvider};
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
    /// Global agent-mode flags. Stored here (not inside the Orchestrator) so they survive
    /// workspace switches and can be set before any workspace is opened.
    pub autonomous: Arc<AtomicBool>,
    pub free_mode: Arc<AtomicBool>,
    /// Cost meter + budget cap, also kept here so they survive workspace switches. `token_budget`
    /// of 0 = unlimited.
    pub tokens_spent: Arc<AtomicU64>,
    pub token_budget: Arc<AtomicU64>,
    /// An in-flight subscription login between `auth_begin` and `auth_complete` (PKCE verifier +
    /// state). Single-use and short-lived, so it lives here rather than in the keychain.
    pub pending_oauth: Mutex<Option<crate::oauth::PendingOauth>>,
}

impl AppState {
    pub fn new(app: AppStore, workspaces_root: PathBuf) -> Self {
        // Restore a persisted budget so a cap survives restarts.
        let budget = app
            .get_setting("token_budget")
            .ok()
            .flatten()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        Self {
            app,
            workspaces_root,
            pty: PtyManager::new(),
            current: Mutex::new(None),
            autonomous: Arc::new(AtomicBool::new(false)),
            free_mode: Arc::new(AtomicBool::new(false)),
            tokens_spent: Arc::new(AtomicU64::new(0)),
            token_budget: Arc::new(AtomicU64::new(budget)),
            pending_oauth: Mutex::new(None),
        }
    }

    /// Clone out the current workspace (lock held only briefly).
    pub fn current(&self) -> AppResult<Arc<CurrentWorkspace>> {
        self.current.lock().unwrap().clone().ok_or(AppError::NoWorkspace)
    }

    /// The selected model id (persisted in app settings; defaults to Opus). A `ollama:<name>`
    /// value selects the local backend.
    pub fn model(&self) -> String {
        self.app
            .get_setting("model")
            .ok()
            .flatten()
            .unwrap_or_else(|| "claude-opus-4-8".to_string())
    }

    /// Zero the cost meter — called when opening a different engagement so spend (and the budget
    /// cap that depends on it) is per-workspace, not cumulative across clients.
    pub fn reset_token_meter(&self) {
        self.tokens_spent.store(0, Ordering::SeqCst);
    }
}

/// Model to use for delegated sub-agents: drop to a cheaper sibling for grunt work. Opus →
/// Sonnet; DeepSeek's reasoner → its (faster, cheaper) chat model. Any other choice — including
/// local `ollama:` models (already free) — passes through unchanged.
fn subagent_model_for(model: &str) -> String {
    if model.contains("opus") {
        "claude-sonnet-4-6".to_string()
    } else if model == "deepseek-reasoner" {
        "deepseek-chat".to_string()
    } else if model == "deepseek-v4-pro" {
        "deepseek-v4-flash".to_string()
    } else {
        model.to_string()
    }
}

/// Default Ollama endpoint when the operator hasn't set one.
pub const DEFAULT_OLLAMA_HOST: &str = "http://localhost:11434";
/// Default Ollama context window. Ollama's own default (~2–4k) is far too small for this agent;
/// 16k comfortably fits the system prompt, tools, and recent history.
pub const DEFAULT_OLLAMA_NUM_CTX: u32 = 16_384;
/// Smallest context window we'll accept — below this the agent's own prompt won't fit.
pub const MIN_OLLAMA_NUM_CTX: u32 = 4_096;
/// At or below this window, switch the orchestrator into small-context mode (terse prompt, harder
/// caps, no delegation) so a tiny local model stays usable.
pub const SMALL_CONTEXT_THRESHOLD: u32 = 8_192;

/// The configured Ollama host (persisted in app settings), normalized without a trailing slash.
pub fn ollama_host(app: &AppStore) -> String {
    app.get_setting("ollama_host")
        .ok()
        .flatten()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_OLLAMA_HOST.to_string())
}

/// Directories scanned for installed Agent Skills (`*/SKILL.md`). An optional `skills_dir` setting
/// is searched first, then the standard `~/.claude/skills` (where `npx skills add …` installs) and a
/// project-local `.claude/skills`. Missing dirs are simply skipped by the scanner.
pub fn skill_dirs(app: &AppStore) -> Vec<std::path::PathBuf> {
    use std::path::PathBuf;
    let mut dirs = Vec::new();
    if let Some(custom) = app.get_setting("skills_dir").ok().flatten() {
        let custom = custom.trim();
        if !custom.is_empty() {
            dirs.push(PathBuf::from(custom));
        }
    }
    let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    if let Ok(home) = std::env::var(home_var) {
        dirs.push(PathBuf::from(&home).join(".claude").join("skills"));
    }
    dirs.push(PathBuf::from(".claude").join("skills"));
    dirs
}

/// The configured Ollama context window (`num_ctx`), clamped to a usable minimum.
pub fn ollama_num_ctx(app: &AppStore) -> u32 {
    app.get_setting("ollama_num_ctx")
        .ok()
        .flatten()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .map(|n| n.max(MIN_OLLAMA_NUM_CTX))
        .unwrap_or(DEFAULT_OLLAMA_NUM_CTX)
}

/// Build the right provider for a model id. A `ollama:<name>` prefix selects the local Ollama
/// backend (free, no API key) at `ollama_host` with the given context window; anything else is a
/// cloud Claude model, authenticated per `auth` (subscription OAuth or API key).
fn build_provider(model: &str, auth: ClaudeAuth, ollama_host: &str, num_ctx: u32) -> Arc<dyn LlmProvider> {
    if let Some(local) = model.strip_prefix("ollama:") {
        Arc::new(
            OllamaProvider::new(local.trim())
                .with_base_url(ollama_host)
                .with_num_ctx(num_ctx),
        )
    } else if model.starts_with("deepseek") {
        // DeepSeek is an OpenAI-compatible cloud model with its own API key (no OAuth, no shared
        // Anthropic auth). An absent key is allowed — the provider errors only when a turn runs.
        let key = crate::secrets::get_api_key("deepseek").ok().flatten().unwrap_or_default();
        Arc::new(DeepSeekProvider::new(key, model))
    } else {
        Arc::new(ClaudeProvider::with_auth(auth).with_model(model))
    }
}

/// Decide how to authenticate Claude requests. A connected Anthropic subscription (OAuth tokens in
/// the keychain) takes precedence over the API key — turns then bill the subscription, not credits.
/// Disconnect the subscription to fall back to the API key.
fn claude_auth() -> ClaudeAuth {
    if crate::oauth::load_tokens().ok().flatten().is_some() {
        ClaudeAuth::Oauth(Arc::new(crate::oauth::KeychainOauthSource))
    } else {
        let key = crate::secrets::get_api_key("anthropic").ok().flatten().unwrap_or_default();
        ClaudeAuth::ApiKey(key)
    }
}

/// The workspace currently in focus. Switching swaps this whole bundle (store, scope, agent).
pub struct CurrentWorkspace {
    pub meta: WorkspaceMeta,
    pub store: WorkspaceStore,
    pub orchestrator: Orchestrator,
}

impl CurrentWorkspace {
    /// Assemble the bundle, wiring the LLM provider (Claude or local Ollama) with the
    /// keychain-stored API key and the selected model (absent key is allowed — the provider errors
    /// only when a turn runs). The mode flags and cost meter are owned by `AppState` so they
    /// survive workspace switches.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        meta: WorkspaceMeta,
        store: WorkspaceStore,
        model: &str,
        autonomous: Arc<AtomicBool>,
        free_mode: Arc<AtomicBool>,
        tokens_spent: Arc<AtomicU64>,
        token_budget: Arc<AtomicU64>,
        app: &AppStore,
    ) -> Self {
        let auth = claude_auth();
        let sudo_pw = crate::secrets::get_api_key("sudo").ok().flatten();
        let host = ollama_host(app);
        let num_ctx = ollama_num_ctx(app);
        let provider = build_provider(model, auth.clone(), &host, num_ctx);

        // Sub-agents do focused grunt work — never run them on Opus. Cap them at Sonnet (or reuse
        // an already-cheaper / local model). A big cost lever: engagements spawn many sub-rounds.
        let subagent_provider = build_provider(&subagent_model_for(model), auth, &host, num_ctx);

        // How much history to pack per turn. On a local model, match its context window (reserving
        // headroom for the reply); on cloud models stay conservative for cost.
        let context_budget = if model.starts_with("ollama:") {
            (num_ctx as usize).saturating_sub(2_048).max(4_000)
        } else {
            // Cloud: the conversation history (re-sent and billed as fresh input every turn, since
            // front-trimming makes it un-cacheable) is the dominant per-message cost. Keep it
            // conservative — key facts survive outside raw history via the notebook/findings/attempt
            // log that get re-injected each turn.
            12_000
        };

        // Small-context mode — a tiny local window (≤ 8k) can't fit the full prompt + a useful
        // conversation, so switch the orchestrator to its terse, hard-bounded, no-delegation variant.
        let small_context = model.starts_with("ollama:") && num_ctx <= SMALL_CONTEXT_THRESHOLD;

        // One cancellation flag shared by the orchestrator AND the command runner, so Stop
        // interrupts an in-flight tool (not just the round loop between tools).
        let cancel = Arc::new(AtomicBool::new(false));
        // RTK output compression: on by default, but a silent no-op unless the `rtk` binary is
        // installed. Operators can disable it by setting `use_rtk` to "false".
        let use_rtk = app.get_setting("use_rtk").ok().flatten().map(|v| v != "false").unwrap_or(true);
        let runner = tianji_agent::ProcessRunner::with_sudo_password(sudo_pw)
            .with_cancel(cancel.clone())
            .with_rtk(use_rtk);
        let orchestrator = Orchestrator::new(provider)
            .with_subagent_provider(subagent_provider)
            .with_runner(Arc::new(runner))
            .with_cancel(cancel)
            .with_budget(tokens_spent, token_budget)
            .with_context_budget(context_budget)
            .with_small_context(small_context)
            .with_skills(tianji_agent::SkillCatalog::discover(&skill_dirs(app)))
            .with_flags(autonomous, free_mode);
        // Restore prior conversations so the agent doesn't start from scratch after a restart,
        // workspace switch, or model/key change (all of which rebuild this bundle).
        orchestrator.hydrate(&store);
        // Load the operator's distilled global habits so they're injected from message one.
        let global_facts = app
            .global_facts()
            .map(|v| v.into_iter().map(|f| f.text).collect())
            .unwrap_or_default();
        orchestrator.set_global_facts(global_facts);
        Self { meta, store, orchestrator }
    }
}
