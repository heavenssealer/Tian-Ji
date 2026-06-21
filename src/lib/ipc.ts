// Typed wrappers over Tauri's invoke() — the inbound half of the IPC contract (SKELETON §5).
// One function per #[tauri::command]; keep names in lockstep with src-tauri/src/commands/.

import { invoke } from "@tauri-apps/api/core";
import type { ApprovalResolution, EventDto, FindingDto, Phase, PolicyRuleDto, ProfileFact, ProfileScope, WorkspaceInfo } from "./types";

export const ipc = {
  workspaceList: () => invoke<WorkspaceInfo[]>("workspace_list"),
  workspaceCreate: (name: string, scopeCidrs: string[]) =>
    invoke<WorkspaceInfo>("workspace_create", { name, scopeCidrs }),
  workspaceOpen: (id: string) => invoke<WorkspaceInfo>("workspace_open", { id }),
  workspaceSetPhase: (phase: Phase) => invoke<void>("workspace_set_phase", { phase }),
  workspaceSetScope: (cidrs: string[], hostnames: string[], urlDomains: string[]) =>
    invoke<void>("workspace_set_scope", { cidrs, hostnames, urlDomains }),
  workspaceRename: (id: string, name: string) => invoke<void>("workspace_rename", { id, name }),
  workspaceDelete: (id: string) => invoke<void>("workspace_delete", { id }),

  terminalSpawn: (title: string) => invoke<string>("terminal_spawn", { title }),
  terminalWrite: (id: string, data: string) => invoke<void>("terminal_write", { id, data }),
  terminalResize: (id: string, cols: number, rows: number) =>
    invoke<void>("terminal_resize", { id, cols, rows }),
  terminalClose: (id: string) => invoke<void>("terminal_close", { id }),

  agentPrompt: (prompt: string) => invoke<void>("agent_prompt", { prompt }),
  agentRunGoal: (goal: string) => invoke<void>("agent_run_goal", { goal }),
  agentSetFreeMode: (enabled: boolean) => invoke<void>("agent_set_free_mode", { enabled }),
  agentSetAutonomous: (enabled: boolean) => invoke<void>("agent_set_autonomous", { enabled }),
  agentSetTokenBudget: (tokens: number) => invoke<void>("agent_set_token_budget", { tokens }),
  agentDistillProfile: () => invoke<void>("agent_distill_profile"),
  profileFactsList: () => invoke<ProfileFact[]>("profile_facts_list"),
  profileFactAdd: (text: string, scope: ProfileScope) => invoke<void>("profile_fact_add", { text, scope }),
  profileFactRemove: (id: number, scope: ProfileScope) => invoke<void>("profile_fact_remove", { id, scope }),
  profileFactPin: (id: number, scope: ProfileScope, pinned: boolean) => invoke<void>("profile_fact_pin", { id, scope, pinned }),
  agentCancel: () => invoke<void>("agent_cancel"),
  agentNewSession: (sessionId: string) => invoke<void>("agent_new_session", { sessionId }),
  agentSwitchSession: (sessionId: string) => invoke<void>("agent_switch_session", { sessionId }),

  policyResolve: (token: string, resolution: ApprovalResolution) =>
    invoke<void>("policy_resolve", { token, resolution }),
  policyRulesList: () => invoke<PolicyRuleDto[]>("policy_rules_list"),
  policyRuleRemove: (ruleJson: string, isGlobal: boolean) =>
    invoke<void>("policy_rule_remove", { ruleJson, isGlobal }),

  notesAdd: (markdown: string) => invoke<void>("notes_add", { markdown }),
  notesDelete: (id: string) => invoke<void>("notes_delete", { id }),
  notesUpdate: (id: string, text: string) => invoke<void>("notes_update", { id, text }),
  eventsQuery: (limit: number) => invoke<EventDto[]>("events_query", { limit }),
  findingsQuery: () => invoke<FindingDto[]>("findings_query"),

  settingsSetApiKey: (key: string) => invoke<void>("settings_set_api_key", { key }),
  settingsHasApiKey: () => invoke<boolean>("settings_has_api_key"),
  settingsSetSudoPassword: (password: string) => invoke<void>("settings_set_sudo_password", { password }),
  settingsHasSudoPassword: () => invoke<boolean>("settings_has_sudo_password"),
  settingsListModels: () => invoke<string[]>("settings_list_models"),
  settingsGetModel: () => invoke<string>("settings_get_model"),
  settingsSetModel: (model: string) => invoke<void>("settings_set_model", { model }),
  settingsGetOllamaHost: () => invoke<string>("settings_get_ollama_host"),
  settingsSetOllamaHost: (host: string) => invoke<void>("settings_set_ollama_host", { host }),
};
