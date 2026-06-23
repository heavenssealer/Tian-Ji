# Tiān Jī (天机)

An LLM-orchestrated pentesting framework. Multiple concurrent terminals, cloud/local agents
that drive system tooling under human-controlled guardrails, per-engagement workspaces,
persistent memory, and a phase-aware UI.

- Architecture & rationale: [`DESIGN.md`](./DESIGN.md)
- Crate/module map: [`SKELETON.md`](./SKELETON.md)
- Codebase guide (for contributors / Claude Code): [`CLAUDE.md`](./CLAUDE.md)

> Status: **v0.1.0 — functional, well past the original vertical slice.** The full loop is wired
> and extended: workspaces → Claude **or local (Ollama)** agent → tiered-approval tool execution
> → append-only event log → live xterm + streaming chat. Beyond the MVP it now ships
> **subscription (OAuth) auth** alongside API keys, an **autonomous goal loop**, **sub-agent
> delegation**, an **attempt/trace log**, **on-demand recall**, **Agent Skills** (CTF playbooks),
> and an aggressive **token-economy** layer (prompt caching, rolling compaction, output
> summarization, read-only command dedup, and optional [RTK](#token-economy) compression).
> `cargo test --workspace` and `cargo check --workspace` pass; the frontend typechecks and builds.
> To use a cloud agent you need an Anthropic API key **or** a Claude Pro/Max subscription (see
> [Authentication](#authentication)); local models need no key.

---

## Prerequisites

| Requirement | Version | Notes |
|---|---|---|
| **Node.js** | ≥ 20 | `node --version` to check |
| **Rust** | stable (≥ 1.77) | install via [rustup.rs](https://rustup.rs) |
| **System libs** | see below | WebKitGTK, D-Bus, etc. (Linux only) |

---

## Linux setup

Tauri 2.0 requires WebKitGTK 4.1 and a handful of other system libraries. The exact package
names differ by distro.

### Ubuntu 22.04+ / Debian 12+

```bash
sudo apt-get update
sudo apt-get install -y \
  build-essential \
  curl \
  wget \
  file \
  pkg-config \
  libssl-dev \
  libwebkit2gtk-4.1-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev \
  libdbus-1-dev \
  libglib2.0-dev \
  libgtk-3-dev
```

> **Ubuntu 20.04**: `libwebkit2gtk-4.1-dev` is not available. Upgrade to 22.04 or use the
> AppImage release instead of building from source.

### Fedora / RHEL 9+

```bash
sudo dnf install -y \
  gcc \
  openssl-devel \
  webkit2gtk4.1-devel \
  libappindicator-gtk3-devel \
  librsvg2-devel \
  dbus-devel \
  gtk3-devel
```

### Arch Linux

```bash
sudo pacman -Syu --needed \
  base-devel \
  openssl \
  webkit2gtk-4.1 \
  libayatana-appindicator \
  librsvg \
  dbus \
  gtk3
```

### Keyring on Linux

The app stores the Anthropic API key via the system **Secret Service** (GNOME Keyring or KDE
Wallet). Make sure one of these is running in your session:

```bash
# GNOME/Ubuntu — usually pre-installed
gnome-keyring-daemon --start

# KDE
# KWallet is started automatically with the KDE session

# Headless / no DE — run a lightweight service:
sudo apt-get install gnome-keyring
dbus-run-session -- gnome-keyring-daemon --unlock
```

If you get a `No such interface` or `org.freedesktop.secrets` error when saving the API key,
install and start `gnome-keyring`.

---

## Windows setup

WebView2 is pre-installed on Windows 10 (v1803+) and Windows 11. No extra system packages
needed beyond Rust and Node.

---

## macOS setup

No extra system packages needed. Xcode Command Line Tools provide the compiler:

```bash
xcode-select --install
```

---

## Install Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
rustup default stable
```

---

## Clone & build

```bash
git clone https://github.com/heavenssealer/Tian-Ji.git
cd Tian-Ji
npm install
```

### Development (hot-reload)

```bash
npm run tauri dev
```

### Production build

```bash
npm run tauri build
# Output: src-tauri/target/release/bundle/
#   Linux  → .deb + .AppImage
#   Windows → .msi + NSIS .exe
#   macOS  → .dmg + .app
```

---

## Privileged tools (sudo)

Some pentesting tools need root access (nmap SYN/raw-socket scans, tcpdump, masscan, arp-scan,
editing `/etc/hosts`, etc.). The agent handles this two ways:

- **Auto-elevation**: `nmap`, `masscan`, `rustscan`, `tcpdump`, `tshark`, `arp-scan`, and
  `netdiscover` are automatically wrapped with `sudo -n` on Linux/macOS. `sudo -n` is
  non-interactive — it fails immediately with a clear error if passwordless sudo is not
  configured, rather than hanging.
- **Explicit sudo**: for everything else (file writes, `ip`, `iptables`, etc.) the LLM uses
  `sudo` as the tool name directly.

**One-time sudoers setup** — grant NOPASSWD for the tools you use:

```bash
sudo visudo -f /etc/sudoers.d/tianji
```

Paste (replace `youruser` with your username):

```
youruser ALL=(ALL) NOPASSWD: /usr/bin/nmap, /usr/bin/masscan, /usr/bin/rustscan, \
    /usr/bin/tcpdump, /usr/bin/tshark, /usr/sbin/arp-scan, /usr/sbin/netdiscover, \
    /usr/bin/tee, /bin/tee, /usr/bin/ip, /sbin/iptables
```

> **Kali Linux**: the default user runs as root — no sudoers config needed.

> **macOS**: use `sudo visudo` and add the same block. Tool paths may differ
> (`/opt/homebrew/bin/nmap` etc.) — check with `which nmap`.

## Authentication

A cloud agent authenticates one of two ways, configured in the Settings panel (⚙ icon):

- **Anthropic subscription (Claude Pro/Max)** — log in via OAuth; turns bill your subscription,
  exactly like the Claude Code CLI. Takes precedence over an API key when both are present.
- **Anthropic API key** — billed to your org's API credits. Get one at
  [console.anthropic.com](https://console.anthropic.com).
- **DeepSeek API key** — for the `deepseek-*` models (OpenAI-compatible). Paste a key from
  [platform.deepseek.com](https://platform.deepseek.com); billed to your DeepSeek account.

Each credential is stored in the OS keychain (Windows Credential Manager / macOS Keychain /
GNOME Keyring or KDE Wallet) and never written to disk in plaintext. Disconnect the subscription
to fall back to the Anthropic API key; the DeepSeek key is only used when a `deepseek-*` model is
selected.

**Local models (no key required):** select an `ollama:<model>` entry in the model picker to run
fully offline against a local [Ollama](https://ollama.com) instance (`ollama pull <model>` first).
Sensitive engagements stay on-box. Configure the Ollama host and context window in Settings.

### Model picker

Pick the orchestrator model in Settings — `claude-opus-4-8` (default), `claude-sonnet-4-6`,
`claude-haiku-4-5`, a DeepSeek model (`deepseek-chat`, `deepseek-reasoner`, `deepseek-v4-pro`,
`deepseek-v4-flash`), or any pulled `ollama:` model. Sub-agents drop to a cheaper sibling for grunt
work: Opus→Sonnet and DeepSeek's reasoner→chat automatically (a major cost lever).

## Token economy

An agent that drives attacker tooling over many rounds burns tokens fast. Several layers keep cost
bounded (see [`CLAUDE.md`](./CLAUDE.md) for the mechanics):

- **Prompt caching** — the stable system prompt + tool schemas are sent as a cached prefix, so from
  turn two they bill at ~10% of input price. The volatile context (scope, notes, attempt log) sits
  after the cache breakpoint.
- **Rolling compaction** — once a session's history passes 75% of the context budget, the oldest
  turns are summarized into a dense brief on the cheap (Sonnet/local) model instead of re-sent.
- **Output summarization + dedup** — large tool output is summarized before it enters context (the
  raw stays addressable via the `recall` tool); identical read-only commands reuse their prior
  result instead of re-running.
- **RTK (Rust Token Killer)** — optional: when the `rtk` binary is installed, output of supported
  read-only tools (`ls`, `grep`, `git`, `cargo`, …) is compressed 60–90% before it reaches the
  model. On by default, a silent no-op if `rtk` isn't found.
- **Cost meter + budget cap** — cumulative token spend is shown per workspace; set a hard cap to
  stop a runaway run. The autonomous goal loop additionally self-stops at 15 iterations or
  ~600k tokens.

> Rough order of magnitude: an autonomous **HTB box (user + root) costs ~250k–600k metered tokens
> ≈ $2–6** on `claude-opus-4-8`, depending on how cleanly it solves. The goal loop's ~600k-token
> ceiling is the hard stop.

---

## Verify the build (no desktop required)

```bash
npx tsc --noEmit              # frontend typecheck
npm run build                 # frontend bundle
cargo test --workspace        # Rust unit tests
cargo check --workspace       # whole workspace typecheck
```

---

## Project layout

```
crates/
  tianji-types     shared domain types (leaf)
  tianji-policy    policy engine — PURE, no I/O (the safety spine)
  tianji-store     event log + read-models (rusqlite, bundled)
  tianji-pty       terminal/PTY manager (portable-pty)
  tianji-llm       LlmProvider trait + Claude adapter
  tianji-agent     orchestrator · MCP host · context assembler · approval gate
src-tauri/         desktop binary — IPC glue only
src/               web frontend (Vite + React + Tailwind)
```

---

## Safety posture

Every agent-proposed command is routed through `tianji-policy` before it can touch a terminal:
scope-check (real argv parsed for targets) → classify → tiered approval. Unknown commands fail
closed to human approval; the LLM never classifies its own risk. See `DESIGN.md` §4.

Three operating modes (toggle in the agent chat toolbar):

| Mode | What it does |
|---|---|
| Default | Every non-read-only command requires explicit approval |
| **⚡ auto** | In-scope commands auto-approve; out-of-scope and explicit denials still block |
| **☢ free** | All policy checks bypassed — LLM runs whatever it judges useful. Use only in lab/trusted environments. |
