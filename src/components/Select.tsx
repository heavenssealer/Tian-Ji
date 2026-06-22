// In-app dropdown replacing the native <select> (which renders with OS-native theming).
// Matches the dark popup aesthetic used by Modal. Closes on outside-click and Esc.

import { useEffect, useRef, useState } from "react";

export type SelectOption = { value: string; label: string };

export default function Select({
  value,
  options,
  onChange,
  onOpen,
  placement = "bottom",
  className = "",
  placeholder = "select",
}: {
  value: string;
  options: SelectOption[];
  onChange: (value: string) => void;
  onOpen?: () => void;
  placement?: "top" | "bottom";
  className?: string;
  placeholder?: string;
}) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onDocClick = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    window.addEventListener("mousedown", onDocClick);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", onDocClick);
      window.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const current = options.find((o) => o.value === value);

  const toggle = () => {
    if (!open) onOpen?.();
    setOpen((v) => !v);
  };

  return (
    <div ref={rootRef} className={`relative min-w-0 ${className}`}>
      <button
        type="button"
        onClick={toggle}
        className="flex w-full min-w-0 items-center gap-1.5 rounded-md border border-base-500 bg-base-800 px-2 py-1 text-left font-mono text-[11px] text-ink-dim outline-none transition-colors hover:border-base-400 focus:border-accent/50"
      >
        <span className="min-w-0 flex-1 truncate">{current?.label ?? value ?? placeholder}</span>
        <span className={`shrink-0 text-ink-faint transition-transform ${open ? "rotate-180" : ""}`}>▾</span>
      </button>

      {open && (
        <div
          className={`absolute z-50 max-h-64 w-full min-w-max overflow-auto rounded-md border border-base-500 bg-base-700 py-1 shadow-2xl ${
            placement === "top" ? "bottom-full mb-1" : "top-full mt-1"
          }`}
        >
          {options.length === 0 && (
            <div className="px-2.5 py-1.5 font-mono text-[11px] text-ink-faint">no options</div>
          )}
          {options.map((o) => (
            <button
              key={o.value}
              type="button"
              onClick={() => {
                onChange(o.value);
                setOpen(false);
              }}
              className={`flex w-full items-center gap-2 px-2.5 py-1.5 text-left font-mono text-[11px] transition-colors ${
                o.value === value
                  ? "bg-accent/12 text-accent"
                  : "text-ink-dim hover:bg-base-600 hover:text-ink"
              }`}
            >
              <span className="w-3 shrink-0 text-accent">{o.value === value ? "✓" : ""}</span>
              <span className="min-w-0 flex-1 truncate">{o.label}</span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
