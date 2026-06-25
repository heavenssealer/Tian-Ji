//! Claude adapter — SSE streaming implementation.
//!
//! Sends `"stream": true`, reads Server-Sent Events line by line, and converts them to
//! `AgentEvent`s in real time. Text tokens arrive as `TextDelta`s as they are generated;
//! tool-use blocks are accumulated across `input_json_delta` events and emitted as a single
//! `ToolCall` on `content_block_stop`. Callers see no difference from the old buffered version
//! — only the latency improves (first token appears immediately instead of after full response).

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures::{
    channel::mpsc::{self, UnboundedSender},
    stream::BoxStream,
    StreamExt,
};
use serde_json::{json, Value};
use tianji_types::{AgentEvent, Content, Message, Role, ToolCall, ToolSpec};

use crate::{LlmError, LlmProvider, Result};

/// Base URL (without /v1/messages) used as the default and for DeepSeek's endpoint.
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Beta flag Anthropic requires on subscription (OAuth) requests — the same one the Claude Code
/// CLI sends. Without it the API rejects a bearer token on `/v1/messages`.
const OAUTH_BETA: &str = "oauth-2025-04-20";

/// Anthropic only honours a subscription token when the request identifies as Claude Code: the
/// **first** system block must carry this exact line. We prepend it automatically in OAuth mode.
const CLAUDE_CODE_IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

/// Yields a valid OAuth access token, refreshing it transparently when it is close to expiry.
/// Implemented in `src-tauri` (which owns the keychain + token endpoint); the adapter only ever
/// sees the resulting bearer string, so no OAuth/keychain wire types leak into this crate.
#[async_trait]
pub trait TokenSource: Send + Sync {
    async fn access_token(&self) -> Result<String>;
}

/// How the Claude adapter authenticates. `ApiKey` bills the org's API credits via `x-api-key`;
/// `Oauth` bills an Anthropic subscription (Claude Pro/Max) via a bearer token — exactly the path
/// the Claude Code CLI uses. The two differ on the wire (header, the `oauth-2025-04-20` beta flag,
/// and the required Claude Code identity), all handled in `run_turn`.
#[derive(Clone)]
pub enum ClaudeAuth {
    ApiKey(String),
    Oauth(Arc<dyn TokenSource>),
}

/// Build the HTTP client with settings that survive flaky networks (notably the HTB/VPN `tun`
/// interfaces on Kali, where a second request reusing a pooled HTTP/2 connection fails with
/// "error sending request for url"):
/// - `pool_max_idle_per_host(0)` — never reuse an idle connection; open a fresh one each turn.
/// - `http1_only` — avoid HTTP/2 GOAWAY / multiplexing issues over VPN tunnels.
/// - `tcp_keepalive` — keep the streaming connection alive during long SSE responses.
/// - `connect_timeout` — fail fast if the server is unreachable (30s).
/// - `timeout` — overall request timeout (300s) prevents permanent hangs if the SSE stream
///   stalls mid-response. This is generous enough for slow LLM generations but prevents
///   the "LLM stalls → whole app freezes" failure mode observed with DeepSeek's endpoint.
fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .http1_only()
        .tcp_keepalive(std::time::Duration::from_secs(60))
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

pub struct ClaudeProvider {
    auth: ClaudeAuth,
    model: String,
    /// Base URL for the Anthropic Messages API. Defaults to `https://api.anthropic.com`.
    /// DeepSeek's Anthropic-compatible endpoint overrides this to `https://api.deepseek.com/anthropic`.
    base_url: String,
    http: reqwest::Client,
}

impl ClaudeProvider {
    /// API-key auth (bills API credits). Kept for callers that pass a raw key.
    pub fn new(api_key: String) -> Self {
        Self::with_auth(ClaudeAuth::ApiKey(api_key))
    }

    /// Build with an explicit auth strategy (API key or subscription OAuth).
    pub fn with_auth(auth: ClaudeAuth) -> Self {
        Self {
            auth,
            model: "claude-opus-4-8".to_string(),
            base_url: DEFAULT_BASE_URL.to_string(),
            http: build_client(),
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Point at an Anthropic-compatible endpoint other than api.anthropic.com. DeepSeek exposes
    /// `https://api.deepseek.com/anthropic` which speaks the full Anthropic Messages API — system
    /// blocks, tool_use/tool_result, SSE streaming, `x-api-key` auth — all wire-compatible.
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into().trim_end_matches('/').to_string();
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
        let oauth = matches!(self.auth, ClaudeAuth::Oauth(_));
        let (system, msgs) = translate_messages(messages);

        // Prompt caching — the single biggest cost lever for a long agentic session. Every turn
        // re-sends the same large, stable prefix (all tool schemas + the system prompt). One
        // `cache_control` breakpoint on the system block caches everything before it (tools come
        // first in the cache order, then system), so from the second turn on that prefix is a
        // cache *read* at ~10% of the input price instead of full-price input tokens. The 5-minute
        // TTL comfortably covers a back-to-back tool loop. We do NOT cache the message tail: the
        // context assembler trims/reorders history when it exceeds budget, which would churn the
        // cache and waste cache-write cost — system+tools is the always-stable, always-hit chunk.
        let body = json!({
            "model": self.model,
            "max_tokens": 4096,
            "stream": true,
            "system": system_blocks(&system, oauth),
            "messages": msgs,
            "tools": tools.iter().map(translate_tool).collect::<Vec<_>>(),
        });

        // Auth diverges only here: an API key goes on `x-api-key`; a subscription token goes on
        // `Authorization: Bearer` with the `oauth-2025-04-20` beta flag (and the Claude Code
        // identity already prepended to `system` above).
        let mut req = self
            .http
            .post(format!("{}/v1/messages", self.base_url))
            .header("anthropic-version", ANTHROPIC_VERSION);
        match &self.auth {
            ClaudeAuth::ApiKey(key) => {
                if key.is_empty() {
                    return Err(LlmError::MissingKey("anthropic"));
                }
                req = req.header("x-api-key", key);
            }
            ClaudeAuth::Oauth(source) => {
                let token = source.access_token().await?;
                req = req
                    .header("authorization", format!("Bearer {token}"))
                    .header("anthropic-beta", OAUTH_BETA);
            }
        }

        let resp = req
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
    fn provider_id(&self) -> &str {
        if self.base_url.contains("deepseek") {
            "deepseek"
        } else {
            "claude"
        }
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
                        // message_stop received — don't wait for the stream to close.
                        // DeepSeek's endpoint may keep the connection alive (HTTP keep-alive),
                        // which would block stream.next() forever. We have everything we need.
                        break;
                    }
                }
                if ended { break; }
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
                        // DeepSeek's Anthropic endpoint wraps text_delta values in \n\n
                        // (e.g. {"text": "\n\nbash\n\n"}). Strip ONLY protocol-level wrapping
                        // while preserving intentional formatting within the text.
                        let cleaned = t.trim_start_matches('\n').trim_end_matches('\n');
                        // If the token was PURELY structural whitespace (a standalone \n\n
                        // paragraph break), preserve it as a single \n so paragraphs don't
                        // fuse together. Empty after trimming means it was all newlines.
                        if cleaned.is_empty() {
                            // Original was only newlines — emit one to preserve the break
                            let _ = tx.unbounded_send(AgentEvent::TextDelta { text: "\n".to_string() });
                        } else {
                            let _ = tx.unbounded_send(AgentEvent::TextDelta { text: cleaned.to_string() });
                        }
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

/// Build the `system` field as text blocks with a single `cache_control` breakpoint on the last
/// one, so the tools+system prefix is served from Anthropic's prompt cache on every turn after the
/// first. In OAuth mode the Claude Code identity is prepended as the first block (required for a
/// subscription token to be accepted). An empty system with no identity collapses to `[]`.
fn system_blocks(system: &[String], oauth: bool) -> Value {
    let mut blocks: Vec<Value> = Vec::new();
    if oauth {
        blocks.push(json!({ "type": "text", "text": CLAUDE_CODE_IDENTITY }));
    }
    for s in system {
        if !s.is_empty() {
            blocks.push(json!({ "type": "text", "text": s }));
        }
    }
    if blocks.is_empty() {
        return json!([]);
    }
    // Cache the STABLE prefix only: the identity (OAuth) plus the first app block (the orchestrator
    // sends stable instructions first, volatile context second). Putting the breakpoint on the
    // first app block means everything before it — tools + identity + stable instructions — is a
    // cache read on every later turn, while the volatile second block stays fresh. (Previously the
    // breakpoint sat on the LAST block, so the volatile context was inside the cached prefix and
    // busted the cache almost every turn.)
    let cache_idx = if oauth && blocks.len() > 1 { 1 } else { 0 };
    blocks[cache_idx]["cache_control"] = json!({ "type": "ephemeral" });
    Value::Array(blocks)
}

fn translate_messages(messages: &[Message]) -> (Vec<String>, Vec<Value>) {
    // Each system Content::Text becomes its own block, preserving the stable/volatile split the
    // orchestrator builds so caching can break between them (see `system_blocks`).
    let mut system: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for m in messages {
        if matches!(m.role, Role::System) {
            for c in &m.content {
                if let Content::Text { text } = c {
                    if !text.is_empty() {
                        system.push(text.clone());
                    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_block_carries_cache_control() {
        let block = system_blocks(&["you are a pentest assistant".to_string()], false);
        let first = &block[0];
        assert_eq!(first["type"], "text");
        assert_eq!(first["text"], "you are a pentest assistant");
        assert_eq!(first["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn empty_system_collapses_to_empty_array() {
        let block = system_blocks(&[String::new()], false);
        assert_eq!(block, json!([]));
    }

    #[test]
    fn oauth_prepends_claude_code_identity() {
        let block = system_blocks(&["you are a pentest assistant".to_string()], true);
        // Identity first (no cache), app system second WITH the cache breakpoint.
        assert_eq!(block[0]["text"], CLAUDE_CODE_IDENTITY);
        assert!(block[0].get("cache_control").is_none());
        assert_eq!(block[1]["text"], "you are a pentest assistant");
        assert_eq!(block[1]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn oauth_with_empty_system_still_sends_identity() {
        let block = system_blocks(&[String::new()], true);
        assert_eq!(block[0]["text"], CLAUDE_CODE_IDENTITY);
        assert_eq!(block[0]["cache_control"]["type"], "ephemeral");
        assert_eq!(block.as_array().unwrap().len(), 1);
    }

    #[test]
    fn cache_breakpoint_sits_before_the_volatile_block() {
        // Stable instructions + volatile context: only the stable (first) block is cached.
        let block = system_blocks(&["stable instructions".into(), "volatile scope+notes".into()], false);
        assert_eq!(block[0]["text"], "stable instructions");
        assert_eq!(block[0]["cache_control"]["type"], "ephemeral");
        assert_eq!(block[1]["text"], "volatile scope+notes");
        assert!(block[1].get("cache_control").is_none(), "volatile block must NOT be cached");
    }
}
