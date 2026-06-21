// Typed subscriptions over Tauri's listen() — the push half of the IPC contract (SKELETON §5).
// Channel names mirror src-tauri/src/events.rs exactly.

import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { AgentDelta, ProposedCall } from "./types";

export const channels = {
  ptyOutput: "pty://output",
  agentDelta: "agent://delta",
  agentApprovalRequest: "agent://approval_request",
  notesUpdated: "notes://updated",
  eventAppended: "event://appended",
} as const;

export function onPtyOutput(cb: (p: { terminal_id: string; chunk: number[] }) => void) {
  return listen<{ terminal_id: string; chunk: number[] }>(channels.ptyOutput, (e) => cb(e.payload));
}

export function onAgentDelta(cb: (d: AgentDelta) => void): Promise<UnlistenFn> {
  return listen<AgentDelta>(channels.agentDelta, (e) => cb(e.payload));
}

export function onApprovalRequest(cb: (c: ProposedCall) => void): Promise<UnlistenFn> {
  return listen<ProposedCall>(channels.agentApprovalRequest, (e) => cb(e.payload));
}

export function onNotesUpdated(cb: () => void): Promise<UnlistenFn> {
  return listen(channels.notesUpdated, () => cb());
}
