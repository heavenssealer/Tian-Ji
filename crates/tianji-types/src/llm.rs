//! Provider-neutral LLM types. The orchestrator speaks **only** these — never an SDK's types
//! (DESIGN.md §7.1). Each `LlmProvider` adapter in `tianji-llm` translates to/from these.

use serde::{Deserialize, Serialize};

/// A conversation message in our own shape (not Anthropic's, not OpenAI's).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<Content>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Content {
    Text { text: String },
    ToolUse { call: ToolCall },
    ToolResult { call_id: String, output: String },
}

/// A tool the model may call. Sourced from the MCP host, so it is provider-neutral by
/// construction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool's arguments.
    pub input_schema: serde_json::Value,
}

/// A model's request to call a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub call_id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Streamed output from a provider turn, normalized into our enum. This is the same shape the
/// event log consumes, so the adapter's only job is translation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    TextDelta { text: String },
    ToolCall { call: ToolCall },
    TokensUsed { input_tokens: u32, output_tokens: u32 },
    TurnEnd,
    Error { message: String },
}
