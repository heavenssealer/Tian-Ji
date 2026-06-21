// Full-screen notes editor. Shows auto-notes (read-only, dismissable) and user notebook entries
// (editable inline). Opened via the ✎ button in the NotesPanel header.

import { useState } from "react";
import { ipc } from "../../lib/ipc";
import type { EventDto } from "../../lib/types";

const AUTO_KINDS = new Set(["tool_output", "finding", "agent_msg", "phase_change", "tool_denied"]);

interface Props {
  events: EventDto[];
  onClose: () => void;
  onChanged: () => void;
}

export default function NotesEditorModal({ events, onClose, onChanged }: Props) {
  const autoNotes = events.filter((e) => AUTO_KINDS.has(e.kind));
  const notebook = events.filter((e) => e.kind === "note" && e.author === "user");
  const [tab, setTab] = useState<"auto" | "notebook">("notebook");
  const [newNote, setNewNote] = useState("");

  const deleteEvent = async (id: string) => {
    await ipc.notesDelete(id).catch(() => {});
    onChanged();
  };

  const addNote = async () => {
    const text = newNote.trim();
    if (!text) return;
    await ipc.notesAdd(text).catch(() => {});
    setNewNote("");
    onChanged();
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-base-900/80 backdrop-blur-sm"
      onClick={onClose}
    >
      <div
        className="flex h-[80vh] w-[700px] max-w-[95vw] flex-col rounded-lg border border-base-500 bg-base-800 shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div className="flex h-11 shrink-0 items-center justify-between border-b border-base-500 px-4">
          <div className="flex items-center gap-3">
            <span className="text-[13px] font-medium text-ink">Notes</span>
            <div className="flex gap-1">
              <TabBtn active={tab === "notebook"} onClick={() => setTab("notebook")}>
                Notebook ({notebook.length})
              </TabBtn>
              <TabBtn active={tab === "auto"} onClick={() => setTab("auto")}>
                Auto-notes ({autoNotes.length})
              </TabBtn>
            </div>
          </div>
          <button
            onClick={onClose}
            className="flex h-6 w-6 items-center justify-center rounded text-ink-faint hover:bg-base-600 hover:text-ink"
          >
            ✕
          </button>
        </div>

        {/* Body */}
        <div className="min-h-0 flex-1 overflow-auto p-4">
          {tab === "notebook" ? (
            <div className="space-y-2">
              {notebook.length === 0 && (
                <p className="text-[13px] text-ink-faint">No notes yet. Add one below.</p>
              )}
              {notebook.map((e) => (
                <NoteEntry key={e.id} event={e} onDelete={deleteEvent} onChanged={onChanged} />
              ))}
            </div>
          ) : (
            <div className="space-y-1.5">
              {autoNotes.length === 0 && (
                <p className="text-[13px] text-ink-faint">No auto-notes yet. Run agent commands to populate.</p>
              )}
              {autoNotes.map((e) => (
                <div key={e.id} className="group flex items-start gap-2 rounded-md border border-base-500 bg-base-700 px-3 py-2">
                  <div className="min-w-0 flex-1">
                    <span className="label mr-2 text-ink-faint">{e.kind.replace(/_/g, " ")}</span>
                    <span className="text-[12px] text-ink-dim">{e.text}</span>
                  </div>
                  <button
                    onClick={() => void deleteEvent(e.id)}
                    className="mt-0.5 shrink-0 opacity-0 transition-opacity group-hover:opacity-100 hover:text-danger"
                    title="Dismiss"
                  >
                    ✕
                  </button>
                </div>
              ))}
            </div>
          )}
        </div>

        {/* New note footer (notebook tab only) */}
        {tab === "notebook" && (
          <div className="shrink-0 border-t border-base-500 p-4">
            <textarea
              value={newNote}
              onChange={(e) => setNewNote(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
                  e.preventDefault();
                  void addNote();
                }
              }}
              placeholder="New note… (Ctrl+Enter to save)"
              rows={3}
              className="w-full resize-none rounded-md border border-base-500 bg-base-700 px-3 py-2 text-[13px] text-ink outline-none placeholder:text-ink-faint focus:border-accent/50"
            />
            <div className="mt-2 flex justify-end">
              <button
                onClick={() => void addNote()}
                disabled={!newNote.trim()}
                className="rounded-md bg-accent px-3 py-1.5 text-[12px] font-medium text-base-900 disabled:opacity-40 hover:opacity-90"
              >
                Add note
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

// Inline-editable user note row
function NoteEntry({
  event,
  onDelete,
  onChanged,
}: {
  event: EventDto;
  onDelete: (id: string) => void;
  onChanged: () => void;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(event.text);

  const save = async () => {
    const text = draft.trim();
    if (!text) return;
    await ipc.notesUpdate(event.id, text).catch(() => {});
    setEditing(false);
    onChanged();
  };

  const cancel = () => {
    setDraft(event.text);
    setEditing(false);
  };

  return (
    <div className="group rounded-md border border-base-500 bg-base-700">
      {editing ? (
        <div className="p-2">
          <textarea
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) { e.preventDefault(); void save(); }
              if (e.key === "Escape") cancel();
            }}
            autoFocus
            rows={Math.max(2, draft.split("\n").length)}
            className="w-full resize-none rounded border border-accent/40 bg-base-800 px-2 py-1.5 text-[13px] text-ink outline-none"
          />
          <div className="mt-1.5 flex justify-end gap-2">
            <button onClick={cancel} className="text-[11px] text-ink-faint hover:text-ink">
              Cancel
            </button>
            <button
              onClick={() => void save()}
              className="rounded bg-accent px-2 py-0.5 text-[11px] font-medium text-base-900"
            >
              Save
            </button>
          </div>
        </div>
      ) : (
        <div className="flex items-start gap-2 px-3 py-2">
          <p className="min-w-0 flex-1 whitespace-pre-wrap text-[13px] text-ink-dim">{event.text}</p>
          <div className="flex shrink-0 gap-1 opacity-0 transition-opacity group-hover:opacity-100">
            <button
              onClick={() => setEditing(true)}
              className="rounded px-1.5 py-0.5 text-[10px] text-ink-faint hover:bg-base-600 hover:text-ink"
              title="Edit"
            >
              edit
            </button>
            <button
              onClick={() => onDelete(event.id)}
              className="rounded px-1.5 py-0.5 text-[10px] text-ink-faint hover:text-danger"
              title="Delete"
            >
              ✕
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

function TabBtn({ active, onClick, children }: { active: boolean; onClick: () => void; children: React.ReactNode }) {
  return (
    <button
      onClick={onClick}
      className={`rounded px-2.5 py-1 text-[11px] font-mono transition-colors ${
        active ? "bg-accent/12 text-accent ring-1 ring-accent/25" : "text-ink-faint hover:bg-base-600 hover:text-ink"
      }`}
    >
      {children}
    </button>
  );
}
