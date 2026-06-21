//! Engagement scope — the allowlist of what the agent may touch (DESIGN.md §10). Scope is a
//! first-class concept; the policy engine enforces it by parsing real targets out of argv.

use serde::{Deserialize, Serialize};

/// A target extracted from a command's arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Target {
    Ip(String),
    Cidr(String),
    Hostname(String),
    Url(String),
}

/// The set of things in scope for an engagement. An empty ruleset means **nothing** is in
/// scope (fail closed), not "everything".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScopeRules {
    pub cidrs: Vec<String>,
    pub hostnames: Vec<String>,
    pub url_domains: Vec<String>,
}
