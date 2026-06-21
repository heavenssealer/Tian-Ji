# Tiān Jī — Design Document

> An LLM-orchestrated pentesting framework: multiple concurrent terminals, cloud/local
> agents that drive system tooling under human-controlled guardrails, per-engagement
> workspaces, persistent memory, and a phase-aware UI.

Status: **architecture locked, pre-implementation.**
Last updated: 2026-06-21

---

## 1. Vision

A desktop application where a pentester works alongside LLM agents. The agents can spawn
and drive real terminals, run system tooling (nmap, curl, ping, msfconsole, …), and assist
across every phase of an engagement — but only ever through a policy layer the human
controls. The app is workspace-oriented (one workspace per engagement), remembers what has
happened and how the user works, and keeps notes automatically while still giving the user
a deliberate notebook of their own.

Design priorities, in order:

1. **Safe by construction** — an LLM that can run attacker tooling is a loaded gun; the
   guardrails are load-bearing architecture, not a feature.
2. **Fast and reliable** — native core (Rust), embedded storage, no external services.
3. **Genuinely useful on a real engagement** — not a tech demo.
4. **Modern, dense, elegant UI** — wazuh-threat-hunter data surfaces + Claude/OpenAI-style
   conversation surfaces.

---

## 2. Stack

| Layer | Choice | Why |
|---|---|---|
| Shell | **Tauri 2.0** | Rust core + web frontend; small, fast, reliable. |
| Core | **Rust** | Memory-safety matters most in the process that executes exploit tooling. |
| Frontend | **React or Svelte + Vite + Tailwind** | Cheap path to the "bleeding-edge" UI feel. |
| Terminals | **`xterm.js`** (render) + **`portable-pty`** (WezTerm, PTY mgmt) | Battle-tested; real terminals, not hidden subprocesses. |
| Storage | **Embedded SQLite** (one DB per workspace) | Zero external services; single-file portability; trivial backup; natural audit store. |
| Vector (v0.2) | **`sqlite-vec`** | Keeps memory in the same embedded store. |
| Agent↔tool boundary | **Model Context Protocol (MCP)** | Provider-neutral tool definitions; clean per-tool permission boundary; reusable community servers. |

Alternative considered: **Go + Wails** (faster solo velocity, simpler concurrency) — rejected
in favour of Rust's safety guarantees for the command-executing supervisor and Tauri's more
polished UI tooling.

---

## 3. Core architecture

```
┌─────────────────────────────────────────────────────────┐
│  Frontend (web): terminal grid, agent chat, phase board, │
│  notes (auto + manual), workspace switcher               │
└───────────────┬─────────────────────────────────────────┘
                │ IPC (events + commands)
┌───────────────▼─────────────────────────────────────────┐
│  Core supervisor (Rust)                                  │
│                                                          │
│  ┌──────────┐  ┌──────────────┐  ┌──────────────────┐    │
│  │ PTY mgr  │  │ Orchestrator │  │  Policy engine   │    │
│  │ (N ptys) │◄─┤ (agents,     │─►│  (scope, classify│    │
│  └──────────┘  │  tool router)│  │   approve, audit)│    │
│                └──────┬───────┘  └──────────────────┘    │
│                       │                                  │
│  ┌────────────────────▼──────────────┐  ┌─────────────┐  │
│  │ Memory: events + vector + profile │  │ LlmProvider │  │
│  │ (SQLite + sqlite-vec)             │  │ trait       │  │
│  └───────────────────────────────────┘  └─────────────┘  │
└──────────────────────────────────────────────────────────┘
```

**Golden rule:** the LLM never touches a PTY directly. Every agent action flows through the
policy engine. Nothing executes that the policy layer hasn't cleared.

---

## 4. The execution spine (load-bearing)

Every agent-proposed action funnels through one path:

```
Agent proposes tool call
        │
        ▼
┌─────────────────┐
│ Policy engine   │  1. Resolve targets — parse real argv for IPs/hosts/URLs
│                 │  2. Scope check  → in workspace allowlist? else BLOCK
│                 │  3. Classify     → READ_ONLY | MUTATING | EXPLOIT | UNKNOWN
│                 │  4. Decide       → AUTO_RUN | NEEDS_APPROVAL | DENY
└────────┬────────┘
         │
   ┌─────┴─────┐
   ▼           ▼
AUTO_RUN   NEEDS_APPROVAL ──► approval card (approve / edit / deny+feedback / always-allow)
   │           │
   └─────┬─────┘
         ▼
   PTY executor (spawn in tracked terminal, stream output)
         ▼
   Event log (append-only) ──► memory, notes, audit, phase board
```

### 4.1 Classification

- **Layered, not naive regex.** Fast allowlist/denylist for known-safe (`ping`, `whois`,
  plain `nmap -sV`) and known-dangerous patterns.
- **Fail closed.** Anything unmatched defaults to `NEEDS_APPROVAL`. Unknown = ask the human.
  Never fail open.
- **Never let the LLM self-classify its own risk.** Classification is a property of the
  supervisor, not the model.

### 4.2 Scope resolution

- Parse the **actual argv** for targets; never trust the agent's narration of what it will do.
- Per-tool argument parsers for the targets-bearing tools (v0.1 seed: nmap, curl, ping,
  whatweb, nslookup/dig). Unknown tools → `NEEDS_APPROVAL`.
- Targets checked against the workspace scope (CIDRs, hostnames, URL domains).

### 4.3 Approval card (a first-class UI surface)

Lives inline in the agent conversation (Claude Code style). Shows the exact command, resolved
targets, and classification. Actions:

- **Approve once**
- **Approve and edit** (tweak argv before running)
- **Deny + feedback** (the reason is fed back to the agent so it adapts instead of retrying)
- **Always allow** → writes a rule into the workspace policy

**"Always allow" granularity** is chosen on the card:
- this exact command, or
- this tool + flag shape against any in-scope target *(useful default)*, or
- this whole tool *(power-user, riskier)*

**"Always allow" rules are workspace-scoped**, never global by default — a rule trusted for a
lab must not silently carry into a client production engagement. Explicit "promote to global"
is offered separately.

---

## 5. Data model — event sourcing (hybrid)

**Append-only event log is the single source of truth; cached read-models give fast queries.**

```
workspace (id, name, root_path, scope_rules, current_phase, created_at)

event (id, workspace_id, phase, type, actor, author, payload, parent_id, ts)
   type ∈ {user_prompt, agent_msg, tool_proposed, tool_approved, tool_denied,
           tool_output, note, phase_change, finding}
   actor  = which agent/human produced it  (reserves multi-agent)
   author = user | agent                   (distinguishes manual vs auto notes)
   parent_id = delegation / causal tree     (reserves multi-agent)

terminal (id, workspace_id, title, pty_state, created_at)     ← cached read-model
finding  (id, workspace_id, severity, target, summary, evidence_event_ids)  ← cached read-model
```

Why event sourcing here (vs plain CRUD `UPDATE`-in-place):

- A pentest **is** a timeline of actions — the log matches the domain exactly.
- **Audit trail is free and tamper-evident** — append-only is what compliance/legal wants.
- Notes, memory, phases, and audit all become **projections of one log**, instead of four
  tables that drift out of sync.
- Replay / "what did we know at time T" come for free.

Cost: querying current state means folding the log — mitigated by the cached read-models.
This hybrid is the pragmatic, recommended form.

---

## 6. Memory

Three layers, each a different read over the same event log.

```
RAW event log  ──►  SEMANTIC vector index  ──►  DERIVED profile + summaries
(everything)        (recall: "what do          (habits: "how this user works",
                     we know about X?")          "state of this engagement")
        └───────────────────┴──────────────────────────┘
                            ▼
              Context Assembler (budgets the prompt)
                            ▼
                          Agent
```

### 6.1 Semantic recall (v0.2)

- Embed only the **meaningful** events (findings, notes, agent conclusions, prompts) — never
  raw tool output (kept in the log, retrievable by reference). Keeps the index sharp.
- Embeddings are **user-configurable per workspace**: local (sensitive data stays on-box) or
  cloud (higher quality). Chat LLM remains cloud regardless.

### 6.2 Habits & profile

- **Periodic distillation**: a background job summarizes recent events into durable facts,
  written back as `note`-type events flagged `profile`.
- **Two scopes**: *per-workspace* facts (this engagement's state — must not leak between
  engagements) vs *global* facts (enduring habits — follow the user across workspaces).
- **Profile is small and always-injected** (~few hundred tokens) — this is what makes the
  agent feel like it "knows you" from message one.
- **Inferred + user-editable**: facts are auto-extracted, but the user can see, pin, and
  delete them. A wrong inferred habit is worse than none.

### 6.3 Context Assembler — where token bloat is won or lost

```
System prompt        → phase-specific instructions      (~fixed)
Profile facts        → always injected, compact          (~300 tok)
Retrieved recall     → top-K relevant past events        (budgeted, e.g. 1500 tok)
Recent transcript    → last few turns verbatim           (budgeted)
Current tool outputs → truncated/summarized if huge      (hard cap)
─────────────────────────────────────────────────────────
                       enforced per-turn ceiling
```

- **Hard token ceiling per turn**, enforced — not hoped for.
- **Tool output summarized on ingest**, not at read time: a 5000-line nmap scan becomes a
  structured finding ("22/80/443 open, Apache 2.4.29") when it happens; raw stays addressable
  by reference.
- **Cost meter visible** per agent/workspace (feeds multi-agent budget caps later).

---

## 7. Agents & orchestration

### 7.1 Provider abstraction (define now, implement once)

The orchestrator speaks only **internal, provider-neutral types** behind a trait:

```rust
trait LlmProvider {
    async fn run_turn(
        &self,
        messages: &[Message],   // internal types, not SDK types
        tools: &[ToolSpec],     // provider-neutral (from MCP)
    ) -> Stream<AgentEvent>;    // internal event enum (same one the log needs)
}
```

- v0.1 implements **only `ClaudeProvider`**. OpenAI later = a new file `OpenAiProvider`, same
  trait — **no change to orchestrator, policy, memory, or UI.**
- The three things that actually differ between providers (tool-call format, streaming event
  shape, message/role structure) are confined to the adapter.
- MCP already neutralizes tool definitions; `AgentEvent` is the enum the event log needs
  anyway. So the abstraction is nearly free.
- **Rule: never import SDK types into core logic.**

### 7.2 Multi-agent (reserved for v0.2)

Supervisor (orchestrator) agent delegates to specialist sub-agents mapped to phases:

```
            Orchestrator agent  (plans, delegates, collects)
        ┌───────────┼────────────┐
     recon-agent  web-agent   exploit-agent
        └───────────┴────────────┘
                    ▼
   SAME execution pipeline + policy engine + event log
   (each sub-agent's actions tagged by `actor`, tree via `parent_id`)
```

- **Delegation is itself an MCP tool** — "spawn a sub-agent with this objective" goes through
  the same policy engine. Orchestration reuses the entire spine; no second system.
- Schema already reserves it (`actor`, `parent_id`). Building single-agent first means the
  orchestrator is later an additive change, not a rewrite.
- Deferred because the failure modes (agents looping, duplicated work, runaway cost, racing on
  a target) need guardrails (loop detection, per-sub-agent budget caps) best built on a solid
  single-agent spine.

---

## 8. UI / information architecture

One screen, five zones — each maps to a requirement.

```
┌──────────┬───────────────────────────────────────────────┐
│ WORKSPACE│  PHASE TIMELINE  recon ▸ hypothesis ▸ PoC ▸ …   │
│ RAIL     ├──────────────────────────┬────────────────────┤
│          │                          │  AGENT CHAT         │
│ agents   │   TERMINAL GRID          │  + inline approval  │
│ roster   │   (xterm.js, N PTYs)     │    card             │
│          │                          ├────────────────────┤
│          │                          │  NOTES              │
│          │                          │  (auto | notebook)  │
└──────────┴──────────────────────────┴────────────────────┘
```

- **Left rail — workspaces + agent roster.** Switching workspace swaps the entire context.
  The roster is where the multi-agent picture surfaces later; clicking an agent filters every
  panel to its activity (via `actor`).
- **Top — phase timeline.** Not just a label: the current phase drives which agent
  system-prompt/toolset is active and stamps every event. Clicking a past phase filters the
  workspace to that phase. Free, because phase is a field on events.
- **Center — terminal grid.** Splittable `xterm.js` panes, each a tracked PTY. The user can
  type here too. When an agent runs an approved command, the user **watches it happen in a
  real terminal** — transparency as a safety feature.
- **Right-top — agent chat + inline approval card**, color-coded by classification.
- **Right-bottom — notes**, two sibling surfaces (below).

### 8.1 Notes — two distinct concepts

- **Auto-notes** — *derived* from events, machine-written, ambient ("what happened").
- **Manual notebook** — *authored* by the user, deliberate ("what I'm thinking"): hypotheses,
  creds to try, threads to pull, reminders. Markdown.

Both are `note`-type events distinguished by `author`. Consequence: **the agent can read the
user's notebook**, so jotting "the /admin form looks custom, worth fuzzing" steers the agent
next turn. A global hotkey captures selected terminal output / agent message → notebook in one
keystroke. A note can be promoted to a `finding`.

### 8.2 Aesthetic

- **wazuh-threat-hunter** for data surfaces: dark desaturated slate base, high information
  density (12–13px), monospace where data lives (terminals, findings, IPs, ports), semantic
  status colors used sparingly and meaningfully (open/filtered/closed, safe/mutating/exploit).
- **Claude/OpenAI** for the conversation surface and approval cards: soft borders, generous
  line-height, smooth streaming.
- Keep the two languages in their lanes: dense+monospace for security data, calm+roomy for
  agent dialogue.
- Lowest-risk part of the project; polish **last**.

---

## 9. MVP scope (v0.1)

**Goal:** the smallest thing genuinely useful on a real engagement.

**The core loop v0.1 must deliver:**
> Open a workspace with a scope → talk to one Claude agent → it proposes commands →
> tiered approve/deny → commands run in real terminals → everything lands in the event log →
> the user sees notes + phase + history, and the agent remembers within the workspace.

### In scope
1. Tauri shell + five-zone layout (functional, not yet themed).
2. Workspaces — create/switch/list; dir + SQLite DB + scope definition each.
3. Terminal manager — spawn/close multiple PTYs, xterm.js, output → event log; usable as a
   plain multi-terminal even with no agent.
4. Execution spine + policy engine — classification (allowlist/denylist, fail-closed), scope
   resolution (argv parsing for nmap, curl, ping, whatweb, nslookup/dig), approval card with
   approve/edit/deny/always-allow (workspace-scoped).
5. One cloud agent — **Claude only**, single-agent multi-tool, tools via MCP, every call
   routed through the policy engine. Behind the `LlmProvider` trait.
6. Event log (hybrid) — append-only SQLite + cached read-models.
7. Notes — auto-notes (derived) + manual notebook (authored) + capture-to-notebook hotkey.
8. Phases — timeline bar, manual switching, phase stamped on events, phase filters the view;
   agent behavior per phase can start as just a different system prompt.

### Deferred (v0.2+)
- Multi-agent orchestration (reserved via `actor`, `parent_id`). *The biggest, rightest cut.*
- Local LLMs / Ollama (cloud-first).
- Semantic recall / vector memory + per-workspace embedding config + habit distillation.
  v0.1 uses recent-events + manual notes + keyword recall (enough for a single engagement).
- Full wazuh theme polish (design system, tokens, animations).
- msfconsole and other interactive/stateful tools (see §11).
- Report generation ("report" phase exists as a label only).
- Cost meter, budget caps (matter most once multi-agent burns tokens in parallel).

### Build order (dependency-driven, not a timeline)
1. Tauri skeleton + SQLite event log + workspace model — *the substrate.*
2. Terminal manager + xterm.js — *usable as a multi-terminal already.*
3. Policy engine + approval card — *the spine, before any agent can run anything.*
4. One Claude agent over MCP, routed through the policy engine — *now it's an assistant.*
5. Notes (auto + manual) + phase bar — *the QoL layer.*
6. Theme pass toward the wazuh aesthetic — *polish last.*

**Step 3 precedes step 4 deliberately: guardrails exist before the agent can run anything.**

---

## 10. Cross-cutting concerns

- **Scope is first-class.** Authorized engagements have defined scope; the workspace model
  enforces it so the agent cannot (by policy) act outside it without explicit override.
- **Audit.** The event table *is* the audit log — append-only, per-workspace, legally relevant.
- **Secrets & loot.** API keys plus harvested credentials/hashes flow through memory and notes.
  Decide encryption-at-rest and redaction-from-log policy before storing real engagement data.
- **Workspace isolation.** One dir + one DB + one scope + workspace-scoped policy rules per
  engagement keeps client data forensically and legally separated.

---

## 11. Open questions / to resolve before they bite

1. **Policy classification details.** Exact allowlist/denylist seed. How argv parsing handles
   messy cases: `curl … | bash`, nmap `--script` that writes files, chained/piped commands,
   shell metacharacters. Pipelines and subshells likely must default to `NEEDS_APPROVAL`.
2. **Interactive / stateful tools.** msfconsole, sqlmap prompts, anything with its own REPL
   break the one-shot "propose → run → capture output" model. Needs a different PTY
   interaction pattern (send-keystrokes, read-until-prompt, resource scripts). Deferred, but
   will bite — design the interaction model before adding the first interactive tool.
3. **Secrets at rest.** Encryption and log-redaction policy (see §10).
4. **Embedding model selection UX** (v0.2) — how per-workspace local/cloud embedding choice is
   surfaced and defaulted.
5. **Multi-agent guardrails** (v0.2) — loop detection, per-sub-agent budget caps, target-race
   prevention.

---

## 12. Decision log (the "why", condensed)

| Decision | Chosen | Rationale |
|---|---|---|
| Core stack | Tauri (Rust) | Safety for the command-executing supervisor; polished UI tooling. |
| Exec model | Tiered approval | Auto-run read-only recon; human-approve mutating/exploit/unknown. |
| Classification default | Fail closed | Unknown → ask the human; never let the LLM self-classify. |
| "Always allow" scope | Workspace-level | Prevents trusted lab rules leaking into client engagements. |
| Data model | Event sourcing (hybrid) | Timeline-shaped domain; free audit/replay; notes/memory/phases as projections. |
| Memory | 3 layers, recall deferred | Recent+notes+keyword enough for v0.1; vector/profile in v0.2. |
| Embeddings | User-configurable per workspace | Sensitive engagements stay on-box; low-sensitivity can use cloud. |
| Habit learning | Inferred + user-editable | Magic with control; wrong inferences correctable. |
| First agent | Claude only, behind trait | Best MCP/tool-use; trait prevents future redesign for OpenAI. |
| Multi-agent | v0.2, reserved now | Schema seats it (`actor`/`parent_id`); needs guardrails on a solid single-agent spine. |
| Local LLM | v0.2 | Cloud-first for fastest path to a usable tool. |
