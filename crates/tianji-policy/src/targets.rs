//! Target resolution (DESIGN.md §4.2). Parse the **real argv** for targets; never trust the
//! agent's narration of what it intends to hit.
//!
//! v0.1 is a heuristic pass (IP / CIDR / URL / bare hostname). Per-tool precision parsers can
//! be added where a tool's argument grammar needs them.

use tianji_types::Target;

pub fn resolve_targets(_tool: &str, argv: &[String]) -> Vec<Target> {
    argv.iter().filter_map(|a| classify_token(a)).collect()
}

fn classify_token(tok: &str) -> Option<Target> {
    if tok.starts_with('-') {
        return None; // a flag, not a target
    }
    if tok.contains('/') && tok.split('/').next().is_some_and(looks_like_ip) {
        return Some(Target::Cidr(tok.to_string()));
    }
    if tok.starts_with("http://") || tok.starts_with("https://") {
        return Some(Target::Url(tok.to_string()));
    }
    if looks_like_ip(tok) {
        return Some(Target::Ip(tok.to_string()));
    }
    if tok.contains('.') && !tok.contains(' ') {
        return Some(Target::Hostname(tok.to_string()));
    }
    None
}

fn looks_like_ip(s: &str) -> bool {
    let octets: Vec<&str> = s.split('.').collect();
    octets.len() == 4 && octets.iter().all(|o| o.parse::<u8>().is_ok())
}
