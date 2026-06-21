// Reusable app modal — replaces native prompt()/alert(). Overlay click and Esc close it.

import { useEffect, type ReactNode } from "react";

export default function Modal({
  title,
  onClose,
  children,
}: {
  title: string;
  onClose: () => void;
  children: ReactNode;
}) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm"
      onClick={onClose}
    >
      <div
        className="w-[420px] max-w-[92vw] rounded-card border border-base-500 bg-base-700 shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center justify-between border-b border-base-500 px-4 py-3">
          <h2 className="text-[13px] font-medium text-ink">{title}</h2>
          <button onClick={onClose} className="text-ink-faint hover:text-ink" aria-label="Close">
            ✕
          </button>
        </div>
        <div className="p-4">{children}</div>
      </div>
    </div>
  );
}

/** Shared input styling for modal forms. */
export const fieldClass =
  "w-full rounded-md border border-base-500 bg-base-800 px-2.5 py-2 text-[13px] text-ink outline-none placeholder:text-ink-faint focus:border-accent/50";

/** Primary (amber) action button. */
export function PrimaryButton({ children, onClick }: { children: ReactNode; onClick: () => void }) {
  return (
    <button
      onClick={onClick}
      className="rounded-md bg-accent px-3 py-1.5 text-[12px] font-medium text-base-900 transition-opacity hover:opacity-90"
    >
      {children}
    </button>
  );
}

/** Secondary (ghost) action button. */
export function GhostButton({ children, onClick }: { children: ReactNode; onClick: () => void }) {
  return (
    <button
      onClick={onClick}
      className="rounded-md px-3 py-1.5 text-[12px] text-ink-dim ring-1 ring-base-500 hover:bg-base-600"
    >
      {children}
    </button>
  );
}
