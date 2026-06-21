import { useEffect, useRef, useState } from "react";
import WorkspaceRail from "./WorkspaceRail";
import PhaseTimeline from "./PhaseTimeline";
import TerminalGrid from "../terminals/TerminalGrid";
import AgentChat from "../agent/AgentChat";
import NotesPanel from "../notes/NotesPanel";
import SettingsButton from "../SettingsButton";
import { useBackendEvents } from "../../lib/backend";
import { useAppStore } from "../../state/stores";
import { ipc } from "../../lib/ipc";

const RIGHT_MIN = 260;
const RIGHT_MAX = 700;

// Fired by both the global keydown handler and the xterm pane handler after saving.
export const NOTEBOOK_SAVED_EVENT = "tianji:notebook-saved";

// Five zones: full-height workspace rail | top bar + terminal grid | agent chat + notes.
export default function AppShell() {
  useBackendEvents();
  const workspaces = useAppStore((s) => s.workspaces);
  const currentId = useAppStore((s) => s.currentWorkspaceId);
  const phase = useAppStore((s) => s.phase);
  const current = workspaces.find((w) => w.id === currentId);

  const [rightWidth, setRightWidth] = useState(340);
  const drag = useRef<{ startX: number; startW: number } | null>(null);

  // Toast shown after a successful Ctrl+Shift+N capture.
  const [toastVisible, setToastVisible] = useState(false);
  const toastTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const showToast = () => {
    setToastVisible(true);
    if (toastTimer.current) clearTimeout(toastTimer.current);
    toastTimer.current = setTimeout(() => setToastVisible(false), 1800);
  };

  useEffect(() => {
    // Global Ctrl+Shift+N handler — captures DOM selection (agent chat, notes, etc.)
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.ctrlKey && e.shiftKey && e.key === "N") {
        const sel = window.getSelection()?.toString().trim();
        if (sel) {
          ipc.notesAdd(sel).then(() => window.dispatchEvent(new CustomEvent(NOTEBOOK_SAVED_EVENT)));
        }
      }
    };
    // Listen for the save event from any source (DOM selection or xterm handler).
    const onSaved = () => showToast();

    window.addEventListener("keydown", onKeyDown);
    window.addEventListener(NOTEBOOK_SAVED_EVENT, onSaved);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener(NOTEBOOK_SAVED_EVENT, onSaved);
    };
  }, []);

  const startDrag = (e: React.MouseEvent) => {
    e.preventDefault();
    drag.current = { startX: e.clientX, startW: rightWidth };
    const onMove = (ev: MouseEvent) => {
      if (!drag.current) return;
      const w = drag.current.startW + (drag.current.startX - ev.clientX);
      setRightWidth(Math.max(RIGHT_MIN, Math.min(RIGHT_MAX, w)));
    };
    const onUp = () => {
      drag.current = null;
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };

  return (
    <div
      className="grid h-screen w-full grid-rows-[48px_minmax(0,1fr)] overflow-hidden bg-base-900 text-ink"
      style={{ gridTemplateColumns: `210px minmax(0,1fr) ${rightWidth}px` }}
    >
      {/* Capture-to-notebook toast */}
      {toastVisible && (
        <div className="pointer-events-none fixed bottom-5 left-1/2 z-50 -translate-x-1/2 rounded-lg border border-ok/30 bg-base-800 px-3 py-1.5 text-[12px] text-ok shadow-lg">
          ✓ Saved to notebook
        </div>
      )}
      {/* Full-height left rail with brand block */}
      <aside className="col-start-1 row-span-2 flex min-h-0 flex-col border-r border-base-500 bg-base-800">
        <Brand />
        <WorkspaceRail />
      </aside>

      {/* Top bar over the main + right columns */}
      <header className="col-start-2 col-span-2 row-start-1 flex items-center gap-3 border-b border-base-500 px-4">
        <span className="text-[13px] font-medium text-ink">
          {current ? current.name : "No workspace"}
        </span>
        {current && (
          <span className="label rounded bg-base-700 px-2 py-0.5 !text-accent">{phase}</span>
        )}
        <div className="ml-auto flex items-center gap-3">
          <SettingsButton />
          <span className="flex items-center gap-1.5 text-[11px] text-ink-faint">
            <span className="h-1.5 w-1.5 rounded-full bg-ok" /> v0.1
          </span>
        </div>
      </header>

      {/* Center: phase timeline + terminal grid */}
      <main className="col-start-2 row-start-2 flex min-h-0 min-w-0 flex-col">
        <PhaseTimeline />
        <TerminalGrid />
      </main>

      {/* Right: agent chat (top) + notes (bottom). Left border doubles as drag handle. */}
      <aside className="relative col-start-3 row-start-2 flex min-h-0 flex-col border-l border-base-500 bg-base-700">
        <div
          className="absolute inset-y-0 left-0 z-20 w-1 cursor-col-resize transition-colors hover:bg-accent/50"
          onMouseDown={startDrag}
          title="Drag to resize"
        />
        <div className="min-h-0 flex-1">
          <AgentChat />
        </div>
        <div className="h-2/5 border-t border-base-500">
          <NotesPanel />
        </div>
      </aside>
    </div>
  );
}

function Brand() {
  return (
    <div className="flex h-12 shrink-0 items-center gap-2.5 border-b border-base-500 px-3">
      <div className="flex h-8 w-8 items-center justify-center rounded-md bg-accent/15 font-mono text-[15px] text-accent ring-1 ring-accent/30">
        天
      </div>
      <div className="leading-tight">
        <div className="text-[13px] font-medium text-ink">Tiān Jī</div>
        <div className="text-[10px] text-ink-faint">Pentest · AI operator</div>
      </div>
    </div>
  );
}
