//! Claude adapter — SSE streaming implementation.
//!
//! Sends `"stream": true`, reads Server-Sent Events line by line, and converts them to
//! `AgentEvent`s in real time. Text tokens arrive as `TextDelta`s as they are generated;
//! tool-use blocks are accumulated across `input_json_delta` events and emitted as a single
//! `ToolCall` on `content_block_stop`. Callers see no difference from the old buffered version
//! — only the latency improves (first token appears immediately instead of after full response).

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

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Build the HTTP client with settings that survive flaky networks (notably the HTB/VPN `tun`
/// interfaces on Kali, where a second request reusing a pooled HTTP/2 connection fails with
/// "error sending request for url"):
/// - `pool_max_idle_per_host(0)` — never reuse an idle connection; open a fresh one each turn.
/// - `http1_only` — avoid HTTP/2 GOAWAY / multiplexing issues over VPN tunnels.
/// - `tcp_keepalive` — keep the streaming connection alive during long SSE responses.
/// - `connect_timeout` only (NOT a request timeout, which would abort long streams).
fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .http1_only()
        .tcp_keepalive(std::time::Duration::from_secs(60))
        .connect_timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

pub struct ClaudeProvider {
    api_key: String,
    model: String,
    http: reqwest::Client,
}

impl ClaudeProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            model: "claude-opus-4-8".to_string(),
            http: build_client(),
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }
}

#[async_trait]
impl LlmProvider for ClaudeProvider {
    async fn run_turn(
        &self,
        messages: &[Message],
        tools: &[ToolSpec],
    ) -> Result<BoxStream<'static, AgentEvent>> {
        if self.api_key.is_empty() {
            return Err(LlmError::MissingKey("anthropic"));
        }

        let (system, msgs) = translate_messages(messages);
        let body = json!({
            "model": self.model,
            "max_tokens": 4096,
            "stream": true,
            "system": system,
            "messages": msgs,
            "tools": tools.iter().map(translate_tool).collect::<Vec<_>>(),
        });

        let resp = self
            .http
            .post(API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
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
        tokio::spawn(async move { pump_sse(resp.bytes_stream(), tx).await });
        Ok(Box::pin(rx))
    }
}

// ── SSE pump ─────────────────────────────────────────────────────────────────

async fn pump_sse<S, B>(mut stream: S, tx: UnboundedSender<AgentEvent>)
where
    S: futures::Stream<Item = reqwest::Result<B>> + Unpin + Send,
    B: AsRef<[u8]>,
{
    let mut buf = String::new();
    // index → (call_id, tool_name, accumulated_input_json)
    let mut tool_blocks: HashMap<usize, (String, String, String)> = HashMap::new();
    let mut input_tokens: u32 = 0;
    let mut output_tokens: u32 = 0;
    let mut ended = false;

    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(bytes) => {
                buf.push_str(&String::from_utf8_lossy(bytes.as_ref()));
                // SSE messages are separated by blank lines (\n\n).
                while let Some(pos) = buf.find("\n\n") {
                    let msg = buf[..pos].to_string();
                    buf = buf[pos + 2..].to_string();
                    if on_sse_message(
                        &msg,
                        &tx,
                        &mut tool_blocks,
                        &mut input_tokens,
                        &mut output_tokens,
                    ) {
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

    if !ended {
        let _ = tx.unbounded_send(AgentEvent::TurnEnd);
    }
}

/// Parse one SSE message and push the appropriate `AgentEvent`. Returns `true` on `message_stop`.
fn on_sse_message(
    msg: &str,
    tx: &UnboundedSender<AgentEvent>,
    tool_blocks: &mut HashMap<usize, (String, String, String)>,
    input_tokens: &mut u32,
    output_tokens: &mut u32,
) -> bool {
    // Each SSE message may have `event:` and `data:` lines.
    let data = msg
        .lines()
        .find(|l| l.starts_with("data: "))
        .map(|l| &l[6..]);

    let Some(data) = data else { return false };
    if data == "[DONE]" {
        return false;
    }
    let Ok(v) = serde_json::from_str::<Value>(data) else {
        return false;
    };

    match v["type"].as_str() {
        Some("message_start") => {
            *input_tokens =
                v["message"]["usage"]["input_tokens"].as_u64().unwrap_or(0) as u32;
        }

        Some("content_block_start") => {
            let idx = v["index"].as_u64().unwrap_or(0) as usize;
            if v["content_block"]["type"].as_str() == Some("tool_use") {
                let id = str_field(&v["content_block"]["id"]);
                let name = str_field(&v["content_block"]["name"]);
                tool_blocks.insert(idx, (id, name, String::new()));
            }
        }

        Some("content_block_delta") => {
            let idx = v["index"].as_u64().unwrap_or(0) as usize;
            match v["delta"]["type"].as_str() {
                Some("text_delta") => {
                    let t = str_field(&v["delta"]["text"]);
                    if !t.is_empty() {
                        let _ = tx.unbounded_send(AgentEvent::TextDelta { text: t });
                    }
                }
                Some("input_json_delta") => {
                    if let Some(block) = tool_blocks.get_mut(&idx) {
                        block.2.push_str(v["delta"]["partial_json"].as_str().unwrap_or(""));
                    }
                }
                _ => {}
            }
        }

        Some("content_block_stop") => {
            let idx = v["index"].as_u64().unwrap_or(0) as usize;
            if let Some((id, name, json_str)) = tool_blocks.remove(&idx) {
                let arguments = serde_json::from_str(&json_str)
                    .unwrap_or(Value::Object(serde_json::Map::new()));
                let _ = tx.unbounded_send(AgentEvent::ToolCall {
                    call: ToolCall { call_id: id, name, arguments },
                });
            }
        }

        Some("message_delta") => {
            if let Some(n) = v["usage"]["output_tokens"].as_u64() {
                *output_tokens = n as u32;
            }
        }

        Some("message_stop") => {
            let _ = tx.unbounded_send(AgentEvent::TokensUsed {
                input_tokens: *input_tokens,
                output_tokens: *output_tokens,
            });
            let _ = tx.unbounded_send(AgentEvent::TurnEnd);
            return true;
        }

        Some("error") => {
            let msg = v["error"]["message"].as_str().unwrap_or("api error").to_string();
            let _ = tx.unbounded_send(AgentEvent::Error { message: msg });
        }

        _ => {}
    }

    false
}

// ── translation helpers ───────────────────────────────────────────────────────

fn str_field(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

fn translate_messages(messages: &[Message]) -> (String, Vec<Value>) {
    let mut system = String::new();
    let mut out = Vec::new();
    for m in messages {
        if matches!(m.role, Role::System) {
            for c in &m.content {
                if let Content::Text { text } = c {
                    if !system.is_empty() {
                        system.push('\n');
                    }
                    system.push_str(text);
                }
            }
            continue;
        }
        let role = if matches!(m.role, Role::Assistant) { "assistant" } else { "user" };
        let content: Vec<Value> = m.content.iter().map(translate_content).collect();
        out.push(json!({ "role": role, "content": content }));
    }
    (system, out)
}

fn translate_content(c: &Content) -> Value {
    match c {
        Content::Text { text } => json!({ "type": "text", "text": text }),
        Content::ToolUse { call } => json!({
            "type": "tool_use",
            "id": call.call_id,
            "name": call.name,
            "input": call.arguments,
        }),
        Content::ToolResult { call_id, output } => json!({
            "type": "tool_result",
            "tool_use_id": call_id,
            "content": output,
        }),
    }
}

fn translate_tool(t: &ToolSpec) -> Value {
    json!({
        "name": t.name,
        "description": t.description,
        "input_schema": t.input_schema,
    })
}
