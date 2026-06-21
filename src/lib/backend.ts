// Bridges backend push-events into the zustand store. Call useBackendEvents() once near the
// root; it subscribes on mount and cleans up on unmount. Safe to no-op outside Tauri (the
// listen() calls simply never fire in a plain browser).

import { useEffect } from "react";
import { onAgentDelta, onApprovalRequest } from "./events";
import { useAppStore } from "../state/stores";

export function useBackendEvents() {
  const applyDelta = useAppStore((s) => s.applyDelta);
  const setPendingApproval = useAppStore((s) => s.setPendingApproval);

  useEffect(() => {
    const unlisteners: Array<Promise<() => void>> = [
      onAgentDelta((d) => applyDelta(d)),
      onApprovalRequest((c) => setPendingApproval(c)),
    ];
    return () => {
      unlisteners.forEach((p) => p.then((un) => un()).catch(() => {}));
    };
  }, [applyDelta, setPendingApproval]);
}
