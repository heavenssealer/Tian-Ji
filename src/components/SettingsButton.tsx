// In-app API-key entry via a proper modal (no native prompt). The key goes straight to the OS
// keychain via the backend.

import { useEffect, useState } from "react";
import { ipc } from "../lib/ipc";
import Modal, { fieldClass, GhostButton, PrimaryButton } from "./Modal";

export default function SettingsButton() {
  const [open, setOpen] = useState(false);
  const [hasKey, setHasKey] = useState(false);
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = () => ipc.settingsHasApiKey().then(setHasKey).catch(() => {});

  useEffect(() => {
    void refresh();
  }, []);

  const save = async () => {
    if (!key.trim()) return;
    setBusy(true);
    setError(null);
    try {
      await ipc.settingsSetApiKey(key.trim());
      setKey("");
      setOpen(false);
      await refresh();
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
        title="Set Anthropic API key"
      >
        <span className={`h-1.5 w-1.5 rounded-full ${hasKey ? "bg-ok" : "bg-warn"}`} />
        API key
      </button>

      {open && (
        <Modal title="Anthropic API key" onClose={() => setOpen(false)}>
          <p className="mb-3 text-[12px] leading-relaxed text-ink-dim">
            Stored in the OS keychain, never on disk. Applies immediately to the open workspace.
          </p>
          <input
            type="password"
            value={key}
            onChange={(e) => setKey(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && void save()}
            placeholder="sk-ant-…"
            autoFocus
            className={fieldClass}
          />
          {hasKey && <p className="mt-2 text-[11px] text-ok">A key is currently set.</p>}
          {error && <p className="mt-2 text-[11px] text-danger">{error}</p>}
          <div className="mt-4 flex justify-end gap-2">
            <GhostButton onClick={() => setOpen(false)}>Cancel</GhostButton>
            <PrimaryButton onClick={() => void save()}>{busy ? "Saving…" : "Save"}</PrimaryButton>
          </div>
        </Modal>
      )}
    </>
  );
}
