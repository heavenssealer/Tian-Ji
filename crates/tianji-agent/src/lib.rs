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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures::StreamExt;
use serde_json::json;
use tokio::sync::mpsc::UnboundedSender;

use tianji_llm::LlmProvider;
use tianji_policy::{classify, decide, resolve_targets, AllowRule};
use tianji_store::WorkspaceStore;
use tianji_types::{
    AgentEvent, AgentId, Author, Content, Decision, Event, EventKind, Message, Phase, Role,
    ScopeRules, ToolCall, WorkspaceId,
};

mod approval;
mod assembler;
mod mcp;
mod runner;

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
    TurnEnded,
    Error(String),
}

/// The per-workspace agent runtime. Owns the provider, the approval gate, the tool host, and a
/// command runner; borrows the workspace store per turn.
pub struct Orchestrator {
    provider: Arc<dyn LlmProvider>,
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
    /// Per-session conversation history. Key = session id, value = past messages.
    histories: std::sync::Mutex<HashMap<String, Vec<Message>>>,
    /// The currently-active session id.
    active_session: std::sync::Mutex<String>,
}

impl Orchestrator {
    pub fn new(provider: Arc<dyn LlmProvider>) -> Self {
        let mut histories = HashMap::new();
        histories.insert("default".to_string(), Vec::new());
        Self {
            provider,
            gate: Arc::new(ApprovalGate::default()),
            mcp: McpHost::new(),
            runner: Arc::new(ProcessRunner),
            actor: AgentId("agent".to_string()),
            autonomous: Arc::new(AtomicBool::new(false)),
            free_mode: Arc::new(AtomicBool::new(false)),
            cancelled: Arc::new(AtomicBool::new(false)),
            histories: std::sync::Mutex::new(histories),
            active_session: std::sync::Mutex::new("default".to_string()),
        }
    }

    /// Inject a custom command runner (used in tests to avoid spawning real processes).
    pub fn with_runner(mut self, runner: Arc<dyn CommandRunner>) -> Self {
        self.runner = runner;
        self
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
    }

    /// Switch to an existing session (or create an empty one if unknown).
    pub fn switch_session(&self, session_id: &str) {
        let mut h = self.histories.lock().unwrap();
        h.entry(session_id.to_string()).or_default();
        *self.active_session.lock().unwrap() = session_id.to_string();
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
                Message { role: Role::System, content: vec![text(system_prompt(phase, &scope, &notes))] },
            ];
            msgs.extend(history.clone());
            msgs
        };
        messages.push(Message { role: Role::User, content: vec![text(prompt.to_string())] });

        let assembler = ContextAssembler::default();
        for _round in 0..MAX_ROUNDS {
            if self.cancelled.load(Ordering::SeqCst) {
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
                let output = if call.name == "record_finding" {
                    self.handle_record_finding(store, &updates, ws, phase, &call)?
                } else if call.name == "delegate_to_agent" {
                    self.run_subagent(store, &updates, ws, phase, &call).await?
                } else {
                    self.handle_tool_call(store, &updates, ws, phase, &scope, &rules, &call)
                        .await?
                };
                results.push(Content::ToolResult { call_id: call.call_id, output });
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
                histories.entry(session_id).or_default().extend(new_entries);
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
            system_prompt(subagent_phase, &scope, &notes)
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
                .provider
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
                        let output = self.runner.run(&tool, &argv);
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
                            let output = self.runner.run(&tool, &argv);
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
                            if self.autonomous.load(Ordering::SeqCst) {
                                let _ = updates.send(AgentUpdate::ToolStarted { tool: tool.clone(), argv: argv.clone() });
                                let output = self.runner.run(&tool, &argv);
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
                                        let output = self.runner.run(&tool, &argv);
                                        let _ = updates.send(AgentUpdate::ToolOutput { text: output.clone() });
                                        output
                                    }
                                    Ok(ApprovalOutcome::ApproveEdited(new_argv)) => {
                                        let _ = updates.send(AgentUpdate::ToolStarted { tool: tool.clone(), argv: new_argv.clone() });
                                        let output = self.runner.run(&tool, &new_argv);
                                        let _ = updates.send(AgentUpdate::ToolOutput { text: output.clone() });
                                        output
                                    }
                                    Ok(ApprovalOutcome::AlwaysAllow(rule)) => {
                                        store.add_allow_rule(&rule)?;
                                        let _ = updates.send(AgentUpdate::ToolStarted { tool: tool.clone(), argv: argv.clone() });
                                        let output = self.runner.run(&tool, &argv);
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
                results.push(Content::ToolResult { call_id: tc.call_id, output: out });
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
                if self.autonomous.load(Ordering::SeqCst) {
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

        let output = self.runner.run(tool, argv);

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
fn parse_run_command(call: &ToolCall) -> (String, Vec<String>) {
    let tool = call.arguments["tool"].as_str().unwrap_or_default().to_string();
    let argv = call.arguments["argv"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    (tool, argv)
}

fn system_prompt(phase: Phase, scope: &ScopeRules, notes: &[tianji_types::Event]) -> String {
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

    // Tell the model the operator OS so it picks the right command syntax.
    let os_hint = if cfg!(windows) {
        "The operator's machine runs Windows (cmd.exe/PowerShell). \
         Use Windows-compatible flags for every command: \
         `ping -n 4 <host>` (NOT `-c`), `nmap` flags are cross-platform, \
         use `ipconfig` not `ifconfig`, `netstat -ano`, `dir` not `ls`, etc. \
         Never emit Unix-only flags."
    } else {
        "The operator's machine runs Linux/macOS. Use POSIX syntax."
    };

    format!(
        "You are an assistant to an authorized penetration tester. \
         {scope_hint}{notes_hint} \
         Use the run_command tool to run system tools. \
         When you discover an open port, vulnerable service, misconfiguration, or any \
         noteworthy security issue, call record_finding immediately with severity \
         (critical/high/medium/low/info), the affected target (e.g. 192.168.1.25:22/ssh), \
         and a concise one-line summary. \
         IMPORTANT: do NOT re-run commands whose output is already in the conversation history — \
         use those results directly instead of repeating scans. \
         Stay strictly within the engagement scope — never target hosts outside it. \
         {os_hint} {phase_hint}"
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
}
