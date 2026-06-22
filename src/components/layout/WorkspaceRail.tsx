import { useEffect, useState } from "react";
import { ipc } from "../../lib/ipc";
import { useAppStore } from "../../state/stores";
import Modal, { fieldClass, GhostButton, PrimaryButton } from "../Modal";
import Select from "../Select";
import type { WorkspaceInfo } from "../../lib/types";
import PolicyRulesModal from "../policy/PolicyRulesModal";

const AGENT_COLORS: Record<string, string> = {
  orchestrator: "bg-accent",
  recon:        "bg-sky-400",
  web:          "bg-violet-400",
  exploit:      "bg-rose-400",
};


export default function WorkspaceRail() {
  const workspaces    = useAppStore((s) => s.workspaces);
  const currentId     = useAppStore((s) => s.currentWorkspaceId);
  const activeAgents  = useAppStore((s) => s.activeAgents);
  const setWorkspaces = useAppStore((s) => s.setWorkspaces);
  const setCurrent    = useAppStore((s) => s.setCurrentWorkspace);
  const setPhase      = useAppStore((s) => s.setPhase);
  const setChat       = useAppStore((s) => s.setChat);
  const [error,         setError]         = useState<string | null>(null);
  const [creating,      setCreating]      = useState(false);
  const [editingScope,  setEditingScope]  = useState(false);
  const [renamingId,    setRenamingId]    = useState<string | null>(null);
  const [deletingId,    setDeletingId]    = useState<string | null>(null);
  const [showRules,     setShowRules]     = useState(false);

  const LAST_KEY = "tianji.lastWorkspaceId";
  const HISTORY_KINDS = new Set(["user_prompt", "agent_msg", "tool_approved", "tool_output"]);

  const reload = async () => {
    const list = await ipc.workspaceList().catch(() => workspaces);
    setWorkspaces(list);
  };

  const open = async (id: string) => {
    try {
      const info = await ipc.workspaceOpen(id);
      setCurrent(id);
      setPhase(info.phase);
      localStorage.setItem(LAST_KEY, id);

      const events = await ipc.eventsQuery(500);
      const history = [...events]
        .reverse()
        .filter((e) => HISTORY_KINDS.has(e.kind))
        .map((e) => ({
          kind: (e.kind === "user_prompt" ? "user"
               : e.kind === "agent_msg"   ? "agent"
               : "tool") as "user" | "agent" | "tool",
          text: e.kind === "tool_approved" ? `$ ${e.text}` : e.text,
        }));
      setChat(history);
    } catch (e) {
      setError(String(e));
    }
  };

  useEffect(() => {
    void (async () => {
      try {
        const list = await ipc.workspaceList();
        setWorkspaces(list);
        const lastId = localStorage.getItem(LAST_KEY);
        const target = lastId ? list.find((w) => w.id === lastId) : list[0];
        if (target) await open(target.id);
      } catch (e) {
        setError(String(e));
      }
    })();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const current = workspaces.find((w) => w.id === currentId);

  const confirmDelete = async (id: string) => {
    await ipc.workspaceDelete(id).catch((e) => setError(String(e)));
    setDeletingId(null);
    // If we deleted the current workspace, switch to the first remaining one.
    const list = await ipc.workspaceList().catch(() => []);
    setWorkspaces(list);
    if (id === currentId) {
      if (list.length > 0) await open(list[0].id);
      else setCurrent(null);
    }
  };

  return (
    <div className="flex h-full min-h-0 flex-col text-xs">
      {showRules && <PolicyRulesModal onClose={() => setShowRules(false)} />}
      <div className="flex h-9 shrink-0 items-center justify-between border-b border-base-500 px-2.5">
        <span className="label">Workspaces</span>
        <div className="flex items-center gap-0.5">
          <button
            onClick={() => setShowRules(true)}
            className="flex h-5 w-5 items-center justify-center rounded text-[11px] text-ink-faint hover:bg-base-600 hover:text-ink"
            title="Allow rules"
          >
            ⊛
          </button>
          <button
            onClick={() => setCreating(true)}
            className="flex h-5 w-5 items-center justify-center rounded text-ink-faint hover:bg-base-600 hover:text-ink"
            title="New workspace"
          >
            +
          </button>
        </div>
      </div>

      <div className="min-h-0 flex-1 space-y-5 overflow-auto p-2.5">
        <section>
          {workspaces.length === 0 && <div className="px-2 text-ink-faint">none yet</div>}
          {workspaces.map((w) => (
            <WorkspaceItem
              key={w.id}
              workspace={w}
              active={w.id === currentId}
              onOpen={() => void open(w.id)}
              onEditScope={() => setEditingScope(true)}
              onRename={() => setRenamingId(w.id)}
              onDelete={() => setDeletingId(w.id)}
            />
          ))}
          {error && <div className="mt-1 px-2 text-[10px] text-danger">{error}</div>}
        </section>

        <section>
          <div className="label mb-2">Agents</div>
          {activeAgents.map((a) => {
            const dotColor = AGENT_COLORS[a.name] ?? "bg-ink-faint";
            const pulse = a.status === "running";
            return (
              <div key={a.name} className="mb-0.5 flex items-start gap-2 rounded px-2 py-1 text-ink-dim">
                <span className={`mt-1 h-1.5 w-1.5 shrink-0 rounded-full ${dotColor} ${pulse ? "animate-pulse" : "opacity-50"}`} />
                <div className="min-w-0">
                  <span className="text-[11px]">{a.name}</span>
                  {a.status === "running" && a.objective && (
                    <div className="truncate text-[10px] text-ink-faint" title={a.objective}>{a.objective}</div>
                  )}
                </div>
              </div>
            );
          })}
        </section>
      </div>

      <ModelSelect />

      {creating && (
        <NewWorkspaceModal
          onClose={() => setCreating(false)}
          onCreated={async (id, phase) => {
            setCreating(false);
            await reload();
            setCurrent(id);
            setPhase(phase);
            setChat([]);
            localStorage.setItem(LAST_KEY, id);
          }}
        />
      )}

      {editingScope && current && (
        <ScopeEditModal
          workspace={current}
          onClose={() => setEditingScope(false)}
          onSaved={async () => { setEditingScope(false); await reload(); }}
        />
      )}

      {renamingId && (
        <RenameModal
          workspace={workspaces.find((w) => w.id === renamingId)!}
          onClose={() => setRenamingId(null)}
          onSaved={async () => { setRenamingId(null); await reload(); }}
        />
      )}

      {deletingId && (
        <ConfirmDeleteModal
          workspace={workspaces.find((w) => w.id === deletingId)!}
          onClose={() => setDeletingId(null)}
          onConfirm={() => void confirmDelete(deletingId)}
        />
      )}
    </div>
  );
}

// ---- sub-components -------------------------------------------------------------------------

function WorkspaceItem({
  workspace: w,
  active,
  onOpen,
  onEditScope,
  onRename,
  onDelete,
}: {
  workspace: WorkspaceInfo;
  active: boolean;
  onOpen: () => void;
  onEditScope: () => void;
  onRename: () => void;
  onDelete: () => void;
}) {
  const allScope = [
    ...(w.scopeCidrs ?? []),
    ...(w.scopeHostnames ?? []),
    ...(w.scopeUrlDomains ?? []),
  ];

  return (
    <div className="group relative mb-0.5">
      <button
        onClick={onOpen}
        className={`flex w-full flex-col rounded-md px-2 py-1.5 text-left transition-colors ${
          active
            ? "bg-accent/12 text-accent ring-1 ring-accent/25"
            : "text-ink-dim hover:bg-base-600"
        }`}
      >
        <div className="flex items-center gap-1.5">
          <span className="opacity-70">›</span>
          <span className="truncate">{w.name}</span>
        </div>
        {allScope.length > 0 && (
          <div className="ml-3.5 mt-0.5 truncate font-mono text-[9px] text-ink-faint">
            {allScope.join(", ")}
          </div>
        )}
      </button>

      {/* Action buttons — visible on hover */}
      <div className="absolute right-1 top-1.5 hidden items-center gap-0.5 group-hover:flex">
        <button
          onClick={(e) => { e.stopPropagation(); onRename(); }}
          className="flex h-5 w-5 items-center justify-center rounded text-[10px] text-ink-faint hover:bg-base-500 hover:text-ink"
          title="Rename"
        >
          ✎
        </button>
        {active && (
          <button
            onClick={(e) => { e.stopPropagation(); onEditScope(); }}
            className="flex h-5 w-5 items-center justify-center rounded text-[10px] text-ink-faint hover:bg-base-500 hover:text-ink"
            title="Edit scope"
          >
            ◎
          </button>
        )}
        <button
          onClick={(e) => { e.stopPropagation(); onDelete(); }}
          className="flex h-5 w-5 items-center justify-center rounded text-[10px] text-ink-faint hover:bg-danger/20 hover:text-danger"
          title="Delete"
        >
          ✕
        </button>
      </div>
    </div>
  );
}

// ---- modals ---------------------------------------------------------------------------------

function ScopeEditModal({
  workspace,
  onClose,
  onSaved,
}: {
  workspace: WorkspaceInfo;
  onClose: () => void;
  onSaved: () => void;
}) {
  const [cidrs,      setCidrs]      = useState<string[]>((workspace.scopeCidrs ?? []).length > 0 ? workspace.scopeCidrs : [""]);
  const [hostnames,  setHostnames]  = useState<string[]>((workspace.scopeHostnames ?? []).length > 0 ? workspace.scopeHostnames : [""]);
  const [urlDomains, setUrlDomains] = useState<string[]>((workspace.scopeUrlDomains ?? []).length > 0 ? workspace.scopeUrlDomains : [""]);
  const [error, setError] = useState<string | null>(null);

  const save = async () => {
    const c = cidrs.map((s) => s.trim()).filter(Boolean);
    const h = hostnames.map((s) => s.trim()).filter(Boolean);
    const u = urlDomains.map((s) => s.trim()).filter(Boolean);
    try {
      await ipc.workspaceSetScope(c, h, u);
      onSaved();
    } catch (e) { setError(String(e)); }
  };

  return (
    <Modal title={`Scope — ${workspace.name}`} onClose={onClose}>
      <p className="mb-3 text-[11px] text-ink-faint">
        Targets outside this scope are denied by policy.
      </p>
      <ScopeSection label="CIDRs" placeholder="192.168.1.0/24" items={cidrs} setItems={setCidrs} />
      <ScopeSection label="Hostnames" placeholder="corp.example.com" items={hostnames} setItems={setHostnames} />
      <ScopeSection label="URL domains" placeholder="app.example.com" items={urlDomains} setItems={setUrlDomains} />
      {error && <p className="mt-2 text-[11px] text-danger">{error}</p>}
      <div className="mt-4 flex justify-end gap-2">
        <GhostButton onClick={onClose}>Cancel</GhostButton>
        <PrimaryButton onClick={() => void save()}>Save scope</PrimaryButton>
      </div>
    </Modal>
  );
}

function ScopeSection({
  label,
  placeholder,
  items,
  setItems,
}: {
  label: string;
  placeholder: string;
  items: string[];
  setItems: (v: string[]) => void;
}) {
  const update = (i: number, v: string) => setItems(items.map((x, idx) => (idx === i ? v : x)));
  const remove = (i: number) => setItems(items.filter((_, idx) => idx !== i));
  return (
    <div className="mb-3">
      <p className="label mb-1">{label}</p>
      <div className="space-y-1.5">
        {items.map((c, i) => (
          <div key={i} className="flex items-center gap-1.5">
            <input
              value={c}
              onChange={(e) => update(i, e.target.value)}
              placeholder={placeholder}
              className={`${fieldClass} flex-1 font-mono`}
              onKeyDown={(e) => e.key === "Enter" && setItems([...items, ""])}
            />
            {items.length > 1 && (
              <button onClick={() => remove(i)} className="text-ink-faint hover:text-danger">✕</button>
            )}
          </div>
        ))}
      </div>
      <button
        onClick={() => setItems([...items, ""])}
        className="mt-1.5 text-[11px] text-ink-faint hover:text-ink"
      >
        + Add
      </button>
    </div>
  );
}

function RenameModal({
  workspace,
  onClose,
  onSaved,
}: {
  workspace: WorkspaceInfo;
  onClose: () => void;
  onSaved: () => void;
}) {
  const [name, setName] = useState(workspace.name);
  const [error, setError] = useState<string | null>(null);

  const save = async () => {
    const n = name.trim();
    if (!n) return;
    try {
      await ipc.workspaceRename(workspace.id, n);
      onSaved();
    } catch (e) { setError(String(e)); }
  };

  return (
    <Modal title="Rename workspace" onClose={onClose}>
      <label className="label mb-1 block">Name</label>
      <input
        value={name}
        onChange={(e) => setName(e.target.value)}
        autoFocus
        className={fieldClass}
        onKeyDown={(e) => e.key === "Enter" && void save()}
      />
      {error && <p className="mt-2 text-[11px] text-danger">{error}</p>}
      <div className="mt-4 flex justify-end gap-2">
        <GhostButton onClick={onClose}>Cancel</GhostButton>
        <PrimaryButton onClick={() => void save()}>Rename</PrimaryButton>
      </div>
    </Modal>
  );
}

function ConfirmDeleteModal({
  workspace,
  onClose,
  onConfirm,
}: {
  workspace: WorkspaceInfo;
  onClose: () => void;
  onConfirm: () => void;
}) {
  return (
    <Modal title="Delete workspace?" onClose={onClose}>
      <p className="mb-4 text-[12px] text-ink-dim">
        Remove <span className="font-semibold text-ink">{workspace.name}</span> from the list?
        The engagement data on disk will not be deleted.
      </p>
      <div className="flex justify-end gap-2">
        <GhostButton onClick={onClose}>Cancel</GhostButton>
        <button
          onClick={onConfirm}
          className="rounded-md bg-danger/80 px-3 py-1.5 text-[12px] font-medium text-white hover:bg-danger"
        >
          Delete
        </button>
      </div>
    </Modal>
  );
}

function ModelSelect() {
  const [models, setModels] = useState<string[]>([]);
  const [model,  setModel]  = useState("");

  useEffect(() => {
    void ipc.settingsListModels().then(setModels).catch(() => {});
    void ipc.settingsGetModel().then(setModel).catch(() => {});
  }, []);

  const change = async (m: string) => {
    setModel(m);
    await ipc.settingsSetModel(m).catch(() => {});
  };

  const options = (models.length > 0 ? models : model ? [model] : []).map((m) => ({
    value: m,
    label: m,
  }));

  return (
    <div className="border-t border-base-500 px-2.5 py-2.5">
      <div className="flex items-center gap-2">
        <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-accent" />
        <Select
          value={model}
          options={options}
          onChange={(m) => void change(m)}
          onOpen={() => void ipc.settingsListModels().then(setModels).catch(() => {})}
          placement="top"
          placeholder="model"
          className="flex-1"
        />
      </div>
    </div>
  );
}

function NewWorkspaceModal({
  onClose,
  onCreated,
}: {
  onClose: () => void;
  onCreated: (id: string, phase: import("../../lib/types").Phase) => void;
}) {
  const [name,      setName]      = useState("");
  const [cidrs,     setCidrs]     = useState<string[]>(["10.0.0.0/24"]);
  const [hostnames, setHostnames] = useState<string[]>([""]);
  const [urlDoms,   setUrlDoms]   = useState<string[]>([""]);
  const [error,     setError]     = useState<string | null>(null);

  const create = async () => {
    if (!name.trim()) return;
    const c = cidrs.map((s) => s.trim()).filter(Boolean);
    try {
      const info = await ipc.workspaceCreate(name.trim(), c);
      // Apply hostnames/url_domains if provided
      const h = hostnames.map((s) => s.trim()).filter(Boolean);
      const u = urlDoms.map((s) => s.trim()).filter(Boolean);
      if (h.length > 0 || u.length > 0) {
        await ipc.workspaceSetScope(c, h, u).catch(() => {});
      }
      onCreated(info.id, info.phase);
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <Modal title="New workspace" onClose={onClose}>
      <label className="label mb-1 block">Name</label>
      <input
        value={name}
        onChange={(e) => setName(e.target.value)}
        placeholder="acme-prod"
        autoFocus
        className={fieldClass}
      />

      <div className="mt-3">
        <ScopeSection label="CIDRs" placeholder="10.0.0.0/24" items={cidrs} setItems={setCidrs} />
        <ScopeSection label="Hostnames (optional)" placeholder="corp.example.com" items={hostnames} setItems={setHostnames} />
        <ScopeSection label="URL domains (optional)" placeholder="app.example.com" items={urlDoms} setItems={setUrlDoms} />
      </div>

      <p className="mt-1 text-[11px] text-ink-faint">
        Targets outside this scope are denied by policy.
      </p>
      {error && <p className="mt-2 text-[11px] text-danger">{error}</p>}
      <div className="mt-4 flex justify-end gap-2">
        <GhostButton onClick={onClose}>Cancel</GhostButton>
        <PrimaryButton onClick={() => void create()}>Create</PrimaryButton>
      </div>
    </Modal>
  );
}
