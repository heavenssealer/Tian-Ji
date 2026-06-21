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
    /// Trim `messages` so the cumulative token estimate fits under `max_tokens`.
    /// Always keeps messages[0] (system prompt). Drops oldest non-system messages first.
    pub fn trim_to_budget(&self, messages: &[Message]) -> Vec<Message> {
        if messages.is_empty() {
            return Vec::new();
        }
        let sys_cost = message_tokens(&messages[0]);
        let mut remaining = self.max_tokens.saturating_sub(sys_cost);

        // Walk from newest to oldest (excluding system prompt at index 0).
        let mut kept: Vec<usize> = Vec::new();
        for (i, msg) in messages[1..].iter().enumerate().rev() {
            let cost = message_tokens(msg);
            if cost <= remaining {
                remaining -= cost;
                kept.push(i + 1);
            }
        }
        kept.sort();

        let mut out = vec![messages[0].clone()];
        for i in kept {
            out.push(messages[i].clone());
        }
        out
    }

    /// Cap tool output in-place to prevent individual messages from dominating the context.
    pub fn cap_tool_output(messages: &mut Vec<Message>) {
        for msg in messages.iter_mut() {
            if msg.role == Role::Tool {
                for c in &mut msg.content {
                    if let Content::ToolResult { output, .. } = c {
                        if output.len() > TOOL_OUTPUT_CAP {
                            *output = format!(
                                "{}…\n[truncated — {} total chars]",
                                &output[..TOOL_OUTPUT_CAP],
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
