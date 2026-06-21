// The tiered-approval card (DESIGN.md §4.3). Shows the exact command, resolved targets, and
// classification; offers approve / edit+approve / deny+reason / always-allow.

import { useState } from "react";
import { ipc } from "../../lib/ipc";
import type { Classification, ProposedCall } from "../../lib/types";

const CLASS_STYLE: Record<Classification, { border: string; text: string; dot: string }> = {
  read_only: { border: "border-ok/40",     text: "text-ok",     dot: "bg-ok"     },
  mutating:  { border: "border-warn/50",   text: "text-warn",   dot: "bg-warn"   },
  exploit:   { border: "border-danger/50", text: "text-danger", dot: "bg-danger" },
  unknown:   { border: "border-warn/50",   text: "text-warn",   dot: "bg-warn"   },
};

type Mode = "idle" | "editing" | "denying";

export default function ApprovalCard({
  call,
  onResolved,
}: {
  call: ProposedCall;
  onResolved?: () => void;
}) {
  const style = CLASS_STYLE[call.classification];
  const [mode, setMode] = useState<Mode>("idle");
  const [editedArgs, setEditedArgs] = useState(call.argv.join(" "));
  const [denyReason, setDenyReason] = useState("");

  const resolve = (resolution: Parameters<typeof ipc.policyResolve>[1]) => {
    void ipc.policyResolve(call.token, resolution).finally(() => onResolved?.());
  };

  const approveEdited = () => {
    const argv = editedArgs.trim().split(/\s+/).filter(Boolean);
    resolve({ kind: "approve_edited", argv });
  };

  const denyWithReason = () => {
    resolve({ kind: "deny", reason: denyReason.trim() });
  };

  return (
    <div className={`rounded-card border bg-base-800 p-2.5 ${style.border}`}>
      {/* Header */}
      <div className={`mb-2 flex items-center gap-1.5 ${style.text}`}>
        <span className={`h-1.5 w-1.5 rounded-full ${style.dot}`} />
        <span className="label !text-current">approval · {call.classification.replace("_", "-")}</span>
      </div>

      {/* Command display (always visible except when editing) */}
      {mode !== "editing" && (
        <>
          <code className="block rounded-md bg-base-900 px-2.5 py-1.5 font-mono text-[11px] text-ink">
            {call.tool} {call.argv.join(" ")}
          </code>
          <div className="mt-1.5 font-mono text-[10px] text-ink-faint">
            targets: {call.targets.join(", ") || "none"}
          </div>
        </>
      )}

      {/* Edit mode */}
      {mode === "editing" && (
        <div className="space-y-1.5">
          <div className="flex items-center gap-1.5 rounded-md bg-base-900 px-2.5 py-1.5 font-mono text-[11px] text-ink">
            <span className="shrink-0 text-ink-faint">{call.tool}</span>
            <input
              value={editedArgs}
              onChange={(e) => setEditedArgs(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") approveEdited();
                if (e.key === "Escape") setMode("idle");
              }}
              autoFocus
              className="min-w-0 flex-1 bg-transparent outline-none"
              placeholder="arguments…"
            />
          </div>
          <div className="flex gap-1.5">
            <button
              onClick={approveEdited}
              className="rounded-md bg-ok/15 px-2.5 py-1 text-[11px] text-ok ring-1 ring-ok/30 hover:bg-ok/25"
            >
              Run edited
            </button>
            <button
              onClick={() => setMode("idle")}
              className="rounded-md px-2.5 py-1 text-[11px] text-ink-dim ring-1 ring-base-500 hover:bg-base-600"
            >
              Cancel
            </button>
          </div>
        </div>
      )}

      {/* Deny reason mode */}
      {mode === "denying" && (
        <div className="mt-2 space-y-1.5">
          <input
            value={denyReason}
            onChange={(e) => setDenyReason(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") denyWithReason();
              if (e.key === "Escape") setMode("idle");
            }}
            autoFocus
            placeholder="Reason (optional — fed back to agent)"
            className="w-full rounded-md border border-base-500 bg-base-900 px-2.5 py-1.5 font-mono text-[11px] text-ink outline-none placeholder:text-ink-faint focus:border-danger/40"
          />
          <div className="flex gap-1.5">
            <button
              onClick={denyWithReason}
              className="rounded-md bg-danger/15 px-2.5 py-1 text-[11px] text-danger ring-1 ring-danger/30 hover:bg-danger/25"
            >
              Deny
            </button>
            <button
              onClick={() => setMode("idle")}
              className="rounded-md px-2.5 py-1 text-[11px] text-ink-dim ring-1 ring-base-500 hover:bg-base-600"
            >
              Cancel
            </button>
          </div>
        </div>
      )}

      {/* Action buttons (idle mode only) */}
      {mode === "idle" && (
        <div className="mt-2.5 flex flex-wrap gap-1.5">
          <button
            onClick={() => resolve({ kind: "approve_once" })}
            className="rounded-md bg-ok/15 px-2.5 py-1 text-[11px] text-ok ring-1 ring-ok/30 hover:bg-ok/25"
          >
            Approve
          </button>
          <button
            onClick={() => setMode("editing")}
            className="rounded-md px-2.5 py-1 text-[11px] text-ink-dim ring-1 ring-base-500 hover:bg-base-600"
          >
            Edit…
          </button>
          <button
            onClick={() => setMode("denying")}
            className="rounded-md px-2.5 py-1 text-[11px] text-ink-dim ring-1 ring-base-500 hover:bg-base-600"
          >
            Deny…
          </button>
          <button
            onClick={() => resolve({ kind: "always_allow", granularity: "tool_flag_shape" })}
            className="rounded-md px-2.5 py-1 text-[11px] text-ink-dim ring-1 ring-base-500 hover:bg-base-600"
          >
            Always allow
          </button>
        </div>
      )}
    </div>
  );
}
