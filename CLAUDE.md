# CLAUDE.md

Guidance for Claude Code (and contributors) working in this repo. Read this first; it captures
what isn't obvious from a single file. For the *why*, see [`DESIGN.md`](./DESIGN.md); for the
structural map, [`SKELETON.md`](./SKELETON.md).

## What this is

**Tiān Jī** — a Tauri 2 desktop app: an LLM-orchestrated pentesting framework. A pentester talks to
a Claude (or local Ollama) agent that proposes shell commands; every proposal is routed through a
pure policy engine and a tiered approval gate before it can touch a real terminal. Everything lands
in an append-only per-workspace SQLite event log.

Rust core (a Cargo workspace of small library crates + a thin Tauri binary) + a Vite/React/Tailwind
frontend. **Primary dev OS here is Windows** (PowerShell); the runner and prompts are
cross-platform.

## Build / test / run

```bash
npm install                   # once
npm run tauri dev             # hot-reload desktop app
npm run tauri build           # production bundle → src-tauri/target/release/bundle/

# Verify without a desktop:
npx tsc --noEmit              # frontend typecheck
npm run build                 # frontend bundle (tsc && vite build)
cargo test --workspace        # Rust unit tests (policy, assembler, orchestrator, …)
cargo check --workspace       # whole-workspace typecheck
```

The policy crate is pure and exhaustively unit-tested — run `cargo test -p tianji-policy` after any
classification/scope change. Orchestrator behavior (loop guard, budget halt, goal sentinels,
compaction split) is tested in `crates/tianji-agent/src/lib.rs` with a scripted provider + stub
runner — extend those rather than reaching for a live model.

## Architecture (crate map)

Dependencies point inward toward `tianji-types`; nothing depends on `src-tauri`; no cycles.

| Crate | Responsibility |
|---|---|
| `tianji-types` | Shared domain types (leaf). Provider-neutral LLM types live here — **never import SDK types into core logic.** |
| `tianji-policy` | **The safety spine.** Pure, no I/O: `resolve_targets` → `classify` → `decide`. Fails closed (unknown ⇒ `NeedsApproval`). |
| `tianji-store` | Append-only SQLite event log + cached read-models (findings, terminals, conversations, facts). One DB per workspace + a global `AppStore`. |
| `tianji-pty` | PTY manager (portable-pty). |
| `tianji-llm` | `LlmProvider` trait + the `ClaudeProvider` (SSE) and `OllamaProvider` adapters. The **only** place SDK/wire types are allowed. |
| `tianji-agent` | The orchestrator loop, in-process MCP host, context assembler, approval gate, command runner, skills, summarizer. |
| `src-tauri` | IPC glue only: commands (`src/commands/`), event emitters (`events.rs`), `AppState`/`CurrentWorkspace` wiring (`state.rs`), keychain (`secrets.rs`), subscription OAuth (`oauth.rs`). |

**Golden rule:** the LLM never touches a PTY directly — every agent action flows through
`tianji-policy`. Step 3 (policy) precedes step 4 (agent) by design.

## The turn loop (`crates/tianji-agent/src/lib.rs`)

`Orchestrator::run_prompt_inner` is the heart. One prompt-cycle:

1. Load scope/rules/phase/notes/attempts/findings from the store; detect machine vs CTF engagement
   (`is_machine_engagement` = any CIDR in scope).
2. Rebuild the system prompt fresh each turn as **two blocks**: `stable_system_prompt` (cached) +
   `volatile_context` (uncached — scope, notebook, profile, attempt log, findings).
3. `maybe_compact` rolls old turns into a summary if history > 75% of `context_budget`.
4. Up to `MAX_ROUNDS` (8) model rounds: assemble → trim to budget → `provider.run_turn` → for each
   tool call route through policy → execute / park for approval / deny → feed results back.
5. Persist new messages into the per-session history (and the workspace DB so it survives restart).

**Modes:** default (approve each mutating command) · `autonomous` (auto-approve in-scope) · `free`
(bypass all policy — lab only). `run_goal` drives an autonomous multi-cycle loop toward an objective
until a `[[GOAL_COMPLETE]]` / `[[GOAL_BLOCKED]]` sentinel, the 15-iteration cap, the ~600k-token
ceiling, or operator Stop.

**Tools** (in-process MCP, `mcp.rs`): `run_command`, `record_finding`, `log_attempt`, `recall`,
`use_skill`, and `delegate_to_agent` (orchestrator only — sub-agents can't re-delegate).

## Token economy — where cost is won or lost

This is the project's recurring concern; touch it carefully and keep the tests green.

- **Prompt caching** (`claude.rs::system_blocks`): one `cache_control` breakpoint after the stable
  system block, so tools+identity+stable-instructions are a cache *read* every later turn. The
  volatile block stays after the breakpoint. **Do not move volatile content into the cached prefix**
  — that was the old ~10k-token-per-turn floor.
- **Context budget** (`state.rs`): cloud = 12k tokens/turn; local = `num_ctx − 2k`. Compaction
  triggers at 75%, keeps 40%.
- **Output handling** (`assembler.rs`, `summary.rs`): tool output capped (`tool_output_cap`) and
  summarized on ingest; raw stays in the log, retrievable by `recall`. Trimming keeps a contiguous
  user-anchored suffix so no `tool_result` is orphaned (Anthropic rejects dangling pairs).
- **Dedup** (`run_cached`): identical read-only commands per session reuse prior output.
- **RTK** (`runner.rs`): optional `rtk <tool>` wrapping for a curated read-only set; no-op if absent.
- **Sub-agents** run on a cheaper model (`subagent_model_for` — Opus→Sonnet) with a focused token
  budget.
- **Cost accounting:** `tokens_spent` sums Anthropic's `input_tokens + output_tokens`, which
  **excludes cache reads** — so the meter undercounts true wire volume. Keep this in mind when
  reasoning about the 600k goal ceiling vs. real billed tokens.

### Order-of-magnitude cost (for reference)

An autonomous HTB box (user + root) is ~40–85 model rounds. Steady-state per round ≈ a ~3.5k cached
prefix (read), ~8k fresh input (volatile + trimmed history), ~0.5k output → ~250k–600k **metered**
tokens for a solve (≈ $2–6 on `claude-opus-4-8` at $5/$25 per MTok, cache reads $0.50/MTok). True
wire volume incl. cache reads is ~1.4–1.6×. The goal loop self-stops at the ~600k metered ceiling.

## Conventions

- Library crates use concrete `thiserror` enums; `src-tauri` is the only place errors flatten to a
  serializable `AppError` for IPC. No `anyhow` inside library crates.
- `tracing` is diagnostics only — **the event log is the audit trail.** Keep them distinct.
- Secrets (API key, sudo password, OAuth tokens) live in the OS keychain via `secrets.rs` /
  `oauth.rs`, never on disk in plaintext.
- Match surrounding style: the codebase favors dense, well-commented Rust with rationale comments on
  load-bearing decisions. Mirror that.
- Storage is `rusqlite` accessed synchronously (fast/local for v0.1); a `spawn_blocking` boundary is
  a later optimization.

## Known issues / active work

See [`current_issues.md`](./current_issues.md). Recurring themes: duplicated scrollbars; per-chat
memory isolation; macOS keyring re-prompts and built-app TERM/PTY lag; making the agent trace its
attempts so it stops re-trying dead ends (the `log_attempt` tool + attempt log address this);
further token reduction; and making `record_finding` capture milestones (flags, shells) rather than
flooding the report with enumeration noise.
