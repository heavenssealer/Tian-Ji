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
      | "subagent_start" | "subagent_text" | "subagent_end"
      | "goal_start" | "goal_iteration" | "goal_end" | "compacted" | "skill_used";
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

/** RTK (Rust Token Killer) status reported by the backend. `active` = enabled && available. */
export interface RtkStatus {
  enabled: boolean;
  available: boolean;
  path: string | null;
}

/** Installed Agent Skills discovered by the backend. */
export interface SkillsStatus {
  count: number;
  names: string[];
  dirs: string[];
}

export interface ChatSession {
  id: string;
  name: string;
  /** Per-chat model id (e.g. "claude-sonnet-4-6" or "ollama:llama3.1"). Lets each chat run a
   *  different model; undefined means "use the current/last model". */
  model?: string;
}

export type ProfileScope = "global" | "workspace";

export interface ProfileFact {
  id: number;
  text: string;
  pinned: boolean;
  scope: ProfileScope;
}

export type Severity = "critical" | "high" | "medium" | "low" | "info";

export interface FindingDto {
  id: string;
  severity: Severity;
  target: string;
  summary: string;
}
