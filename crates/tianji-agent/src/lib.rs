//! # tianji-agent — the runtime that ties it together (DESIGN.md §3.6)
//!
//! One turn: build context → [`LlmProvider::run_turn`] → for each proposed tool call, route
//! through `tianji-policy` → (auto-run | park for approval | deny) → run via the
//! [`CommandRunner`] → append events to the [`WorkspaceStore`] → feed results back → repeat.
//!
//! v0.1: single agent, in-process MCP host with a `run_command` tool only (DESIGN.md §9.4).
//! Tool execution is a captured one-shot subprocess; mirroring into a live terminal is a later
//! enhancement tied to the interactive-tools work (DESIGN.md §11.2). Multi-agent orchestration
//! is reserved (the `actor`/`parent_id` fields already exist).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use futures::StreamExt;
use serde_json::json;
use tokio::sync::mpsc::UnboundedSender;

use tianji_llm::LlmProvider;
use tianji_policy::{classify, decide, resolve_targets, AllowRule};
use tianji_store::WorkspaceStore;
use tianji_types::{
    AgentEvent, AgentId, Author, Classification, Content, Decision, Event, EventKind, Message,
    Phase, Role, ScopeRules, ToolCall, WorkspaceId,
};

mod skills;
mod approval;
mod assembler;
mod mcp;
mod runner;
mod summary;

pub use approval::{ApprovalGate, ApprovalOutcome, ApprovalToken, ProposedCall};
pub use assembler::ContextAssembler;
pub use mcp::McpHost;
pub use runner::{detect_rtk, CommandRunner, ProcessRunner};
pub use skills::{Skill, SkillCatalog};

/// Hard cap on tool-use rounds per orchestrator prompt.
const MAX_ROUNDS: usize = 8;
/// Stricter cap for sub-agents — they are focused tasks, not open-ended sessions.
const MAX_SUBAGENT_ROUNDS: usize = 5;
/// Default and maximum token budgets for a single delegation.
const DEFAULT_SUBAGENT_BUDGET: u32 = 4_000;
const MAX_SUBAGENT_BUDGET: u32 = 8_000;

/// Standalone (autonomous goal) mode safety rails: how many orchestrator prompt-cycles the goal
/// loop may run, and the cumulative token ceiling across them, before it stops on its own.
const MAX_GOAL_ITERATIONS: usize = 15;
const MAX_GOAL_TOKENS: u64 = 600_000;
/// How many times the agent may issue the exact same command within one prompt-cycle before the
/// loop guard refuses to run it again (it's spinning — the repeat won't help).
const LOOP_LIMIT: usize = 3;
/// How many recent events the profile distiller reviews, and the minimum needed to bother.
const DISTILL_EVENT_SCAN: usize = 60;
const DISTILL_MIN_EVENTS: usize = 4;
/// Compaction (rolling summary): once the stored history exceeds `COMPACT_TRIGGER_PCT`% of the
/// context budget, summarize the oldest turns into a compact brief and keep only the newest
/// `COMPACT_KEEP_PCT`% verbatim. This is how a long agentic run stays cheap — instead of re-sending
/// (and re-billing) an ever-growing transcript every turn, old turns collapse to a dense summary.
const COMPACT_TRIGGER_PCT: usize = 75;
const COMPACT_KEEP_PCT: usize = 40;
/// `recall` tool: how many matching events to return and the per-event char cap, so an on-demand
/// recall can't itself blow the window (the agent narrows its query if it needs more).
const RECALL_HITS: usize = 4;
const RECALL_CHARS_PER_HIT: usize = 2_500;
/// Sentinels the model emits to end the goal loop. Kept distinctive so they don't appear by
/// accident in ordinary prose.
const GOAL_DONE_TOKEN: &str = "[[GOAL_COMPLETE]]";
const GOAL_STUCK_TOKEN: &str = "[[GOAL_BLOCKED]]";

/// How a standalone goal run ended. The label is surfaced to the operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GoalOutcome {
    Completed,
    Blocked,
    MaxIterations,
    BudgetExhausted,
    Cancelled,
}

impl GoalOutcome {
    fn label(self) -> &'static str {
        match self {
            GoalOutcome::Completed => "completed",
            GoalOutcome::Blocked => "blocked",
            GoalOutcome::MaxIterations => "max-iterations",
            GoalOutcome::BudgetExhausted => "budget-exhausted",
            GoalOutcome::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error(transparent)]
    Store(#[from] tianji_store::StoreError),
    #[error("llm error: {0}")]
    Llm(String),
}

type Result<T> = std::result::Result<T, AgentError>;

/// A message pushed toward the UI during a turn. The Tauri layer maps these to events.
#[derive(Debug, Clone)]
pub enum AgentUpdate {
    Text(String),
    ToolStarted { tool: String, argv: Vec<String> },
    ToolOutput { text: String },
    ApprovalRequest { token: ApprovalToken, call: ProposedCall },
    Denied { reason: String },
    FindingRecorded { severity: String, target: String, summary: String },
    TokensUsed { input: u32, output: u32 },
    /// A sub-agent was spawned via delegate_to_agent.
    SubAgentStarted { name: String, objective: String },
    /// Streaming text from a running sub-agent.
    SubAgentText { name: String, text: String },
    /// A sub-agent completed and produced a summary.
    SubAgentFinished { name: String, summary: String },
    /// A standalone (autonomous goal) run began.
    GoalStarted { goal: String },
    /// The goal loop advanced to its Nth self-directed iteration.
    GoalIteration { iteration: u32 },
    /// The goal loop ended; `outcome` is a [`GoalOutcome`] label.
    GoalFinished { outcome: String, iterations: u32 },
    /// Old turns were rolled up into a summary; `summarized` is how many messages collapsed.
    Compacted { summarized: usize },
    /// The agent loaded an installed skill via `use_skill`.
    SkillUsed { name: String },
    TurnEnded,
    Error(String),
}

/// The per-workspace agent runtime. Owns the provider, the approval gate, the tool host, and a
/// command runner; borrows the workspace store per turn.
pub struct Orchestrator {
    provider: Arc<dyn LlmProvider>,
    /// Provider used for delegated sub-agents. Defaults to `provider`, but the host wires a
    /// cheaper model here (sub-agents do focused grunt work — paying Opus rates for it is waste).
    subagent_provider: Arc<dyn LlmProvider>,
    gate: Arc<ApprovalGate>,
    mcp: McpHost,
    runner: Arc<dyn CommandRunner>,
    actor: AgentId,
    /// When true, `NeedsApproval` decisions are auto-executed without prompting the user.
    /// Scope and explicit-deny policy still apply.
    autonomous: Arc<AtomicBool>,
    /// When true, ALL policy checks are skipped — scope, classification, approval, everything.
    /// The LLM runs any command it judges useful. Use only in lab/trusted engagements.
    free_mode: Arc<AtomicBool>,
    /// Set to true by `cancel()`; checked between rounds and stream events.
    cancelled: Arc<AtomicBool>,
    /// True while a standalone goal run is in progress. Like `autonomous`, it auto-approves
    /// NeedsApproval tools (a goal loop must not stall on an approval dialog), but it is internal
    /// and transient so it never clobbers the operator's persisted autonomous setting.
    goal_active: Arc<AtomicBool>,
    /// Cumulative input+output tokens spent on this workspace (the cost meter).
    tokens_spent: Arc<AtomicU64>,
    /// Hard cap on cumulative tokens; 0 means unlimited. When `tokens_spent` reaches it, runs stop
    /// before starting another model round.
    token_budget: Arc<AtomicU64>,
    /// Per-session conversation history. Key = session id, value = past messages.
    histories: std::sync::Mutex<HashMap<String, Vec<Message>>>,
    /// The currently-active session id.
    active_session: std::sync::Mutex<String>,
    /// Dedup cache for read-only commands: key = session+tool+argv, value = prior output. Stops
    /// the model from burning tokens re-running identical scans (DESIGN.md §efficiency).
    command_cache: std::sync::Mutex<HashMap<String, String>>,
    /// The operator's distilled global habits (cross-engagement), loaded from the app store and
    /// always injected into the system prompt. Per-workspace facts are read from the store per
    /// turn; these aren't in the workspace DB, so the orchestrator caches them here.
    global_facts: std::sync::Mutex<Vec<String>>,
    /// Max tokens of context to pack per turn. Defaults conservatively (cost control on cloud
    /// models); the host raises it to match a local model's configured context window.
    context_budget: usize,
    /// Small-context mode — set when the active model has a tiny window (e.g. an 8k local LLM).
    /// Switches the system prompt to a terse variant, bounds notes/profile harder, tightens the
    /// tool-output cap, and tells the agent not to delegate (sub-agents multiply context).
    small_context: bool,
    /// Installed Agent Skills (CTF playbooks etc.). Catalog goes into the prompt; the `use_skill`
    /// tool loads full instructions on demand. Available to cloud and local agents alike.
    skills: SkillCatalog,
}

impl Orchestrator {
    pub fn new(provider: Arc<dyn LlmProvider>) -> Self {
        let mut histories = HashMap::new();
        histories.insert("default".to_string(), Vec::new());
        Self {
            subagent_provider: provider.clone(),
            provider,
            gate: Arc::new(ApprovalGate::default()),
            mcp: McpHost::new(),
            runner: Arc::new(ProcessRunner::new()),
            actor: AgentId("agent".to_string()),
            autonomous: Arc::new(AtomicBool::new(false)),
            free_mode: Arc::new(AtomicBool::new(false)),
            cancelled: Arc::new(AtomicBool::new(false)),
            goal_active: Arc::new(AtomicBool::new(false)),
            tokens_spent: Arc::new(AtomicU64::new(0)),
            token_budget: Arc::new(AtomicU64::new(0)),
            histories: std::sync::Mutex::new(histories),
            active_session: std::sync::Mutex::new("default".to_string()),
            command_cache: std::sync::Mutex::new(HashMap::new()),
            global_facts: std::sync::Mutex::new(Vec::new()),
            context_budget: 16_000,
            small_context: false,
            skills: SkillCatalog::empty(),
        }
    }

    /// Install the discovered Agent Skills catalog (cloud + local agents share it).
    pub fn with_skills(mut self, skills: SkillCatalog) -> Self {
        self.skills = skills;
        self
    }

    /// Replace the cached global habits (called at build time from the app store and after each
    /// distillation pass).
    pub fn set_global_facts(&self, facts: Vec<String>) {
        *self.global_facts.lock().unwrap() = facts;
    }

    /// Set how many tokens of context to pack per turn (e.g. matched to a local model's window).
    pub fn with_context_budget(mut self, budget: usize) -> Self {
        self.context_budget = budget.max(2_000);
        self
    }

    /// Enable small-context mode (terse prompt, harder caps, no delegation) — for tiny local
    /// windows. The host sets this from the model's configured context length.
    pub fn with_small_context(mut self, small: bool) -> Self {
        self.small_context = small;
        self
    }

    /// Per-turn tool-output cap — tighter when the model's window is tiny. Tool results are the
    /// bulkiest thing in the re-sent history, so keeping this modest directly cuts per-message cost
    /// (the raw output is still in the event log if the operator needs it).
    fn tool_output_cap(&self) -> usize {
        if self.small_context { 768 } else { 1_400 }
    }

    fn assembler(&self) -> ContextAssembler {
        ContextAssembler { max_tokens: self.context_budget }
    }

    /// Run a tool through the runner, deduping identical read-only commands within a session.
    /// Mutating/exploit commands always run fresh (their effects aren't idempotent).
    fn run_cached(&self, tool: &str, argv: &[String]) -> String {
        if !matches!(classify(tool, argv), Classification::ReadOnly) {
            return self.runner.run(tool, argv);
        }
        let session = self.active_session.lock().unwrap().clone();
        let key = format!("{session}\u{1}{tool}\u{1}{}", argv.join("\u{1}"));
        if let Some(prev) = self.command_cache.lock().unwrap().get(&key).cloned() {
            return format!(
                "(NOTE: this exact read-only command already ran earlier in the session — reusing \
                 the prior result instead of re-running, to save time and tokens. Change the \
                 command if you genuinely need fresh data.)\n{prev}"
            );
        }
        let out = self.runner.run(tool, argv);
        self.command_cache.lock().unwrap().insert(key, out.clone());
        out
    }

    /// Inject a custom command runner (used in tests to avoid spawning real processes).
    pub fn with_runner(mut self, runner: Arc<dyn CommandRunner>) -> Self {
        self.runner = runner;
        self
    }

    /// Use a separate (typically cheaper) provider for delegated sub-agents.
    pub fn with_subagent_provider(mut self, provider: Arc<dyn LlmProvider>) -> Self {
        self.subagent_provider = provider;
        self
    }

    /// Share the cancellation flag with an externally-owned atom so it can be propagated to the
    /// command runner — letting Stop interrupt an in-flight tool, not just the round loop.
    pub fn with_cancel(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancelled = cancel;
        self
    }

    /// Share the cost meter (spent) and budget cap with externally-owned atoms so they survive
    /// workspace switches and the operator can read/set them from the UI. `budget` of 0 = no cap.
    pub fn with_budget(mut self, spent: Arc<AtomicU64>, budget: Arc<AtomicU64>) -> Self {
        self.tokens_spent = spent;
        self.token_budget = budget;
        self
    }

    /// Cumulative tokens spent on this workspace so far.
    pub fn tokens_spent(&self) -> u64 {
        self.tokens_spent.load(Ordering::SeqCst)
    }

    /// Set the cumulative token budget (0 = unlimited).
    pub fn set_token_budget(&self, budget: u64) {
        self.token_budget.store(budget, Ordering::SeqCst);
    }

    /// True once a non-zero budget has been reached or exceeded.
    fn budget_exhausted(&self) -> bool {
        let cap = self.token_budget.load(Ordering::SeqCst);
        cap > 0 && self.tokens_spent.load(Ordering::SeqCst) >= cap
    }

    /// Replace the internal autonomous/free_mode atoms with externally-owned ones so the caller
    /// can set them without holding a reference to the orchestrator.
    pub fn with_flags(mut self, autonomous: Arc<AtomicBool>, free_mode: Arc<AtomicBool>) -> Self {
        self.autonomous = autonomous;
        self.free_mode = free_mode;
        self
    }

    /// Shared handle to the approval gate, so `policy_resolve` can wake a parked turn.
    pub fn gate(&self) -> Arc<ApprovalGate> {
        self.gate.clone()
    }

    /// When enabled, `NeedsApproval` decisions execute without prompting the operator.
    /// Scope and explicit-deny policy are still enforced.
    pub fn set_autonomous(&self, enabled: bool) {
        self.autonomous.store(enabled, Ordering::SeqCst);
    }

    pub fn is_autonomous(&self) -> bool {
        self.autonomous.load(Ordering::SeqCst)
    }

    pub fn set_free_mode(&self, enabled: bool) {
        self.free_mode.store(enabled, Ordering::SeqCst);
    }

    pub fn is_free_mode(&self) -> bool {
        self.free_mode.load(Ordering::SeqCst)
    }

    /// Signal the running turn to stop after the current operation.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Create a new empty conversation session and make it active.
    pub fn new_session(&self, session_id: &str) {
        let mut h = self.histories.lock().unwrap();
        h.insert(session_id.to_string(), Vec::new());
        *self.active_session.lock().unwrap() = session_id.to_string();
        self.command_cache.lock().unwrap().clear();
    }

    /// Switch to an existing session (or create an empty one if unknown).
    pub fn switch_session(&self, session_id: &str) {
        let mut h = self.histories.lock().unwrap();
        h.entry(session_id.to_string()).or_default();
        *self.active_session.lock().unwrap() = session_id.to_string();
    }

    /// Restore persisted conversations from the workspace store so history survives app restarts
    /// and orchestrator rebuilds. Called once when a workspace is opened. Malformed blobs are
    /// skipped rather than failing the open.
    pub fn hydrate(&self, store: &WorkspaceStore) {
        let Ok(convs) = store.load_conversations() else { return };
        let mut histories = self.histories.lock().unwrap();
        for (session_id, json) in convs {
            if let Ok(msgs) = serde_json::from_str::<Vec<Message>>(&json) {
                histories.insert(session_id, msgs);
            }
        }
    }

    /// Number of stored messages for a session (excludes the per-turn system prompt). Mainly for
    /// tests and diagnostics.
    pub fn history_len(&self, session_id: &str) -> usize {
        self.histories.lock().unwrap().get(session_id).map_or(0, |h| h.len())
    }

    /// The always-injected profile for a turn: the operator's global habits followed by this
    /// engagement's per-workspace facts.
    fn profile_for(&self, store: &WorkspaceStore) -> Vec<String> {
        let mut facts = self.global_facts.lock().unwrap().clone();
        if let Ok(ws) = store.workspace_facts() {
            facts.extend(ws.into_iter().map(|f| f.text));
        }
        facts
    }

    /// Drive one prompt to completion. Long-running; the caller spawns it as a task.
    /// Accumulates conversation history across calls so the model remembers previous results.
    pub async fn handle_prompt(
        &self,
        store: &WorkspaceStore,
        updates: UnboundedSender<AgentUpdate>,
        prompt: &str,
    ) -> Result<()> {
        self.cancelled.store(false, Ordering::SeqCst);
        self.run_prompt_inner(store, updates, prompt).await
    }

    /// Standalone (autonomous goal) mode: drive the agent toward `goal` across multiple
    /// self-directed prompt-cycles until it signals completion, gets stuck, or hits a safety rail
    /// (iteration cap, cumulative token budget, or operator Stop). Auto-approval is forced on for
    /// the duration so the loop never blocks on an approval dialog; scope and explicit-deny policy
    /// still apply, so it cannot act outside the engagement.
    pub async fn run_goal(
        &self,
        store: &WorkspaceStore,
        updates: UnboundedSender<AgentUpdate>,
        goal: &str,
    ) -> Result<()> {
        self.cancelled.store(false, Ordering::SeqCst);
        self.goal_active.store(true, Ordering::SeqCst);
        let _ = updates.send(AgentUpdate::GoalStarted { goal: goal.to_string() });

        let mut iteration: usize = 0;
        let mut total_tokens: u64 = 0;
        let mut prompt = goal_prompt(goal, true);

        let outcome = loop {
            if self.cancelled.load(Ordering::SeqCst) {
                break GoalOutcome::Cancelled;
            }
            if iteration >= MAX_GOAL_ITERATIONS {
                break GoalOutcome::MaxIterations;
            }
            if total_tokens >= MAX_GOAL_TOKENS {
                break GoalOutcome::BudgetExhausted;
            }
            iteration += 1;
            let _ = updates.send(AgentUpdate::GoalIteration { iteration: iteration as u32 });

            // Tee this cycle's updates to the UI, but (a) swallow the per-cycle TurnEnded so the
            // UI stays "running" across the whole loop, and (b) sniff the assistant text for the
            // completion sentinel and sum token usage for the budget rail.
            let (tee_tx, mut tee_rx) = tokio::sync::mpsc::unbounded_channel::<AgentUpdate>();
            let fwd = updates.clone();
            let watcher = tokio::spawn(async move {
                let mut text = String::new();
                let mut toks: u64 = 0;
                while let Some(u) = tee_rx.recv().await {
                    match &u {
                        AgentUpdate::Text(t) => text.push_str(t),
                        AgentUpdate::TokensUsed { input, output } => {
                            toks += *input as u64 + *output as u64;
                        }
                        AgentUpdate::TurnEnded => continue,
                        _ => {}
                    }
                    let _ = fwd.send(u);
                }
                (text, toks)
            });

            let res = self.run_prompt_inner(store, tee_tx, &prompt).await;
            let (text, toks) = watcher.await.unwrap_or_default();
            total_tokens += toks;

            if let Err(e) = res {
                // Unwind cleanly: clear the flag and close out the run before propagating.
                self.goal_active.store(false, Ordering::SeqCst);
                let _ = updates.send(AgentUpdate::GoalFinished {
                    outcome: "error".to_string(),
                    iterations: iteration as u32,
                });
                let _ = updates.send(AgentUpdate::TurnEnded);
                return Err(e);
            }

            if text.contains(GOAL_DONE_TOKEN) {
                break GoalOutcome::Completed;
            }
            if text.contains(GOAL_STUCK_TOKEN) {
                break GoalOutcome::Blocked;
            }
            prompt = goal_prompt(goal, false);
        };

        self.goal_active.store(false, Ordering::SeqCst);
        let _ = updates.send(AgentUpdate::GoalFinished {
            outcome: outcome.label().to_string(),
            iterations: iteration as u32,
        });
        let _ = updates.send(AgentUpdate::TurnEnded);
        Ok(())
    }

    /// Distill durable profile facts from recent activity (DESIGN §6.2 — "learn the operator").
    /// Runs on the cheaper sub-agent provider (free if it's local), writes per-workspace facts to
    /// the store, and RETURNS the global (cross-engagement) facts for the caller to persist in the
    /// app store. Global facts are scrubbed of engagement specifics by the prompt — they describe
    /// the operator's habits, never a target.
    pub async fn distill_profile(&self, store: &WorkspaceStore) -> Vec<String> {
        let events = store.recent_events(DISTILL_EVENT_SCAN).unwrap_or_default();
        if events.len() < DISTILL_MIN_EVENTS {
            return Vec::new();
        }

        let messages = vec![
            Message { role: Role::System, content: vec![text(DISTILL_SYSTEM.to_string())] },
            Message {
                role: Role::User,
                content: vec![text(format!("Recent activity:\n{}", distill_transcript(&events)))],
            },
        ];
        let trimmed = self.assembler().trim_to_budget(&messages);
        let Ok(mut stream) = self.subagent_provider.run_turn(&trimmed, &[]).await else {
            return Vec::new();
        };

        let mut out = String::new();
        while let Some(ev) = stream.next().await {
            match ev {
                AgentEvent::TextDelta { text } => out.push_str(&text),
                AgentEvent::TokensUsed { input_tokens, output_tokens } => {
                    self.tokens_spent
                        .fetch_add(input_tokens as u64 + output_tokens as u64, Ordering::SeqCst);
                }
                AgentEvent::TurnEnd => break,
                _ => {}
            }
        }

        let mut global = Vec::new();
        for (scope, fact) in parse_distilled_facts(&out) {
            if scope == "global" {
                global.push(fact);
            } else {
                let _ = store.add_workspace_fact(&fact);
            }
        }
        global
    }

    fn compact_trigger(&self) -> usize {
        self.context_budget.saturating_mul(COMPACT_TRIGGER_PCT) / 100
    }
    fn compact_keep(&self) -> usize {
        self.context_budget.saturating_mul(COMPACT_KEEP_PCT) / 100
    }

    /// If this session's stored history has grown past the trigger, summarize its oldest turns into a
    /// dense brief and keep only the recent tail verbatim — the core "don't re-send an ever-growing
    /// transcript" optimization. Runs on the (cheap) sub-agent model; never holds the history lock
    /// across the await; and leaves the history untouched if summarization fails.
    async fn maybe_compact(
        &self,
        store: &WorkspaceStore,
        session_id: &str,
        updates: &UnboundedSender<AgentUpdate>,
    ) {
        let (old, suffix): (Vec<Message>, Vec<Message>) = {
            let histories = self.histories.lock().unwrap();
            let Some(history) = histories.get(session_id) else { return };
            if ContextAssembler::estimate_tokens(history) <= self.compact_trigger() {
                return;
            }
            let split = ContextAssembler::compaction_split(history, self.compact_keep());
            if split == 0 {
                return;
            }
            (history[..split].to_vec(), history[split..].to_vec())
        };
        if old.is_empty() {
            return;
        }

        let summary = self.summarize_transcript(&old).await;
        if summary.trim().is_empty() {
            return; // summarization failed/cancelled — keep the original history intact
        }

        let header = format!(
            "[Condensed summary of {} earlier messages in this engagement — preserved facts only]\n{}\n\
             [End of summary; recent activity continues below.]",
            old.len(),
            summary.trim()
        );

        // Merge the summary into the first kept message when it's a user turn (keeps roles valid and
        // tool-use/result pairs intact — the kept suffix begins on a user turn by construction).
        let mut suffix = suffix;
        let starts_with_user = suffix.first().map(|m| m.role == Role::User).unwrap_or(false);
        let mut new_history: Vec<Message> = Vec::with_capacity(suffix.len() + 1);
        if starts_with_user {
            let first = suffix.remove(0);
            let existing = collect_text(&first.content);
            let merged = if existing.is_empty() { header } else { format!("{header}\n\n{existing}") };
            new_history.push(Message { role: Role::User, content: vec![text(merged)] });
        } else {
            new_history.push(Message { role: Role::User, content: vec![text(header)] });
        }
        new_history.extend(suffix);

        {
            let mut histories = self.histories.lock().unwrap();
            histories.insert(session_id.to_string(), new_history.clone());
        }
        if let Ok(json) = serde_json::to_string(&new_history) {
            let _ = store.save_conversation(session_id, &json);
        }
        let _ = updates.send(AgentUpdate::Compacted { summarized: old.len() });
    }

    /// One-shot summarization of an old transcript slice on the sub-agent model (no tools).
    async fn summarize_transcript(&self, old: &[Message]) -> String {
        let messages = vec![
            Message { role: Role::System, content: vec![text(COMPACT_SYSTEM.to_string())] },
            Message {
                role: Role::User,
                content: vec![text(format!("Transcript to compact:\n{}", transcript_for_summary(old)))],
            },
        ];
        let trimmed = self.assembler().trim_to_budget(&messages);
        let Ok(mut stream) = self.subagent_provider.run_turn(&trimmed, &[]).await else {
            return String::new();
        };
        let mut out = String::new();
        while let Some(ev) = stream.next().await {
            match ev {
                AgentEvent::TextDelta { text } => out.push_str(&text),
                AgentEvent::TokensUsed { input_tokens, output_tokens } => {
                    self.tokens_spent
                        .fetch_add(input_tokens as u64 + output_tokens as u64, Ordering::SeqCst);
                }
                AgentEvent::TurnEnd => break,
                _ => {}
            }
        }
        out
    }

    /// One prompt-cycle: build context → run tool-rounds → persist. Unlike [`handle_prompt`] this
    /// does NOT reset the cancel flag, so the standalone goal loop can keep a Stop latched across
    /// iterations (otherwise each new cycle would clear it and ignore the operator's Stop).
    async fn run_prompt_inner(
        &self,
        store: &WorkspaceStore,
        updates: UnboundedSender<AgentUpdate>,
        prompt: &str,
    ) -> Result<()> {
        let ws = store.workspace_id();
        let scope = store.scope()?;
        let rules = store.allow_rules()?;
        let phase = store.current_phase()?;
        let notes = store.notes(20).unwrap_or_default();
        let attempts = store.attempts(24).unwrap_or_default();
        let findings = store.findings().unwrap_or_default();
        let machine = is_machine_engagement(&scope);
        // CTF triage playbook only applies to jeopardy-style challenges; on a machine it's dead
        // weight (~2k cached tokens) and the kill-chain/privesc methodology takes its place.
        let solve_section =
            if machine { String::new() } else { self.skills.preloaded_system_section(self.small_context) };

        store.append(Event::new(
            ws,
            phase,
            EventKind::UserPrompt,
            AgentId::human(),
            Author::User,
            json!({ "text": prompt }),
        ))?;

        // Load this session's history. Clone to avoid holding the lock across awaits.
        let session_id = self.active_session.lock().unwrap().clone();
        // Roll up old turns into a summary before they cost us another full re-send.
        self.maybe_compact(store, &session_id, &updates).await;
        let history_before_len;
        let mut messages = {
            let mut histories = self.histories.lock().unwrap();
            let history = histories.entry(session_id.clone()).or_default();
            history_before_len = history.len();
            // System prompt is rebuilt fresh each turn (scope/phase/notes may have changed).
            let mut msgs = vec![
                Message { role: Role::System, content: vec![
                    // Block 1 (cached): stable instructions. Block 2 (uncached): volatile context.
                    text(stable_system_prompt(self.small_context, self.free_mode.load(Ordering::SeqCst), &self.skills.catalog_text(), &solve_section, machine)),
                    text(volatile_context(phase, &scope, &notes, &self.profile_for(store), &attempts, &findings, self.small_context)),
                ] },
            ];
            msgs.extend(history.clone());
            msgs
        };
        messages.push(Message { role: Role::User, content: vec![text(prompt.to_string())] });

        // Repeated-command counts for this prompt-cycle, feeding the loop guard.
        let mut loop_counts: HashMap<String, usize> = HashMap::new();

        let assembler = self.assembler();
        for _round in 0..MAX_ROUNDS {
            if self.cancelled.load(Ordering::SeqCst) {
                break;
            }
            if self.budget_exhausted() {
                let cap = self.token_budget.load(Ordering::SeqCst);
                let _ = updates.send(AgentUpdate::Error(format!(
                    "Token budget reached ({} / {cap}). Stopping. Raise or clear the budget to continue.",
                    self.tokens_spent()
                )));
                break;
            }

            ContextAssembler::cap_tool_output_with(&mut messages, self.tool_output_cap());
            let trimmed = assembler.trim_to_budget(&messages);
            let mut stream = self
                .provider
                .run_turn(&trimmed, self.mcp.orchestrator_specs())
                .await
                .map_err(|e| AgentError::Llm(e.to_string()))?;

            let mut assistant_content = Vec::new();
            let mut tool_calls = Vec::new();
            while let Some(ev) = stream.next().await {
                if self.cancelled.load(Ordering::SeqCst) {
                    break;
                }
                match ev {
                    AgentEvent::TextDelta { text: t } => {
                        let _ = updates.send(AgentUpdate::Text(t.clone()));
                        assistant_content.push(text(t));
                    }
                    AgentEvent::ToolCall { call } => {
                        assistant_content.push(Content::ToolUse { call: call.clone() });
                        tool_calls.push(call);
                    }
                    AgentEvent::TokensUsed { input_tokens, output_tokens } => {
                        self.tokens_spent.fetch_add(
                            input_tokens as u64 + output_tokens as u64,
                            Ordering::SeqCst,
                        );
                        let _ = updates.send(AgentUpdate::TokensUsed {
                            input: input_tokens,
                            output: output_tokens,
                        });
                    }
                    AgentEvent::TurnEnd => {}
                    AgentEvent::Error { message } => {
                        let _ = updates.send(AgentUpdate::Error(message));
                    }
                }
            }

            if !assistant_content.is_empty() {
                store.append(Event::new(
                    ws,
                    phase,
                    EventKind::AgentMsg,
                    self.actor.clone(),
                    Author::Agent,
                    json!({ "text": collect_text(&assistant_content) }),
                ))?;
                messages.push(Message { role: Role::Assistant, content: assistant_content });
            }

            if tool_calls.is_empty() || self.cancelled.load(Ordering::SeqCst) {
                break;
            }

            let mut results = Vec::new();
            for call in tool_calls {
                if self.cancelled.load(Ordering::SeqCst) {
                    break;
                }
                let context_output = if call.name == "record_finding" {
                    self.handle_record_finding(store, &updates, ws, phase, &call)?
                } else if call.name == "log_attempt" {
                    self.handle_log_attempt(store, ws, phase, &call)?
                } else if call.name == "recall" {
                    // Returns full stored text on purpose — do NOT route through the summarizer.
                    self.handle_recall(store, &call)
                } else if call.name == "use_skill" {
                    self.handle_use_skill(&updates, store, ws, phase, &call)
                } else if call.name == "delegate_to_agent" {
                    self.run_subagent(store, &updates, ws, phase, &call).await?
                } else {
                    let (tool, argv) = parse_run_command(&call);
                    // Loop guard: if the agent keeps re-issuing the exact same command this cycle,
                    // refuse to run it again and tell it to change approach.
                    let sig = format!("{tool}\u{1}{}", argv.join("\u{1}"));
                    let hits = {
                        let c = loop_counts.entry(sig).or_insert(0);
                        *c += 1;
                        *c
                    };
                    let raw = if hits > LOOP_LIMIT {
                        let reason = format!(
                            "loop guard: `{tool} {}` was already issued {hits}× this turn",
                            argv.join(" ")
                        );
                        let _ = updates.send(AgentUpdate::Denied { reason });
                        format!(
                            "LOOP GUARD: you have run `{tool} {}` {hits} times this turn with no new \
                             result. Not running it again — change approach or conclude.",
                            argv.join(" ")
                        )
                    } else {
                        self.handle_tool_call(store, &updates, ws, phase, &scope, &rules, &call)
                            .await?
                    };
                    // Summarize large command output before it enters the conversation history; the
                    // full raw output is already persisted in the event log and streamed to the UI.
                    summary::summarize_tool_output(&tool, &raw)
                };
                results.push(Content::ToolResult { call_id: call.call_id, output: context_output });
            }
            if !results.is_empty() {
                messages.push(Message { role: Role::Tool, content: results });
            }
        }

        // Persist this turn into the session history (skip on cancellation — partial turns
        // would leave the conversation in a malformed assistant/tool-result state).
        if !self.cancelled.load(Ordering::SeqCst) {
            let new_entries = messages[(1 + history_before_len)..].to_vec();
            if !new_entries.is_empty() {
                let mut histories = self.histories.lock().unwrap();
                let h = histories.entry(session_id.clone()).or_default();
                h.extend(new_entries);
                // Cache the conversation to the workspace DB so it survives restarts/rebuilds.
                if let Ok(json) = serde_json::to_string(&*h) {
                    let _ = store.save_conversation(&session_id, &json);
                }
            }
        }

        let _ = updates.send(AgentUpdate::TurnEnded);
        Ok(())
    }

    /// Spawn a focused sub-agent via a `delegate_to_agent` tool call. The sub-agent runs its
    /// own tool loop (no further delegation), shares the same provider/runner/gate, and returns
    /// a text summary that the orchestrator sees as the tool result.
    async fn run_subagent(
        &self,
        store: &WorkspaceStore,
        updates: &UnboundedSender<AgentUpdate>,
        ws: WorkspaceId,
        _parent_phase: Phase,
        call: &ToolCall,
    ) -> Result<String> {
        let agent_name = call.arguments["agent"].as_str().unwrap_or("recon").to_string();
        let objective  = call.arguments["objective"].as_str().unwrap_or("").to_string();
        let token_budget = call.arguments["token_budget"]
            .as_u64()
            .map(|v| (v as u32).min(MAX_SUBAGENT_BUDGET))
            .unwrap_or(DEFAULT_SUBAGENT_BUDGET);

        let _ = updates.send(AgentUpdate::SubAgentStarted {
            name: agent_name.clone(),
            objective: objective.clone(),
        });

        // Determine the sub-agent's phase.
        let subagent_phase = match agent_name.as_str() {
            "exploit" => Phase::Exploit,
            "web"     => Phase::Poc,
            _         => Phase::Recon,
        };
        let subagent_actor = AgentId(format!("agent:{agent_name}"));
        let scope = store.scope()?;
        let rules = store.allow_rules()?;
        let notes = store.notes(10).unwrap_or_default();
        let attempts = store.attempts(16).unwrap_or_default();
        let findings = store.findings().unwrap_or_default();
        let machine = is_machine_engagement(&scope);
        let solve_section =
            if machine { String::new() } else { self.skills.preloaded_system_section(self.small_context) };

        // Keyword recall: surface events relevant to the objective so the sub-agent doesn't
        // repeat work the orchestrator or a prior agent already did.
        let past_events = store.recent_events(200).unwrap_or_default();
        let recalled = ContextAssembler::keyword_recall(&past_events, &objective);
        let recalled_hint = if recalled.is_empty() {
            String::new()
        } else {
            format!(
                "\n\nRelevant past events (do NOT repeat these — build on them):\n{}",
                recalled.join("\n")
            )
        };

        // Sub-agents run on the cheaper model where prompt caching matters less, so keep the system
        // prompt as one block (stable instructions + volatile context + recall).
        let free = self.free_mode.load(Ordering::SeqCst);
        let mut messages = vec![
            Message {
                role: Role::System,
                content: vec![text(format!(
                    "{}\n{}{recalled_hint}",
                    stable_system_prompt(self.small_context, free, &self.skills.catalog_text(), &solve_section, machine),
                    volatile_context(subagent_phase, &scope, &notes, &self.profile_for(store), &attempts, &findings, self.small_context),
                ))],
            },
            Message { role: Role::User, content: vec![text(objective.clone())] },
        ];

        let assembler = self.assembler();
        let mut tokens_used: u32 = 0;

        for _round in 0..MAX_SUBAGENT_ROUNDS {
            if self.cancelled.load(Ordering::SeqCst) { break; }
            if tokens_used >= token_budget {
                // Push a soft stop — let the model wrap up in one last turn.
                messages.push(Message {
                    role: Role::User,
                    content: vec![text("Token budget reached. Summarise your findings and stop.".into())],
                });
            }

            ContextAssembler::cap_tool_output_with(&mut messages, self.tool_output_cap());
            let trimmed = assembler.trim_to_budget(&messages);
            let mut stream = self
                .subagent_provider
                .run_turn(&trimmed, self.mcp.subagent_specs())
                .await
                .map_err(|e| AgentError::Llm(e.to_string()))?;

            let mut assistant_content = Vec::new();
            let mut tool_calls: Vec<ToolCall> = Vec::new();
            while let Some(ev) = stream.next().await {
                if self.cancelled.load(Ordering::SeqCst) { break; }
                match ev {
                    AgentEvent::TextDelta { text: t } => {
                        let _ = updates.send(AgentUpdate::SubAgentText {
                            name: agent_name.clone(),
                            text: t.clone(),
                        });
                        assistant_content.push(text(t));
                    }
                    AgentEvent::ToolCall { call: tc } => {
                        assistant_content.push(Content::ToolUse { call: tc.clone() });
                        tool_calls.push(tc);
                    }
                    AgentEvent::TokensUsed { input_tokens, output_tokens } => {
                        let used = input_tokens + output_tokens;
                        tokens_used = tokens_used.saturating_add(used);
                        self.tokens_spent.fetch_add(used as u64, Ordering::SeqCst);
                        let _ = updates.send(AgentUpdate::TokensUsed {
                            input: input_tokens,
                            output: output_tokens,
                        });
                    }
                    AgentEvent::TurnEnd | AgentEvent::Error { .. } => {}
                }
            }

            if !assistant_content.is_empty() {
                store.append(Event::new(
                    ws, subagent_phase, EventKind::AgentMsg,
                    subagent_actor.clone(), Author::Agent,
                    json!({ "text": collect_text(&assistant_content) }),
                ))?;
                messages.push(Message { role: Role::Assistant, content: assistant_content });
            }

            if tool_calls.is_empty() || self.cancelled.load(Ordering::SeqCst) { break; }

            let mut results = Vec::new();
            for tc in tool_calls {
                if self.cancelled.load(Ordering::SeqCst) { break; }
                let out = if tc.name == "record_finding" {
                    // Use the sub-agent's actor for the finding event.
                    let severity = tc.arguments["severity"].as_str().unwrap_or("info").to_string();
                    let target   = tc.arguments["target"].as_str().unwrap_or("").to_string();
                    let summary  = tc.arguments["summary"].as_str().unwrap_or("").to_string();
                    store.append(Event::new(
                        ws, subagent_phase, EventKind::Finding,
                        subagent_actor.clone(), Author::Agent,
                        json!({ "severity": severity, "target": target, "summary": summary }),
                    ))?;
                    let _ = updates.send(AgentUpdate::FindingRecorded {
                        severity, target, summary: summary.clone(),
                    });
                    format!("Finding recorded: {summary}")
                } else if tc.name == "log_attempt" {
                    let action = tc.arguments["action"].as_str().unwrap_or("").to_string();
                    let status = tc.arguments["status"].as_str().unwrap_or("trying").to_string();
                    let result = tc.arguments["result"].as_str().unwrap_or("").to_string();
                    store.append(Event::new(
                        ws, subagent_phase, EventKind::Attempt,
                        subagent_actor.clone(), Author::Agent,
                        json!({ "action": action, "status": status, "result": result }),
                    ))?;
                    format!("Attempt logged: [{status}] {action}")
                } else if tc.name == "recall" {
                    self.handle_recall(store, &tc)
                } else if tc.name == "use_skill" {
                    self.handle_use_skill(&updates, store, ws, subagent_phase, &tc)
                } else {
                    // All other calls go through the same policy engine, but with the sub-agent's actor.
                    let (tool, argv) = parse_run_command(&tc);
                    store.append(Event::new(
                        ws, subagent_phase, EventKind::ToolProposed,
                        subagent_actor.clone(), Author::Agent,
                        json!({ "tool": tool, "argv": argv }),
                    ))?;
                    if self.free_mode.load(Ordering::SeqCst) {
                        store.append(Event::new(
                            ws, subagent_phase, EventKind::ToolApproved,
                            subagent_actor.clone(), Author::Agent,
                            json!({ "tool": tool, "argv": argv }),
                        ))?;
                        let _ = updates.send(AgentUpdate::ToolStarted { tool: tool.clone(), argv: argv.clone() });
                        let output = self.run_cached(&tool, &argv);
                        store.append(Event::new(
                            ws, subagent_phase, EventKind::ToolOutput,
                            subagent_actor.clone(), Author::Agent,
                            json!({ "tool": tool, "output": output }),
                        ))?;
                        let _ = updates.send(AgentUpdate::ToolOutput { text: output.clone() });
                        output
                    } else { match decide(&tool, &argv, &scope, &rules) {
                        Decision::AutoRun => {
                            store.append(Event::new(
                                ws, subagent_phase, EventKind::ToolApproved,
                                subagent_actor.clone(), Author::Agent,
                                json!({ "tool": tool, "argv": argv }),
                            ))?;
                            let _ = updates.send(AgentUpdate::ToolStarted {
                                tool: tool.clone(), argv: argv.clone(),
                            });
                            let output = self.run_cached(&tool, &argv);
                            store.append(Event::new(
                                ws, subagent_phase, EventKind::ToolOutput,
                                subagent_actor.clone(), Author::Agent,
                                json!({ "tool": tool, "output": output }),
                            ))?;
                            let _ = updates.send(AgentUpdate::ToolOutput { text: output.clone() });
                            output
                        }
                        Decision::Deny { reason } => {
                            store.append(Event::new(
                                ws, subagent_phase, EventKind::ToolDenied,
                                subagent_actor.clone(), Author::Agent,
                                json!({ "tool": tool, "argv": argv, "reason": reason }),
                            ))?;
                            let _ = updates.send(AgentUpdate::Denied { reason: reason.clone() });
                            format!("DENIED (policy): {reason}")
                        }
                        Decision::NeedsApproval => {
                            if self.autonomous.load(Ordering::SeqCst) || self.goal_active.load(Ordering::SeqCst) {
                                let _ = updates.send(AgentUpdate::ToolStarted { tool: tool.clone(), argv: argv.clone() });
                                let output = self.run_cached(&tool, &argv);
                                let _ = updates.send(AgentUpdate::ToolOutput { text: output.clone() });
                                output
                            } else {
                                let proposed = ProposedCall {
                                    tool: tool.clone(), argv: argv.clone(),
                                    targets: resolve_targets(&tool, &argv),
                                    classification: classify(&tool, &argv),
                                };
                                let (token, rx) = self.gate.open(proposed.clone());
                                let _ = updates.send(AgentUpdate::ApprovalRequest { token, call: proposed });
                                match rx.await {
                                    Ok(ApprovalOutcome::ApproveOnce) => {
                                        let _ = updates.send(AgentUpdate::ToolStarted { tool: tool.clone(), argv: argv.clone() });
                                        let output = self.run_cached(&tool, &argv);
                                        let _ = updates.send(AgentUpdate::ToolOutput { text: output.clone() });
                                        output
                                    }
                                    Ok(ApprovalOutcome::ApproveEdited(new_argv)) => {
                                        let _ = updates.send(AgentUpdate::ToolStarted { tool: tool.clone(), argv: new_argv.clone() });
                                        let output = self.run_cached(&tool, &new_argv);
                                        let _ = updates.send(AgentUpdate::ToolOutput { text: output.clone() });
                                        output
                                    }
                                    Ok(ApprovalOutcome::AlwaysAllow(rule)) => {
                                        store.add_allow_rule(&rule)?;
                                        let _ = updates.send(AgentUpdate::ToolStarted { tool: tool.clone(), argv: argv.clone() });
                                        let output = self.run_cached(&tool, &argv);
                                        let _ = updates.send(AgentUpdate::ToolOutput { text: output.clone() });
                                        output
                                    }
                                    Ok(ApprovalOutcome::Deny(reason)) => {
                                        let _ = updates.send(AgentUpdate::Denied { reason: reason.clone() });
                                        format!("DENIED: {reason}")
                                    }
                                    Err(_) => "DENIED: approval channel closed".to_string(),
                                }
                            }
                        } } // close free_mode else + match
                    }
                };
                // Summarize large command output before it enters the sub-agent's context (raw
                // output is already in the event log). Finding/denial strings are already compact.
                let context_out = if matches!(tc.name.as_str(), "record_finding" | "log_attempt" | "recall" | "use_skill") {
                    out
                } else {
                    let (tool, _) = parse_run_command(&tc);
                    summary::summarize_tool_output(&tool, &out)
                };
                results.push(Content::ToolResult { call_id: tc.call_id, output: context_out });
            }
            if !results.is_empty() {
                messages.push(Message { role: Role::Tool, content: results });
            }
        }

        // Extract the last assistant text as the summary returned to the orchestrator.
        let summary = messages.iter().rev()
            .find(|m| m.role == Role::Assistant)
            .map(|m| collect_text(&m.content))
            .unwrap_or_else(|| format!("{agent_name} agent completed with no text output."));

        let _ = updates.send(AgentUpdate::SubAgentFinished {
            name: agent_name.clone(),
            summary: summary.clone(),
        });

        // Tag the delegation in the event log with parent_id = None for now (v0.3 will wire it).
        store.append(Event::new(
            ws, subagent_phase, EventKind::AgentMsg,
            AgentId(format!("agent:{agent_name}:done")), Author::Agent,
            json!({ "text": format!("[{agent_name} agent complete] {summary}") }),
        ))?;

        Ok(summary)
    }

    /// Route one tool call through scope → classify → decide, then execute or refuse. Returns the
    /// text fed back to the model as the tool result.
    #[allow(clippy::too_many_arguments)]
    async fn handle_tool_call(
        &self,
        store: &WorkspaceStore,
        updates: &UnboundedSender<AgentUpdate>,
        ws: WorkspaceId,
        phase: Phase,
        scope: &tianji_types::ScopeRules,
        rules: &[AllowRule],
        call: &ToolCall,
    ) -> Result<String> {
        let (tool, argv) = parse_run_command(call);

        store.append(Event::new(
            ws,
            phase,
            EventKind::ToolProposed,
            self.actor.clone(),
            Author::Agent,
            json!({ "tool": tool, "argv": argv }),
        ))?;

        // Free mode: bypass all policy checks entirely.
        if self.free_mode.load(Ordering::SeqCst) {
            return self.execute(store, updates, ws, phase, &tool, &argv);
        }

        let output = match decide(&tool, &argv, scope, rules) {
            Decision::AutoRun => self.execute(store, updates, ws, phase, &tool, &argv)?,
            Decision::Deny { reason } => {
                self.record_denied(store, ws, phase, &tool, &argv, &reason)?;
                let _ = updates.send(AgentUpdate::Denied { reason: reason.clone() });
                format!("DENIED (policy): {reason}")
            }
            Decision::NeedsApproval => {
                if self.autonomous.load(Ordering::SeqCst) || self.goal_active.load(Ordering::SeqCst) {
                    // Autonomous mode: skip the approval gate, execute directly.
                    self.execute(store, updates, ws, phase, &tool, &argv)?
                } else {
                    let proposed = ProposedCall {
                        tool: tool.clone(),
                        argv: argv.clone(),
                        targets: resolve_targets(&tool, &argv),
                        classification: classify(&tool, &argv),
                    };
                    let (token, rx) = self.gate.open(proposed.clone());
                    let _ = updates.send(AgentUpdate::ApprovalRequest { token, call: proposed });

                    match rx.await {
                        Ok(ApprovalOutcome::ApproveOnce) => {
                            self.execute(store, updates, ws, phase, &tool, &argv)?
                        }
                        Ok(ApprovalOutcome::ApproveEdited(new_argv)) => {
                            self.execute(store, updates, ws, phase, &tool, &new_argv)?
                        }
                        Ok(ApprovalOutcome::AlwaysAllow(rule)) => {
                            store.add_allow_rule(&rule)?;
                            self.execute(store, updates, ws, phase, &tool, &argv)?
                        }
                        Ok(ApprovalOutcome::Deny(reason)) => {
                            self.record_denied(store, ws, phase, &tool, &argv, &reason)?;
                            let _ = updates.send(AgentUpdate::Denied { reason: reason.clone() });
                            format!("DENIED (user): {reason}")
                        }
                        Err(_) => {
                            let reason = "approval channel closed".to_string();
                            self.record_denied(store, ws, phase, &tool, &argv, &reason)?;
                            format!("DENIED: {reason}")
                        }
                    }
                }
            }
        };
        Ok(output)
    }

    fn execute(
        &self,
        store: &WorkspaceStore,
        updates: &UnboundedSender<AgentUpdate>,
        ws: WorkspaceId,
        phase: Phase,
        tool: &str,
        argv: &[String],
    ) -> Result<String> {
        store.append(Event::new(
            ws,
            phase,
            EventKind::ToolApproved,
            self.actor.clone(),
            Author::Agent,
            json!({ "tool": tool, "argv": argv }),
        ))?;
        let _ = updates.send(AgentUpdate::ToolStarted { tool: tool.to_string(), argv: argv.to_vec() });

        let output = self.run_cached(tool, argv);

        store.append(Event::new(
            ws,
            phase,
            EventKind::ToolOutput,
            self.actor.clone(),
            Author::Agent,
            json!({ "tool": tool, "output": output }),
        ))?;
        let _ = updates.send(AgentUpdate::ToolOutput { text: output.clone() });
        Ok(output)
    }

    fn handle_record_finding(
        &self,
        store: &WorkspaceStore,
        updates: &UnboundedSender<AgentUpdate>,
        ws: WorkspaceId,
        phase: Phase,
        call: &ToolCall,
    ) -> Result<String> {
        let severity = call.arguments["severity"].as_str().unwrap_or("info").to_string();
        let target = call.arguments["target"].as_str().unwrap_or("").to_string();
        let summary = call.arguments["summary"].as_str().unwrap_or("").to_string();

        // Dedup safety net: the model tends to re-log the same issue across turns under slightly
        // different target strings ("…:443/https" vs "…:443/tcp"). If an existing finding is on the
        // same normalized host:port AND its summary is substantially the same, skip it rather than
        // flooding the report — and tell the agent it's already recorded.
        if let Ok(existing) = store.findings() {
            if existing.iter().any(|f| {
                norm_target(&f.target) == norm_target(&target)
                    && summary_overlap(&f.summary, &summary) >= 0.6
            }) {
                return Ok(format!(
                    "Already recorded a finding for {target} (\"{}\") — not duplicating. Move on.",
                    trunc(&summary, 80)
                ));
            }
        }

        store.append(Event::new(
            ws,
            phase,
            EventKind::Finding,
            self.actor.clone(),
            Author::Agent,
            json!({ "severity": severity, "target": target, "summary": summary }),
        ))?;

        let _ = updates.send(AgentUpdate::FindingRecorded {
            severity: severity.clone(),
            target: target.clone(),
            summary: summary.clone(),
        });

        Ok(format!("Finding recorded: [{severity}] {target} — {summary}"))
    }

    /// Append a traced attempt (status = trying/succeeded/failed/abandoned). The event log is the
    /// source of truth; recent attempts are fed back into the system prompt so the agent stops
    /// re-trying dead ends.
    /// Append a traced attempt. We deliberately do NOT push it as a chat `Text` update (that would
    /// concatenate onto the in-flight agent bubble); it surfaces in the Auto/Trace tab and the
    /// event log, and is fed back into the next system prompt via the attempt log.
    fn handle_log_attempt(
        &self,
        store: &WorkspaceStore,
        ws: WorkspaceId,
        phase: Phase,
        call: &ToolCall,
    ) -> Result<String> {
        let action = call.arguments["action"].as_str().unwrap_or("").to_string();
        let status = call.arguments["status"].as_str().unwrap_or("trying").to_string();
        let result = call.arguments["result"].as_str().unwrap_or("").to_string();

        store.append(Event::new(
            ws,
            phase,
            EventKind::Attempt,
            self.actor.clone(),
            Author::Agent,
            json!({ "action": action, "status": status, "result": result }),
        ))?;

        Ok(format!("Attempt logged: [{status}] {action}"))
    }

    /// `recall` tool: pull the FULL stored text of matching events back into context on demand, so a
    /// detail dropped by summarization/compaction isn't lost. Bounded by hit count + per-hit chars.
    fn handle_recall(&self, store: &WorkspaceStore, call: &ToolCall) -> String {
        let query = call.arguments["query"].as_str().unwrap_or("").trim().to_string();
        if query.is_empty() {
            return "recall: provide a non-empty `query` — a keyword that appears in what you want (an IP, port, path, tool name, CVE).".to_string();
        }
        let hits = store.search_events(&query, RECALL_HITS).unwrap_or_default();
        if hits.is_empty() {
            return format!(
                "recall: nothing in the engagement log matches \"{query}\". Try a different keyword (IP, port, path, tool name)."
            );
        }
        let mut out = format!("Full recall for \"{query}\" — {} match(es), newest first:", hits.len());
        for e in &hits {
            out.push_str(&format!(
                "\n\n=== [{}] ===\n{}",
                recall_kind_label(e.kind),
                trunc(&full_event_text(e), RECALL_CHARS_PER_HIT)
            ));
        }
        out
    }

    /// `use_skill` tool: load a named skill's full instructions on demand (progressive disclosure).
    fn handle_use_skill(
        &self,
        updates: &UnboundedSender<AgentUpdate>,
        store: &WorkspaceStore,
        ws: WorkspaceId,
        phase: Phase,
        call: &ToolCall,
    ) -> String {
        let name = call.arguments["name"].as_str().unwrap_or("").trim().to_string();
        if name.is_empty() {
            return "use_skill: provide the `name` of a skill from the catalog in your system prompt.".to_string();
        }
        // Second-level disclosure: a specific technique file within the skill.
        let file = call.arguments["file"].as_str().map(|s| s.trim()).filter(|s| !s.is_empty());

        let loaded = match file {
            Some(f) => self.skills.load_file(&name, f),
            None => self.skills.load(&name),
        };
        match loaded {
            Some(body) => {
                // Surface it so the operator sees which skill the agent pulled in.
                let shown = match file {
                    Some(f) => format!("{name}/{f}"),
                    None => name.clone(),
                };
                // Log it so the operator can verify skill usage after the fact (Auto tab / report).
                let _ = store.append(Event::new(
                    ws,
                    phase,
                    EventKind::AgentMsg,
                    self.actor.clone(),
                    Author::Agent,
                    json!({ "text": format!("Loaded skill: {shown}") }),
                ));
                let _ = updates.send(AgentUpdate::SkillUsed { name: shown });
                body
            }
            None => {
                if let Some(f) = file {
                    return format!(
                        "use_skill: skill \"{name}\" has no file \"{f}\" (or it couldn't be read). \
                         Call use_skill name=\"{name}\" (no file) first to see its bundled-files list, \
                         then pass an exact name from it."
                    );
                }
                let available = self
                    .skills
                    .skills()
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                if available.is_empty() {
                    "use_skill: no skills are installed. (Operator: install with `npx skills add …`.)".to_string()
                } else {
                    format!("use_skill: no skill named \"{name}\". Available: {available}.")
                }
            }
        }
    }

    fn record_denied(
        &self,
        store: &WorkspaceStore,
        ws: WorkspaceId,
        phase: Phase,
        tool: &str,
        argv: &[String],
        reason: &str,
    ) -> Result<()> {
        store.append(Event::new(
            ws,
            phase,
            EventKind::ToolDenied,
            self.actor.clone(),
            Author::Agent,
            json!({ "tool": tool, "argv": argv, "reason": reason }),
        ))?;
        Ok(())
    }
}

// ---- helpers --------------------------------------------------------------------------------

fn text(s: String) -> Content {
    Content::Text { text: s }
}

fn collect_text(content: &[Content]) -> String {
    content
        .iter()
        .filter_map(|c| match c {
            Content::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Render an event's stored payload as full text for the `recall` tool (no summarization).
fn full_event_text(e: &Event) -> String {
    let p = &e.payload;
    if let Some(output) = p.get("output").and_then(|v| v.as_str()) {
        let tool = p.get("tool").and_then(|v| v.as_str()).unwrap_or("");
        return if tool.is_empty() { output.to_string() } else { format!("$ {tool}\n{output}") };
    }
    if let Some(summary) = p.get("summary").and_then(|v| v.as_str()) {
        let sev = p.get("severity").and_then(|v| v.as_str()).unwrap_or("");
        let target = p.get("target").and_then(|v| v.as_str()).unwrap_or("");
        return format!("{sev} {target} {summary}").trim().to_string();
    }
    if let Some(text) = p.get("text").and_then(|v| v.as_str()) {
        return text.to_string();
    }
    if let Some(action) = p.get("action").and_then(|v| v.as_str()) {
        let status = p.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let result = p.get("result").and_then(|v| v.as_str()).unwrap_or("");
        return format!("[{status}] {action} {result}").trim().to_string();
    }
    p.to_string()
}

fn recall_kind_label(k: EventKind) -> &'static str {
    match k {
        EventKind::ToolOutput => "tool output",
        EventKind::Finding => "finding",
        EventKind::Note => "note",
        EventKind::AgentMsg => "agent note",
        EventKind::Attempt => "attempt",
        _ => "event",
    }
}

/// Extract `(tool, argv)` from a `run_command` tool call. `argv` excludes the tool name.
///
/// Defends against two recurring model mistakes:
/// 1. Nesting the MCP tool name into the `tool` field (`tool="run_command"`), which used to
///    produce `failed to run run_command`.
/// 2. Using shell pipes/redirects (`| > >> < && || ;`) with a non-shell tool — those tokens
///    were passed as literal argv and silently broke. We collapse such calls into `bash -c`.
fn parse_run_command(call: &ToolCall) -> (String, Vec<String>) {
    let mut tool = call.arguments["tool"].as_str().unwrap_or_default().to_string();
    let mut argv: Vec<String> = call.arguments["argv"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    // Unwrap a nested MCP tool name: tool="run_command", argv=["nmap", "-sV", ...].
    while (tool == "run_command" || tool == "run") && !argv.is_empty() {
        tool = argv.remove(0);
    }

    if needs_shell(&tool, &argv) {
        let line = std::iter::once(tool.as_str())
            .chain(argv.iter().map(|s| s.as_str()))
            .collect::<Vec<_>>()
            .join(" ");
        return ("bash".to_string(), vec!["-c".to_string(), line]);
    }

    (tool, argv)
}

/// True when the call uses shell pipes/redirects but the tool isn't itself a shell, so it must
/// be re-issued under `bash -c`. Windows commands already route through PowerShell in the runner.
fn needs_shell(tool: &str, argv: &[String]) -> bool {
    if cfg!(windows) {
        return false;
    }
    let base = tool.rsplit(['/', '\\']).next().unwrap_or(tool);
    if matches!(base, "bash" | "sh" | "zsh" | "dash" | "pwsh" | "powershell" | "cmd" | "sudo") {
        return false;
    }
    argv.iter().any(|a| {
        a == "|" || a == "<" || a == ">" || a == ">>" || a == "&&" || a == "||" || a == ";"
            || a.starts_with("2>")
            || a.contains('|')
            || a.contains('>')
            || a.contains("$(")
            || a.contains('`')
    })
}

const COMPACT_SYSTEM: &str = "You are compacting a penetration-testing session transcript to free \
up context space. Produce a DENSE factual brief (bullet points, not prose) preserving EVERYTHING \
needed to continue the engagement: target hosts with open ports/services/versions; discovered \
endpoints, paths, parameters; credentials, tokens, hashes and flags VERBATIM; confirmed findings \
and how they were verified; approaches already TRIED and their outcome (succeeded/failed/abandoned) \
so they are not repeated; and the current access/foothold plus the next logical step. Keep exact \
values (IPs, ports, paths, creds). Omit chit-chat and raw tool noise. Be terse.";

/// Render an old transcript slice into a compact plaintext form for the summarizer.
fn transcript_for_summary(msgs: &[Message]) -> String {
    let mut s = String::new();
    for m in msgs {
        let role = match m.role {
            Role::User => "USER",
            Role::Assistant => "ASSISTANT",
            Role::Tool => "TOOL",
            Role::System => "SYSTEM",
        };
        for c in &m.content {
            match c {
                Content::Text { text } => s.push_str(&format!("{role}: {}\n", trunc(text, 600))),
                Content::ToolUse { call } => {
                    s.push_str(&format!("{role} ran {}: {}\n", call.name, trunc(&call.arguments.to_string(), 200)))
                }
                Content::ToolResult { output, .. } => s.push_str(&format!("RESULT: {}\n", trunc(output, 500))),
            }
        }
    }
    s
}

const DISTILL_SYSTEM: &str = "You maintain a durable profile of a penetration tester you assist. \
From the recent activity, extract a SHORT list of durable facts worth remembering long-term. \
Classify each:\n\
- \"global\": an enduring habit or preference of THIS OPERATOR to apply on EVERY future \
engagement (preferred tools, methodology, conventions, wordlists, reporting style). It MUST NOT \
contain target-specific data — no IPs, hostnames, credentials, or client names.\n\
- \"workspace\": a detail specific to THIS engagement worth remembering (open services, \
footholds, target quirks, credentials found).\n\
Only include genuinely durable, useful facts; skip transient noise and anything already obvious. \
Output ONLY a JSON array, e.g. \
[{\"scope\":\"global\",\"text\":\"...\"},{\"scope\":\"workspace\",\"text\":\"...\"}]. \
If nothing is worth saving, output [].";

/// Render recent events (newest-first) into a compact chronological transcript for distillation.
fn distill_transcript(events: &[tianji_types::Event]) -> String {
    let mut lines = Vec::new();
    for e in events.iter().rev() {
        if let Some(t) = distill_event_text(e) {
            let snippet: String = t.chars().take(200).collect();
            lines.push(format!("[{:?}] {snippet}", e.kind));
        }
    }
    let mut s = lines.join("\n");
    s.truncate(s.char_indices().nth(6000).map_or(s.len(), |(i, _)| i));
    s
}

fn distill_event_text(e: &tianji_types::Event) -> Option<String> {
    e.payload
        .get("summary")
        .and_then(|v| v.as_str())
        .or_else(|| e.payload.get("text").and_then(|v| v.as_str()))
        .or_else(|| e.payload.get("output").and_then(|v| v.as_str()))
        .map(|s| s.to_string())
}

/// Parse the model's JSON array of `{scope, text}` facts. Tolerant: extracts the array even if the
/// model wrapped it in prose, and defaults an unknown scope to "workspace" (the safe, isolated one).
fn parse_distilled_facts(text: &str) -> Vec<(String, String)> {
    let (Some(start), Some(end)) = (text.find('['), text.rfind(']')) else {
        return Vec::new();
    };
    if end < start {
        return Vec::new();
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text[start..=end]) else {
        return Vec::new();
    };
    let Some(arr) = value.as_array() else { return Vec::new() };
    arr.iter()
        .filter_map(|item| {
            let fact = item.get("text").and_then(|x| x.as_str()).unwrap_or("").trim();
            if fact.is_empty() {
                return None;
            }
            let scope = item.get("scope").and_then(|x| x.as_str()).unwrap_or("workspace");
            let scope = if scope.eq_ignore_ascii_case("global") { "global" } else { "workspace" };
            Some((scope.to_string(), fact.to_string()))
        })
        .collect()
}

/// The instruction injected at the top of each standalone goal cycle. `first` distinguishes the
/// opening objective from the lighter continuation nudge sent on later iterations.
fn goal_prompt(goal: &str, first: bool) -> String {
    if first {
        format!(
            "AUTONOMOUS GOAL MODE. You are operating WITHOUT a human in the loop. Objective:\n\
             {goal}\n\n\
             Work toward this objective end to end — enumerate, hypothesise, exploit, and verify — \
             using tools and sub-agents as needed. Record each concrete result with record_finding \
             as you go. Do NOT ask the operator questions; decide and act. \
             When the objective is FULLY achieved, send a final message whose LAST line is exactly \
             `{GOAL_DONE_TOKEN}`. If you exhaust every avenue and genuinely cannot proceed, send a \
             final message whose LAST line is exactly `{GOAL_STUCK_TOKEN}` followed by the reason."
        )
    } else {
        format!(
            "Continue autonomously toward the objective. Build on what you already learned — do \
             NOT repeat completed scans. If the objective is now fully achieved, end your message \
             with `{GOAL_DONE_TOKEN}` on its own last line; if you are truly blocked, end with \
             `{GOAL_STUCK_TOKEN}` and the reason."
        )
    }
}

/// Normalize a finding target to `host:port` so trivial suffix differences ("/https", "/tcp",
/// "/admin") don't read as distinct targets when deduping.
fn norm_target(t: &str) -> String {
    let head = t.split_whitespace().next().unwrap_or(t); // drop trailing " — description"
    let base = head.split('/').next().unwrap_or(head); // drop "/https", "/admin/…"
    base.trim().to_lowercase()
}

/// Rough word-overlap (Jaccard over lowercased word sets) between two finding summaries, ignoring
/// short stop-ish tokens. 1.0 = identical wording, 0.0 = nothing in common.
fn summary_overlap(a: &str, b: &str) -> f32 {
    let words = |s: &str| -> std::collections::HashSet<String> {
        s.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 3)
            .map(|w| w.to_string())
            .collect()
    };
    let (wa, wb) = (words(a), words(b));
    if wa.is_empty() || wb.is_empty() {
        return if a.trim().eq_ignore_ascii_case(b.trim()) { 1.0 } else { 0.0 };
    }
    let inter = wa.intersection(&wb).count() as f32;
    let union = wa.union(&wb).count() as f32;
    if union == 0.0 { 0.0 } else { inter / union }
}

/// Truncate to at most `n` chars (char-safe), appending an ellipsis when cut.
fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        format!("{}…", s.chars().take(n).collect::<String>())
    } else {
        s.to_string()
    }
}

/// The per-turn system prompt is split into two blocks so Anthropic prompt caching actually works:
///
/// - [`stable_system_prompt`] — instructions + OS/install policy. Changes only with mode flags, so
///   it's byte-identical across turns and is sent as the CACHED prefix (tools cache with it).
/// - [`volatile_context`] — scope, notes, profile, attempt log, phase. Changes most turns, so it's
///   sent as a SEPARATE, UNcached block. Baking this into the cached prefix (as it used to be) made
///   the cache miss every turn and re-billed the whole prefix as fresh input — the ~10k floor.
///
/// `slim` (small-context mode) trims the prose and caps the volatile lists far harder.
/// Machine engagement = there's a network host target in scope → drive the full kill chain
/// (recon → foothold → loot → privesc → root). Otherwise it's a jeopardy-style challenge
/// (file / single web service) and the CTF triage playbook applies instead.
fn is_machine_engagement(scope: &ScopeRules) -> bool {
    !scope.cidrs.is_empty()
}

/// Compact, always-cached methodology for machine engagements. Replaces the (irrelevant) CTF
/// triage playbook on a box and — crucially — supplies the privilege-escalation checklist that the
/// CTF skills don't cover. Cheap (~250 tok) and cached, so it costs once per session.
fn machine_methodology(slim: bool) -> String {
    if slim {
        return "\n MACHINE: run the FULL chain — recon → foothold (known CVE for the exact \
                version / default-weak creds / web exploit) → grab the user flag → harvest creds \
                (configs, keys, history; TEST every password for reuse via su/ssh) → PRIVESC: \
                sudo -n -l, SUID `find / -perm -4000 -type f 2>/dev/null`, `getcap -r / 2>/dev/null`, \
                root cron/writable scripts, readable secrets, and files a ROOT process reads that \
                YOU can write (configs, hook/spool dirs, signature/whitelist files), else a \
                kernel/service exploit for the exact version. Confirm uid=0, then record root.txt. \
                Build on the Confirmed/Attempt log below — don't re-enumerate."
            .to_string();
    }
    "\n\n## MACHINE ENGAGEMENT — run the FULL kill chain, do NOT stop at a foothold:\n\
     1) RECON: one full port scan per host; identify each service and its exact version.\n\
     2) FOOTHOLD: get code-exec/shell — a known CVE for the exact version, default/weak creds, or a \
     web exploit (for web, call use_skill(\"ctf-web\") and follow its technique file).\n\
     3) LOOT: grab the user flag, then from the shell harvest credentials — app/db/service config \
     files, history, SSH keys — and TEST every password for reuse (su/ssh to other users).\n\
     4) PRIVESC TO ROOT — work this checklist in order, log_attempt each step before moving on:\n\
     • sudo -n -l (GTFOBins for any allowed binary)\n\
     • SUID/SGID: find / -perm -4000 -type f 2>/dev/null (GTFOBins)\n\
     • capabilities: getcap -r / 2>/dev/null\n\
     • cron jobs and writable scripts/PATH run by root\n\
     • readable secrets: config files, private keys, DB creds, /etc/shadow — then reuse them\n\
     • files writable by you that a ROOT process reads or executes (configs, hook/spool dirs, \
     signature/whitelist files) — the highest-value, box-specific path; read the consumer's source \
     ONCE with grep to find the exact check, don't page it\n\
     • a known local-exploit for the exact kernel/service version found\n\
     Confirm root (id shows uid=0), then read and record_finding root.txt.\n\
     Build on what's already in your Confirmed-findings and Attempt log below — do NOT re-scan or \
     re-read anything you've already covered."
        .to_string()
}

fn stable_system_prompt(slim: bool, free_mode: bool, skills_catalog: &str, solve_challenge: &str, machine: bool) -> String {
    // Tell the model the operator OS so it picks the right command syntax. Slim mode uses a one-line
    // form to save tokens.
    let os_hint = match (slim, cfg!(windows)) {
        (true, true) => "Operator OS: Windows (cmd/PowerShell): `ping -n`, `ipconfig`, `dir`, \
                         `netstat -ano`; never Unix-only flags.",
        (true, false) => "Operator OS: Linux/macOS (POSIX). For root actions prefix sudo \
                          (tool=sudo, argv=[real command]); nmap/masscan auto-elevate.",
        (false, true) => "The operator's machine runs Windows (cmd.exe/PowerShell). \
                          Use Windows-compatible flags for every command: \
                          `ping -n 4 <host>` (NOT `-c`), `nmap` flags are cross-platform, \
                          use `ipconfig` not `ifconfig`, `netstat -ano`, `dir` not `ls`, etc. \
                          Never emit Unix-only flags.",
        (false, false) => "The operator's machine runs Linux/macOS. Use POSIX syntax. \
                           For commands that need root (editing /etc/hosts, writing to /etc/, ip route, iptables, \
                           raw packet tools, etc.), prefix with `sudo` — use it as the tool name and pass the real \
                           command as arguments (e.g. tool=sudo argv=[\"tee\",\"-a\",\"/etc/hosts\"]). \
                           Network scanning tools such as nmap and masscan are auto-elevated by the runner when \
                           NOPASSWD sudo is configured.",
    };

    // Missing-tool policy depends on the mode. OPEN mode = the operator's lab/own box, so the
    // agent may install what it needs; CONTROLLED mode = don't touch the system, just advise.
    let install_hint = match (slim, free_mode, cfg!(windows)) {
        (true, true, _) => "OPEN MODE: install missing tools yourself, non-interactively \
                            (apt-get -y / pip / choco -y / git clone).",
        (true, false, _) => "CONTROLLED MODE: do not install or modify the machine; if a tool is \
                             missing, give the exact install command and move on.",
        (false, true, true) => "OPEN MODE: if a required tool is missing (\"command not found\"), install it yourself \
             before retrying — prefer non-interactive package managers (`choco install -y <pkg>`, \
             `winget install --silent <pkg>`, `pip install <pkg>`) or `git clone` the project. \
             Never get stuck repeating a command for a tool that isn't installed.",
        (false, true, false) => "OPEN MODE: if a required tool is missing (\"command not found\"), install it yourself \
             before retrying — use the platform package manager non-interactively \
             (`sudo apt-get install -y <pkg>`, `sudo apt update` first if needed, or pip/gem/go \
             install), or `git clone` the repo and run it. Never get stuck repeating a command for \
             a tool that isn't installed.",
        (false, false, _) => "CONTROLLED MODE: you must NOT install software or modify the operator's machine. If a \
         required tool is missing (\"command not found\"), do NOT keep retrying it — state which \
         tool is missing and the exact command the operator should run to install it (e.g. \
         `sudo apt-get install -y gobuster`), then continue making progress with the tools that \
         ARE available.",
    };

    if slim {
        // Terse core — every token counts on an 8k-ish window.
        return format!(
            "You are an assistant to an authorized penetration tester. \
             Use run_command to run tools (tool name = the bare executable like \"nmap\"; \
             pipes/redirects need tool=\"bash\", argv=[\"-c\", \"<full line>\"]). \
             TRACE YOUR WORK: before starting an exploit/credential/path attempt call log_attempt \
             (status=trying); after seeing the result call it again (succeeded/failed/abandoned + \
             one-line result). Check the attempt log and NEVER redo a failed/abandoned path. \
             record_finding is ONLY for confirmed results and milestones — a captured flag, a \
             working shell/RCE, valid creds, a verified vuln (put the proof in the summary). Do NOT \
             log routine enumeration as findings. \
             If a detail was summarized away or compacted out of context, use the recall tool to \
             fetch the full original output before assuming it's lost. \
             BE EXTREMELY TERSE: a few words of reasoning at most — no narration, no restating \
             output, no plans. Spend tokens on tool calls and findings, not prose. \
             Do NOT delegate to sub-agents — do the work inline (delegation multiplies context and \
             won't fit this window). \
             Never re-run a scan whose output is already in context; read the earlier result. One \
             port scan per host. To inspect a file, grep for what you need rather than dumping it \
             whole; if a result was truncated, call recall — never re-page it with sed ranges. \
             Stay strictly within scope — never touch hosts outside it. \
             {os_hint} {install_hint}{skills_catalog}{solve_challenge}{machine_block}",
            machine_block = if machine { machine_methodology(true) } else { String::new() }
        );
    }

    format!(
        "You are an assistant to an authorized penetration tester. \
         Use the run_command tool to run system tools. \
         TRACE YOUR WORK: before you start an exploit / credential-guess / attack path, call \
         log_attempt with status=\"trying\" and a one-line `action`; once you see the outcome, call \
         it again with status succeeded/failed/abandoned and a one-line `result`. Consult the \
         attempt log and NEVER repeat an approach already marked failed or abandoned — that \
         is the #1 source of wasted tokens. \
         record_finding the INSTANT you reach a milestone — ABOVE ALL, a captured flag: the moment \
         you read user.txt / root.txt or see any `HTB{{...}}` / `flag{{...}}`, call record_finding with \
         severity critical and the flag value in the summary (this is the goal — never just print a \
         flag without recording it). Also record a working shell/RCE (with the command/URL), valid \
         credentials (the pair), and each confirmed vulnerability. ONE finding per distinct issue — \
         near-duplicates are auto-rejected, so log confirmed results freely rather than hoarding \
         them; routine version banners alone aren't findings. \
         If a detail was summarized away or compacted out of context, use the recall tool to fetch \
         the full original output (by keyword) before assuming it's lost. \
         BE TERSE: reason internally in as few words as possible — a sentence or two, never \
         paragraphs. Do NOT narrate your plan, restate command output back to the operator, or \
         write long commentary. Spend tokens on tool calls and findings, not prose. \
         WORK INLINE — sub-agents are NOT free and NOT parallel here: each delegate_to_agent reloads \
         the ENTIRE system prompt + tools and runs to completion before you continue, so delegating \
         multiplies token cost for zero speed gain. Do recon, web, and exploit yourself in one loop. \
         Only delegate when there is a genuinely separate target (e.g. a second host) you would \
         otherwise ignore — NEVER to split phases on a single box. \
         EFFICIENCY (critical — wasted commands cost time and tokens): \
         (1) Do NOT re-run a scan or request whose output is already in the conversation — read \
         the earlier result instead. One full nmap port scan per host is enough; never repeat it. \
         (2) Pipes/redirects need a shell: use tool=\"bash\", argv=[\"-c\", \"<full line>\"]. \
         (3) The tool name is the bare executable (e.g. \"nmap\"), never \"run_command\". \
         (4) To read a file, grep for the part you need (e.g. grep -n -A5 'keyword' file) instead of \
         dumping the whole thing; if a tool result came back truncated, call recall to fetch the \
         full text — do NOT re-run the command or page it with sed/head/tail line ranges (that is a \
         top source of wasted tokens). \
         Stay strictly within the engagement scope — never target hosts outside it. \
         {os_hint} {install_hint}{skills_catalog}{solve_challenge}{machine_block}",
        machine_block = if machine { machine_methodology(false) } else { String::new() }
    )
}

/// The volatile half of the system prompt: engagement scope, operator notebook, distilled profile,
/// the attempt log, and the current phase. Changes most turns → sent UNcached (after the cache
/// breakpoint). Empty when there's nothing to say, which keeps the cached prefix the only system
/// content on a fresh engagement.
fn volatile_context(
    phase: Phase,
    scope: &ScopeRules,
    notes: &[tianji_types::Event],
    profile: &[String],
    attempts: &[tianji_types::Event],
    findings: &[tianji_types::Finding],
    slim: bool,
) -> String {
    // Caps: (max notes, note chars, max facts, fact chars, max attempts, max findings). Kept tight —
    // this block is re-sent every turn, so every line here is a recurring tax.
    let (note_max, note_len, fact_max, fact_len, attempt_max, finding_max) =
        if slim { (4usize, 140usize, 5usize, 140usize, 6usize, 5usize) } else { (6, 160, 8, 160, 8, 7) };

    let phase_hint = match phase {
        Phase::Recon => "Phase: RECON — enumerate hosts/services with read-only tools.",
        Phase::Hypothesis => "Phase: HYPOTHESIS — reason about likely weaknesses.",
        Phase::Poc => "Phase: PoC — build minimal proofs of concept.",
        Phase::Exploit => "Phase: EXPLOIT — act carefully; destructive actions need approval.",
        Phase::Report => "Phase: REPORT — summarize findings and evidence.",
    };

    let mut scope_entries = scope.cidrs.clone();
    scope_entries.extend(scope.hostnames.clone());
    scope_entries.extend(scope.url_domains.clone());
    let scope_hint = if scope_entries.is_empty() {
        "No engagement scope is defined yet — ask the operator for the target before running any tools.".to_string()
    } else {
        format!("Engagement scope: {}.", scope_entries.join(", "))
    };

    // Notes arrive oldest-first; keep the most recent `note_max` (the tail) and bound each line.
    let note_texts: Vec<&str> =
        notes.iter().filter_map(|e| e.payload.get("text").and_then(|v| v.as_str())).collect();
    let note_start = note_texts.len().saturating_sub(note_max);
    let notes_hint = if note_texts.is_empty() {
        String::new()
    } else {
        let lines = note_texts[note_start..]
            .iter()
            .map(|t| format!("- {}", trunc(t, note_len)))
            .collect::<Vec<_>>()
            .join("\n");
        format!(" Operator notebook:\n{lines}")
    };

    // Distilled profile — bounded so a long-lived profile can't dominate the window.
    let profile_hint = if profile.is_empty() {
        String::new()
    } else {
        let lines = profile
            .iter()
            .take(fact_max)
            .map(|f| format!("- {}", trunc(f, fact_len)))
            .collect::<Vec<_>>()
            .join("\n");
        format!(" Learned about this operator/engagement (apply proactively; explicit instructions win):\n{lines}")
    };

    // Attempt log — `attempts` arrives newest-first; show the most recent in chronological order.
    let attempts_hint = if attempts.is_empty() {
        String::new()
    } else {
        let mut recent: Vec<&tianji_types::Event> = attempts.iter().take(attempt_max).collect();
        recent.reverse();
        let lines = recent
            .iter()
            .filter_map(|e| {
                let p = &e.payload;
                let action = p.get("action").and_then(|v| v.as_str()).unwrap_or("");
                if action.is_empty() {
                    return None;
                }
                let status = p.get("status").and_then(|v| v.as_str()).unwrap_or("trying");
                let result = p.get("result").and_then(|v| v.as_str()).unwrap_or("");
                Some(if result.is_empty() {
                    format!("- [{status}] {}", trunc(action, 110))
                } else {
                    format!("- [{status}] {} → {}", trunc(action, 110), trunc(result, 110))
                })
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!(" Attempt log — already tried (do NOT repeat failed/abandoned; build on succeeded):\n{lines}")
    };

    // Confirmed findings — authoritative milestones already captured. Surfacing them stops the
    // agent from re-deriving what it already proved (e.g. re-finding a flag) when a standalone run
    // resumes after "keep going".
    let findings_hint = if findings.is_empty() {
        String::new()
    } else {
        let mut recent: Vec<&tianji_types::Finding> = findings.iter().rev().take(finding_max).collect();
        recent.reverse();
        let lines = recent
            .iter()
            .map(|f| format!("- [{}] {}: {}", f.severity, trunc(&f.target, 60), trunc(&f.summary, 130)))
            .collect::<Vec<_>>()
            .join("\n");
        format!(" Confirmed so far (authoritative — do NOT re-derive or re-enumerate these):\n{lines}")
    };

    format!("{scope_hint}{notes_hint}{profile_hint}{findings_hint}{attempts_hint} {phase_hint}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures::stream::{self, BoxStream};
    use std::sync::Mutex;
    use tianji_llm::{LlmError, LlmProvider};
    use tianji_types::{ScopeRules, ToolSpec};

    fn tool_call(tool: &str, argv: &[&str]) -> ToolCall {
        ToolCall {
            call_id: "t1".into(),
            name: "run_command".into(),
            arguments: json!({ "tool": tool, "argv": argv }),
        }
    }

    #[test]
    fn parse_unwraps_nested_run_command() {
        let (tool, argv) = parse_run_command(&tool_call("run_command", &["nmap", "-sV", "10.0.0.1"]));
        assert_eq!(tool, "nmap");
        assert_eq!(argv, vec!["-sV", "10.0.0.1"]);
    }

    #[test]
    #[cfg(not(windows))]
    fn parse_wraps_pipes_into_bash() {
        let (tool, argv) = parse_run_command(&tool_call("curl", &["-s", "http://x/", "|", "head"]));
        assert_eq!(tool, "bash");
        assert_eq!(argv, vec!["-c", "curl -s http://x/ | head"]);
    }

    #[test]
    #[cfg(not(windows))]
    fn parse_leaves_plain_commands_alone() {
        let (tool, argv) = parse_run_command(&tool_call("nmap", &["-sC", "-sV", "10.0.0.1"]));
        assert_eq!(tool, "nmap");
        assert_eq!(argv, vec!["-sC", "-sV", "10.0.0.1"]);
    }

    #[test]
    fn open_mode_lets_the_agent_install_tools() {
        let p = stable_system_prompt(false, true, "", "", false);
        assert!(p.contains("OPEN MODE"));
        assert!(p.to_lowercase().contains("install it yourself"));
    }

    #[test]
    fn controlled_mode_forbids_install_and_advises_operator() {
        let p = stable_system_prompt(false, false, "", "", false);
        assert!(p.contains("CONTROLLED MODE"));
        assert!(p.contains("must NOT install"));
    }

    #[test]
    fn prompt_discourages_delegation_for_single_target() {
        let p = stable_system_prompt(false, false, "", "", false);
        assert!(p.contains("delegate_to_agent"));
        assert!(p.contains("WORK INLINE"));
        assert!(!p.contains("DEFAULT for separable work"));
    }

    #[test]
    fn slim_prompt_is_smaller_and_forbids_delegation() {
        let full = stable_system_prompt(false, false, "", "", false);
        let slim = stable_system_prompt(true, false, "", "", false);
        assert!(slim.len() < full.len(), "slim prompt should be shorter");
        assert!(slim.contains("Do NOT delegate"), "slim must forbid delegation");
        assert!(!slim.contains("DEFAULT for separable work"));
    }

    #[test]
    fn machine_mode_injects_privesc_methodology_not_ctf_playbook() {
        let ctf_playbook = "## MANDATORY CTF PLAYBOOK";
        // Machine engagement: methodology + privesc checklist, and the CTF playbook is gated off
        // (the caller passes solve_challenge="" for machines).
        let machine = stable_system_prompt(false, false, "", "", true);
        assert!(machine.contains("MACHINE ENGAGEMENT"), "machine methodology present");
        assert!(machine.contains("PRIVESC TO ROOT"), "privesc checklist present");
        assert!(machine.contains("perm -4000"), "SUID step present");
        assert!(!machine.contains(ctf_playbook), "CTF playbook must be gated off on a machine");
        // Challenge engagement: no machine methodology block.
        let challenge = stable_system_prompt(false, false, "", ctf_playbook, false);
        assert!(!challenge.contains("MACHINE ENGAGEMENT"), "no machine block for a challenge");
        assert!(challenge.contains(ctf_playbook), "CTF playbook present for a challenge");
    }

    #[test]
    fn machine_detection_keys_on_network_target() {
        let mut s = ScopeRules::default();
        assert!(!is_machine_engagement(&s), "empty scope is not a machine");
        s.url_domains.push("chal.ctf.io".into());
        assert!(!is_machine_engagement(&s), "a web challenge URL alone is not a machine");
        s.cidrs.push("10.129.48.177".into());
        assert!(is_machine_engagement(&s), "a host/IP target means machine mode");
    }

    #[test]
    fn confirmed_findings_surface_in_volatile_context() {
        let f = tianji_types::Finding {
            id: tianji_types::EventId::new(),
            workspace_id: tianji_types::WorkspaceId::new(),
            severity: "high".into(),
            target: "10.129.48.177".into(),
            summary: "user.txt captured: deadbeef".into(),
            evidence_event_ids: vec![],
        };
        let v = volatile_context(Phase::Exploit, &ScopeRules::default(), &[], &[], &[], &[f], false);
        assert!(v.contains("Confirmed so far"), "findings header present");
        assert!(v.contains("user.txt captured"), "finding summary surfaced");
    }

    #[test]
    fn volatile_block_excludes_the_stable_instructions() {
        // The volatile context must NOT carry the big instruction prose (that lives in the cached
        // block) — otherwise the split wouldn't reduce per-turn tokens.
        let v = volatile_context(Phase::Recon, &ScopeRules::default(), &[], &[], &[], &[], false);
        assert!(!v.contains("delegate_to_agent"));
        assert!(!v.contains("OPEN MODE") && !v.contains("CONTROLLED MODE"));
        assert!(v.contains("Phase: RECON"));
    }

    #[test]
    fn volatile_context_bounds_the_profile() {
        let facts: Vec<String> = (0..50).map(|i| format!("habit number {i}")).collect();
        let v = volatile_context(Phase::Recon, &ScopeRules::default(), &[], &facts, &[], &[], false);
        // Only the first 8 facts survive the normal cap.
        assert!(v.contains("habit number 7"));
        assert!(!v.contains("habit number 8"));
    }

    /// A scripted provider: each `run_turn` returns the next pre-baked round of events.
    struct ScriptedProvider {
        rounds: Mutex<std::collections::VecDeque<Vec<AgentEvent>>>,
    }

    impl ScriptedProvider {
        fn new(rounds: Vec<Vec<AgentEvent>>) -> Self {
            Self { rounds: Mutex::new(rounds.into()) }
        }
    }

    #[async_trait]
    impl LlmProvider for ScriptedProvider {
        async fn run_turn(
            &self,
            _messages: &[Message],
            _tools: &[ToolSpec],
        ) -> std::result::Result<BoxStream<'static, AgentEvent>, LlmError> {
            let next = self.rounds.lock().unwrap().pop_front().unwrap_or_default();
            Ok(Box::pin(stream::iter(next)))
        }
    }

    /// A runner that records calls and returns a fixed string — no real processes.
    #[derive(Default)]
    struct StubRunner {
        calls: Mutex<Vec<(String, Vec<String>)>>,
    }

    impl CommandRunner for StubRunner {
        fn run(&self, tool: &str, argv: &[String]) -> String {
            self.calls.lock().unwrap().push((tool.to_string(), argv.to_vec()));
            "STUB-OUTPUT".to_string()
        }
    }

    fn run_command_call(tool: &str, argv: &[&str]) -> AgentEvent {
        AgentEvent::ToolCall {
            call: ToolCall {
                call_id: "c1".into(),
                name: "run_command".into(),
                arguments: json!({ "tool": tool, "argv": argv }),
            },
        }
    }

    fn store_with_scope(cidr: &str) -> WorkspaceStore {
        let store = WorkspaceStore::open_in_memory().unwrap();
        store
            .set_scope(&ScopeRules { cidrs: vec![cidr.into()], ..Default::default() })
            .unwrap();
        store
    }

    fn drain(mut rx: tokio::sync::mpsc::UnboundedReceiver<AgentUpdate>) -> Vec<AgentUpdate> {
        let mut out = Vec::new();
        while let Ok(u) = rx.try_recv() {
            out.push(u);
        }
        out
    }

    #[tokio::test]
    async fn in_scope_readonly_tool_auto_runs() {
        let store = store_with_scope("10.0.0.0/24");
        let runner = Arc::new(StubRunner::default());
        let orch = Orchestrator::new(Arc::new(ScriptedProvider::new(vec![
            vec![run_command_call("ping", &["10.0.0.5"]), AgentEvent::TurnEnd],
            vec![AgentEvent::TextDelta { text: "done".into() }, AgentEvent::TurnEnd],
        ])))
        .with_runner(runner.clone());

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        orch.handle_prompt(&store, tx, "scan .5").await.unwrap();

        assert_eq!(runner.calls.lock().unwrap().len(), 1, "ping should auto-run");
        let updates = drain(rx);
        assert!(updates.iter().any(|u| matches!(u, AgentUpdate::ToolStarted { .. })));
        assert!(updates.iter().any(|u| matches!(u, AgentUpdate::TurnEnded)));
        assert!(!updates.iter().any(|u| matches!(u, AgentUpdate::Denied { .. })));
    }

    #[tokio::test]
    async fn out_of_scope_tool_is_denied_and_not_run() {
        let store = store_with_scope("10.0.0.0/24");
        let runner = Arc::new(StubRunner::default());
        let orch = Orchestrator::new(Arc::new(ScriptedProvider::new(vec![
            vec![run_command_call("ping", &["8.8.8.8"]), AgentEvent::TurnEnd],
            vec![AgentEvent::TurnEnd],
        ])))
        .with_runner(runner.clone());

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        orch.handle_prompt(&store, tx, "scan google dns").await.unwrap();

        assert_eq!(runner.calls.lock().unwrap().len(), 0, "out-of-scope must not run");
        assert!(drain(rx).iter().any(|u| matches!(u, AgentUpdate::Denied { .. })));
    }

    #[test]
    fn goal_prompt_carries_sentinels() {
        let first = goal_prompt("retrieve user and root flags", true);
        assert!(first.contains("retrieve user and root flags"));
        assert!(first.contains(GOAL_DONE_TOKEN));
        assert!(first.contains(GOAL_STUCK_TOKEN));
        let cont = goal_prompt("retrieve user and root flags", false);
        assert!(cont.contains(GOAL_DONE_TOKEN));
        assert!(cont.contains(GOAL_STUCK_TOKEN));
    }

    #[tokio::test]
    async fn run_goal_completes_on_sentinel() {
        let store = store_with_scope("10.0.0.0/24");
        let orch = Orchestrator::new(Arc::new(ScriptedProvider::new(vec![vec![
            AgentEvent::TextDelta { text: format!("flag captured {GOAL_DONE_TOKEN}") },
            AgentEvent::TurnEnd,
        ]])));

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        orch.run_goal(&store, tx, "capture the user flag").await.unwrap();

        let u = drain(rx);
        assert!(u.iter().any(|x| matches!(x, AgentUpdate::GoalStarted { .. })));
        assert!(u.iter().any(|x| matches!(
            x,
            AgentUpdate::GoalFinished { outcome, iterations }
                if outcome == "completed" && *iterations == 1
        )));
        // The per-cycle TurnEnded is swallowed; exactly one final TurnEnded reaches the UI.
        assert_eq!(
            u.iter().filter(|x| matches!(x, AgentUpdate::TurnEnded)).count(),
            1
        );
    }

    #[tokio::test]
    async fn run_goal_stops_at_iteration_cap() {
        let store = store_with_scope("10.0.0.0/24");
        // Provider never emits the sentinel (always an empty round) → the loop must terminate on
        // the iteration safety rail rather than spinning forever.
        let orch = Orchestrator::new(Arc::new(ScriptedProvider::new(vec![])));

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        orch.run_goal(&store, tx, "an unreachable objective").await.unwrap();

        assert!(drain(rx).iter().any(|x| matches!(
            x,
            AgentUpdate::GoalFinished { outcome, iterations }
                if outcome == "max-iterations" && *iterations == MAX_GOAL_ITERATIONS as u32
        )));
    }

    #[test]
    fn profile_facts_are_injected() {
        let v = volatile_context(
            Phase::Recon,
            &ScopeRules::default(),
            &[],
            &["prefers ffuf over gobuster".to_string()],
            &[],
            &[],
            false,
        );
        assert!(v.contains("prefers ffuf over gobuster"));
        assert!(v.contains("Learned about this operator"));
    }

    #[tokio::test]
    async fn distillation_extracts_and_routes_facts() {
        let store = store_with_scope("10.0.0.0/24");
        for i in 0..6 {
            store
                .append(Event::new(
                    store.workspace_id(),
                    Phase::Recon,
                    EventKind::AgentMsg,
                    AgentId("agent".into()),
                    Author::Agent,
                    json!({ "text": format!("did thing {i}") }),
                ))
                .unwrap();
        }
        let facts_json = r#"[{"scope":"global","text":"prefers ffuf over gobuster"},
                             {"scope":"workspace","text":"port 8080 runs Tomcat"}]"#;
        let orch = Orchestrator::new(Arc::new(ScriptedProvider::new(vec![vec![
            AgentEvent::TextDelta { text: facts_json.to_string() },
            AgentEvent::TurnEnd,
        ]])));

        let global = orch.distill_profile(&store).await;
        assert_eq!(global, vec!["prefers ffuf over gobuster"]);
        let ws = store.workspace_facts().unwrap();
        assert!(ws.iter().any(|f| f.text.contains("Tomcat")));
        // Global facts must never carry per-engagement specifics into the workspace store.
        assert!(!ws.iter().any(|f| f.text.contains("ffuf")));
    }

    #[tokio::test]
    async fn conversation_persists_and_hydrates() {
        let store = store_with_scope("10.0.0.0/24");
        let orch = Orchestrator::new(Arc::new(ScriptedProvider::new(vec![vec![
            AgentEvent::TextDelta { text: "hello".into() },
            AgentEvent::TurnEnd,
        ]])))
        .with_runner(Arc::new(StubRunner::default()));

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        orch.handle_prompt(&store, tx, "hi there").await.unwrap();
        let saved_len = orch.history_len("default");
        assert!(saved_len > 0, "the turn should have populated history");

        // A fresh orchestrator (simulating a restart/rebuild) starts empty, then hydrates.
        let orch2 = Orchestrator::new(Arc::new(ScriptedProvider::new(vec![])));
        assert_eq!(orch2.history_len("default"), 0);
        orch2.hydrate(&store);
        assert_eq!(orch2.history_len("default"), saved_len, "history should be restored");
    }

    #[tokio::test]
    async fn loop_guard_stops_repeated_command() {
        let store = store_with_scope("10.0.0.0/24");
        let runner = Arc::new(StubRunner::default());
        // The model insists on the same (in-scope, auto-running) command every round.
        let rounds = (0..6)
            .map(|_| vec![run_command_call("ping", &["10.0.0.5"]), AgentEvent::TurnEnd])
            .collect();
        let orch = Orchestrator::new(Arc::new(ScriptedProvider::new(rounds)))
            .with_runner(runner.clone());

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        orch.handle_prompt(&store, tx, "hit it").await.unwrap();

        assert!(drain(rx).iter().any(|x| matches!(
            x,
            AgentUpdate::Denied { reason } if reason.contains("loop guard")
        )));
    }

    #[tokio::test]
    async fn token_budget_halts_the_run() {
        let store = store_with_scope("10.0.0.0/24");
        let runner = Arc::new(StubRunner::default());
        let orch = Orchestrator::new(Arc::new(ScriptedProvider::new(vec![
            vec![
                AgentEvent::TokensUsed { input_tokens: 500, output_tokens: 500 },
                run_command_call("ping", &["10.0.0.5"]),
                AgentEvent::TurnEnd,
            ],
            vec![AgentEvent::TextDelta { text: "again".into() }, AgentEvent::TurnEnd],
        ])))
        .with_runner(runner.clone());
        orch.set_token_budget(100); // tiny — the first round blows past it

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        orch.handle_prompt(&store, tx, "go").await.unwrap();

        assert!(orch.tokens_spent() >= 1000);
        assert!(drain(rx).iter().any(|x| matches!(
            x,
            AgentUpdate::Error(m) if m.contains("Token budget reached")
        )));
    }
}
