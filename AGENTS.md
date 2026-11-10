# Orion: Engineering Guidelines & Agent Invariants

> **Scope:** These rules govern any engineer or AI coding assistant modifying the Orion codebase (`C:\rust-projects\orion`). Orion is a local-first, standalone AI agent orchestrator and multi-device control panel built with a native Rust core (`orion-core`) and a native GPUI desktop shell (`crates/orion-ui`). Tauri (`src-tauri`), where retained, is a launcher/tray host only and does not render the UI (see `docs/adr/desktop-shell-ownership.md`).

---

## 1. Core Architecture & Philosophy

1. **Agent-Only Orchestration (No Generic Shells):** Orion orchestrates AI agent CLIs exclusively (`claude`, `codex`, `opencode`, `cursor`, `grok`, `omp`, `omo`, `pi`, `agy`, `hermes`, `devin`). Orion does **not** provide generic bash/zsh/PowerShell terminal tabs. Every workspace pane is strictly a dedicated AI agent instance. The terminal lens in Orion is the live, inspectable PTY of that *agent's* interactive CLI, not an empty shell prompt.
2. **No External Multiplexer:** Orion is an independent orchestrator. It does not wrap, bridge, or depend on any external terminal multiplexer. Orion directly allocates PTYs, supervises agent subprocesses, manages workspaces/worktrees, and serves remote clients.
3. **Dual Delivery:**
   - **Native GPUI Desktop App:** Runs locally on Windows, macOS, and Linux with native OS window management and system tray integration.
   - **Headless Server (`orion serve`):** Runs on headless workstations or devboxes, serving the Engine over Tailscale or LAN.
4. **Bring-Your-Own-Subscription:** Orion drives standard agent CLIs using credentials already authenticated on the host machine (`claude auth login`, `codex login`, etc.).
5. **Chat-First Clarity, Terminal Truth:** The primary interface renders structured conversational turns, syntax-highlighted diffs, and prompt approval cards; a live xterm.js PTY terminal of the agent CLI is always one click or tap away with zero state loss.

---

## 2. Technical Invariants & Traps

### 2.1 PTY Engine & Process Control (`orion-core::pty`, `orion-core::agents`)
- **Agent CLI Target Exclusivity:** Never spawn a generic `sh`, `bash`, `cmd`, or `powershell` as a top-level pane. Every spawned PTY must execute an identified AI agent CLI binary discovered on `PATH`.
- **PTY Threading Invariant:** PTY reads are blocking. Never perform blocking PTY reads directly on a Tokio async worker thread. Always spawn a dedicated background thread or use `tokio::task::spawn_blocking` with bounded `mpsc` channels.
- **ConPTY Windows Parity:** On Windows, use `portable-pty` with ConPTY enabled (`CreatePseudoConsole`). Ensure terminal resize events send accurate character and pixel dimensions to prevent text clipping in full-screen TUIs.
- **Process Group Termination:** When terminating an agent, terminate the entire process tree.
  - On Windows: Assign child processes to a Windows Job Object configured with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
  - On Unix: Set process group ID (`setpgid`) at spawn and signal the process group (`kill(-pgid, SIGTERM)` followed by `SIGKILL` after a timeout).
- **Signal Escalation:** Always follow graceful degradation: send `SIGINT` (Ctrl+C equivalent), wait up to 3 seconds for graceful cleanup, escalate to `SIGTERM`, and finally `SIGKILL` if unresponsive.
- **Zero Shell Wrapper Bloat:** Spawn agent executables directly whenever possible rather than wrapping in `sh -c` or `powershell -c`, avoiding unnecessary shell wrapper processes.

### 2.2 Agent Session Introspection (`orion-core::transcript`)
- **Read-Only Invariant:** Never modify an agent's internal state files or databases. Open SQLite session databases with `SQLITE_OPEN_READONLY` and `WAL` mode compatibility.
- **Atomic JSONL Streaming:** When reading Claude Code or omp JSONL files, stream line-by-line using file byte offsets. Never reload the entire JSONL file on each file change event.
- **Graceful Parser Fallback:** If a CLI's internal transcript format changes, the parser must return a structured fallback error and fall back cleanly to raw terminal scrollback without crashing the application.
- **Process Identification:** Never match agent sessions solely by working directory. Verify process PID ancestry, environment locks, or explicit session IDs.

### 2.3 Tauri v2 & IPC (`src-tauri`)
- **Tauri v2 Permissions:** All IPC commands must be explicitly registered in `src-tauri/capabilities/default.json`. Do not use wildcard `*` permissions.
- **State Management:** Tauri managed state (`tauri::State`) must be thread-safe (`Arc<RwLock<T>>` or `tokio::sync::RwLock`).
- **Typed Command Envelopes:** All Tauri IPC commands and WebSocket messages must use strongly-typed Serde structs that mirror `src/types/protocol.ts`.
- **Event Namespacing:** Use namespaced event keys:
  - `orion://pty-output` (PTY stdout streaming to terminal)
  - `orion://agent-status` (Live state updates: Idle, Running, NeedsInput, Done)
  - `orion://prompt-ask` (Structured approval requests)
  - `orion://workspace-update` (Workspace and tab layout sync)

### 2.4 Remote Gateway & Security (`orion-core::server`)
- **Local-First & Zero Telemetry:** Never send analytics, telemetry, or user prompts to external servers. No telemetry dependencies are permitted.
- **Tailscale Ingress Safety:** When serving over Tailscale, use `tailscale serve --https` strictly. **Never invoke `tailscale funnel`**, which exposes ports to the public internet.
- **Timing-Safe Auth:** Authenticate device tokens and pairing codes using constant-time comparison (`subtle::ConstantTimeEq`) to prevent timing attacks.
- **Secret Isolation:** User API keys (Anthropic, OpenAI, GitHub tokens) stored in settings must never be sent in client WebSocket messages or logged to stdout.
- **Draft Safety:** User input in the composer is held in local client storage. It must never be dispatched automatically upon reconnection; the user must explicitly review and press Send.

### 2.5 Frontend & Touch Ergonomics (`src/`)
- **Design System ("Quiet Slate"):**
  - Dark mode: Near-black plum ground (`--surface-ground`), flat panels (`--surface-panel`), muted border outlines.
  - Single Chrome Accent: Muted rose is reserved for active focus, selected tabs, cursor, and primary action buttons.
  - Semantic Status Colors: Agent states have reserved tokens: `RUN` (blue/cyan pulse), `INPUT` (amber attention), `DONE` (emerald green), `ERROR` (ruby red). Never use rose for agent status.
- **Touch-First Hit Targets:** Every clickable element on mobile viewports must meet or exceed the 44px minimum touch target size (`--touch-target`).
- **Mobile Touch Key Bar:** The terminal view on touch devices must render the on-screen key bar (Esc, Tab, Ctrl, Alt, Shift, arrows, Enter) without dismissing the soft keyboard.
- **Secret Input Isolation:** Sensitive inputs (passwords, PINs, sudo prompts) must stay in DOM state and never enter undo queues, React persistent storage, or unencrypted local storage.
- **Terminal Resize Debouncing:** Debounce xterm.js resize observer events by 50ms to prevent flooding the backend PTY with ConPTY resize requests during window dragging.

---

## 3. Directory Layout Conventions

```
orion/
├── crates/orion-core/src/     # Domain logic, PTY, agent parsers, Axum gateway
├── src-tauri/                 # Tauri v2 wrapper, desktop windows, IPC handlers
├── src/                       # React 19 frontend (components, hooks, styles)
├── docs/                      # Technical specs, architecture decision records (ADRs)
└── tests/                     # Integration tests and end-to-end fixtures
```

---

## 4. Verification & Testing Standards

Before submitting any code change:
1. **Rust Quality:**
   ```bash
   cargo check -p orion-core
   cargo test -p orion-core
   cargo clippy -p orion-core -- -D warnings
   cargo fmt --check
   ```
   A change in `crates/orion-ui` (the native GPUI shell) runs the same gates for that crate:
   ```bash
   cargo check -p orion-ui
   cargo test -p orion-ui
   cargo clippy -p orion-ui -- -D warnings
   ```
2. **Frontend Quality:**
   ```bash
   bun run typecheck
   bun run build
   ```
   The React frontend is gone; `crates/orion-ui` replaces it, and its gates are the `cargo` ones above.
3. **Smoke Proof:** Any change touching agent launching or PTY streaming must verify that the spawned process is an identified AI agent CLI and that stdin/stdout/resize function properly.

---

## Agent skills

### Issue tracker

Issues and specs live in GitHub Issues for `noemarch/orion`. See `docs/agents/issue-tracker.md`.

### Triage labels

The default five-role triage vocabulary: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, and `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context layout using root `GLOSSARY.md` and `docs/adr/`. See `docs/agents/domain.md`.
