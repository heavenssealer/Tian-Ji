// TS mirror of `tianji-types`. Hand-maintained for now; switch to `ts-rs`-generated types so
// the IPC contract can't silently diverge (SKELETON §6).

export type Phase = "recon" | "hypothesis" | "poc" | "exploit" | "report";

export type Classification = "read_only" | "mutating" | "exploit" | "unknown";

export type Author = "user" | "agent";

export interface WorkspaceInfo {
  id: string;
  name: string;
  phase: Phase;
  scopeCidrs: string[];
  scopeHostnames: string[];
  scopeUrlDomains: string[];
}

export interface ProposedCall {
  token: string;
  tool: string;
  argv: string[];
  targets: string[];
  classification: Classification;
}

export type ApprovalResolution =
  | { kind: "approve_once" }
  | { kind: "approve_edited"; argv: string[] }
  | { kind: "deny"; reason: string }
  | { kind: "always_allow"; granularity: "exact_command" | "tool_flag_shape" | "whole_tool" };

export interface AgentDelta {
  type: "text_delta" | "tool_call" | "tool_output" | "turn_end" | "error" | "denied" | "finding" | "token_usage"
      | "subagent_start" | "subagent_text" | "subagent_end";
  text?: string;
  input?: number;
  output?: number;
  agentName?: string;
  objective?: string;
}

export type AgentStatus = "idle" | "running" | "done";

export interface ActiveAgent {
  name: string;
  status: AgentStatus;
  objective?: string;
}

export interface EventDto {
  id: string;
  kind: string;
  author: Author;
  phase: string;
  text: string;
  ts: string;
}

export interface PolicyRuleDto {
  tool: string;
  granularity: "exact_command" | "tool_flag_shape" | "whole_tool";
  fingerprint: string[];
  scope: "workspace" | "global";
  ruleJson: string;
}

export interface ChatSession {
  id: string;
  name: string;
}

export type Severity = "critical" | "high" | "medium" | "low" | "info";

export interface FindingDto {
  id: string;
  severity: Severity;
  target: string;
  summary: string;
}
