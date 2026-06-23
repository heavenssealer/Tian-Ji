import { useEffect, useState } from "react";
import { ipc } from "../lib/ipc";
import type { RtkStatus, SkillsStatus } from "../lib/types";
import Modal, { fieldClass, GhostButton, PrimaryButton } from "./Modal";

export default function SettingsButton() {
  const [open, setOpen] = useState(false);
  const [hasKey, setHasKey] = useState(false);
  const [key, setKey] = useState("");
  const [hasSub, setHasSub] = useState(false);
  const [authUrl, setAuthUrl] = useState("");
  const [authCode, setAuthCode] = useState("");
  const [hasSudo, setHasSudo] = useState(false);
  const [sudoPw, setSudoPw] = useState("");
  const [ollamaHost, setOllamaHost] = useState("");
  const [numCtx, setNumCtx] = useState(16384);
  const [rtk, setRtk] = useState<RtkStatus | null>(null);
  const [skills, setSkills] = useState<SkillsStatus | null>(null);
  const [skillsDir, setSkillsDir] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = () => {
    ipc.settingsHasApiKey().then(setHasKey).catch(() => {});
    ipc.authStatus().then(setHasSub).catch(() => {});
    ipc.settingsHasSudoPassword().then(setHasSudo).catch(() => {});
    ipc.settingsGetOllamaHost().then(setOllamaHost).catch(() => {});
    ipc.settingsGetOllamaNumCtx().then(setNumCtx).catch(() => {});
    ipc.settingsGetRtk().then(setRtk).catch(() => {});
    ipc.settingsGetSkills().then(setSkills).catch(() => {});
  };

  // Apply the (optional) custom dir AND rebuild the agent so newly-installed skills take effect.
  const applySkills = async () => {
    setBusy(true);
    setError(null);
    try {
      await ipc.settingsSetSkillsDir(skillsDir.trim());
      await ipc.settingsGetSkills().then(setSkills);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const toggleRtk = async (enabled: boolean) => {
    setRtk((r) => (r ? { ...r, enabled } : r)); // optimistic
    try {
      await ipc.settingsSetRtk(enabled);
      await ipc.settingsGetRtk().then(setRtk);
    } catch (e) {
      setError(String(e));
      void ipc.settingsGetRtk().then(setRtk).catch(() => {});
    }
  };

  useEffect(() => { refresh(); }, []);

  // Step 1: get the authorization URL and open it in the browser. The operator signs in, approves,
  // and copies the code shown on Anthropic's callback page back into the field below (step 2).
  const connectSubscription = async () => {
    setBusy(true);
    setError(null);
    try {
      const url = await ipc.authBegin();
      setAuthUrl(url);
      window.open(url, "_blank");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  // Step 2: exchange the pasted code for tokens.
  const finishSubscription = async () => {
    if (!authCode.trim()) return;
    setBusy(true);
    setError(null);
    try {
      await ipc.authComplete(authCode.trim());
      setAuthCode("");
      setAuthUrl("");
      refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const disconnectSubscription = async () => {
    setBusy(true);
    setError(null);
    try {
      await ipc.authDisconnect();
      setAuthUrl("");
      setAuthCode("");
      refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

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
      await ipc.settingsSetOllamaHost(ollamaHost.trim());
      await ipc.settingsSetOllamaNumCtx(numCtx);
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
        <span className={`h-1.5 w-1.5 rounded-full ${hasKey || hasSub ? "bg-ok" : "bg-warn"}`} />
        Settings
      </button>

      {open && (
        <Modal
          title="Settings"
          onClose={() => setOpen(false)}
          footer={
            <>
              {error && <p className="mb-2 text-[11px] text-danger">{error}</p>}
              <div className="flex justify-end gap-2">
                <GhostButton onClick={() => setOpen(false)}>Cancel</GhostButton>
                <PrimaryButton onClick={() => void save()}>{busy ? "Saving…" : "Save"}</PrimaryButton>
              </div>
            </>
          }
        >
          <p className="mb-4 text-[11px] text-ink-faint">
            All secrets are stored in the OS keychain — never written to disk.
          </p>

          <div className="mb-4 rounded-md border border-base-600 p-3">
            <label className="mb-1.5 block text-[11px] font-medium text-ink-dim">
              Anthropic subscription <span className="font-normal text-ink-faint">(Claude Pro/Max — bills your plan, not API credits)</span>
            </label>
            {hasSub ? (
              <div className="flex items-center justify-between gap-2">
                <p className="text-[11px] text-ok">Connected — turns bill your Anthropic subscription.</p>
                <GhostButton onClick={() => void disconnectSubscription()}>Disconnect</GhostButton>
              </div>
            ) : authUrl ? (
              <div>
                <p className="mb-1.5 text-[11px] text-ink-faint">
                  A browser window should have opened. Sign in, approve access, then paste the code
                  shown on the callback page here. If it didn't open, copy this link into your browser:
                </p>
                <input
                  type="text"
                  readOnly
                  value={authUrl}
                  onFocus={(e) => e.currentTarget.select()}
                  className={`${fieldClass} mb-2 text-[10px] text-accent`}
                />
                <input
                  type="text"
                  value={authCode}
                  onChange={(e) => setAuthCode(e.target.value)}
                  onKeyDown={(e) => e.key === "Enter" && void finishSubscription()}
                  placeholder="paste the authorization code"
                  autoFocus
                  className={fieldClass}
                />
                <div className="mt-2 flex justify-end">
                  <PrimaryButton onClick={() => void finishSubscription()}>
                    {busy ? "Connecting…" : "Finish connecting"}
                  </PrimaryButton>
                </div>
              </div>
            ) : (
              <div>
                <p className="mb-2 text-[11px] text-ink-faint">
                  Connect your Claude account to run the agent on your subscription instead of an API key.
                </p>
                <PrimaryButton onClick={() => void connectSubscription()}>
                  {busy ? "Starting…" : "Connect with Anthropic subscription"}
                </PrimaryButton>
              </div>
            )}
          </div>

          <div className="mb-4">
            <label className="mb-1.5 block text-[11px] font-medium text-ink-dim">
              Anthropic API key <span className="font-normal text-ink-faint">{hasSub ? "(unused while a subscription is connected)" : ""}</span>
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

          <div className="mb-4">
            <label className="mb-1.5 block text-[11px] font-medium text-ink-dim">
              Ollama host <span className="font-normal text-ink-faint">(for local <code>ollama:</code> models)</span>
            </label>
            <input
              type="text"
              value={ollamaHost}
              onChange={(e) => setOllamaHost(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && void save()}
              placeholder="http://localhost:11434"
              className={fieldClass}
            />
            <p className="mt-1 text-[11px] text-ink-faint">
              Where your Ollama server runs. Use your host's IP/name if the app can't reach it on localhost. Leave blank to reset to the default.
            </p>
          </div>

          <div className="mb-4">
            <label className="mb-1.5 block text-[11px] font-medium text-ink-dim">
              Ollama context window <span className="font-normal text-ink-faint">(num_ctx — tokens)</span>
            </label>
            <input
              type="number"
              min={4096}
              step={2048}
              value={numCtx}
              onChange={(e) => setNumCtx(Number(e.target.value) || 0)}
              onKeyDown={(e) => e.key === "Enter" && void save()}
              placeholder="16384"
              className={fieldClass}
            />
            <p className="mt-1 text-[11px] text-ink-faint">
              Ollama's default (~2–4k) is too small and silently truncates the prompt. 16k is a good
              start; raise it (32k+) for long engagements if your model and VRAM allow. Our history
              budget tracks this value automatically.
            </p>
          </div>

          <div className="mb-4 rounded-md border border-base-600 p-3">
            <div className="flex items-center justify-between gap-3">
              <label className="text-[11px] font-medium text-ink-dim">
                RTK output compression{" "}
                <span className="font-normal text-ink-faint">(Rust Token Killer — shrinks output of ls/grep/git/find/…)</span>
              </label>
              <button
                role="switch"
                aria-checked={rtk?.enabled ?? true}
                onClick={() => void toggleRtk(!(rtk?.enabled ?? true))}
                className={`relative h-5 w-9 shrink-0 rounded-full transition-colors ${
                  rtk?.enabled ?? true ? "bg-accent" : "bg-base-500"
                }`}
                title="Toggle RTK"
              >
                <span
                  className={`absolute top-0.5 h-4 w-4 rounded-full bg-base-900 transition-all ${
                    rtk?.enabled ?? true ? "left-[18px]" : "left-0.5"
                  }`}
                />
              </button>
            </div>
            <p className="mt-1.5 text-[11px]">
              {rtk?.enabled && rtk?.available && (
                <span className="text-ok">● Active — wrapping supported commands via <code>{rtk.path}</code>.</span>
              )}
              {rtk?.enabled && rtk && !rtk.available && (
                <span className="text-warn">
                  ● Enabled, but <code>rtk</code> isn't installed. Run <code>cargo install rtk</code> (or <code>brew install rtk</code>), then reopen Settings.
                </span>
              )}
              {rtk && !rtk.enabled && <span className="text-ink-faint">○ Disabled — commands run uncompressed.</span>}
            </p>
          </div>

          <div className="mb-4 rounded-md border border-base-600 p-3">
            <div className="mb-1.5 flex items-center justify-between gap-2">
              <label className="text-[11px] font-medium text-ink-dim">
                Agent skills{" "}
                <span className="font-normal text-ink-faint">(playbooks the agents can load on demand — cloud & local)</span>
              </label>
              <span className={`shrink-0 text-[11px] ${skills && skills.count > 0 ? "text-ok" : "text-ink-faint"}`}>
                {skills ? `${skills.count} loaded` : "…"}
              </span>
            </div>

            {skills && skills.count > 0 && (
              <div className="mb-2 flex flex-wrap gap-1">
                {skills.names.map((n) => (
                  <span key={n} className="rounded bg-base-700 px-1.5 py-0.5 font-mono text-[10px] text-ink-dim">
                    {n}
                  </span>
                ))}
              </div>
            )}

            <p className="mb-2 text-[11px] text-ink-faint">
              Install in the terminal, then Apply:
              {" "}<code>npx skills add ljagiello/ctf-skills</code>{" "}and{" "}
              <code>bash scripts/install_ctf_tools.sh all</code> (installs the tools).
              {skills?.dirs?.length ? (
                <> Searched: <span className="font-mono text-[10px]">{skills.dirs.join(", ")}</span>.</>
              ) : null}
            </p>

            <div className="flex items-center gap-2">
              <input
                type="text"
                value={skillsDir}
                onChange={(e) => setSkillsDir(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && void applySkills()}
                placeholder="custom skills dir (optional — leave blank for defaults)"
                className={`${fieldClass} text-[11px]`}
              />
              <GhostButton onClick={() => void applySkills()}>{busy ? "…" : "Apply / rescan"}</GhostButton>
            </div>
          </div>

        </Modal>
      )}
    </>
  );
}
