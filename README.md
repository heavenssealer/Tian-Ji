# Tiān Jī

An LLM-orchestrated pentesting framework. Multiple concurrent terminals, cloud/local agents
that drive system tooling under human-controlled guardrails, per-engagement workspaces,
persistent memory, and a phase-aware UI.

- Architecture & rationale: [`DESIGN.md`](./DESIGN.md)
- Crate/module map: [`SKELETON.md`](./SKELETON.md)

> Status: **v0.1 vertical slice — implemented & verified building.** The full loop is wired:
> workspaces → Claude agent → tiered-approval tool execution → event log → live xterm + streaming
> chat. `cargo test --workspace` (12 tests) and `cargo build -p tianji` pass; the frontend
> typechecks and builds. Not yet run: the live desktop app (`npm run tauri dev`). To use the
> agent, store an Anthropic API key in the OS keychain (service `dev.tianji.app`, account
> `anthropic`). Remaining hardening is tracked in `DESIGN.md` §11.

## Layout

```
crates/          Rust core (Tauri-agnostic, independently testable)
  tianji-types     shared domain types (leaf)
  tianji-policy    policy engine — PURE, no I/O (the safety spine)
  tianji-store     event log + read-models (rusqlite)
  tianji-pty       terminal/PTY manager (portable-pty)
  tianji-llm       LlmProvider trait + Claude adapter
  tianji-agent     orchestrator · MCP host · context assembler · approval gate
src-tauri/       desktop binary — IPC glue only
src/             web frontend (Vite + React + Tailwind), the five-zone shell
```

## Prerequisites

- **Node ≥ 20** (present) — frontend.
- **Rust toolchain** — not yet installed. Install via <https://rustup.rs>:
  ```sh
  rustup default stable
  ```
- **Tauri system deps** (WebView2 is preinstalled on Windows 11).
- **App icon** — `tauri.conf.json` references `src-tauri/icons/icon.png`; add one (or run
  `npm run tauri icon <path>`) before the first desktop build.

## Develop

```sh
npm install          # frontend deps (done)
npm run dev          # vite dev server (frontend only)
npm run tauri dev    # full desktop app (needs Rust toolchain)
```

## Verify

```sh
npx tsc --noEmit     # frontend typecheck  ✓
npm run build        # frontend bundle     ✓
cargo test -p tianji-policy   # the safety spine's unit tests (needs Rust)
cargo check --workspace       # whole Rust workspace (needs Rust)
```

## Safety posture

Every agent-proposed command is routed through `tianji-policy` before it can touch a terminal:
scope-check (real argv parsed for targets) → classify → tiered approval. Unknown commands fail
closed to human approval; the LLM never classifies its own risk. See `DESIGN.md` §4.
