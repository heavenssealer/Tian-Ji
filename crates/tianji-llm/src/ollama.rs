//! Ollama adapter — local, zero-cost inference over Ollama's `/api/chat` endpoint.
//!
//! Lets the operator run a local model (llama3.1, qwen2.5, …) instead of paying for a cloud
//! API. Same [`LlmProvider`] contract as [`crate::ClaudeProvider`]: messages + tools in, a
//! stream of normalized [`AgentEvent`]s out. The wire format differs (NDJSON stream, OpenAI-style
//! function tools) and is confined to this file.
//!
//! Tool calling requires a model that supports it (e.g. llama3.1, qwen2.5-coder, mistral-nemo).

use std::collections::HashMap;

use async_trait::async_trait;
use futures::{
    channel::mpsc::{self, UnboundedSender},
    stream::BoxStream,
    StreamExt,
};
use serde_json::{json, Value};
use tianji_types::{AgentEvent, Content, Message, Role, ToolCall, ToolSpec};

use crate::{LlmError, LlmProvider, Result};

const DEFAULT_BASE_URL: &str = "http://localhost:11434";

pub struct OllamaProvider {
    base_url: String,
    model: String,
    /// Context window (`options.num_ctx`) to request. `None` lets Ollama use its (small) default;
    /// for this agent that truncates the prompt, so the host always sets one.
    num_ctx: Option<u32>,
    http: reqwest::Client,
}

impl OllamaProvider {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            model: model.into(),
            num_ctx: None,
            http: reqwest::Client::new(),
        }
    }

    /// Point at a non-default Ollama host (e.g. a remote box on the LAN).
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Set the context window Ollama should allocate for this model.
    pub fn with_num_ctx(mut self, num_ctx: u32) -> Self {
        self.num_ctx = Some(num_ctx);
        self
    }

    /// Build the `/api/chat` request body. Adds `options.num_ctx` when a window is configured —
    /// without it Ollama defaults to ~2–4k and silently drops the front of the prompt (system
    /// prompt + tool defs), breaking the agent.
    fn request_body(&self, messages: &[Message], tools: &[ToolSpec]) -> Value {
        let mut body = json!({
            "model": self.model,
            "messages": translate_messages(messages),
            "tools": tools.iter().map(translate_tool).collect::<Vec<_>>(),
            "stream": true,
        });
        if let Some(n) = self.num_ctx {
            body["options"] = json!({ "num_ctx": n });
        }
        body
    }
}

#[async_trait]
impl LlmProvider for OllamaProvider {
    async fn run_turn(
        &self,
        messages: &[Message],
        tools: &[ToolSpec],
    ) -> Result<BoxStream<'static, AgentEvent>> {
        let body = self.request_body(messages, tools);

        let resp = self
            .http
            .post(format!("{}/api/chat", self.base_url))
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Http(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(LlmError::Provider(format!("{status}: {text}")));
        }

        let (tx, rx) = mpsc::unbounded::<AgentEvent>();
        tokio::spawn(async move { pump_ndjson(resp.bytes_stream(), tx).await });
        Ok(Box::pin(rx))
    }
}

// ── NDJSON pump ────────────────────────────────────────────────────────────────
// Ollama streams one JSON object per line: incremental `message.content` tokens, optional
// `message.tool_calls`, and a final `{"done":true, ...usage}` object.

async fn pump_ndjson<S, B>(mut stream: S, tx: UnboundedSender<AgentEvent>)
where
    S: futures::Stream<Item = reqwest::Result<B>> + Unpin + Send,
    B: AsRef<[u8]>,
{
    let mut buf = String::new();
    let mut tool_seq: usize = 0;
    let mut ended = false;

    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(bytes) => {
                buf.push_str(&String::from_utf8_lossy(bytes.as_ref()));
                while let Some(pos) = buf.find('\n') {
                    let line = buf[..pos].trim().to_string();
                    buf = buf[pos + 1..].to_string();
                    if !line.is_empty() && on_line(&line, &tx, &mut tool_seq) {
                        ended = true;
                    }
                }
            }
            Err(e) => {
                let _ = tx.unbounded_send(AgentEvent::Error { message: e.to_string() });
                return;
            }
        }
    }

    let rest = buf.trim();
    if !rest.is_empty() && on_line(rest, &tx, &mut tool_seq) {
        ended = true;
    }
    if !ended {
        let _ = tx.unbounded_send(AgentEvent::TurnEnd);
    }
}

/// Parse one NDJSON line and push the appropriate `AgentEvent`(s). Returns `true` once the stream
/// reports `done`.
fn on_line(line: &str, tx: &UnboundedSender<AgentEvent>, tool_seq: &mut usize) -> bool {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return false;
    };

    if let Some(msg) = v.get("message") {
        if let Some(content) = msg.get("content").and_then(|c| c.as_str()) {
            if !content.is_empty() {
                let _ = tx.unbounded_send(AgentEvent::TextDelta { text: content.to_string() });
            }
        }
        if let Some(calls) = msg.get("tool_calls").and_then(|c| c.as_array()) {
            for call in calls {
                let Some(f) = call.get("function") else { continue };
                let name = f.get("name").and_then(|n| n.as_str()).unwrap_or_default().to_string();
                let arguments = normalize_args(f.get("arguments").cloned().unwrap_or(json!({})));
                *tool_seq += 1;
                let call_id = format!("ollama-{tool_seq}");
                let _ = tx.unbounded_send(AgentEvent::ToolCall {
                    call: ToolCall { call_id, name, arguments },
                });
            }
        }
    }

    if v.get("done").and_then(|d| d.as_bool()).unwrap_or(false) {
        let input = v.get("prompt_eval_count").and_then(|n| n.as_u64()).unwrap_or(0) as u32;
        let output = v.get("eval_count").and_then(|n| n.as_u64()).unwrap_or(0) as u32;
        let _ = tx.unbounded_send(AgentEvent::TokensUsed {
            input_tokens: input,
            output_tokens: output,
        });
        let _ = tx.unbounded_send(AgentEvent::TurnEnd);
        return true;
    }

    false
}

/// Some models emit `arguments` as a JSON-encoded string rather than an object; normalize both to
/// a JSON object so the orchestrator's argument parser sees a consistent shape.
fn normalize_args(v: Value) -> Value {
    if let Value::String(s) = &v {
        if let Ok(parsed) = serde_json::from_str::<Value>(s) {
            return parsed;
        }
    }
    v
}

// ── translation ────────────────────────────────────────────────────────────────

fn translate_messages(messages: &[Message]) -> Vec<Value> {
    let mut out = Vec::new();
    // Ollama tool-result messages correlate by `tool_name`, not an id, so remember each tool's
    // name as we pass its `tool_use` on the way to its `tool_result`.
    let mut names: HashMap<String, String> = HashMap::new();

    for m in messages {
        if matches!(m.role, Role::Tool) {
            for c in &m.content {
                if let Content::ToolResult { call_id, output } = c {
                    let mut msg = json!({ "role": "tool", "content": output });
                    if let Some(name) = names.get(call_id) {
                        msg["tool_name"] = json!(name);
                    }
                    out.push(msg);
                }
            }
            continue;
        }

        let role = match m.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        let mut text = String::new();
        let mut tool_calls = Vec::new();
        for c in &m.content {
            match c {
                Content::Text { text: t } => {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(t);
                }
                Content::ToolUse { call } => {
                    names.insert(call.call_id.clone(), call.name.clone());
                    tool_calls.push(json!({
                        "function": { "name": call.name, "arguments": call.arguments }
                    }));
                }
                Content::ToolResult { .. } => {}
            }
        }
        let mut msg = json!({ "role": role, "content": text });
        if !tool_calls.is_empty() {
            msg["tool_calls"] = json!(tool_calls);
        }
        out.push(msg);
    }
    out
}

fn translate_tool(t: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": t.name,
            "description": t.description,
            "parameters": t.input_schema,
        }
    })
}

// ── model discovery ──────────────────────────────────────────────────────────────

/// Ask a running Ollama server which models it has pulled (`GET /api/tags`). Returns the bare
/// model names (e.g. "llama3.1:latest"); the caller prefixes them with `ollama:`. Best-effort —
/// a short timeout keeps the UI responsive when no server is running, and errors are surfaced so
/// the caller can fall back to a static list.
pub async fn list_ollama_models(base_url: &str) -> Result<Vec<String>> {
    let url = format!("{}/api/tags", base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let resp = client.get(&url).send().await.map_err(|e| LlmError::Http(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(LlmError::Provider(format!("{}: /api/tags", resp.status())));
    }
    let body: Value = resp.json().await.map_err(|e| LlmError::Http(e.to_string()))?;
    Ok(parse_tag_names(&body))
}

fn parse_tag_names(body: &Value) -> Vec<String> {
    body["models"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|m| m["name"].as_str().map(String::from)).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tianji_types::ToolCall as TC;

    #[test]
    fn num_ctx_is_sent_only_when_configured() {
        let with = OllamaProvider::new("llama3.1").with_num_ctx(32768);
        assert_eq!(with.request_body(&[], &[])["options"]["num_ctx"], 32768);

        let without = OllamaProvider::new("llama3.1");
        assert!(without.request_body(&[], &[]).get("options").is_none());
    }

    #[test]
    fn parse_tag_names_extracts_model_names() {
        let body = json!({
            "models": [
                { "name": "llama3.1:latest", "size": 1 },
                { "name": "qwen2.5-coder:7b" },
                { "notname": "ignored" }
            ]
        });
        assert_eq!(parse_tag_names(&body), vec!["llama3.1:latest", "qwen2.5-coder:7b"]);
    }

    #[test]
    fn parse_tag_names_handles_missing_or_empty() {
        assert!(parse_tag_names(&json!({})).is_empty());
        assert!(parse_tag_names(&json!({ "models": [] })).is_empty());
    }

    #[test]
    fn translates_tool_to_openai_function() {
        let spec = ToolSpec {
            name: "run_command".into(),
            description: "run a command".into(),
            input_schema: json!({ "type": "object" }),
        };
        let v = translate_tool(&spec);
        assert_eq!(v["type"], "function");
        assert_eq!(v["function"]["name"], "run_command");
        assert_eq!(v["function"]["parameters"]["type"], "object");
    }

    #[test]
    fn tool_result_is_correlated_by_name() {
        let messages = vec![
            Message {
                role: Role::Assistant,
                content: vec![Content::ToolUse {
                    call: TC { call_id: "x1".into(), name: "run_command".into(), arguments: json!({}) },
                }],
            },
            Message {
                role: Role::Tool,
                content: vec![Content::ToolResult { call_id: "x1".into(), output: "OK".into() }],
            },
        ];
        let out = translate_messages(&messages);
        let tool_msg = out.iter().find(|m| m["role"] == "tool").unwrap();
        assert_eq!(tool_msg["content"], "OK");
        assert_eq!(tool_msg["tool_name"], "run_command");
    }

    #[test]
    fn normalize_args_parses_stringified_json() {
        assert_eq!(normalize_args(json!("{\"a\":1}")), json!({ "a": 1 }));
        assert_eq!(normalize_args(json!({ "a": 1 })), json!({ "a": 1 }));
    }

    #[test]
    fn on_line_emits_text_then_done() {
        let (tx, mut rx) = mpsc::unbounded::<AgentEvent>();
        let mut seq = 0;
        assert!(!on_line(r#"{"message":{"content":"hi"},"done":false}"#, &tx, &mut seq));
        assert!(on_line(r#"{"done":true,"prompt_eval_count":10,"eval_count":3}"#, &tx, &mut seq));
        drop(tx);
        let mut events = Vec::new();
        while let Ok(Some(e)) = rx.try_next() {
            events.push(e);
        }
        assert!(matches!(events[0], AgentEvent::TextDelta { .. }));
        assert!(matches!(events[1], AgentEvent::TokensUsed { input_tokens: 10, output_tokens: 3 }));
        assert!(matches!(events[2], AgentEvent::TurnEnd));
    }

    #[test]
    fn on_line_emits_tool_call() {
        let (tx, mut rx) = mpsc::unbounded::<AgentEvent>();
        let mut seq = 0;
        let line = r#"{"message":{"tool_calls":[{"function":{"name":"run_command","arguments":{"tool":"nmap","argv":["-sV"]}}}]},"done":false}"#;
        on_line(line, &tx, &mut seq);
        drop(tx);
        let e = rx.try_next().unwrap().unwrap();
        match e {
            AgentEvent::ToolCall { call } => {
                assert_eq!(call.name, "run_command");
                assert_eq!(call.arguments["tool"], "nmap");
            }
            _ => panic!("expected ToolCall"),
        }
    }
}
