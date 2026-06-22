//! Context assembler — where token bloat is won or lost (DESIGN.md §6.3).
//!
//! Budget per turn: system prompt + recalled events + recent transcript + capped tool output.
//! Token estimate: chars / 4 (rough but consistent; real tiktoken is v0.3).

use tianji_types::{Content, Event, EventKind, Message, Role};

/// Max chars of tool output kept per message (avoids 5000-line nmap scans drowning context).
const TOOL_OUTPUT_CAP: usize = 2_048;
/// Max chars per recalled event snippet injected into context.
const RECALL_SNIPPET_CAP: usize = 512;
/// How many past events to scan for keyword recall.
const RECALL_SCAN_LIMIT: usize = 200;
/// How many recalled events to inject per turn.
const RECALL_INJECT_LIMIT: usize = 6;

fn estimate(chars: usize) -> usize {
    (chars / 4).max(1)
}

/// Move `start` to an index whose message is a `user` turn, so the trimmed window begins on a real
/// user message (never a dangling tool-result or an assistant turn). Prefers the nearest user at or
/// after `start`; failing that, backs up to the most recent user before it (keeps the objective /
/// current prompt anchored). Returns `messages.len()` only if there is no user message at all.
fn anchor_to_user(messages: &[Message], start: usize) -> usize {
    if let Some(i) = (start..messages.len()).find(|&i| messages[i].role == Role::User) {
        return i;
    }
    (1..start).rev().find(|&i| messages[i].role == Role::User).unwrap_or(messages.len())
}

fn message_tokens(m: &Message) -> usize {
    let content_cost: usize = m.content.iter().map(|c| {
        let chars = match c {
            Content::Text { text } => text.len(),
            Content::ToolResult { output, .. } => output.len(),
            Content::ToolUse { call } => call.name.len() + call.arguments.to_string().len(),
        };
        estimate(chars)
    }).sum();
    content_cost + 4 // role overhead
}

pub struct ContextAssembler {
    /// Hard ceiling enforced per turn.
    pub max_tokens: usize,
}

impl Default for ContextAssembler {
    fn default() -> Self {
        Self { max_tokens: 16_000 }
    }
}

impl ContextAssembler {
    /// Trim `messages` so the cumulative token estimate fits under `max_tokens`, keeping the
    /// system prompt plus a **contiguous suffix** of the conversation.
    ///
    /// Contiguity matters: the Anthropic API rejects a `tool_result` block whose matching
    /// `tool_use` isn't in the immediately-preceding message, and rejects a conversation that
    /// begins with a dangling `tool_result`. Dropping arbitrary middle messages (or starting the
    /// window on a tool-result) would split those pairs. So we keep the newest messages that fit,
    /// then anchor the window to a `user` turn — guaranteeing every kept `tool_result` still has
    /// its `tool_use`, and the first sent message is a real user message.
    pub fn trim_to_budget(&self, messages: &[Message]) -> Vec<Message> {
        if messages.is_empty() {
            return Vec::new();
        }
        let sys_cost = message_tokens(&messages[0]);
        let mut remaining = self.max_tokens.saturating_sub(sys_cost);

        // Newest→oldest, stop at the first message that doesn't fit (keep a contiguous suffix).
        let mut start = messages.len();
        for i in (1..messages.len()).rev() {
            let cost = message_tokens(&messages[i]);
            if cost > remaining {
                break;
            }
            remaining -= cost;
            start = i;
        }

        start = anchor_to_user(messages, start);

        let mut out = Vec::with_capacity(1 + messages.len().saturating_sub(start));
        out.push(messages[0].clone());
        if start < messages.len() {
            out.extend(messages[start..].iter().cloned());
        }
        out
    }

    /// Cap tool output in-place to prevent individual messages from dominating the context.
    pub fn cap_tool_output(messages: &mut Vec<Message>) {
        Self::cap_tool_output_with(messages, TOOL_OUTPUT_CAP);
    }

    /// Same as [`cap_tool_output`] but with a caller-chosen cap — small-context (local) models use
    /// a tighter cap so a single tool result can't swallow their tiny window.
    pub fn cap_tool_output_with(messages: &mut Vec<Message>, cap: usize) {
        let cap = cap.max(256);
        for msg in messages.iter_mut() {
            if msg.role == Role::Tool {
                for c in &mut msg.content {
                    if let Content::ToolResult { output, .. } = c {
                        if output.len() > cap {
                            // Respect UTF-8 boundaries — `output` may contain multibyte chars.
                            let end = (0..=cap).rev().find(|&i| output.is_char_boundary(i)).unwrap_or(0);
                            *output = format!(
                                "{}…\n[truncated — {} total chars]",
                                &output[..end],
                                output.len()
                            );
                        }
                    }
                }
            }
        }
    }

    /// Keyword recall: score past events against `query`, return the top matches formatted as
    /// context snippets. Used to remind sub-agents of already-discovered information.
    pub fn keyword_recall(events: &[Event], query: &str) -> Vec<String> {
        let query_words: Vec<String> = query
            .split_whitespace()
            .filter(|w| w.len() > 3)
            .map(|w| w.to_lowercase())
            .collect();

        if query_words.is_empty() {
            return Vec::new();
        }

        let mut scored: Vec<(usize, &Event)> = events
            .iter()
            .rev()
            .take(RECALL_SCAN_LIMIT)
            .filter(|e| {
                matches!(e.kind, EventKind::Finding | EventKind::ToolOutput | EventKind::AgentMsg)
            })
            .filter_map(|e| {
                let text = event_text(e)?;
                let lower = text.to_lowercase();
                let score = query_words.iter().filter(|w| lower.contains(w.as_str())).count();
                if score > 0 { Some((score, e)) } else { None }
            })
            .collect();

        scored.sort_by(|a, b| b.0.cmp(&a.0));

        scored
            .into_iter()
            .take(RECALL_INJECT_LIMIT)
            .filter_map(|(_, e)| {
                let text = event_text(e)?;
                let snippet = if text.len() > RECALL_SNIPPET_CAP {
                    format!("{}…", &text[..RECALL_SNIPPET_CAP])
                } else {
                    text.to_string()
                };
                Some(format!("[{}] {}", kind_label(e.kind), snippet))
            })
            .collect()
    }
}

fn event_text(e: &Event) -> Option<&str> {
    e.payload.get("summary").and_then(|v| v.as_str())
        .or_else(|| e.payload.get("text").and_then(|v| v.as_str()))
        .or_else(|| e.payload.get("output").and_then(|v| v.as_str()))
}

fn kind_label(k: EventKind) -> &'static str {
    match k {
        EventKind::Finding    => "finding",
        EventKind::ToolOutput => "tool_output",
        EventKind::AgentMsg   => "agent",
        _                     => "event",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tianji_types::ToolCall;

    fn system(t: &str) -> Message {
        Message { role: Role::System, content: vec![Content::Text { text: t.into() }] }
    }
    fn user(t: &str) -> Message {
        Message { role: Role::User, content: vec![Content::Text { text: t.into() }] }
    }
    fn assistant_tool(id: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![Content::ToolUse {
                call: ToolCall { call_id: id.into(), name: "run_command".into(), arguments: serde_json::json!({}) },
            }],
        }
    }
    fn tool_result(id: &str) -> Message {
        Message { role: Role::Tool, content: vec![Content::ToolResult { call_id: id.into(), output: "out".into() }] }
    }

    /// The Anthropic invariants every trimmed window must satisfy.
    fn assert_valid(out: &[Message]) {
        assert_eq!(out[0].role, Role::System, "system stays first");
        if out.len() > 1 {
            assert_eq!(out[1].role, Role::User, "conversation must begin on a user turn");
        }
        for i in 1..out.len() {
            if out[i].role == Role::Tool {
                assert_eq!(out[i - 1].role, Role::Assistant, "tool_result must follow an assistant");
                assert!(
                    out[i - 1].content.iter().any(|c| matches!(c, Content::ToolUse { .. })),
                    "the preceding assistant must carry a tool_use",
                );
            }
        }
    }

    #[test]
    fn trim_drops_oldest_pairs_but_stays_valid() {
        let pad = "padding ".repeat(40); // make early user turns expensive
        let messages = vec![
            system("sys"),
            user(&format!("question one {pad}")),
            assistant_tool("toolu_A"),
            tool_result("toolu_A"),
            user(&format!("question two {pad}")),
            assistant_tool("toolu_B"),
            tool_result("toolu_B"),
            user("current question"),
        ];
        let out = ContextAssembler { max_tokens: 20 }.trim_to_budget(&messages);
        assert!(out.len() < messages.len(), "trimming should have engaged");
        assert_valid(&out);
    }

    #[test]
    fn trim_anchors_back_to_user_when_suffix_starts_on_a_tool_result() {
        // A sub-agent-style tail ending on an open tool pair; budget only fits the tool_result,
        // so the naive contiguous suffix would start on a dangling tool_result. Anchoring must
        // back up to the objective and keep the pair intact.
        let obj = "enumerate the host ".repeat(8);
        let messages =
            vec![system("s"), user(&obj), assistant_tool("toolu_A"), tool_result("toolu_A")];
        let out = ContextAssembler { max_tokens: 12 }.trim_to_budget(&messages);
        assert_valid(&out);
        assert_eq!(out.len(), 4, "the pair is pulled back in to stay valid");
    }
}
