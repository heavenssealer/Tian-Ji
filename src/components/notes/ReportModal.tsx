import { useState } from "react";
import type { FindingDto } from "../../lib/types";

const SEV_ORDER: Record<string, number> = { critical: 0, high: 1, medium: 2, low: 3, info: 4 };

function buildMarkdown(findings: FindingDto[], workspaceName: string): string {
  const sorted = [...findings].sort(
    (a, b) => (SEV_ORDER[a.severity] ?? 9) - (SEV_ORDER[b.severity] ?? 9)
  );

  const date = new Date().toISOString().slice(0, 10);

  const body = sorted.length === 0
    ? "_No findings recorded._\n"
    : sorted
        .map(
          (f, i) =>
            `### ${i + 1}. [${f.severity.toUpperCase()}] ${f.summary}\n\n` +
            `**Target:** ${f.target}\n`
        )
        .join("\n");

  return `# Engagement Report — ${workspaceName}\n\n` +
    `**Date:** ${date}  \n` +
    `**Findings:** ${findings.length}\n\n` +
    `---\n\n## Findings\n\n${body}`;
}

export default function ReportModal({
  findings,
  workspaceName,
  onClose,
}: {
  findings: FindingDto[];
  workspaceName: string;
  onClose: () => void;
}) {
  const md = buildMarkdown(findings, workspaceName);
  const [copied, setCopied] = useState(false);

  const copy = async () => {
    await navigator.clipboard.writeText(md).catch(() => {});
    setCopied(true);
    setTimeout(() => setCopied(false), 1800);
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-base-900/80 backdrop-blur-sm"
      onClick={onClose}
    >
      <div
        className="flex h-[85vh] w-[760px] max-w-[95vw] flex-col rounded-lg border border-base-500 bg-base-800 shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex h-11 shrink-0 items-center justify-between border-b border-base-500 px-4">
          <span className="text-[13px] font-medium text-ink">Engagement Report</span>
          <div className="flex items-center gap-2">
            <button
              onClick={() => void copy()}
              className={`rounded px-2.5 py-1 text-[11px] ring-1 transition-colors ${
                copied
                  ? "bg-ok/15 text-ok ring-ok/30"
                  : "text-ink-dim ring-base-500 hover:bg-base-600"
              }`}
            >
              {copied ? "Copied ✓" : "Copy markdown"}
            </button>
            <button
              onClick={onClose}
              className="flex h-6 w-6 items-center justify-center rounded text-ink-faint hover:bg-base-600 hover:text-ink"
            >
              ✕
            </button>
          </div>
        </div>
        <pre className="min-h-0 flex-1 overflow-auto whitespace-pre-wrap p-4 font-mono text-[12px] leading-relaxed text-ink-dim">
          {md}
        </pre>
      </div>
    </div>
  );
}
