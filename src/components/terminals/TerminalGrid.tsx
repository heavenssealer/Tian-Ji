// Tabbed terminals, each backed by its own tracked PTY. The "+" spawns another; the user types
// here and approved agent commands run here too. Panes stay mounted (so their PTYs survive tab
// switches); inactive ones are hidden and refit on re-show via a ResizeObserver.

import { useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { ipc } from "../../lib/ipc";
import { onPtyOutput } from "../../lib/events";
import { NOTEBOOK_SAVED_EVENT } from "../layout/AppShell";
import { useEffect } from "react";

export default function TerminalGrid() {
  const [tabs, setTabs] = useState<number[]>([1]);
  const [active, setActive] = useState(1);
  const nextId = useRef(2);

  const add = () => {
    const id = nextId.current++;
    setTabs((t) => [...t, id]);
    setActive(id);
  };

  const close = (id: number) => {
    setTabs((t) => {
      const rest = t.filter((x) => x !== id);
      if (id === active && rest.length) setActive(rest[rest.length - 1]);
      return rest;
    });
  };

  return (
    <div className="flex flex-1 min-h-0 flex-col bg-base-800">
      <div className="flex h-9 shrink-0 items-center gap-1 border-b border-base-500 px-2.5">
        <span className="label mr-1">terminals</span>
        {tabs.map((id, i) => (
          <span
            key={id}
            onClick={() => setActive(id)}
            className={`group flex cursor-pointer items-center gap-1.5 rounded-md px-2 py-0.5 text-[11px] font-mono transition-colors ${
              id === active ? "bg-accent/12 text-accent ring-1 ring-accent/25" : "text-ink-faint hover:bg-base-600"
            }`}
          >
            term {i + 1}
            {tabs.length > 1 && (
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  close(id);
                }}
                className="opacity-0 transition-opacity hover:text-danger group-hover:opacity-100"
                title="Close terminal"
              >
                ✕
              </button>
            )}
          </span>
        ))}
        <button
          onClick={add}
          className="flex h-5 w-5 items-center justify-center rounded text-ink-faint hover:bg-base-600 hover:text-ink"
          title="New terminal"
        >
          +
        </button>
      </div>

      <div className="relative min-h-0 flex-1">
        {tabs.map((id) => (
          <div key={id} className={`absolute inset-0 ${id === active ? "" : "hidden"}`}>
            <XtermPane />
          </div>
        ))}
      </div>
    </div>
  );
}

function XtermPane() {
  const hostRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const term = new Terminal({
      fontFamily: "Cascadia Code, JetBrains Mono, Consolas, monospace",
      fontSize: 12,
      theme: { background: "#0d0d0f", foreground: "#e9e9ec", cursor: "#e8833a" },
      cursorBlink: true,
      scrollback: 5000,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    if (hostRef.current) term.open(hostRef.current);

    let terminalId = "";
    let unlisten: (() => void) | undefined;

    // Refit whenever the host changes size. After fitting, explicitly push the new dimensions
    // to the PTY — belt-and-suspenders on top of onResize, handles cases where xterm's
    // dimensions didn't change (no onResize fires) but the displayed area did.
    const refit = () =>
      requestAnimationFrame(() => {
        try {
          if (hostRef.current && hostRef.current.clientHeight > 0) {
            fit.fit();
            if (terminalId && term.cols && term.rows) {
              void ipc.terminalResize(terminalId, term.cols, term.rows);
            }
          }
        } catch {
          /* host not measured yet */
        }
      });

    (async () => {
      try {
        terminalId = await ipc.terminalSpawn("term");
        unlisten = await onPtyOutput((p) => {
          if (p.terminal_id === terminalId) term.write(new Uint8Array(p.chunk));
        });
            term.onData((d) => void ipc.terminalWrite(terminalId, d));
        // Ctrl+Shift+N while the terminal is focused — capture selection to notebook.
        term.attachCustomKeyEventHandler((e) => {
          if (e.ctrlKey && e.shiftKey && e.key === "N" && e.type === "keydown") {
            const sel = term.getSelection();
            if (sel) {
              ipc.notesAdd(sel).then(() =>
                window.dispatchEvent(new CustomEvent(NOTEBOOK_SAVED_EVENT))
              );
            }
            return false; // prevent xterm from handling this key
          }
          return true;
        });
        // Fit now that the id is known. Double-rAF so the explicit resize in refit fires
        // after refit's own rAF has had time to complete.
        refit();
        requestAnimationFrame(() =>
          requestAnimationFrame(() => {
            if (terminalId && term.cols && term.rows) {
              void ipc.terminalResize(terminalId, term.cols, term.rows);
            }
          })
        );
      } catch (e) {
        term.write(`\r\n\x1b[31mterminal unavailable: ${String(e)}\x1b[0m\r\n`);
      }
    })();

    const ro = new ResizeObserver(refit);
    if (hostRef.current) ro.observe(hostRef.current);

    return () => {
      ro.disconnect();
      unlisten?.();
      if (terminalId) void ipc.terminalClose(terminalId);
      term.dispose();
    };
  }, []);

  // Absolute fill gives xterm a definite box (fixes scroll/sizing glitches). `inset-2` provides
  // breathing room WITHOUT internal padding, which would otherwise skew FitAddon's row math.
  return <div ref={hostRef} className="absolute inset-2" />;
}
