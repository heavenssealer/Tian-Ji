//! # tianji-policy — the execution spine (DESIGN.md §4)
//!
//! The load-bearing safety crate. **Pure logic: no async, no I/O, no DB, no network.** Inputs
//! in, [`Decision`] out. That purity is what makes the guardrails exhaustively testable.
//!
//! Pipeline: [`resolve_targets`] → scope check → [`classify`] → [`decide`].
//! Fail closed: anything unmatched ⇒ [`Decision::NeedsApproval`]; out-of-scope ⇒
//! [`Decision::Deny`]; the LLM never classifies its own risk.

use tianji_types::{Classification, Decision, ScopeRules, Target};

mod classify;
mod rules;
mod scope;
mod targets;

pub use classify::classify;
pub use rules::{AllowGranularity, AllowRule};
pub use scope::in_scope;
pub use targets::resolve_targets;

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("could not parse arguments for tool `{0}`")]
    UnparsableArgs(String),
}

/// The top-level decision: scope-check → classify → decide.
///
/// Order matters: out-of-scope is denied **before** classification, and an unknown
/// classification falls through to human approval (never auto-run).
pub fn decide(tool: &str, argv: &[String], scope: &ScopeRules, rules: &[AllowRule]) -> Decision {
    let targets = resolve_targets(tool, argv);

    // 1. Scope first. Any target outside scope is an immediate deny.
    for t in &targets {
        if !in_scope(t, scope) {
            return Decision::Deny {
                reason: format!("target {t:?} is outside engagement scope"),
            };
        }
    }

    // 2. A matching workspace allow-rule short-circuits to auto-run.
    if rules.iter().any(|r| r.matches(tool, argv, &targets)) {
        return Decision::AutoRun;
    }

    // 3. Classify and decide. Fail closed on anything not known-safe.
    match classify(tool, argv) {
        Classification::ReadOnly => Decision::AutoRun,
        Classification::Mutating | Classification::Exploit | Classification::Unknown => {
            Decision::NeedsApproval
        }
    }
}

/// Helper kept public for the orchestrator's logging/preview needs.
pub fn preview(tool: &str, argv: &[String]) -> (Vec<Target>, Classification) {
    (resolve_targets(tool, argv), classify(tool, argv))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope_with(cidr: &str) -> ScopeRules {
        ScopeRules {
            cidrs: vec![cidr.to_string()],
            ..Default::default()
        }
    }

    fn argv(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn unknown_command_needs_approval_not_autorun() {
        // Fail-closed invariant: never auto-run something we don't recognize.
        let d = decide(
            "some-unknown-tool",
            &argv(&["10.0.0.5"]),
            &scope_with("10.0.0.0/24"),
            &[],
        );
        assert_eq!(d, Decision::NeedsApproval);
    }

    #[test]
    fn out_of_scope_target_is_denied() {
        let d = decide(
            "nmap",
            &argv(&["-sV", "8.8.8.8"]),
            &scope_with("10.0.0.0/24"),
            &[],
        );
        assert!(matches!(d, Decision::Deny { .. }));
    }

    #[test]
    fn empty_scope_denies_everything() {
        // Empty scope means nothing is in scope — not "everything".
        let d = decide("ping", &argv(&["10.0.0.5"]), &ScopeRules::default(), &[]);
        assert!(matches!(d, Decision::Deny { .. }));
    }

    #[test]
    fn piped_command_must_not_autorun() {
        // `curl ... | bash` style — shell metacharacters force human review. (Seeded test;
        // classify() must treat metacharacters as Unknown/Exploit.)
        let d = decide(
            "curl",
            &argv(&["http://10.0.0.5/x.sh", "|", "bash"]),
            &scope_with("10.0.0.0/24"),
            &[],
        );
        assert_ne!(d, Decision::AutoRun);
    }
}
