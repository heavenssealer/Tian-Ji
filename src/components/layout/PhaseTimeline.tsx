// Top phase bar. The current phase drives the active agent prompt/toolset and stamps every
// event; selecting a phase persists it (DESIGN.md §8).

import { ipc } from "../../lib/ipc";
import type { Phase } from "../../lib/types";
import { useAppStore } from "../../state/stores";

const PHASES: Phase[] = ["recon", "hypothesis", "poc", "exploit", "report"];
const LABEL: Record<Phase, string> = {
  recon: "recon",
  hypothesis: "hypothesis",
  poc: "PoC",
  exploit: "exploit",
  report: "report",
};

export default function PhaseTimeline() {
  const phase = useAppStore((s) => s.phase);
  const setPhase = useAppStore((s) => s.setPhase);

  const select = (p: Phase) => {
    setPhase(p);
    void ipc.workspaceSetPhase(p).catch(() => {});
  };

  return (
    <div className="flex h-9 shrink-0 items-center gap-1 border-b border-base-500 bg-base-800 px-3">
      <span className="label mr-2">Phase</span>
      {PHASES.map((p, i) => (
        <span key={p} className="flex items-center gap-1">
          <button
            onClick={() => select(p)}
            className={`rounded-md px-2.5 py-0.5 text-[11px] font-mono transition-colors ${
              p === phase
                ? "bg-accent/15 text-accent ring-1 ring-accent/40"
                : "text-ink-faint hover:bg-base-600 hover:text-ink-dim"
            }`}
          >
            {LABEL[p]}
          </button>
          {i < PHASES.length - 1 && <span className="text-base-400">›</span>}
        </span>
      ))}
    </div>
  );
}
