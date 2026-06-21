import { useEffect, useState } from "react";
import { ipc } from "../../lib/ipc";
import type { PolicyRuleDto } from "../../lib/types";

interface Props {
  onClose: () => void;
}

const GRANULARITY_LABELS: Record<string, string> = {
  exact_command:   "exact cmd",
  tool_flag_shape: "flag shape",
  whole_tool:      "whole tool",
};

export default function PolicyRulesModal({ onClose }: Props) {
  const [rules, setRules] = useState<PolicyRuleDto[]>([]);
  const [loading, setLoading] = useState(true);

  const load = async () => {
    setLoading(true);
    try {
      setRules(await ipc.policyRulesList());
    } catch {}
    setLoading(false);
  };

  useEffect(() => { void load(); }, []);

  const remove = async (rule: PolicyRuleDto) => {
    await ipc.policyRuleRemove(rule.ruleJson, rule.scope === "global").catch(() => {});
    void load();
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60"
      onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}
    >
      <div className="w-[520px] max-h-[70vh] flex flex-col rounded-xl border border-base-500 bg-base-900 shadow-xl">
        <div className="flex items-center justify-between border-b border-base-500 px-4 py-3">
          <span className="text-[13px] font-medium text-ink">Allow rules</span>
          <button onClick={onClose} className="text-ink-faint hover:text-ink text-[16px]">×</button>
        </div>

        <div className="flex-1 overflow-auto px-4 py-3">
          {loading && <p className="text-[12px] text-ink-faint">Loading…</p>}
          {!loading && rules.length === 0 && (
            <p className="text-[12px] text-ink-faint">No allow rules yet. Rules are created when you approve a command and choose "Always allow".</p>
          )}
          {!loading && rules.length > 0 && (
            <table className="w-full text-[12px]">
              <thead>
                <tr className="text-left text-ink-faint">
                  <th className="pb-1.5 pr-3 font-normal">Tool</th>
                  <th className="pb-1.5 pr-3 font-normal">Args</th>
                  <th className="pb-1.5 pr-3 font-normal">Level</th>
                  <th className="pb-1.5 pr-3 font-normal">Scope</th>
                  <th className="pb-1.5" />
                </tr>
              </thead>
              <tbody className="divide-y divide-base-700">
                {rules.map((r, i) => (
                  <tr key={i}>
                    <td className="py-1.5 pr-3 font-mono text-ink">{r.tool}</td>
                    <td className="py-1.5 pr-3 font-mono text-ink-dim max-w-[180px] truncate" title={r.fingerprint.join(" ")}>
                      {r.fingerprint.length > 0 ? r.fingerprint.join(" ") : "—"}
                    </td>
                    <td className="py-1.5 pr-3 text-ink-faint">{GRANULARITY_LABELS[r.granularity] ?? r.granularity}</td>
                    <td className="py-1.5 pr-3">
                      <span className={`rounded px-1 py-0.5 text-[10px] ${r.scope === "global" ? "bg-accent/15 text-accent" : "bg-base-700 text-ink-faint"}`}>
                        {r.scope}
                      </span>
                    </td>
                    <td className="py-1.5">
                      <button
                        onClick={() => void remove(r)}
                        className="rounded px-1.5 py-0.5 text-[11px] text-danger hover:bg-danger/10"
                        title="Remove rule"
                      >
                        ×
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>

        <div className="border-t border-base-500 px-4 py-2.5 text-right">
          <button onClick={onClose} className="btn-ghost text-[12px]">Close</button>
        </div>
      </div>
    </div>
  );
}
