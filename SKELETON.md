# Tiān Jī — Project Skeleton

> Structural map derived from `DESIGN.md`. Directory layout, per-module responsibilities,
> key type/trait signatures, dependency direction, and the IPC contract. **No implementation
> yet** — this is the scaffold you fill in.

Status: **structure proposed, pre-scaffold.**
Last updated: 2026-06-21

---

## 1. Guiding structural principles

1. **A Cargo workspace of small library crates + one thin Tauri binary.** Not one mega-crate.
2. **The policy engine is pure and I/O-free** (`tianji-policy`) so it can be exhaustively
   unit-tested in isolation. Crate boundaries enforce this — the compiler won't let it reach
   the DB or network.
3. **Dependencies point inward toward types.** `tianji-types` is the leaf everyone depends on;
   no crate depends on `src-tauri`; no cycles.
4. **`src-tauri` is glue, not logic.** Commands/events marshal between the frontend and the
   library crates; the real work lives in the crates so it stays testable and Tauri-agnostic.
5. **No SDK types in core logic** (DESIGN.md §7.1) — Anthropic types live only inside
   `tianji-llm`'s Claude adapter.

---

## 2. Top-level layout

```
Tiān Jī/
├─ DESIGN.md                 architecture + rationale
├─ SKELETON.md               this file
├─ Cargo.toml                workspace manifest
├─ rust-toolchain.toml
│
├─ crates/                   the Rust core (Tauri-agnostic, independently testable)
│  ├─ tianji-types/          shared domain types — the leaf crate
│  ├─ tianji-policy/         policy engine: classify · scope · decide  (PURE, no I/O)
│  ├─ tianji-store/          event log (append-only) + read-model projections  (SQLite)
│  ├─ tianji-pty/            terminal/PTY manager (portable-pty)
│  ├─ tianji-llm/            LlmProvider trait + Claude adapter
│  └─ tianji-agent/          orchestrator · MCP host · context assembler · memory(v0.1)
│
├─ src-tauri/                the desktop binary — wiring only
│  ├─ Cargo.toml
│  ├─ tauri.conf.json
│  ├─ build.rs
│  └─ src/
│     ├─ main.rs             app entry; build AppState; register commands/events
│     ├─ state.rs            AppState: handles to each crate's service
│     ├─ commands/           #[tauri::command] fns (the IPC inbound surface)
│     │  ├─ mod.rs
│     │  ├─ workspace.rs
│     │  ├─ terminal.rs
│     │  ├─ agent.rs
│     │  ├─ policy.rs        approve/deny/always-allow handlers
│     │  └─ notes.rs
│     └─ events.rs           outbound event emitters (Rust → frontend)
│
└─ src/                      the web frontend (Vite + React/Svelte + Tailwind)
   ├─ index.html
   ├─ main.tsx
   ├─ App.tsx                the five-zone AppShell
   ├─ lib/
   │  ├─ ipc.ts              typed invoke() wrappers (mirror of commands/)
   │  ├─ events.ts           typed listen() subscriptions (mirror of events.rs)
   │  └─ types.ts            TS mirror of tianji-types (kept in sync; see §6)
   ├─ state/                 client stores (workspace, terminals, agent, phase, notes)
   ├─ components/
   │  ├─ layout/             AppShell · WorkspaceRail · PhaseTimeline
   │  ├─ workspace/          WorkspaceSwitcher · CreateWorkspace · ScopeEditor
   │  ├─ terminals/          TerminalGrid · TerminalPane (xterm.js)
   │  ├─ agent/              AgentChat · MessageList · ApprovalCard
   │  └─ notes/              AutoNotesFeed · Notebook
   └─ theme/                 design tokens + tailwind config (wazuh aesthetic, polished last)
```

---

## 3. Rust crates — responsibilities & key signatures

Signatures below are **shape, not final** — illustrative of each crate's surface.

### 3.1 `tianji-types` — the leaf

Shared domain types. No logic, minimal deps (`serde`, `uuid`, `time`). Everyone depends on it;
it depends on nothing internal.

```rust
pub struct WorkspaceId(Uuid);
pub struct EventId(Uuid);
pub struct AgentId(String);

pub enum Phase { Recon, Hypothesis, Poc, Exploit, Report }

pub struct Event {
    pub id: EventId,
    pub workspace_id: WorkspaceId,
    pub phase: Phase,
    pub kind: EventKind,
    pub actor: AgentId,          // which agent/human — reserves multi-agent
    pub author: Author,          // User | Agent — manual vs auto notes
    pub parent_id: Option<EventId>, // causal/delegation tree — reserves multi-agent
    pub payload: serde_json::Value,
    pub ts: OffsetDateTime,
}

pub enum EventKind {
    UserPrompt, AgentMsg, ToolProposed, ToolApproved, ToolDenied,
    ToolOutput, Note, PhaseChange, Finding,
}
pub enum Author { User, Agent }

// provider-neutral LLM types (DESIGN.md §7.1) — NO SDK types ever
pub struct Message { /* role, content, tool calls/results */ }
pub struct ToolSpec { /* name, description, json schema — sourced from MCP */ }
pub enum AgentEvent { TextDelta(String), ToolCall(ToolCall), TurnEnd, /* … */ }

pub struct ScopeRules { /* CIDRs, hostnames, URL domains */ }
pub struct Classification { /* ReadOnly | Mutating | Exploit | Unknown */ }
pub enum Decision { AutoRun, NeedsApproval, Deny(String) }
```

### 3.2 `tianji-policy` — the spine (PURE, exhaustively tested)

The load-bearing safety crate. **No I/O, no async, no DB, no network** — input types in,
`Decision` out. This is what makes the guardrails verifiable.

Depends on: `tianji-types`.

```rust
/// Parse the REAL argv for targets — never trust agent narration.
pub fn resolve_targets(tool: &str, argv: &[String]) -> Vec<Target>;

/// Layered allowlist/denylist; unknown → Unknown (caller fails closed).
pub fn classify(tool: &str, argv: &[String]) -> Classification;

/// scope-check → classify → decide. Fail closed: Unknown ⇒ NeedsApproval.
pub fn decide(
    tool: &str,
    argv: &[String],
    scope: &ScopeRules,
    rules: &[AllowRule],      // workspace-scoped "always allow"
) -> Decision;

pub struct AllowRule { /* exact | tool+flag-shape | whole-tool ; workspace-scoped */ }
```

Test focus: pipes/subshells/chained commands default to `NeedsApproval`; out-of-scope targets
always `Deny`; `always-allow` rules match the intended granularity and nothing broader.

### 3.3 `tianji-store` — event log + read-models

Append-only SQLite (one DB per workspace) + cached projection tables (`terminal`, `finding`).
The single source of truth.

Depends on: `tianji-types`. (`sqlx` or `rusqlite`.)

```rust
pub struct Store { /* connection pool, per workspace */ }

impl Store {
    pub async fn open(workspace_root: &Path) -> Result<Store>;
    pub async fn append(&self, event: Event) -> Result<EventId>;      // never UPDATE
    pub async fn events_since(&self, cursor: EventId) -> Result<Vec<Event>>;
    pub async fn events_in_phase(&self, p: Phase) -> Result<Vec<Event>>;
    pub async fn keyword_recall(&self, q: &str, k: usize) -> Result<Vec<Event>>; // v0.1 recall
    // read-models (projections kept current from the log)
    pub async fn findings(&self) -> Result<Vec<Finding>>;
    pub async fn terminals(&self) -> Result<Vec<TerminalRow>>;
}
```

> v0.2 adds `tianji-store` ⟶ `sqlite-vec` semantic recall + a `tianji-memory` crate for
> profile distillation. Reserved, not built.

### 3.4 `tianji-pty` — terminal manager

Spawn/track/close PTYs; stream output. Output lines are emitted as `ToolOutput` events.

Depends on: `tianji-types`. (`portable-pty`.)

```rust
pub struct PtyManager { /* map<TerminalId, PtyHandle> */ }

impl PtyManager {
    pub fn spawn(&self, title: &str) -> Result<TerminalId>;
    pub fn write(&self, id: TerminalId, bytes: &[u8]) -> Result<()>;  // user keystrokes
    pub fn run(&self, id: TerminalId, argv: &[String]) -> Result<()>; // approved command
    pub fn subscribe(&self, id: TerminalId) -> Receiver<PtyChunk>;    // stream → frontend + log
    pub fn close(&self, id: TerminalId) -> Result<()>;
}
```

### 3.5 `tianji-llm` — provider abstraction

The `LlmProvider` trait + the **only** place SDK types are allowed (inside the Claude adapter).

Depends on: `tianji-types`. (`reqwest`, Anthropic SDK or raw HTTP — confined here.)

```rust
#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn run_turn(
        &self,
        messages: &[Message],
        tools: &[ToolSpec],
    ) -> Result<BoxStream<'static, AgentEvent>>;
}

pub struct ClaudeProvider { /* api key, model id, http client */ }
impl LlmProvider for ClaudeProvider { /* translate ⇄ Anthropic format */ }

// v0.2: pub struct OpenAiProvider; pub struct LocalProvider; — same trait, new files.
```

### 3.6 `tianji-agent` — the runtime that ties it together

The orchestrator loop + MCP host (tools) + context assembler + v0.1 memory. This is where a
turn actually happens: assemble context → `LlmProvider::run_turn` → for each proposed tool
call, route through `tianji-policy` → (auto-run | request approval) → execute via `tianji-pty`
→ append events via `tianji-store` → repeat.

Depends on: `tianji-types`, `tianji-policy`, `tianji-store`, `tianji-pty`, `tianji-llm`.

```rust
pub struct Orchestrator {
    provider: Arc<dyn LlmProvider>,
    store: Store,
    pty: PtyManager,
    mcp: McpHost,
}

impl Orchestrator {
    pub async fn handle_prompt(&self, ws: WorkspaceId, prompt: &str) -> Result<()>;
    // emits AgentEvents/approval requests outward via a channel the Tauri layer forwards
}

pub struct ContextAssembler { /* enforces the per-turn token ceiling (DESIGN.md §6.3) */ }
pub struct McpHost { /* registers tools as MCP; every call goes through tianji-policy */ }

// pending-approval registry: agent turn parks here until the UI resolves the card
pub struct ApprovalGate {
    pub fn request(&self, proposed: ProposedCall) -> ApprovalToken;
    pub fn resolve(&self, token: ApprovalToken, outcome: ApprovalOutcome);
}
pub enum ApprovalOutcome { ApproveOnce, ApproveEdited(Vec<String>), Deny(String), AlwaysAllow(AllowRule) }
```

> Why a single `tianji-agent` crate in v0.1: memory is just keyword recall + the context
> assembler. When v0.2 adds vector recall + profile distillation, split out `tianji-memory`.

---

## 4. Dependency graph (no cycles, points inward)

```
                    tianji-types  ◄──────────────┐ (leaf; everyone depends on it)
                        ▲  ▲   ▲   ▲              │
        ┌───────────────┘   │   │   └───────────┐   │
   tianji-policy     tianji-store  tianji-pty   tianji-llm
        ▲               ▲             ▲            ▲
        └────────────────┴──────┬──────┴─────────────┘
                          tianji-agent
                                ▲
                            src-tauri  (binary; wires all, depended-on by none)
                                ▲
                              src/   (frontend, via IPC only)
```

`tianji-policy` depends on **types only** — that's deliberate; it keeps the safety logic pure.

---

## 5. The IPC contract (the front/back seam)

`src-tauri` exposes a small, typed surface. Commands are frontend→Rust requests; events are
Rust→frontend pushes. Keep this contract small and explicit — it's the only coupling between
the two halves.

### Commands (`#[tauri::command]`, in `src-tauri/src/commands/`)

| Command | Purpose |
|---|---|
| `workspace_list / create / open / close` | manage engagements (dir + DB + scope) |
| `workspace_set_phase` | move the phase pointer; emits a `PhaseChange` event |
| `terminal_spawn / close / write` | PTY lifecycle + user keystrokes |
| `agent_prompt` | send a user prompt to the orchestrator |
| `policy_resolve` | approve / edit / deny / always-allow a parked tool call |
| `policy_rules_list / promote_global` | view/manage allow-rules |
| `notes_add / promote_to_finding` | manual notebook authoring |
| `events_query` | history/phase-filtered reads (projection of the log) |

### Events (emitted from `src-tauri/src/events.rs`)

| Event | Payload |
|---|---|
| `pty://output` | `{ terminal_id, chunk }` — streamed to the matching xterm pane |
| `agent://delta` | `AgentEvent` (text/tool-call deltas) for the chat surface |
| `agent://approval_request` | the proposed call → renders an `ApprovalCard` |
| `notes://updated` | auto-notes feed refresh |
| `event://appended` | new event for any live projection (phase board, findings) |

---

## 6. Type sync between Rust and TS

`src/lib/types.ts` mirrors `tianji-types`. Two ways to keep them honest — pick one at scaffold
time:

- **`ts-rs`** — derive TS types from the Rust structs at build time (recommended; single
  source of truth in Rust).
- Hand-maintained `types.ts` — simpler to start, drifts over time.

Recommendation: `ts-rs`, so the IPC contract can't silently diverge.

---

## 7. Mapping to the DESIGN.md build order

| Build step (DESIGN.md §9) | Crates/dirs that come alive |
|---|---|
| 1. Substrate | `tianji-types`, `tianji-store`, `workspace` command/UI |
| 2. Terminals | `tianji-pty`, `terminals/` components, `pty://output` |
| 3. Policy spine | `tianji-policy`, `ApprovalGate`, `ApprovalCard`, `policy_resolve` |
| 4. Claude agent | `tianji-llm` (Claude), `tianji-agent` (orchestrator + MCP host) |
| 5. Notes + phases | `notes/`, `PhaseTimeline`, `notes_*` commands |
| 6. Theme pass | `theme/` (wazuh aesthetic) — last |

Note the order respects the safety invariant: `tianji-policy` (step 3) is wired **before**
`tianji-agent` can run any tool (step 4).

---

## 8. External crate shortlist (to pin at scaffold time)

- Tauri 2.x, `serde`, `serde_json`, `uuid`, `time`, `thiserror`, `tracing`
- `tokio` (async runtime), `async-trait`, `futures`
- `sqlx` *or* `rusqlite` (storage) — decide on async vs sync ergonomics
- `portable-pty` (terminals)
- `reqwest` (LLM HTTP), MCP client lib (or thin custom client)
- `ts-rs` (type sync)
- Frontend: `vite`, React or Svelte, `tailwindcss`, `xterm` + `xterm-addon-fit`, a small store
  (Zustand/Svelte stores)

---

## 9. Refinements (decided before scaffold)

Six things the layout above left implicit. Each affects file structure, so they're pinned now.

### 9.1 Global vs per-workspace state

There are **two** stores, both in `tianji-store`:

- **`AppStore`** (global, one per install — app data dir): the workspace registry (names +
  paths), global allow-rules, app settings, last-opened workspace. Knows *which* workspaces
  exist; holds nothing engagement-specific.
- **`WorkspaceStore`** (one per engagement — workspace dir): the append-only event log + read
  models + scope rules + workspace-scoped allow-rules.

This split is why "always allow" rules can be workspace-scoped by default with explicit
promotion to global (DESIGN.md §4.3): promotion = copy a rule from a `WorkspaceStore` into the
`AppStore`.

### 9.2 Secrets — never in plaintext config

API keys go in the **OS keychain** (`keyring` crate → Windows Credential Manager), never in a
config file or either DB. This is the first concrete resolution of the secrets open question
(DESIGN.md §11.3) — for *API keys*. Harvested loot/creds-at-rest in the event log is still
open and tracked separately. A tiny `secrets` module in `src-tauri` wraps `keyring`.

### 9.3 The ApprovalGate concurrency model

The tricky bit: an async agent turn must *pause* mid-flight waiting for a human click, without
blocking anything else. Mechanism:

1. Orchestrator hits `Decision::NeedsApproval` → mints an `ApprovalToken`, creates a
   `oneshot` channel, stores `token → Sender` in a map, emits `agent://approval_request`.
2. The turn `.await`s the `oneshot::Receiver` — parked, not blocking the runtime.
3. UI resolves the card → `policy_resolve` command → looks up the token → sends the
   `ApprovalOutcome` on the channel → the parked turn wakes and continues (run / edit-run /
   abort / write allow-rule).

So `ApprovalGate` owns `Map<ApprovalToken, oneshot::Sender<ApprovalOutcome>>`. Tokens expire
(timeout → treated as deny) so a forgotten card can't park a turn forever.

### 9.4 MCP scope in v0.1 — in-process only

`McpHost` exposes tools through MCP-shaped schemas, but in v0.1 the **only** tool is an
in-process `run_command` (and a couple of read helpers). **No external subprocess MCP servers
yet** — that's deferred. This keeps the provider-neutral tool boundary (so adapters and future
external servers slot in unchanged) without shipping a process-management layer in v0.1.

### 9.5 Error handling

- Each crate defines a concrete `thiserror` enum (`PolicyError`, `StoreError`, …) and returns
  `Result<T, ThatError>`. No `anyhow` inside library crates — callers must see real variants.
- `src-tauri` is the only place that flattens to `anyhow`/a serializable `AppError` for IPC.
- Tracing via `tracing` is **diagnostic only**; it is *not* the audit trail. The event log is
  the audit trail. Keep them distinct so debug verbosity never pollutes the legal record.

### 9.6 Storage library

**`rusqlite`**, accessed off the async executor via a dedicated DB task (or `spawn_blocking`).
Chosen over `sqlx` for: full control of the embedded file, simple migrations, and clean
loadable-extension support — which matters for the v0.2 `sqlite-vec` recall step. The async
boundary is a thin wrapper so call sites still `.await`.

---

## 10. Next step — scaffold

Generate the real files from this map: workspace `Cargo.toml`, the six crate skeletons (each
with `Cargo.toml` + `lib.rs` carrying module stubs, the key types from §3, and doc-comments),
the `src-tauri` binary (config + state + command/event stubs), and the frontend
(Vite + Tailwind + the five-zone `AppShell`, `lib/ipc.ts`, `lib/events.ts`). Stubs compile but
do nothing yet — bodies get filled in build-order (§7).
