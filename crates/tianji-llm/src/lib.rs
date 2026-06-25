//! # tianji-llm — provider abstraction (DESIGN.md §7.1)
//!
//! The orchestrator depends on the [`LlmProvider`] trait and our own neutral types only. Adapters
//! are additive files implementing the same trait — no change to the orchestrator, policy, memory,
//! or UI: [`ClaudeProvider`] (Anthropic, SSE + prompt caching), [`OllamaProvider`] (local), and
//! [`DeepSeekProvider`] (OpenAI-compatible Chat Completions).
//!
//! **Rule: provider SDK/wire types never escape this crate.**

use async_trait::async_trait;
use futures::stream::BoxStream;
use tianji_types::{AgentEvent, Message, ToolSpec};

mod claude;
mod deepseek;
mod ollama;
pub use claude::{ClaudeAuth, ClaudeProvider, TokenSource};
pub use deepseek::DeepSeekProvider;
pub use ollama::{list_ollama_models, OllamaProvider};

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("http error: {0}")]
    Http(String),
    #[error("provider returned an error: {0}")]
    Provider(String),
    #[error("missing api key for {0}")]
    MissingKey(&'static str),
}

type Result<T> = std::result::Result<T, LlmError>;

/// One model turn: messages + available tools in, a stream of normalized [`AgentEvent`]s out.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn run_turn(
        &self,
        messages: &[Message],
        tools: &[ToolSpec],
    ) -> Result<BoxStream<'static, AgentEvent>>;

    /// Identifies the provider so the orchestrator can adjust its prompt style. Returns
    /// `"claude"`, `"deepseek"`, `"ollama"`, or `"generic"`. Claude-tuned prompts (terse,
    /// inline-only) cripple DeepSeek (no internal reasoning, follows rules literally); when
    /// this returns `"deepseek"` the orchestrator switches to DeepSeek-optimized instructions.
    fn provider_id(&self) -> &str {
        "generic"
    }
}
