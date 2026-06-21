use tianji_types::ToolSpec;

pub struct McpHost {
    /// Tools available to the top-level orchestrator (includes delegation).
    orchestrator_tools: Vec<ToolSpec>,
    /// Tools available to sub-agents — delegation excluded to prevent infinite recursion.
    subagent_tools: Vec<ToolSpec>,
}

impl McpHost {
    pub fn new() -> Self {
        let base = vec![run_command_spec(), record_finding_spec()];
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

fn record_finding_spec() -> ToolSpec {
    ToolSpec {
        name: "record_finding".to_string(),
        description: "Log a structured security finding to the engagement report. Call this \
                      whenever you discover an open port, vulnerable service, misconfiguration, \
                      or any other noteworthy security issue."
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
