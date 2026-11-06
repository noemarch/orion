# Orion: Engineering Guidelines & Agent Invariants

> **Scope:** These rules govern any engineer or AI coding assistant modifying the Orion codebase (`C:\rust-projects\orion`). Orion is a local-first, standalone AI agent orchestrator and multi-device control panel built with a native Rust core (`orion-core`), a Tauri v2 desktop shell (`src-tauri`), and a React 19 companion frontend (`src/`).

---

## 1. Core Architecture & Philosophy

1. **No External Multiplexer:** Orion is an independent orchestrator. It does not wrap, bridge, or depend on `herdr`, `tmux`, or `zellij`. Orion directly allocates PTYs, supervises agent subprocesses, manages workspaces/worktrees, and serves remote clients.
2. **Dual Delivery:**
   - **Tauri v2 Desktop App:** Runs locally on Windows, macOS, and Linux with native OS window management and system tray integration.
   - **Headless Server (`orion serve`):** Runs on headless workstations or devboxes, serving the exact same React 19 UI over Tailscale or LAN as a mobile-optimized PWA.
3. **Bring-Your-Own-Subscription:** Orion drives standard agent CLIs (`claude`, `codex`, `opencode`, `cursor`, `grok`, `omp`, etc.) using credentials already authenticated on the host machine.
4. **Chat-First Clarity, Terminal Truth:** The primary interface renders structured conversational turns, syntax-highlighted diffs, and prompt approval cards; a live xterm.js PTY terminal is always one click or tap away with zero state loss.

---

## 2. Technical Invariants & Traps

### 2.1 PTY Engine & Process Control (`orion-core::pty`, `orion-core::agents`)
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

- Crate code in `orion-core` must remain independent of Tauri GUI APIs so it can be compiled directly into the standalone `orion` headless CLI.
- Shared types between Rust and TypeScript are defined in `crates/orion-core/src/protocol.rs` and mirrored in `src/types/protocol.ts`.

---

## 4. Verification & Testing Standards

Before submitting any code change:
1. **Rust Quality:**
   ```bash
   cargo check --workspace
   cargo test --workspace
   cargo clippy --workspace -- -D warnings
   cargo fmt --check
   ```
2. **Frontend Quality:**
   ```bash
   bun run typecheck
   bun run lint
   bun test
   ```
3. **Smoke Proof:** Any PR altering PTY allocation, agent session parsing, or WebSocket routing must exercise the changed path with a real or simulated agent turn and verify both terminal and chat rendering.

---

## 5. Decision Hierarchy

When trade-offs arise:
1. **Correctness & Process Safety:** An agent subprocess must never be orphaned, leak handles, or run uncontrolled.
2. **Companion Responsiveness:** A phone user should approve an agent prompt in under 2 seconds from notification.
3. **Fidelity:** Terminal output in xterm.js must be byte-for-byte identical to the raw PTY stream.
4. **Code Simplicity:** Prefer boring, direct Rust implementations over complex macro-heavy or over-abstracted layers.
