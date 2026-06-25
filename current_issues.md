# Current issues

## Recently fixed

| Issue | Fix | File(s) |
|---|---|---|
| DeepSeek V4 Pro underperformance | Anthropic endpoint routing, provider-aware prompt, tool output cap 6000, SSE stability | `deepseek.rs`, `claude.rs`, `lib.rs` |
| "I'm Claude" identity hallucination | Identity prefix in DeepSeek adapter | `deepseek.rs` |
| No visible reasoning | "REASON OUT LOUD" prompt for DeepSeek | `lib.rs` |
| Never delegates to sub-agents | "USE SUB-AGENTS" prompt for DeepSeek | `lib.rs` |
| Skills ignored by DeepSeek | `sanitize_for_model()`, "HOW TO USE" header, two-level disclosure emphasis | `skills.rs`, `mcp.rs` |
| System crash (GPU lockup) | `WEBKIT_DISABLE_COMPOSITING_MODE=1` on Linux | `main.rs` |
| SSE stream hang → app freeze | Break on `message_stop`/`[DONE]`, 300s request timeout | `claude.rs`, `deepseek.rs` |
| Unbounded memory growth | `MAX_STORED_MESSAGES=300`, persistence throttle | `lib.rs` |
| Text rendering word-per-line | `.trim_end()` on SSE text_deltas | `claude.rs`, `deepseek.rs` |
| Tool output cap too aggressive (1400 chars) | Raised to 6000 chars | `lib.rs` |
| Agent fixates on reverse shells | Flag shortcut in ANTI-PATTERNS, operator priority rule | `lib.rs` |

## Remaining

1. **Duplicate scrollbars** — xterm and notes panel both show scrollbars, creating visual double-scroll
2. **Per-chat memory isolation** — opening a new chat should use isolated context, not shared history, so different models can be used independently
3. **macOS keyring re-prompts** — keyring prompts repeat every few minutes on macOS
4. **macOS built-app terminal issues** — "TERM not defined" and lag in compiled/built app
5. **Finding deduplication** — the agent records near-duplicate findings (same CVE, same target). Near-duplicate rejection works but could be tighter
6. **Token consumption** — further reductions possible via smarter summarization and context trimming
