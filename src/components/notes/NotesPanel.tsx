import { useEffect, useState } from "react";
import { ipc } from "../../lib/ipc";
import { onNotesUpdated } from "../../lib/events";
import type { EventDto, FindingDto, Phase } from "../../lib/types";
import { useAppStore } from "../../state/stores";
import NotesEditorModal from "./NotesEditorModal";
import ReportModal from "./ReportModal";

const AUTO_KINDS = new Set(["tool_output", "finding", "agent_msg", "phase_change", "tool_denied"]);

const SEV_STYLE: Record<string, string> = {
  critical: "bg-danger/20 text-danger ring-danger/30",
  high:     "bg-orange-500/20 text-orange-400 ring-orange-400/30",
  medium:   "bg-warn/20 text-warn ring-warn/30",
  low:      "bg-ok/20 text-ok ring-ok/30",
  info:     "bg-base-600 text-ink-faint ring-base-500",
};

export default function NotesPanel() {
  const [tab, setTab] = useState<"auto" | "notebook" | "findings">("auto");
  const [draft, setDraft] = useState("");
  const [events, setEvents] = useState<EventDto[]>([]);
  const [findings, setFindings] = useState<FindingDto[]>([]);
  const [editorOpen, setEditorOpen] = useState(false);
  const [reportOpen, setReportOpen] = useState(false);
  const [phaseFilter, setPhaseFilter] = useState<Phase | null>(null);
  const currentWorkspace = useAppStore((s) => s.workspaces.find((w) => w.id === s.currentWorkspaceId));
  const chatLen = useAppStore((s) => s.chat.length);
  const currentId = useAppStore((s) => s.currentWorkspaceId);

  const loadEvents = () => ipc.eventsQuery(100).then(setEvents).catch(() => setEvents([]));
  const loadFindings = () => ipc.findingsQuery().then(setFindings).catch(() => setFindings([]));
  const load = () => { void loadEvents(); void loadFindings(); };

  useEffect(() => {
    setPhaseFilter(null);
  }, [currentId]);

  useEffect(() => {
    setEvents([]);
    setFindings([]);
    if (!currentId) return;
    load();
    const un = onNotesUpdated(() => load());
    return () => {
      void un.then((u) => u()).catch(() => {});
    };
  }, [currentId]);

  useEffect(() => {
    if (currentId) load();
  }, [chatLen]);

  const autoNotes = events
    .filter((e) => AUTO_KINDS.has(e.kind))
    .filter((e) => !phaseFilter || e.phase === phaseFilter);
  const notebook  = events.filter((e) => e.kind === "note" && e.author === "user");

  const deleteEvent = async (id: string) => {
    await ipc.notesDelete(id).catch(() => {});
    await loadEvents();
  };

  const save = async () => {
    const text = draft.trim();
    if (!text) return;
    setDraft("");
    try { await ipc.notesAdd(text); await loadEvents(); } catch { /* no workspace */ }
  };

  return (
    <div className="flex h-full flex-col bg-base-700 text-xs">
      <div className="flex h-9 shrink-0 items-center gap-1 border-b border-base-500 px-2">
        <Tab active={tab === "auto"} onClick={() => setTab("auto")}>
          Auto
        </Tab>
        <Tab active={tab === "notebook"} onClick={() => setTab("notebook")}>
          Notes
        </Tab>
        <Tab active={tab === "findings"} onClick={() => setTab("findings")}>
          Findings
          {findings.length > 0 && (
            <span className="ml-1 inline-flex h-3.5 min-w-[14px] items-center justify-center rounded-full bg-danger/80 px-0.5 text-[9px] font-bold leading-none text-base-900">
              {findings.length}
            </span>
          )}
        </Tab>
        <button
          onClick={() => setEditorOpen(true)}
          className="ml-auto flex h-5 w-5 items-center justify-center rounded text-ink-faint hover:bg-base-600 hover:text-ink"
          title="Open notes editor"
        >
          ✎
        </button>
      </div>

      {tab === "auto" && (
        <div className="flex shrink-0 flex-wrap gap-1 border-b border-base-500 px-2 py-1.5">
          {(["all", "recon", "hypothesis", "poc", "exploit", "report"] as const).map((p) => (
            <button
              key={p}
              onClick={() => setPhaseFilter(p === "all" ? null : p)}
              className={`rounded px-1.5 py-0.5 font-mono text-[9px] transition-colors ${
                (p === "all" ? phaseFilter === null : phaseFilter === p)
                  ? "bg-accent/20 text-accent ring-1 ring-accent/30"
                  : "text-ink-faint hover:bg-base-600 hover:text-ink"
              }`}
            >
              {p}
            </button>
          ))}
        </div>
      )}
      {tab === "auto" && (
        <ul className="flex-1 space-y-0.5 overflow-auto p-2 text-ink-dim">
          {autoNotes.length === 0 && <li className="text-ink-faint">{phaseFilter ? `nothing in ${phaseFilter} phase` : "nothing yet"}</li>}
          {autoNotes.map((e) => (
            <li key={e.id} className="group flex items-start gap-1.5 border-l-2 border-base-500 pl-2">
              <span className="min-w-0 flex-1">
                <span className="text-ink-faint">{e.kind.replace(/_/g, " ")}</span>
                {" · "}
                {truncate(e.text)}
              </span>
              <button
                onClick={() => void deleteEvent(e.id)}
                className="mt-0.5 shrink-0 opacity-0 transition-opacity group-hover:opacity-100 hover:text-danger"
                title="Dismiss"
              >
                ✕
              </button>
            </li>
          ))}
        </ul>
      )}

      {tab === "notebook" && (
        <div className="flex flex-1 flex-col overflow-hidden p-2">
          <ul className="mb-2 min-h-0 flex-1 space-y-1 overflow-auto text-ink-dim">
            {notebook.length === 0 && <li className="text-ink-faint">no notes yet</li>}
            {notebook.map((e) => (
              <li key={e.id} className="group flex items-start gap-1.5 border-l-2 border-accent/40 pl-2">
                <span className="min-w-0 flex-1">{truncate(e.text)}</span>
                <button
                  onClick={() => void deleteEvent(e.id)}
                  className="mt-0.5 shrink-0 opacity-0 transition-opacity group-hover:opacity-100 hover:text-danger"
                  title="Delete"
                >
                  ✕
                </button>
              </li>
            ))}
          </ul>
          <textarea
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) { e.preventDefault(); void save(); }
            }}
            placeholder="Hypotheses, creds, threads to pull… (Ctrl+Enter)"
            className="h-20 shrink-0 resize-none rounded bg-base-800 p-2 text-ink outline-none ring-1 ring-base-500 focus:ring-accent/50"
          />
          <button
            onClick={save}
            className="mt-2 self-end rounded px-2 py-1 text-[10px] text-ink-dim ring-1 ring-base-500 hover:bg-base-600"
          >
            Save note
          </button>
        </div>
      )}

      {tab === "findings" && (
        <ul className="flex-1 space-y-1 overflow-auto p-2">
          {findings.length === 0 && (
            <li className="text-ink-faint">no findings yet — the agent logs them automatically</li>
          )}
          {findings.length > 0 && (
            <li>
              <button
                onClick={() => setReportOpen(true)}
                className="mb-1 w-full rounded border border-base-500 px-2 py-1 text-[10px] text-ink-faint hover:bg-base-600 hover:text-ink"
              >
                ↓ Generate report
              </button>
            </li>
          )}
          {findings.map((f) => (
            <li key={f.id} className="rounded-md border border-base-500 bg-base-800 px-2.5 py-2">
              <div className="mb-1 flex items-center gap-1.5">
                <span
                  className={`inline-flex items-center rounded px-1.5 py-0.5 text-[9px] font-bold uppercase tracking-wide ring-1 ${SEV_STYLE[f.severity] ?? SEV_STYLE.info}`}
                >
                  {f.severity}
                </span>
                <span className="font-mono text-[10px] text-ink-faint">{f.target}</span>
              </div>
              <p className="text-[11px] leading-relaxed text-ink-dim">{f.summary}</p>
            </li>
          ))}
        </ul>
      )}

      {editorOpen && (
        <NotesEditorModal events={events} onClose={() => setEditorOpen(false)} onChanged={loadEvents} />
      )}
      {reportOpen && (
        <ReportModal
          findings={findings}
          workspaceName={currentWorkspace?.name ?? "engagement"}
          onClose={() => setReportOpen(false)}
        />
      )}
    </div>
  );
}

function truncate(s: string): string {
  return s.length > 120 ? s.slice(0, 120) + "…" : s;
}

function Tab({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      onClick={onClick}
      className={`label flex items-center rounded-md px-2 py-1 transition-colors ${
        active ? "bg-accent/12 !text-accent ring-1 ring-accent/25" : "hover:bg-base-600"
      }`}
    >
      {children}
    </button>
  );
}
