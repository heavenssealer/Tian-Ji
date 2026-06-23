//! DeepSeek adapter — OpenAI-compatible Chat Completions over SSE.
//!
//! DeepSeek's API is a drop-in for the OpenAI ChatCompletions format, so this adapter speaks that
//! wire shape: a single `messages` array (system/user/assistant/tool), OpenAI-style `tools`
//! (function declarations), and an SSE stream of `choices[].delta` chunks. Same [`LlmProvider`]
//! contract as [`crate::ClaudeProvider`] and [`crate::OllamaProvider`] — messages + tools in,
//! normalized [`AgentEvent`]s out; the wire format stays confined to this file.
//!
//! Auth is a single API key (`Authorization: Bearer`). DeepSeek does context caching server-side
//! automatically, so there is nothing cache-related to send (no `cache_control`, unlike Claude).
//! `deepseek-reasoner`'s `reasoning_content` (chain-of-thought) is intentionally dropped: DeepSeek
//! requires it NOT be echoed back in later turns, and folding it into the assistant message would
//! bloat — and corrupt — the re-sent history.

use std::collections::BTreeMap;

use async_trait::async_trait;
use futures::{
    channel::mpsc::{self, UnboundedSender},
    stream::BoxStream,
    StreamExt,
};
use serde_json::{json, Value};
use tianji_types::{AgentEvent, Content, Message, Role, ToolCall, ToolSpec};

use crate::{LlmError, LlmProvider, Result};

const DEFAULT_BASE_URL: &str = "https://api.deepseek.com";

pub struct DeepSeekProvider {
    api_key: String,
    model: String,
    base_url: String,
    http: reqwest::Client,
}

impl DeepSeekProvider {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
            http: reqwest::Client::new(),
        }
    }

    /// Point at an OpenAI-compatible base URL other than DeepSeek's (e.g. a proxy or self-host).
    /// Trailing slashes are trimmed so the joined `/chat/completions` path stays well-formed.
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into().trim_end_matches('/').to_string();
        self
    }
}

#[async_trait]
impl LlmProvider for DeepSeekProvider {
    async fn run_turn(
        &self,
        messages: &[Message],
        tools: &[ToolSpec],
    ) -> Result<BoxStream<'static, AgentEvent>> {
        if self.api_key.trim().is_empty() {
            return Err(LlmError::MissingKey("deepseek"));
        }

        // `stream_options.include_usage` makes DeepSeek emit a final usage-only chunk so the cost
        // meter sees real token counts (OpenAI omits usage from streamed responses otherwise).
        let mut body = json!({
            "model": self.model,
            "messages": translate_messages(messages),
            "stream": true,
            "stream_options": { "include_usage": true },
            "max_tokens": 4096,
        });
        if !tools.is_empty() {
            body["tools"] = json!(tools.iter().map(translate_tool).collect::<Vec<_>>());
        }

        let resp = self
            .http
            .post(format!("{}/chat/completions", self.base_url))
            .header("authorization", format!("Bearer {}", self.api_key))
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
// OpenAI-style stream: blank-line-separated `data: {…}` chunks, each a partial completion. Text
// arrives as `choices[0].delta.content`; tool calls arrive as `choices[0].delta.tool_calls[]`
// fragments accumulated by index (the id + name land on the first fragment, the JSON `arguments`
// stream in across the rest). A trailing `data: [DONE]` (or stream close) ends the turn.

/// One tool call being assembled across deltas.
#[derive(Default)]
struct ToolAcc {
    id: String,
    name: String,
    args: String,
}

async fn pump_sse<S, B>(mut stream: S, tx: UnboundedSender<AgentEvent>)
where
    S: futures::Stream<Item = reqwest::Result<B>> + Unpin + Send,
    B: AsRef<[u8]>,
{
    let mut buf = String::new();
    let mut tools: BTreeMap<u64, ToolAcc> = BTreeMap::new();
    let mut input_tokens: u32 = 0;
    let mut output_tokens: u32 = 0;
    let mut ended = false;

    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(bytes) => {
                buf.push_str(&String::from_utf8_lossy(bytes.as_ref()));
                while let Some(pos) = buf.find("\n\n") {
                    let msg = buf[..pos].to_string();
                    buf = buf[pos + 2..].to_string();
                    if on_sse_message(&msg, &tx, &mut tools, &mut input_tokens, &mut output_tokens) {
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

    // Flush a final chunk that wasn't terminated by a blank line.
    let rest = buf.trim().to_string();
    if !rest.is_empty()
        && on_sse_message(&rest, &tx, &mut tools, &mut input_tokens, &mut output_tokens)
    {
        ended = true;
    }
    // Stream closed without an explicit `[DONE]` — still emit whatever we accumulated.
    if !ended {
        finish(&tx, &mut tools, input_tokens, output_tokens);
    }
}

/// Parse one SSE message and push the appropriate `AgentEvent`(s). Returns `true` on `[DONE]`,
/// after flushing accumulated tool calls + usage + `TurnEnd`.
fn on_sse_message(
    msg: &str,
    tx: &UnboundedSender<AgentEvent>,
    tools: &mut BTreeMap<u64, ToolAcc>,
    input_tokens: &mut u32,
    output_tokens: &mut u32,
) -> bool {
    let Some(data) = msg.lines().find_map(|l| l.strip_prefix("data:")).map(str::trim) else {
        return false;
    };
    if data == "[DONE]" {
        finish(tx, tools, *input_tokens, *output_tokens);
        return true;
    }
    let Ok(v) = serde_json::from_str::<Value>(data) else {
        return false;
    };

    // Usage rides on its own trailing chunk (choices is empty there).
    if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
        *input_tokens = u["prompt_tokens"].as_u64().unwrap_or(*input_tokens as u64) as u32;
        *output_tokens = u["completion_tokens"].as_u64().unwrap_or(*output_tokens as u64) as u32;
    }

    let Some(choice) = v["choices"].get(0) else {
        return false;
    };
    let delta = &choice["delta"];

    // `reasoning_content` (deepseek-reasoner CoT) is deliberately ignored — see module docs.
    if let Some(text) = delta.get("content").and_then(|c| c.as_str()) {
        if !text.is_empty() {
            let _ = tx.unbounded_send(AgentEvent::TextDelta { text: text.to_string() });
        }
    }

    if let Some(calls) = delta.get("tool_calls").and_then(|c| c.as_array()) {
        for call in calls {
            let idx = call["index"].as_u64().unwrap_or(0);
            let entry = tools.entry(idx).or_default();
            if let Some(id) = call.get("id").and_then(|i| i.as_str()) {
                if !id.is_empty() {
                    entry.id = id.to_string();
                }
            }
            if let Some(f) = call.get("function") {
                if let Some(name) = f.get("name").and_then(|n| n.as_str()) {
                    if !name.is_empty() {
                        entry.name = name.to_string();
                    }
                }
                if let Some(args) = f.get("arguments").and_then(|a| a.as_str()) {
                    entry.args.push_str(args);
                }
            }
        }
    }

    false
}

/// Emit the assembled tool calls (in index order), the token usage, and `TurnEnd`.
fn finish(
    tx: &UnboundedSender<AgentEvent>,
    tools: &mut BTreeMap<u64, ToolAcc>,
    input_tokens: u32,
    output_tokens: u32,
) {
    for (idx, t) in std::mem::take(tools) {
        if t.name.is_empty() {
            continue;
        }
        // Args stream in as a JSON string; if a call carried none, default to an empty object.
        let arguments = serde_json::from_str::<Value>(&t.args).unwrap_or_else(|_| json!({}));
        let call_id = if t.id.is_empty() { format!("deepseek-{idx}") } else { t.id };
        let _ = tx.unbounded_send(AgentEvent::ToolCall {
            call: ToolCall { call_id, name: t.name, arguments },
        });
    }
    let _ = tx.unbounded_send(AgentEvent::TokensUsed { input_tokens, output_tokens });
    let _ = tx.unbounded_send(AgentEvent::TurnEnd);
}

// ── translation ────────────────────────────────────────────────────────────────

fn translate_messages(messages: &[Message]) -> Vec<Value> {
    let mut out = Vec::new();
    for m in messages {
        match m.role {
            // The two-block system prompt (stable + volatile) is merged into one system message;
            // DeepSeek has no separate system field and no cache breakpoint to preserve.
            Role::System => out.push(json!({ "role": "system", "content": join_text(&m.content) })),
            Role::User => out.push(json!({ "role": "user", "content": join_text(&m.content) })),
            Role::Assistant => {
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
                        Content::ToolUse { call } => tool_calls.push(json!({
                            "id": call.call_id,
                            "type": "function",
                            // OpenAI expects `arguments` as a JSON STRING, not an object.
                            "function": { "name": call.name, "arguments": call.arguments.to_string() },
                        })),
                        Content::ToolResult { .. } => {}
                    }
                }
                let mut msg = serde_json::Map::new();
                msg.insert("role".into(), json!("assistant"));
                if tool_calls.is_empty() {
                    msg.insert("content".into(), json!(text));
                } else {
                    // An assistant turn carrying tool_calls uses null content when there's no text.
                    msg.insert(
                        "content".into(),
                        if text.is_empty() { Value::Null } else { json!(text) },
                    );
                    msg.insert("tool_calls".into(), json!(tool_calls));
                }
                out.push(Value::Object(msg));
            }
            Role::Tool => {
                // Each tool result is its own message, correlated back to its call by id.
                for c in &m.content {
                    if let Content::ToolResult { call_id, output } = c {
                        out.push(json!({
                            "role": "tool",
                            "tool_call_id": call_id,
                            "content": output,
                        }));
                    }
                }
            }
        }
    }
    out
}

fn join_text(content: &[Content]) -> String {
    let mut s = String::new();
    for c in content {
        if let Content::Text { text } = c {
            if !s.is_empty() {
                s.push_str("\n\n");
            }
            s.push_str(text);
        }
    }
    s
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn assistant_tool_call_and_result_translate_to_openai() {
        let messages = vec![
            Message {
                role: Role::Assistant,
                content: vec![Content::ToolUse {
                    call: ToolCall {
                        call_id: "c1".into(),
                        name: "run_command".into(),
                        arguments: json!({ "tool": "id" }),
                    },
                }],
            },
            Message {
                role: Role::Tool,
                content: vec![Content::ToolResult { call_id: "c1".into(), output: "uid=0".into() }],
            },
        ];
        let out = translate_messages(&messages);

        let asst = &out[0];
        assert_eq!(asst["role"], "assistant");
        assert_eq!(asst["content"], Value::Null, "tool-only assistant turn uses null content");
        assert_eq!(asst["tool_calls"][0]["id"], "c1");
        assert_eq!(asst["tool_calls"][0]["function"]["name"], "run_command");
        // arguments must be a JSON-encoded STRING, not an object.
        assert_eq!(asst["tool_calls"][0]["function"]["arguments"], "{\"tool\":\"id\"}");

        let tool = &out[1];
        assert_eq!(tool["role"], "tool");
        assert_eq!(tool["tool_call_id"], "c1");
        assert_eq!(tool["content"], "uid=0");
    }

    #[test]
    fn system_blocks_merge_into_one_message() {
        let msg = Message {
            role: Role::System,
            content: vec![
                Content::Text { text: "stable instructions".into() },
                Content::Text { text: "volatile scope".into() },
            ],
        };
        let out = translate_messages(&[msg]);
        assert_eq!(out[0]["role"], "system");
        assert_eq!(out[0]["content"], "stable instructions\n\nvolatile scope");
    }

    #[test]
    fn sse_accumulates_tool_call_text_and_usage() {
        let (tx, mut rx) = mpsc::unbounded::<AgentEvent>();
        let mut tools = BTreeMap::new();
        let (mut i, mut o) = (0u32, 0u32);

        on_sse_message(
            r#"data: {"choices":[{"index":0,"delta":{"content":"hi"}}]}"#,
            &tx, &mut tools, &mut i, &mut o,
        );
        // First tool fragment carries id + name + start of the args JSON.
        on_sse_message(
            r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"run_command","arguments":"{\"tool\":\"nmap\","}}]}}]}"#,
            &tx, &mut tools, &mut i, &mut o,
        );
        // Second fragment streams the rest of the args.
        on_sse_message(
            r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"argv\":[\"-sV\"]}"}}]}}]}"#,
            &tx, &mut tools, &mut i, &mut o,
        );
        // Usage-only chunk, then [DONE].
        on_sse_message(
            r#"data: {"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":7}}"#,
            &tx, &mut tools, &mut i, &mut o,
        );
        assert!(on_sse_message("data: [DONE]", &tx, &mut tools, &mut i, &mut o));

        drop(tx);
        let mut events = Vec::new();
        while let Ok(Some(e)) = rx.try_next() {
            events.push(e);
        }

        assert!(matches!(&events[0], AgentEvent::TextDelta { text } if text == "hi"));
        let call = events
            .iter()
            .find_map(|e| if let AgentEvent::ToolCall { call } = e { Some(call) } else { None })
            .expect("a tool call should be emitted");
        assert_eq!(call.name, "run_command");
        assert_eq!(call.arguments["tool"], "nmap");
        assert_eq!(call.arguments["argv"][0], "-sV");
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::TokensUsed { input_tokens: 12, output_tokens: 7 })));
        assert!(events.iter().any(|e| matches!(e, AgentEvent::TurnEnd)));
    }
}
