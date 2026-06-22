import { memo, useCallback, useEffect, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { ipc } from "../../lib/ipc";
import { useAppStore, type ChatLine } from "../../state/stores";
import ApprovalCard from "./ApprovalCard";

const COLLAPSE_LINES = 10;

// Token budget cap presets the toolbar button cycles through (0 = unlimited).
const BUDGET_PRESETS = [0, 100_000, 250_000, 500_000, 1_000_000];
const fmtBudget = (n: number) =>
  n === 0 ? "off" : n >= 1_000_000 ? `${n / 1_000_000}M` : `${n / 1000}k`;

export default function AgentChat() {
  const chat = useAppStore((s) => s.chat);
  const pending = useAppStore((s) => s.pendingApproval);
  const isRunning = useAppStore((s) => s.isRunning);
  const isAutonomous = useAppStore((s) => s.isAutonomous);
  const setAutonomous = useAppStore((s) => s.setAutonomous);
  const isFreeMode = useAppStore((s) => s.isFreeMode);
  const setFreeMode = useAppStore((s) => s.setFreeMode);
  const isStandalone = useAppStore((s) => s.isStandalone);
  const setStandalone = useAppStore((s) => s.setStandalone);
  const goalIteration = useAppStore((s) => s.goalIteration);
  const tokenBudget = useAppStore((s) => s.tokenBudget);
  const setTokenBudget = useAppStore((s) => s.setTokenBudget);
  const totalTokens = useAppStore((s) => s.totalTokens);
  const sessions = useAppStore((s) => s.sessions);
  const currentSessionId = useAppStore((s) => s.currentSessionId);
  const pushChat = useAppStore((s) => s.pushChat);
  const setChat = useAppStore((s) => s.setChat);
  const setPendingApproval = useAppStore((s) => s.setPendingApproval);
  const setRunning = useAppStore((s) => s.setRunning);
  const newSession = useAppStore((s) => s.newSession);
  const switchSession = useAppStore((s) => s.switchSession);
  const endRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    endRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [chat.length, pending]);

  // Stable across renders so the memoized <Composer> below doesn't re-render while the user types
  // (the textarea's own state is local — typing never touches AgentChat or the message list).
  const send = useCallback(
    async (text: string) => {
      pushChat({ kind: "user", text });
      setRunning(true);
      try {
        // In standalone mode the message is the objective for an autonomous goal run; otherwise
        // it's a single chat turn.
        await (isStandalone ? ipc.agentRunGoal(text) : ipc.agentPrompt(text));
      } catch (e) {
        pushChat({ kind: "error", text: String(e) });
        setRunning(false);
      }
    },
    [isStandalone, pushChat, setRunning]
  );

  const cancel = () => {
    ipc.agentCancel().catch(() => {});
  };

  const toggleStandalone = () => {
    setStandalone(!isStandalone);
  };

  const cycleBudget = () => {
    const idx = BUDGET_PRESETS.indexOf(tokenBudget);
    const next = BUDGET_PRESETS[(idx + 1) % BUDGET_PRESETS.length] ?? 0;
    setTokenBudget(next);
    ipc.agentSetTokenBudget(next).catch(() => {});
  };

  const [learning, setLearning] = useState(false);
  const learn = async () => {
    setLearning(true);
    try {
      await ipc.agentDistillProfile();
    } catch {
      /* non-fatal */
    } finally {
      setLearning(false);
    }
  };

  const toggleAutonomous = () => {
    const next = !isAutonomous;
    setAutonomous(next);
    ipc.agentSetAutonomous(next).catch(() => {});
  };

  const toggleFreeMode = () => {
    const next = !isFreeMode;
    setFreeMode(next);
    ipc.agentSetFreeMode(next).catch(() => {});
  };

  const handleNewSession = async () => {
    const session = newSession();
    try {
      await ipc.agentNewSession(session.id);
    } catch {}
  };

  const handleSwitchSession = async (id: string) => {
    switchSession(id);
    try {
      await ipc.agentSwitchSession(id);
    } catch {}
  };

  const clearChat = () => {
    setChat([]);
  };

  return (
    <div className="flex h-full flex-col">
      {/* Session tab bar */}
      <div className="flex h-9 shrink-0 items-center gap-0.5 overflow-x-auto border-b border-base-500 bg-base-900 px-1">
        {sessions.map((s) => (
          <button
            key={s.id}
            onClick={() => void handleSwitchSession(s.id)}
            className={`flex h-6 shrink-0 items-center rounded px-2.5 text-[11px] transition-colors ${
              s.id === currentSessionId
                ? "bg-base-700 text-ink"
                : "text-ink-faint hover:bg-base-800 hover:text-ink"
            }`}
          >
            {s.name}
          </button>
        ))}
        <button
          onClick={() => void handleNewSession()}
          className="ml-0.5 flex h-6 w-6 shrink-0 items-center justify-center rounded text-[13px] text-ink-faint hover:bg-base-800 hover:text-ink"
          title="New chat"
        >
          +
        </button>
      </div>

      {/* Toolbar — single row; the panel min-width guarantees it fits, with horizontal scroll as a
          last-resort safety net so controls never wrap or clip. */}
      <div className="flex h-9 shrink-0 items-center gap-2 overflow-x-auto border-b border-base-500 px-3">
        {isRunning && (
          <span className="flex shrink-0 items-center gap-1 text-[10px] text-ink-faint">
            <span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-accent" />
            {isStandalone && goalIteration > 0 ? `autonomous · step ${goalIteration}` : "thinking…"}
          </span>
        )}
        <div className="ml-auto flex shrink-0 items-center gap-1.5">
          {(totalTokens.input + totalTokens.output) > 0 && (
            <span
              className={`shrink-0 font-mono text-[10px] tabular-nums ${
                tokenBudget > 0 && totalTokens.input + totalTokens.output >= tokenBudget
                  ? "text-danger"
                  : "text-ink-faint"
              }`}
              title={`Cumulative this session · ↑${totalTokens.input} in ↓${totalTokens.output} out`}
            >
              {(totalTokens.input + totalTokens.output).toLocaleString()}
              {tokenBudget > 0 ? ` / ${fmtBudget(tokenBudget)}` : "t"}
            </span>
          )}
          <button
            onClick={cycleBudget}
            className={`flex h-5 items-center gap-1 rounded border px-1.5 text-[10px] transition-colors ${
              tokenBudget > 0
                ? "border-accent/50 bg-accent/10 text-accent"
                : "border-base-600 text-ink-faint hover:border-base-500 hover:text-ink"
            }`}
            title="Cumulative token budget cap — click to cycle. Runs stop when reached."
          >
            ⛽ {fmtBudget(tokenBudget)}
          </button>
          {!isRunning && chat.length > 0 && (
            <button
              onClick={clearChat}
              className="rounded px-1.5 py-0.5 text-[10px] text-ink-faint hover:bg-base-700 hover:text-ink"
              title="Clear chat history"
            >
              clear
            </button>
          )}
          <button
            onClick={toggleFreeMode}
            className={`flex h-5 items-center gap-1 rounded border px-1.5 text-[10px] font-medium transition-colors ${
              isFreeMode
                ? "border-danger/60 bg-danger/20 text-danger"
                : "border-base-600 text-ink-faint hover:border-base-500 hover:text-ink"
            }`}
            title={isFreeMode ? "FREE mode ON — all policy checks bypassed, LLM runs anything" : "Enable free mode (bypasses all scope/policy checks)"}
          >
            {isFreeMode ? "☢ free" : "☢ free"}
          </button>
          <button
            onClick={toggleAutonomous}
            className={`flex h-5 items-center gap-1 rounded border px-1.5 text-[10px] transition-colors ${
              isAutonomous
                ? "border-warn/50 bg-warn/15 text-warn"
                : "border-base-600 text-ink-faint hover:border-base-500 hover:text-ink"
            }`}
            title={isAutonomous ? "Autonomous mode ON — in-scope commands auto-approved" : "Enable autonomous mode (auto-approve in-scope commands)"}
          >
            {isAutonomous ? "⚡ auto" : "⚡ auto"}
          </button>
          <button
            onClick={toggleStandalone}
            disabled={isRunning}
            className={`flex h-5 items-center gap-1 rounded border px-1.5 text-[10px] transition-colors disabled:opacity-40 ${
              isStandalone
                ? "border-accent/60 bg-accent/15 text-accent"
                : "border-base-600 text-ink-faint hover:border-base-500 hover:text-ink"
            }`}
            title={isStandalone
              ? "Standalone mode ON — your next message is an objective the agent pursues autonomously until done"
              : "Enable standalone mode (agent iterates toward a goal on its own; auto-approves in-scope commands)"}
          >
            🎯 solo
          </button>
          <button
            onClick={() => void learn()}
            disabled={isRunning || learning}
            className="flex h-5 items-center gap-1 rounded border border-base-600 px-1.5 text-[10px] text-ink-faint transition-colors hover:border-base-500 hover:text-ink disabled:opacity-40"
            title="Distill durable habits from recent activity into the agent's profile (runs on the sub-agent model — free if local)"
          >
            {learning ? "🧠 …" : "🧠 learn"}
          </button>
          {isRunning && (
            <button
              onClick={cancel}
              className="flex h-5 items-center gap-1 rounded border border-danger/40 bg-danger/10 px-1.5 text-[10px] text-danger hover:bg-danger/20"
              title="Stop the current turn"
            >
              ■ stop
            </button>
          )}
        </div>
      </div>

      <div className="min-h-0 flex-1 space-y-3 overflow-auto p-3">
        {chat.length === 0 && (
          <p className="text-[13px] leading-relaxed text-ink-faint">
            {isStandalone
              ? 'Standalone mode — describe an objective and the agent pursues it on its own, e.g. "get the user and root flags on 10.0.0.5".'
              : 'Ask the agent to begin — e.g. "enumerate services on 10.0.0.5".'}
          </p>
        )}
        {chat.map((line, i) => (
          <Message key={i} line={line} />
        ))}
        {pending && <ApprovalCard call={pending} onResolved={() => setPendingApproval(null)} />}
        <div ref={endRef} />
      </div>

      <Composer isRunning={isRunning} isStandalone={isStandalone} onSend={send} />
    </div>
  );
}

// Isolated composer with its OWN text state. Keeping the prompt here (instead of in AgentChat)
// means each keystroke re-renders only this small component — not the growing, markdown-heavy
// message list. That's what fixes the input getting laggier as the conversation grows.
const Composer = memo(function Composer({
  isRunning,
  isStandalone,
  onSend,
}: {
  isRunning: boolean;
  isStandalone: boolean;
  onSend: (text: string) => void;
}) {
  const [prompt, setPrompt] = useState("");

  const submit = () => {
    const text = prompt.trim();
    if (!text || isRunning) return;
    setPrompt("");
    onSend(text);
  };

  return (
    <div className="border-t border-base-500 p-2.5">
      <div className="flex items-end gap-2 rounded-card border border-base-500 bg-base-800 p-2 focus-within:border-accent/40">
        <textarea
          value={prompt}
          onChange={(e) => setPrompt(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              submit();
            }
          }}
          rows={2}
          placeholder={isStandalone ? "Describe the objective to pursue autonomously…" : "Ask the agent, or describe a target…"}
          className="min-h-0 flex-1 resize-none bg-transparent text-[13px] text-ink outline-none placeholder:text-ink-faint"
        />
        <button
          onClick={submit}
          disabled={isRunning}
          className="flex h-7 w-7 shrink-0 items-center justify-center rounded-md bg-accent text-base-900 transition-opacity hover:opacity-90 disabled:opacity-40"
          title="Send (Enter)"
        >
          ↑
        </button>
      </div>
      <div className="mt-1.5 px-1 text-right text-[10px] text-ink-faint">
        Enter to send · Shift+Enter for newline
      </div>
    </div>
  );
});

const SUBAGENT_COLORS: Record<string, string> = {
  recon:   "text-sky-400",
  web:     "text-violet-400",
  exploit: "text-rose-400",
};

// Memoized: chat lines keep object identity across store updates (pushChat appends; applyDelta
// only swaps the last line), so only the changed/streaming message re-runs ReactMarkdown — the
// rest of the history stays put.
const Message = memo(function Message({ line }: { line: ChatLine }) {
  if (line.kind === "user") {
    return (
      <div className="flex justify-end">
        <div className="max-w-[85%] rounded-card rounded-br-sm bg-accent px-3 py-1.5 text-[13px] text-base-900">
          {line.text}
        </div>
      </div>
    );
  }
  if (line.kind === "tool") {
    return <ToolBlock text={line.text} />;
  }
  if (line.kind === "error") {
    return (
      <div className="rounded-card border border-danger/30 bg-danger/10 px-2.5 py-1.5 text-[12px] text-danger">
        {line.text}
      </div>
    );
  }
  if (line.kind === "finding") {
    return (
      <div className="rounded-card border border-warn/30 bg-warn/8 px-2.5 py-1.5 text-[11px] text-warn">
        ◆ Finding: {line.text}
      </div>
    );
  }
  // Sub-agent bubbles are identified by a "[sub:name]" internal prefix.
  if (line.kind === "agent" && line.text.startsWith("[sub:")) {
    const end = line.text.indexOf("]");
    const name = line.text.slice(5, end);
    const body = line.text.slice(end + 1);
    return <SubAgentBubble name={name} text={body} />;
  }
  return <AgentBubble text={line.text} />;
});

function ToolBlock({ text }: { text: string }) {
  const isCommand = text.startsWith("$ ");
  const lines = text.split("\n");
  const isLong = !isCommand && lines.length > COLLAPSE_LINES;
  const [expanded, setExpanded] = useState(false);
  const [saved, setSaved] = useState(false);

  const displayText = isLong && !expanded
    ? lines.slice(0, COLLAPSE_LINES).join("\n")
    : text;

  const saveToNotebook = async () => {
    await ipc.notesAdd(text).catch(() => {});
    setSaved(true);
    setTimeout(() => setSaved(false), 1800);
  };

  return (
    <div className="group relative rounded-card border border-base-500 bg-base-800">
      <pre className="overflow-x-auto px-2.5 py-1.5 font-mono text-[11px] leading-relaxed text-mono whitespace-pre-wrap">
        {displayText}
      </pre>

      {isLong && (
        <button
          onClick={() => setExpanded((v) => !v)}
          className="w-full border-t border-base-600 px-2.5 py-1 text-left font-mono text-[10px] text-ink-faint hover:bg-base-700 hover:text-ink"
        >
          {expanded
            ? "▴ collapse"
            : `▾ show ${lines.length - COLLAPSE_LINES} more lines`}
        </button>
      )}

      {!isCommand && (
        <button
          onClick={() => void saveToNotebook()}
          className={`absolute right-1.5 top-1.5 flex h-5 w-5 items-center justify-center rounded text-[10px] transition-all ${
            saved
              ? "bg-ok/20 text-ok"
              : "bg-base-700 text-ink-faint opacity-0 group-hover:opacity-100 hover:text-ink"
          }`}
          title="Save to notebook"
        >
          {saved ? "✓" : "✚"}
        </button>
      )}
    </div>
  );
}

function SubAgentBubble({ name, text }: { name: string; text: string }) {
  const color = SUBAGENT_COLORS[name] ?? "text-ink-faint";
  return (
    <div className="flex gap-2 pl-4">
      <div className={`mt-0.5 flex h-5 w-5 shrink-0 items-center justify-center rounded border border-current/20 font-mono text-[9px] ${color}`}>
        {name[0]?.toUpperCase()}
      </div>
      <div className="min-w-0 flex-1">
        <div className={`mb-0.5 font-mono text-[10px] ${color}`}>{name} agent</div>
        <div className="prose-agent">
          <ReactMarkdown
            remarkPlugins={[remarkGfm]}
            components={{
              p: ({ children }) => <p className="mb-1.5 text-[12px] leading-relaxed text-ink-dim last:mb-0">{children}</p>,
              code: ({ children, className }) =>
                className ? (
                  <pre className="my-1 overflow-x-auto rounded border border-base-500 bg-base-800 px-2 py-1.5 font-mono text-[11px] text-mono">
                    <code>{children}</code>
                  </pre>
                ) : (
                  <code className="rounded bg-base-700 px-1 py-0.5 font-mono text-[10px] text-accent">{children}</code>
                ),
              ul: ({ children }) => <ul className="mb-1 ml-3 list-disc space-y-0.5 text-[12px] text-ink-dim">{children}</ul>,
              li: ({ children }) => <li className="leading-relaxed">{children}</li>,
            }}
          >
            {text}
          </ReactMarkdown>
        </div>
      </div>
    </div>
  );
}

function AgentBubble({ text }: { text: string }) {
  const [saved, setSaved] = useState(false);

  const saveToNotebook = async () => {
    await ipc.notesAdd(text).catch(() => {});
    setSaved(true);
    setTimeout(() => setSaved(false), 1800);
  };

  return (
    <div className="group flex gap-2">
      <div className="mt-0.5 flex h-5 w-5 shrink-0 items-center justify-center rounded bg-accent/15 font-mono text-[10px] text-accent">
        天
      </div>
      <div className="relative min-w-0 flex-1">
        <div className="prose-agent">
          <ReactMarkdown
            remarkPlugins={[remarkGfm]}
            components={{
              p:      ({ children }) => <p className="mb-1.5 text-[13px] leading-relaxed text-ink-dim last:mb-0">{children}</p>,
              strong: ({ children }) => <strong className="font-semibold text-ink">{children}</strong>,
              em:     ({ children }) => <em className="text-ink-dim">{children}</em>,
              ul:     ({ children }) => <ul className="mb-1.5 ml-3 list-disc space-y-0.5 text-[13px] text-ink-dim">{children}</ul>,
              ol:     ({ children }) => <ol className="mb-1.5 ml-3 list-decimal space-y-0.5 text-[13px] text-ink-dim">{children}</ol>,
              li:     ({ children }) => <li className="leading-relaxed">{children}</li>,
              code:   ({ children, className }) =>
                className ? (
                  <pre className="my-1.5 overflow-x-auto rounded-md border border-base-500 bg-base-800 px-2.5 py-2 font-mono text-[11px] text-mono">
                    <code>{children}</code>
                  </pre>
                ) : (
                  <code className="rounded bg-base-700 px-1 py-0.5 font-mono text-[11px] text-accent">{children}</code>
                ),
              h1: ({ children }) => <p className="mb-1 text-[13px] font-semibold text-ink">{children}</p>,
              h2: ({ children }) => <p className="mb-1 text-[13px] font-semibold text-ink">{children}</p>,
              h3: ({ children }) => <p className="mb-1 text-[13px] font-medium text-ink">{children}</p>,
              a:  ({ children, href }) => <a href={href} className="text-accent underline underline-offset-2">{children}</a>,
            }}
          >
            {text}
          </ReactMarkdown>
        </div>
        <button
          onClick={() => void saveToNotebook()}
          className={`absolute -right-1 top-0 flex h-5 w-5 items-center justify-center rounded text-[10px] transition-all ${
            saved
              ? "bg-ok/20 text-ok"
              : "bg-base-700 text-ink-faint opacity-0 group-hover:opacity-100 hover:text-ink"
          }`}
          title="Save to notebook"
        >
          {saved ? "✓" : "✚"}
        </button>
      </div>
    </div>
  );
}
