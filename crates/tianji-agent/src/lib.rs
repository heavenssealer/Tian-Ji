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

mod approval;
mod assembler;
mod mcp;
mod runner;
mod summary;

pub use approval::{ApprovalGate, ApprovalOutcome, ApprovalToken, ProposedCall};
pub use assembler::ContextAssembler;
pub use mcp::McpHost;
pub use runner::{CommandRunner, ProcessRunner};

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
        }
    }

    /// Replace the cached global habits (called at build time from the app store and after each
    /// distillation pass).
    pub fn set_global_facts(&self, facts: Vec<String>) {
        *self.global_facts.lock().unwrap() = facts;
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
        let trimmed = ContextAssembler::default().trim_to_budget(&messages);
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
        let history_before_len;
        let mut messages = {
            let mut histories = self.histories.lock().unwrap();
            let history = histories.entry(session_id.clone()).or_default();
            history_before_len = history.len();
            // System prompt is rebuilt fresh each turn (scope/phase/notes may have changed).
            let mut msgs = vec![
                Message { role: Role::System, content: vec![text(system_prompt(phase, &scope, &notes, self.free_mode.load(Ordering::SeqCst), &self.profile_for(store)))] },
            ];
            msgs.extend(history.clone());
            msgs
        };
        messages.push(Message { role: Role::User, content: vec![text(prompt.to_string())] });

        // Repeated-command counts for this prompt-cycle, feeding the loop guard.
        let mut loop_counts: HashMap<String, usize> = HashMap::new();

        let assembler = ContextAssembler::default();
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

            ContextAssembler::cap_tool_output(&mut messages);
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

        let sys = format!(
            "{}{recalled_hint}",
            system_prompt(
                subagent_phase, &scope, &notes,
                self.free_mode.load(Ordering::SeqCst), &self.profile_for(store),
            )
        );

        let mut messages = vec![
            Message { role: Role::System, content: vec![text(sys)] },
            Message { role: Role::User,   content: vec![text(objective.clone())] },
        ];

        let assembler = ContextAssembler::default();
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

            ContextAssembler::cap_tool_output(&mut messages);
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
                let context_out = if tc.name == "record_finding" {
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

fn system_prompt(
    phase: Phase,
    scope: &ScopeRules,
    notes: &[tianji_types::Event],
    free_mode: bool,
    profile: &[String],
) -> String {
    let phase_hint = match phase {
        Phase::Recon => "You are in the RECON phase: enumerate hosts/services with read-only tools.",
        Phase::Hypothesis => "You are in the HYPOTHESIS phase: reason about likely weaknesses.",
        Phase::Poc => "You are in the PoC phase: build minimal proofs of concept.",
        Phase::Exploit => "You are in the EXPLOIT phase: act carefully; destructive actions need approval.",
        Phase::Report => "You are in the REPORT phase: summarize findings and evidence.",
    };

    let mut scope_entries = scope.cidrs.clone();
    scope_entries.extend(scope.hostnames.clone());
    scope_entries.extend(scope.url_domains.clone());
    let scope_hint = if scope_entries.is_empty() {
        "No engagement scope is defined yet — ask the operator for the target before running any tools.".to_string()
    } else {
        format!("Engagement scope: {}.", scope_entries.join(", "))
    };

    let notes_hint = if notes.is_empty() {
        String::new()
    } else {
        let lines = notes
            .iter()
            .filter_map(|e| e.payload.get("text").and_then(|v| v.as_str()))
            .map(|t| format!("- {t}"))
            .collect::<Vec<_>>()
            .join("\n");
        format!(" Operator notebook:\n{lines}")
    };

    // Distilled profile — the operator's habits + what we know about this engagement. Always
    // injected so the agent applies it proactively from the first message.
    let profile_hint = if profile.is_empty() {
        String::new()
    } else {
        let lines = profile.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n");
        format!(
            " What you've learned about this operator and engagement (apply it proactively, but \
             the operator's explicit instructions always win):\n{lines}"
        )
    };

    // Tell the model the operator OS so it picks the right command syntax.
    let os_hint = if cfg!(windows) {
        "The operator's machine runs Windows (cmd.exe/PowerShell). \
         Use Windows-compatible flags for every command: \
         `ping -n 4 <host>` (NOT `-c`), `nmap` flags are cross-platform, \
         use `ipconfig` not `ifconfig`, `netstat -ano`, `dir` not `ls`, etc. \
         Never emit Unix-only flags."
    } else {
        "The operator's machine runs Linux/macOS. Use POSIX syntax. \
         For commands that need root (editing /etc/hosts, writing to /etc/, ip route, iptables, \
         raw packet tools, etc.), prefix with `sudo` — use it as the tool name and pass the real \
         command as arguments (e.g. tool=sudo argv=[\"tee\",\"-a\",\"/etc/hosts\"]). \
         Network scanning tools such as nmap and masscan are auto-elevated by the runner when \
         NOPASSWD sudo is configured."
    };

    // Missing-tool policy depends on the mode. OPEN mode = the operator's lab/own box, so the
    // agent may install what it needs; CONTROLLED mode = don't touch the system, just advise.
    let install_hint = if free_mode {
        if cfg!(windows) {
            "OPEN MODE: if a required tool is missing (\"command not found\"), install it yourself \
             before retrying — prefer non-interactive package managers (`choco install -y <pkg>`, \
             `winget install --silent <pkg>`, `pip install <pkg>`) or `git clone` the project. \
             Never get stuck repeating a command for a tool that isn't installed."
        } else {
            "OPEN MODE: if a required tool is missing (\"command not found\"), install it yourself \
             before retrying — use the platform package manager non-interactively \
             (`sudo apt-get install -y <pkg>`, `sudo apt update` first if needed, or pip/gem/go \
             install), or `git clone` the repo and run it. Never get stuck repeating a command for \
             a tool that isn't installed."
        }
    } else {
        "CONTROLLED MODE: you must NOT install software or modify the operator's machine. If a \
         required tool is missing (\"command not found\"), do NOT keep retrying it — state which \
         tool is missing and the exact command the operator should run to install it (e.g. \
         `sudo apt-get install -y gobuster`), then continue making progress with the tools that \
         ARE available."
    };

    format!(
        "You are an assistant to an authorized penetration tester. \
         {scope_hint}{notes_hint}{profile_hint} \
         Use the run_command tool to run system tools. \
         When you discover an open port, vulnerable service, misconfiguration, or any \
         noteworthy security issue, call record_finding immediately with severity \
         (critical/high/medium/low/info), the affected target (e.g. 192.168.1.25:22/ssh), \
         and a concise one-line summary. \
         BE TERSE: reason internally in as few words as possible — a sentence or two, never \
         paragraphs. Do NOT narrate your plan, restate command output back to the operator, or \
         write long commentary. Spend tokens on tool calls and findings, not prose. \
         STRATEGY: when work splits into independent streams (different hosts, or recon vs. web \
         vs. exploit on the same target), delegate those streams to sub-agents via \
         delegate_to_agent and let them run in parallel rather than doing everything yourself in \
         one long serial loop. Delegation is the DEFAULT for separable work — efficiency rule (2) \
         is about scoping each sub-agent tightly, NOT about avoiding delegation. \
         EFFICIENCY (critical — wasted commands cost time and tokens): \
         (1) Do NOT re-run a scan or request whose output is already in the conversation — read \
         the earlier result instead. One full nmap port scan per host is enough; never repeat it. \
         (2) Before delegating, check what is already known; give sub-agents a NARROW objective and \
         tell them to build on existing results, not redo recon. \
         (3) Pipes/redirects need a shell: use tool=\"bash\", argv=[\"-c\", \"<full line>\"]. \
         (4) The tool name is the bare executable (e.g. \"nmap\"), never \"run_command\". \
         Stay strictly within the engagement scope — never target hosts outside it. \
         {os_hint} {install_hint} {phase_hint}"
    )
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
        let p = system_prompt(Phase::Recon, &ScopeRules::default(), &[], true, &[]);
        assert!(p.contains("OPEN MODE"));
        assert!(p.to_lowercase().contains("install it yourself"));
    }

    #[test]
    fn controlled_mode_forbids_install_and_advises_operator() {
        let p = system_prompt(Phase::Recon, &ScopeRules::default(), &[], false, &[]);
        assert!(p.contains("CONTROLLED MODE"));
        assert!(p.contains("must NOT install"));
    }

    #[test]
    fn prompt_makes_delegation_the_default() {
        let p = system_prompt(Phase::Recon, &ScopeRules::default(), &[], false, &[]);
        assert!(p.contains("delegate_to_agent"));
        assert!(p.contains("DEFAULT for separable work"));
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
        let p = system_prompt(
            Phase::Recon,
            &ScopeRules::default(),
            &[],
            false,
            &["prefers ffuf over gobuster".to_string()],
        );
        assert!(p.contains("prefers ffuf over gobuster"));
        assert!(p.contains("learned about this operator"));
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
