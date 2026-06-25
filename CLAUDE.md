# CLAUDE.md

Guidance for Claude Code (and contributors) working in this repo. Read this first; it captures
what isn't obvious from a single file. For the *why*, see [`DESIGN.md`](./DESIGN.md); for the
structural map, [`SKELETON.md`](./SKELETON.md).

## What this is

**Tiān Jī** — a Tauri 2 desktop app: an LLM-orchestrated pentesting framework. A pentester talks to
a cloud (DeepSeek, Anthropic) or local (Ollama) agent that proposes shell commands; every proposal
is routed through a pure policy engine and a tiered approval gate before it can touch a real
terminal. Everything lands in an append-only per-workspace SQLite event log.

Rust core (a Cargo workspace of small library crates + a thin Tauri binary) + a Vite/React/Tailwind
frontend.

## Build / test / run

```bash
npm install                   # once
npm run tauri dev             # hot-reload desktop app
npm run tauri build           # production bundle → src-tauri/target/release/bundle/

# Verify without a desktop:
npx tsc --noEmit              # frontend typecheck
npm run build                 # frontend bundle (tsc && vite build)
cargo test --workspace        # Rust unit tests (76 tests)
cargo check --workspace       # whole-workspace typecheck
```

The policy crate is pure and exhaustively unit-tested — run `cargo test -p tianji-policy` after any
classification/scope change.

## Architecture (crate map)

Dependencies point inward toward `tianji-types`; nothing depends on `src-tauri`; no cycles.

| Crate | Responsibility |
|---|---|
| `tianji-types` | Shared domain types (leaf). Provider-neutral LLM types live here — **never import SDK types into core logic.** |
| `tianji-policy` | **The safety spine.** Pure, no I/O: `resolve_targets` → `classify` → `decide`. Fails closed (unknown ⇒ `NeedsApproval`). |
| `tianji-store` | Append-only SQLite event log + cached read-models (findings, terminals, conversations, facts). One DB per workspace + a global `AppStore`. |
| `tianji-pty` | PTY manager (portable-pty). |
| `tianji-llm` | `LlmProvider` trait + the `ClaudeProvider` (Anthropic SSE + prompt caching), `OllamaProvider` (local), and `DeepSeekProvider` (OpenAI-compatible, legacy). The **only** place SDK/wire types are allowed. |
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
5. Persist new messages into the per-session history (throttled to every N turns; the workspace DB
   gets a full save on compaction/pause/stop so it survives restart).

**Modes:** default (approve each mutating command) · `autonomous` (auto-approve in-scope) · `free`
(bypass all policy — lab only). `run_goal` drives an autonomous multi-cycle loop toward an objective
until a `[[GOAL_COMPLETE]]` / `[[GOAL_BLOCKED]]` sentinel, the 15-iteration cap, the ~600k-token
ceiling, or operator Stop.

**Tools** (in-process MCP, `mcp.rs`): `run_command`, `record_finding`, `log_attempt`, `recall`,
`use_skill`, and `delegate_to_agent` (orchestrator only — sub-agents can't re-delegate).

## Provider architecture

### DeepSeek via Anthropic endpoint (primary path)

DeepSeek models (`deepseek-v4-pro`, `deepseek-v4-flash`, etc.) are routed through
`ClaudeProvider` pointed at `https://api.deepseek.com/anthropic` — DeepSeek's native
**Anthropic Messages API** endpoint. This means:

- The two-block system prompt, Anthropic-format tools, and SSE streaming all work without translation
- `ClaudeProvider` auto-detects DeepSeek via `base_url.contains("deepseek")` and returns
  `provider_id() = "deepseek"` to the orchestrator
- The orchestrator uses this to switch prompt style: DeepSeek gets "REASON OUT LOUD" + "USE
  SUB-AGENTS", Claude gets the original "BE TERSE" + "WORK INLINE" paragraphs
- The old `DeepSeekProvider` (OpenAI format) is retained but no longer the default path

### `LlmProvider::provider_id()`

Returns `"claude"`, `"deepseek"`, `"ollama"`, or `"generic"`. The orchestrator passes this to
`stable_system_prompt()` which gates the reasoning and delegation paragraphs per provider.
Claude behavior is completely unchanged.

### SSE stability

Both SSE parsers (`claude.rs`, `deepseek.rs`) break immediately on `message_stop`/`[DONE]` rather
than waiting for the HTTP stream to close — this prevents hangs when the server uses HTTP keep-alive.
A 300s request timeout provides a safety net.

## Token economy

- **Prompt caching** (`claude.rs::system_blocks`): one `cache_control` breakpoint after the stable
  system block. DeepSeek ignores `cache_control` (server-side caching instead).
- **Context budget** (`state.rs`): default 16k tokens/turn. Compaction triggers at 75%, keeps 40%.
- **Output handling** (`assembler.rs`, `summary.rs`): tool output capped at 6000 chars (non-slim);
  raw stays in the log, retrievable by `recall`. Trimming keeps a contiguous user-anchored suffix.
- **Memory cap** (`lib.rs`): `MAX_STORED_MESSAGES = 300` — oldest messages drained when exceeded.
- **Persistence throttle**: conversation saved to SQLite every 8 turns (not every turn) to avoid
  per-turn 9MB JSON serialization. Full save on compaction/pause/stop.
- **Dedup** (`run_cached`): identical read-only commands per session reuse prior output.
- **RTK** (`runner.rs`): optional `rtk <tool>` wrapping; no-op if absent.
- **Sub-agents** run on a cheaper model with a focused token budget.
- **GPU stability** (`main.rs`): `WEBKIT_DISABLE_COMPOSITING_MODE=1` on Linux to prevent
  WebKitGTK GPU compositing crashes.

## Conventions

- Library crates use concrete `thiserror` enums; `src-tauri` is the only place errors flatten to a
  serializable `AppError` for IPC. No `anyhow` inside library crates.
- `tracing` is diagnostics only — **the event log is the audit trail.** Keep them distinct.
- Secrets (API key, sudo password, OAuth tokens) live in the OS keychain via `secrets.rs` /
  `oauth.rs`, never on disk in plaintext.
- Match surrounding style: dense, well-commented Rust with rationale comments on load-bearing
  decisions.
- Storage is `rusqlite` accessed synchronously (fast/local for v0.1).

## System prompt structure

The prompt was rewritten with a Numasec-inspired structure:

- **Persona**: "Senior penetration tester on an authorized assignment"
- **Sections**: RULES OF ENGAGEMENT, TOOLS, ATTEMPT TRACKING, FINDINGS, SKILLS-FIRST,
  ANTI-PATTERNS, OPERATOR PRIORITY, METHODOLOGY
- **Anti-patterns**: scan-and-report, stopping at injectable, repeating dead commands 5+ times,
  ignoring workspace files, fixating on reverse shells when direct flag reading exists
- **Operator priority**: operator messages override methodology — execute direct instructions
  immediately
- **Provider-gated**: DeepSeek sees "REASON OUT LOUD" + "USE SUB-AGENTS"; Claude sees
  "POSTURE: Do, then narrate" + "DELEGATION: spawn specialists"

## Skills

Skill bodies are sanitized via `sanitize_for_model()`: "Claude Code" references replaced with
"this application", `/ctf-*` slash commands rewritten to `use_skill()` calls. Loaded routers
get a "HOW TO USE THIS SKILL" header with numbered steps for the two-level disclosure pattern.

## Known issues

See [`current_issues.md`](./current_issues.md).
