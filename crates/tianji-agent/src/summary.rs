//! Tool-output summarization on ingest (DESIGN.md §6.3).
//!
//! A 5000-line nmap scan or a giant gobuster dump should not be fed verbatim into the model's
//! context every turn — it drowns the budget and costs tokens. We compress large outputs into a
//! compact, structured digest *as they happen*; the full raw output still lives in the event log
//! (addressable by reference), so nothing is lost.
//!
//! Summarization is heuristic and free (no LLM call) — deterministic parsers for the noisy tools
//! we run most (port scanners, web fuzzers), with a head+tail fallback for everything else.

/// Outputs at or below this size are passed through untouched — small results are already cheap
/// and summarizing them would lose detail for no benefit.
const SUMMARY_TRIGGER: usize = 1_600;
/// Head/tail line counts for the generic fallback.
const HEAD_LINES: usize = 24;
const TAIL_LINES: usize = 8;

/// Compress `output` for the conversation context if it is large; otherwise return it unchanged.
/// `tool` selects the parser (the bare executable name; paths are stripped).
pub fn summarize_tool_output(tool: &str, output: &str) -> String {
    if output.len() <= SUMMARY_TRIGGER {
        return output.to_string();
    }
    let base = tool.rsplit(['/', '\\']).next().unwrap_or(tool);
    let structured = match base {
        "nmap" | "rustscan" | "masscan" => summarize_ports(output),
        "gobuster" | "ffuf" | "feroxbuster" | "dirsearch" | "wfuzz" | "nikto" => summarize_web(output),
        _ => None,
    };
    match structured {
        Some(s) => format!(
            "{s}\n[output summarized on ingest — {} raw chars kept in the event log]",
            output.len()
        ),
        None => head_tail(output),
    }
}

/// Pull `PORT  STATE  SERVICE  VERSION` rows for open ports out of an nmap/rustscan/masscan run.
fn summarize_ports(out: &str) -> Option<String> {
    let mut ports = Vec::new();
    for line in out.lines() {
        let l = line.trim();
        let mut it = l.split_whitespace();
        let (Some(portproto), Some(state)) = (it.next(), it.next()) else { continue };
        // First token like "22/tcp" or "443/udp"; second token "open".
        let is_port = portproto
            .split_once('/')
            .is_some_and(|(n, p)| n.chars().all(|c| c.is_ascii_digit()) && (p == "tcp" || p == "udp"));
        if !is_port || state != "open" {
            continue;
        }
        let rest: Vec<&str> = it.collect();
        if rest.is_empty() {
            ports.push(portproto.to_string());
        } else {
            ports.push(format!("{portproto} {}", rest.join(" ")));
        }
    }
    if ports.is_empty() {
        return None;
    }
    Some(format!("Open ports ({}):\n{}", ports.len(), ports.join("\n")))
}

/// Pull discovered paths / status lines out of a web-fuzzer run. We keep lines that look like a
/// hit (contain an HTTP status or a leading path) and cap the count so a huge wordlist run stays
/// compact.
fn summarize_web(out: &str) -> Option<String> {
    const MAX_HITS: usize = 60;
    let mut hits = Vec::new();
    for line in out.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        let looks_like_hit = l.contains("Status:")
            || l.contains("[Status")
            || l.starts_with('/')
            || l.starts_with("200")
            || l.starts_with("301")
            || l.starts_with("302")
            || l.starts_with("401")
            || l.starts_with("403");
        if looks_like_hit {
            hits.push(l.to_string());
        }
    }
    if hits.is_empty() {
        return None;
    }
    let total = hits.len();
    hits.truncate(MAX_HITS);
    let mut s = format!("Discovered {total} path(s):\n{}", hits.join("\n"));
    if total > MAX_HITS {
        s.push_str(&format!("\n… {} more (see event log)", total - MAX_HITS));
    }
    Some(s)
}

/// Generic fallback: keep the head and tail, drop the middle.
fn head_tail(out: &str) -> String {
    let lines: Vec<&str> = out.lines().collect();
    if lines.len() <= HEAD_LINES + TAIL_LINES {
        // Big by bytes but few lines (e.g. one enormous line) — hard-cap by chars instead.
        let mut s: String = out.chars().take(SUMMARY_TRIGGER).collect();
        s.push_str(&format!("\n… [truncated — {} raw chars in the event log]", out.len()));
        return s;
    }
    let head = lines[..HEAD_LINES].join("\n");
    let tail = lines[lines.len() - TAIL_LINES..].join("\n");
    format!(
        "{head}\n… [{} middle lines omitted — full output in the event log] …\n{tail}",
        lines.len() - HEAD_LINES - TAIL_LINES
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_output_passes_through() {
        let out = "22/tcp open ssh";
        assert_eq!(summarize_tool_output("nmap", out), out);
    }

    #[test]
    fn nmap_summary_lists_open_ports() {
        let mut out = String::from("Starting Nmap\nHost is up\nPORT     STATE SERVICE VERSION\n");
        out.push_str("22/tcp   open  ssh     OpenSSH 8.2p1\n");
        out.push_str("80/tcp   open  http    Apache 2.4.41\n");
        out.push_str("443/tcp  closed https\n");
        out.push_str(&"filler line to push past the trigger size\n".repeat(60));
        let s = summarize_tool_output("nmap", &out);
        assert!(s.contains("Open ports (2)"));
        assert!(s.contains("22/tcp ssh OpenSSH 8.2p1"));
        assert!(s.contains("80/tcp http Apache 2.4.41"));
        assert!(!s.contains("closed"));
        assert!(s.len() < out.len());
    }

    #[test]
    fn generic_output_keeps_head_and_tail() {
        let out = (0..500).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let s = summarize_tool_output("somerandomtool", &out);
        assert!(s.contains("line 0"));
        assert!(s.contains("line 499"));
        assert!(s.contains("middle lines omitted"));
        assert!(s.len() < out.len());
    }

    #[test]
    fn web_summary_collects_hits() {
        let mut out = String::from("===============\nGobuster\n===============\n");
        for i in 0..200 {
            out.push_str(&format!("/path{i}               (Status: 200) [Size: 10]\n"));
        }
        let s = summarize_tool_output("gobuster", &out);
        assert!(s.contains("Discovered 200 path(s)"));
        assert!(s.contains("more (see event log)"));
        assert!(s.len() < out.len());
    }
}
