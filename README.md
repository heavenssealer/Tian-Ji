# Tiān Jī (天机)

An LLM-orchestrated pentesting framework. Multiple concurrent terminals, cloud/local agents
that drive system tooling under human-controlled guardrails, per-engagement workspaces,
persistent memory, and a phase-aware UI.

- Architecture & rationale: [`DESIGN.md`](./DESIGN.md)
- Crate/module map: [`SKELETON.md`](./SKELETON.md)

> Status: **v0.1 vertical slice — building and functional.** The full loop is wired:
> workspaces → Claude agent → tiered-approval tool execution → event log → live xterm + streaming
> chat. `cargo test --workspace` and `cargo check --workspace` pass; the frontend typechecks and
> builds. To use the agent you need an Anthropic API key (see [API key](#api-key) below).

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

## API key

The agent requires an **Anthropic API key**. Enter it once in the Settings panel (⚙ icon) —
it is stored in the OS keychain and never written to disk in plaintext.

Get a key at [console.anthropic.com](https://console.anthropic.com).

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
