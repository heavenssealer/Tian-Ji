//! Command classification (DESIGN.md §4.1). Layered allow/deny matching; **everything
//! unmatched returns [`Classification::Unknown`]** so the caller fails closed.
//!
//! TODO(open-question §11.1): expand the seed lists; the real hardening is here.

use tianji_types::Classification;

/// Shell metacharacters that turn a single command into something we can't reason about. Their
/// presence forces human review regardless of the base tool.
const SHELL_METACHARS: &[&str] = &["|", "||", "&", "&&", ";", ">", ">>", "<", "$(", "`"];

/// Tools whose default invocation only reads/observes. Conservative seed — grow deliberately.
const READ_ONLY_TOOLS: &[&str] = &["ping", "whois", "nslookup", "dig", "host", "whatweb"];

/// Flags that flip an otherwise-readonly tool into something mutating/dangerous.
const DANGEROUS_FLAGS: &[(&str, &[&str])] = &[
    // nmap NSE scripts can write files / brute-force / exploit.
    ("nmap", &["--script", "-sU", "-O"]),
    // curl writing to disk or following arbitrary redirects to upload.
    ("curl", &["-o", "-O", "--upload-file", "-T"]),
];

pub fn classify(tool: &str, argv: &[String]) -> Classification {
    // Any shell metacharacter ⇒ we can't bound the behavior ⇒ treat as Exploit-tier.
    if argv.iter().any(|a| SHELL_METACHARS.contains(&a.as_str())) {
        return Classification::Exploit;
    }

    if let Some((_, flags)) = DANGEROUS_FLAGS.iter().find(|(t, _)| *t == tool) {
        if argv.iter().any(|a| flags.contains(&a.as_str())) {
            return Classification::Mutating;
        }
    }

    // Plain nmap service/version scan is read-only; with dangerous flags it was caught above.
    if tool == "nmap" {
        return Classification::ReadOnly;
    }

    if READ_ONLY_TOOLS.contains(&tool) {
        return Classification::ReadOnly;
    }

    Classification::Unknown
}
