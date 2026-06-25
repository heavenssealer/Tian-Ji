use tianji_types::ToolSpec;

pub struct McpHost {
    /// Tools available to the top-level orchestrator (includes delegation).
    orchestrator_tools: Vec<ToolSpec>,
    /// Tools available to sub-agents — delegation excluded to prevent infinite recursion.
    subagent_tools: Vec<ToolSpec>,
}

impl McpHost {
    pub fn new() -> Self {
        let base = vec![
            run_command_spec(),
            record_finding_spec(),
            log_attempt_spec(),
            recall_spec(),
            use_skill_spec(),
        ];
        let mut orchestrator = base.clone();
        orchestrator.push(delegate_agent_spec());
        Self { orchestrator_tools: orchestrator, subagent_tools: base }
    }

    /// Tool list for the top-level orchestrator turn.
    pub fn orchestrator_specs(&self) -> &[ToolSpec] {
        &self.orchestrator_tools
    }

    /// Tool list for a sub-agent turn (no delegation).
    pub fn subagent_specs(&self) -> &[ToolSpec] {
        &self.subagent_tools
    }
}

impl Default for McpHost {
    fn default() -> Self {
        Self::new()
    }
}

fn run_command_spec() -> ToolSpec {
    ToolSpec {
        name: "run_command".to_string(),
        description:
            "Run a single system command. Subject to scope + tiered approval policy.\n\
             - `tool` is the bare executable name only (e.g. \"nmap\"), NEVER \"run_command\".\n\
             - `argv` is the argument list, one element per token.\n\
             - The command runs WITHOUT a shell, so pipes/redirects/globs do NOT work as separate \
             argv tokens. To use `| > >> && ;` etc., set tool=\"bash\" and argv=[\"-c\", \"<the \
             whole command line as one string>\"].\n\
             - Quote any single argument that itself contains spaces or shell metacharacters \
             (e.g. a POST body \"username=admin&password=admin\") as ONE argv element.\n\
             - Read-only commands are cached per session: re-issuing an identical command returns \
             the cached result. Do NOT repeat scans you have already run — reuse earlier output."
                .to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "tool": { "type": "string", "description": "bare executable, e.g. nmap, curl, bash" },
                "argv": { "type": "array", "items": { "type": "string" }, "description": "arguments, one token per element" }
            },
            "required": ["tool", "argv"]
        }),
    }
}

fn use_skill_spec() -> ToolSpec {
    ToolSpec {
        name: "use_skill".to_string(),
        description: "Load an installed skill (a proven playbook — see the catalog in your system \
                      prompt). TWO-LEVEL: call use_skill(name=\"ctf-web\") first to get the router \
                      (a list of technique files with one-line descriptions). \
                      THEN — before doing anything else — pick the file that matches your target \
                      and call use_skill AGAIN with file=\"<that filename>\" (e.g. \
                      file=\"sql-injection.md\"). The second call loads the detailed step-by-step \
                      procedure. FOLLOW those steps and run the commands it gives you. \
                      Do NOT just acknowledge the router list — you MUST load a specific file \
                      and execute its procedure."
            .to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "the skill name exactly as shown in the catalog, e.g. \"ctf-web\" or \"ctf-pwn\""
                },
                "file": {
                    "type": "string",
                    "description": "optional: a specific technique file to load (a name from the skill's bundled-files list), e.g. \"sql-injection.md\". Omit to load the skill's main router (SKILL.md)."
                }
            },
            "required": ["name"]
        }),
    }
}

fn recall_spec() -> ToolSpec {
    ToolSpec {
        name: "recall".to_string(),
        description: "Retrieve the FULL, un-summarized text of earlier work from the engagement log. \
                      Tool outputs are condensed when they enter the conversation and old turns get \
                      compacted, but the raw is always stored — use recall when you need a detail \
                      that was dropped (the complete nmap/gobuster output, a long HTTP response, an \
                      earlier finding). Search by a keyword that would appear in it (an IP, port, \
                      path, tool name, CVE, etc.). Returns the matching stored events verbatim."
            .to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "keyword/substring to find, e.g. \"nmap 10.0.0.5\" or \"/admin/config.php\""
                }
            },
            "required": ["query"]
        }),
    }
}

fn log_attempt_spec() -> ToolSpec {
    ToolSpec {
        name: "log_attempt".to_string(),
        description: "Record an approach you are taking and its outcome, so you (and the operator) \
                      have a timestamped trail and you never repeat a dead end. Call it TWICE per \
                      approach: once with status=\"trying\" before you start (e.g. action=\"Exploit \
                      CVE-2025-57819 SQLi→RCE on /admin/ajax.php\"), and once after with \
                      status=\"succeeded\", \"failed\", or \"abandoned\" plus a one-line `result`. \
                      Before starting any new approach, check the attempt log already provided to \
                      you — do NOT retry an approach already marked failed/abandoned."
            .to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "description": "the approach/path, e.g. \"brute-force SSH with rockyou\" or \"exploit CVE-2025-57819\""
                },
                "status": {
                    "type": "string",
                    "enum": ["trying", "succeeded", "failed", "abandoned"],
                    "description": "trying = starting now; succeeded/failed/abandoned = outcome"
                },
                "result": {
                    "type": "string",
                    "description": "one-line outcome / why it failed / what it yielded (optional for status=trying)"
                }
            },
            "required": ["action", "status"]
        }),
    }
}

fn record_finding_spec() -> ToolSpec {
    ToolSpec {
        name: "record_finding".to_string(),
        description: "Record a CONFIRMED, report-worthy finding or a key milestone — a captured \
                      flag, a working shell/RCE, valid credentials, or a vulnerability you have \
                      actually verified. Include concrete proof in the summary (the flag value, the \
                      webshell URL, the cred pair, the command that worked). Do NOT log routine \
                      enumeration (open ports, version banners, \"panel exposed\") as findings — \
                      those already live in the event log; logging them here floods the report with \
                      duplicates. One finding per distinct issue; never re-record the same thing."
            .to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "severity": {
                    "type": "string",
                    "enum": ["critical", "high", "medium", "low", "info"],
                    "description": "CVSS-style severity"
                },
                "target": {
                    "type": "string",
                    "description": "affected host/service, e.g. 192.168.1.25:22/ssh"
                },
                "summary": {
                    "type": "string",
                    "description": "concise one-line description of the finding"
                }
            },
            "required": ["severity", "target", "summary"]
        }),
    }
}

fn delegate_agent_spec() -> ToolSpec {
    ToolSpec {
        name: "delegate_to_agent".to_string(),
        description: "Delegate a focused sub-task to a specialist agent. The sub-agent runs \
                      its own tool loop within a token budget and returns a summary of what it \
                      found. Use this to parallelise workstreams or bring in a specialist. \
                      Do NOT re-delegate from a sub-agent (no nesting)."
            .to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "agent": {
                    "type": "string",
                    "enum": ["recon", "web", "exploit"],
                    "description": "recon = port/service enumeration; web = HTTP/webapp; exploit = vuln verification and PoC"
                },
                "objective": {
                    "type": "string",
                    "description": "Clear one-sentence task for the sub-agent, naming the target explicitly"
                },
                "token_budget": {
                    "type": "integer",
                    "description": "Max tokens to spend on this sub-task (default 4000, max 8000)"
                }
            },
            "required": ["agent", "objective"]
        }),
    }
}
