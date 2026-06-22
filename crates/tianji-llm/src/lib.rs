//! # tianji-llm — provider abstraction (DESIGN.md §7.1)
//!
//! The orchestrator depends on the [`LlmProvider`] trait and our own neutral types only. v0.1
//! ships **`ClaudeProvider`** as the single implementation; OpenAI/local are later additive
//! files implementing the same trait — no change to the orchestrator, policy, memory, or UI.
//!
//! **Rule: provider SDK/wire types never escape this crate.**

use async_trait::async_trait;
use futures::stream::BoxStream;
use tianji_types::{AgentEvent, Message, ToolSpec};

mod claude;
mod ollama;
pub use claude::{ClaudeAuth, ClaudeProvider, TokenSource};
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
}
