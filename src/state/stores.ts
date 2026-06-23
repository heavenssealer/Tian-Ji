import { create } from "zustand";
import type { ActiveAgent, AgentDelta, ChatSession, Phase, ProposedCall, WorkspaceInfo } from "../lib/types";

export interface ChatLine {
  kind: "user" | "agent" | "tool" | "error" | "finding" | "info";
  text: string;
}

export interface TokenCount { input: number; output: number }

const DEFAULT_SESSION: ChatSession = { id: "default", name: "Chat 1" };

interface AppUiState {
  workspaces: WorkspaceInfo[];
  currentWorkspaceId: string | null;
  phase: Phase;
  // Session management
  sessions: ChatSession[];
  currentSessionId: string;
  sessionLines: Record<string, ChatLine[]>;
  // chat = current session's lines (kept in sync for backward compat)
  chat: ChatLine[];
  pendingApproval: ProposedCall | null;
  isRunning: boolean;
  isAutonomous: boolean;
  isFreeMode: boolean;
  // Standalone mode: when on, a submitted message launches an autonomous goal run instead of a
  // single chat turn. `goalIteration` tracks the current self-directed cycle while a run is live.
  isStandalone: boolean;
  goalIteration: number;
  // Cumulative token budget cap (0 = unlimited). Persisted backend-side; mirrored here for the UI.
  tokenBudget: number;
  totalTokens: TokenCount;
  activeAgents: ActiveAgent[];
  // The model the backend provider is currently built for (mirrors the active chat's model).
  activeModel: string;

  setWorkspaces: (w: WorkspaceInfo[]) => void;
  setCurrentWorkspace: (id: string | null) => void;
  setPhase: (p: Phase) => void;
  // Session actions
  newSession: () => ChatSession;
  switchSession: (id: string) => void;
  setActiveModel: (m: string) => void;
  setSessionModel: (id: string, model: string) => void;
  // Chat actions (operate on current session)
  setChat: (lines: ChatLine[]) => void;
  pushChat: (line: ChatLine) => void;
  applyDelta: (d: AgentDelta) => void;
  setPendingApproval: (c: ProposedCall | null) => void;
  setRunning: (v: boolean) => void;
  setAutonomous: (v: boolean) => void;
  setFreeMode: (v: boolean) => void;
  setStandalone: (v: boolean) => void;
  setTokenBudget: (n: number) => void;
}

let sessionCounter = 1;

export const useAppStore = create<AppUiState>((set, get) => ({
  workspaces: [],
  currentWorkspaceId: null,
  phase: "recon",
  sessions: [DEFAULT_SESSION],
  currentSessionId: DEFAULT_SESSION.id,
  sessionLines: { [DEFAULT_SESSION.id]: [] },
  chat: [],
  pendingApproval: null,
  isRunning: false,
  isAutonomous: false,
  isFreeMode: false,
  isStandalone: false,
  goalIteration: 0,
  tokenBudget: 0,
  totalTokens: { input: 0, output: 0 },
  activeAgents: [{ name: "orchestrator", status: "idle" }],
  activeModel: "",

  setWorkspaces: (workspaces) => set({ workspaces }),

  setCurrentWorkspace: (currentWorkspaceId) => {
    const session = DEFAULT_SESSION;
    set({
      currentWorkspaceId,
      sessions: [session],
      currentSessionId: session.id,
      sessionLines: { [session.id]: [] },
      chat: [],
      isRunning: false,
      isAutonomous: false,
      isFreeMode: false,
      isStandalone: false,
      goalIteration: 0,
      totalTokens: { input: 0, output: 0 },
      activeAgents: [{ name: "orchestrator", status: "idle" }],
    });
    sessionCounter = 1;
  },

  setPhase: (phase) => set({ phase }),

  newSession: () => {
    sessionCounter += 1;
    // Inherit the current chat's model so a new chat starts on the same model unless changed.
    const model = get().activeModel || undefined;
    const session: ChatSession = { id: `session-${Date.now()}`, name: `Chat ${sessionCounter}`, model };
    set((s) => ({
      sessions: [...s.sessions, session],
      currentSessionId: session.id,
      sessionLines: { ...s.sessionLines, [session.id]: [] },
      chat: [],
      isRunning: false,
    }));
    return session;
  },

  switchSession: (id: string) => {
    const lines = get().sessionLines[id] ?? [];
    set({ currentSessionId: id, chat: lines, isRunning: false });
  },

  setActiveModel: (activeModel) => set({ activeModel }),

  setSessionModel: (id, model) =>
    set((s) => ({
      activeModel: id === s.currentSessionId ? model : s.activeModel,
      sessions: s.sessions.map((sess) => (sess.id === id ? { ...sess, model } : sess)),
    })),

  setChat: (lines) =>
    set((s) => ({
      chat: lines,
      sessionLines: { ...s.sessionLines, [s.currentSessionId]: lines },
    })),

  pushChat: (line) =>
    set((s) => {
      const updated = [...s.chat, line];
      return {
        chat: updated,
        sessionLines: { ...s.sessionLines, [s.currentSessionId]: updated },
      };
    }),

  setRunning: (isRunning) =>
    set((s) => ({
      isRunning,
      activeAgents: s.activeAgents.map((a) =>
        a.name === "orchestrator" ? { ...a, status: isRunning ? "running" : "idle" } : a
      ),
    })),

  applyDelta: (d) =>
    set((s) => {
      const updateChat = (newChat: ChatLine[]) => ({
        chat: newChat,
        sessionLines: { ...s.sessionLines, [s.currentSessionId]: newChat },
      });

      switch (d.type) {
        case "text_delta": {
          const last = s.chat[s.chat.length - 1];
          if (last?.kind === "agent") {
            const updated = [...s.chat];
            updated[updated.length - 1] = { kind: "agent", text: last.text + (d.text ?? "") };
            return updateChat(updated);
          }
          return updateChat([...s.chat, { kind: "agent", text: d.text ?? "" }]);
        }
        case "tool_call":
          return updateChat([...s.chat, { kind: "tool", text: `$ ${d.text ?? ""}` }]);
        case "tool_output":
          return updateChat([...s.chat, { kind: "tool", text: d.text ?? "" }]);
        case "denied":
          return updateChat([...s.chat, { kind: "error", text: `⊘ Denied: ${d.text ?? ""}` }]);
        case "finding":
          return updateChat([...s.chat, { kind: "finding", text: d.text ?? "" }]);
        case "compacted": {
          const n = d.input ?? 0;
          return updateChat([
            ...s.chat,
            { kind: "info", text: `🗜 Compacted ${n} earlier message${n === 1 ? "" : "s"} into a summary to save tokens. Full raw output stays in the event log.` },
          ]);
        }
        case "skill_used":
          return updateChat([
            ...s.chat,
            { kind: "info", text: `🧩 Using skill: ${d.text ?? ""}` },
          ]);
        case "error":
          return updateChat([...s.chat, { kind: "error", text: d.text ?? "" }]);
        case "token_usage":
          return {
            totalTokens: {
              input: s.totalTokens.input + (d.input ?? 0),
              output: s.totalTokens.output + (d.output ?? 0),
            },
          };
        case "subagent_start": {
          const newAgent: ActiveAgent = {
            name: d.agentName ?? "agent",
            status: "running",
            objective: d.objective,
          };
          const newChat = [...s.chat, {
            kind: "agent" as const,
            text: `▷ **[${d.agentName ?? "agent"} agent]** ${d.objective ?? ""}`,
          }];
          return {
            activeAgents: [...s.activeAgents.filter((a) => a.name !== newAgent.name), newAgent],
            ...updateChat(newChat),
          };
        }
        case "subagent_text": {
          const label = `[${d.agentName ?? "agent"}]`;
          const last = s.chat[s.chat.length - 1];
          if (last?.kind === "agent" && last.text.startsWith(`▷ **${label}`)) {
            // First text chunk after the header — start a new line
          }
          // Accumulate into a sub-agent bubble identified by its prefix
          const marker = `[sub:${d.agentName ?? ""}]`;
          const lastSub = s.chat.length > 0 ? s.chat[s.chat.length - 1] : null;
          if (lastSub?.kind === "agent" && lastSub.text.startsWith(marker)) {
            const updated = [...s.chat];
            updated[updated.length - 1] = {
              kind: "agent",
              text: marker + lastSub.text.slice(marker.length) + (d.text ?? ""),
            };
            return updateChat(updated);
          }
          return updateChat([...s.chat, { kind: "agent", text: marker + (d.text ?? "") }]);
        }
        case "subagent_end": {
          return {
            activeAgents: s.activeAgents.map((a) =>
              a.name === (d.agentName ?? "") ? { ...a, status: "done" } : a
            ),
          };
        }
        case "goal_start":
          return updateChat([
            ...s.chat,
            { kind: "finding", text: `🎯 Standalone run started — objective: ${d.text ?? ""}` },
          ]);
        case "goal_iteration":
          return { goalIteration: d.input ?? 0 };
        case "goal_end":
          return {
            goalIteration: 0,
            ...updateChat([
              ...s.chat,
              { kind: "finding", text: `🎯 Standalone run ${d.text ?? "ended"} after ${d.input ?? 0} iteration(s).` },
            ]),
          };
        case "turn_end":
          return {
            isRunning: false,
            activeAgents: s.activeAgents.map((a) => ({ ...a, status: "idle" as const })),
          };
        default:
          return {};
      }
    }),

  setPendingApproval: (pendingApproval) => set({ pendingApproval }),

  setAutonomous: (isAutonomous) => set({ isAutonomous }),
  setFreeMode: (isFreeMode) => set({ isFreeMode }),
  setStandalone: (isStandalone) => set({ isStandalone }),
  setTokenBudget: (tokenBudget) => set({ tokenBudget }),
}));
