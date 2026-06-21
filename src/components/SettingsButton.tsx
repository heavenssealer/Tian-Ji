import { useEffect, useState } from "react";
import { ipc } from "../lib/ipc";
import Modal, { fieldClass, GhostButton, PrimaryButton } from "./Modal";

export default function SettingsButton() {
  const [open, setOpen] = useState(false);
  const [hasKey, setHasKey] = useState(false);
  const [key, setKey] = useState("");
  const [hasSudo, setHasSudo] = useState(false);
  const [sudoPw, setSudoPw] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = () => {
    ipc.settingsHasApiKey().then(setHasKey).catch(() => {});
    ipc.settingsHasSudoPassword().then(setHasSudo).catch(() => {});
  };

  useEffect(() => { refresh(); }, []);

  const save = async () => {
    setBusy(true);
    setError(null);
    try {
      if (key.trim()) {
        await ipc.settingsSetApiKey(key.trim());
        setKey("");
      }
      if (sudoPw.trim()) {
        await ipc.settingsSetSudoPassword(sudoPw.trim());
        setSudoPw("");
      }
      setOpen(false);
      refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <button
        onClick={() => setOpen(true)}
        className="flex items-center gap-1.5 rounded-md px-2 py-1 text-[11px] text-ink-dim hover:bg-base-600"
        title="Settings"
      >
        <span className={`h-1.5 w-1.5 rounded-full ${hasKey ? "bg-ok" : "bg-warn"}`} />
        Settings
      </button>

      {open && (
        <Modal title="Settings" onClose={() => setOpen(false)}>
          <p className="mb-4 text-[11px] text-ink-faint">
            All secrets are stored in the OS keychain — never written to disk.
          </p>

          <div className="mb-4">
            <label className="mb-1.5 block text-[11px] font-medium text-ink-dim">
              Anthropic API key
            </label>
            <input
              type="password"
              value={key}
              onChange={(e) => setKey(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && void save()}
              placeholder="sk-ant-…"
              autoFocus
              className={fieldClass}
            />
            {hasKey && <p className="mt-1 text-[11px] text-ok">A key is currently set.</p>}
          </div>

          <div className="mb-4">
            <label className="mb-1.5 block text-[11px] font-medium text-ink-dim">
              Sudo password <span className="font-normal text-ink-faint">(Linux / macOS — for privileged tools)</span>
            </label>
            <input
              type="password"
              value={sudoPw}
              onChange={(e) => setSudoPw(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && void save()}
              placeholder="your sudo password"
              className={fieldClass}
            />
            {hasSudo
              ? <p className="mt-1 text-[11px] text-ok">Sudo password is set — nmap, tcpdump, etc. run as root automatically.</p>
              : <p className="mt-1 text-[11px] text-ink-faint">Without this, privileged tools require NOPASSWD sudoers or will fail.</p>
            }
          </div>

          {error && <p className="mb-2 text-[11px] text-danger">{error}</p>}

          <div className="mt-4 flex justify-end gap-2">
            <GhostButton onClick={() => setOpen(false)}>Cancel</GhostButton>
            <PrimaryButton onClick={() => void save()}>{busy ? "Saving…" : "Save"}</PrimaryButton>
          </div>
        </Modal>
      )}
    </>
  );
}
