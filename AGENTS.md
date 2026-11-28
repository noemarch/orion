# Orion: Engineering Guidelines & Agent Invariants

> **Scope:** These rules govern any engineer or AI coding assistant modifying the Orion codebase (`C:\rust-projects\orion`). Orion is a local-first, standalone AI agent orchestrator and multi-device control panel built with a native Rust core (`orion-core`) and a native GPUI desktop shell (`crates/orion-ui`). The app is pure Rust: there is no web view and no JavaScript (see `docs/adr/desktop-shell-ownership.md`).

---

## 1. Core Architecture & Philosophy

1. **Agent-Only Orchestration (With One Shell Exception):** Orion orchestrates AI agent CLIs (`claude`, `codex`, `opencode`, `cursor`, `grok`, `omp`, `omo`, `pi`, `agy`, `hermes`, `devin`). Agent panes are strictly dedicated AI agent instances and their terminal lens is that agent's live, inspectable PTY. The one deliberate exception is the user-invoked shell terminal pane: it runs a shell chosen in the UI (Windows PowerShell, PowerShell 7, Bash, or Command Prompt) in the same native libghostty-backed view, and is created only through `POST /api/pane/shell`. Shell panes are the only panes allowed a generic shell; they never run agent status detection.
2. **No External Multiplexer:** Orion is an independent orchestrator. It does not wrap, bridge, or depend on any external terminal multiplexer. Orion directly allocates PTYs, supervises agent subprocesses, manages workspaces/worktrees, and serves remote clients.
3. **Dual Delivery:**
   - **Native GPUI Desktop App:** Runs locally on Windows, macOS, and Linux with native OS window management. No system tray, desktop notifications, in-app updater, or single-instance guard; see [`docs/adr/desktop-shell-ownership.md`](docs/adr/desktop-shell-ownership.md).
   - **Headless Server (`orion serve`):** Runs on headless workstations or devboxes, serving the Engine over Tailscale or LAN.
4. **Bring-Your-Own-Subscription:** Orion drives standard agent CLIs using credentials already authenticated on the host machine (`claude auth login`, `codex login`, etc.).
5. **Chat-First Clarity, Terminal Truth:** The primary interface renders structured conversational turns, syntax-highlighted diffs, and prompt approval cards; a live PTY terminal of the agent CLI is always one click or tap away with zero state loss.

---

## 2. Technical Invariants & Traps

### 2.1 PTY Engine & Process Control (`orion-core::pty`, `orion-core::agents`)
- **Agent CLI Target Exclusivity:** Agent panes must execute an identified AI agent CLI binary discovered on `PATH`; never spawn a generic shell for them. The exception is `POST /api/pane/shell`, which spawns a shell selected by the user from an allow-list and discovered on `PATH`.
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

### 2.3 The Runtime Wire (`orion-core::runtime`)
- **One Wire, Two Answers:** The shell talks to a `Runtime` and a `PaneHandle`. `Engine` and `Pane` answer in-process, `RuntimeClient` and `RemotePane` answer over the socket, and the views are the same either way.
- **State Management:** Runtime state must be thread-safe (`Arc<RwLock<T>>`, `Arc<Mutex<T>>` or `tokio::sync::RwLock`). The wire is length-prefixed JSON frames over one connection that carries both requests and pushed frames.
- **Typed Command Envelopes:** Every frame is a strongly-typed Serde struct. A client that speaks another protocol version is refused, and the refusal names both versions.
- **Frame Names:** The pushed frames are `{"type": "pty-data"}` (PTY output), `{"type": "pane-geometry"}` (a size change), and the status and prompt frames the engine pushes. There is no `orion://` event namespace: the names above are the wire.
- **Attach Replay:** A client that attaches is sent the screen as ANSI before the live output, so its grid matches the daemon's exactly, colours included.

### 2.4 Remote Gateway & Security (`orion-core::server`)
- **Local-First & Zero Telemetry:** Never send analytics, telemetry, or user prompts to external servers. No telemetry dependencies are permitted.
- **Tailscale Ingress Safety:** When serving over Tailscale, use `tailscale serve --https` strictly. **Never invoke `tailscale funnel`**, which exposes ports to the public internet.
- **Timing-Safe Auth:** Authenticate device tokens and pairing codes using constant-time comparison (`subtle::ConstantTimeEq`) to prevent timing attacks.
- **Secret Isolation:** User API keys (Anthropic, OpenAI, GitHub tokens) stored in settings must never be sent in client WebSocket messages or logged to stdout.
- **Draft Safety:** User input in the composer is held in local client storage. It must never be dispatched automatically upon reconnection; the user must explicitly review and press Send.

### 2.5 Shell & Touch Ergonomics (`crates/orion-ui`)
- **Design System ("Quiet Slate"):**
  - Dark mode: Near-black plum ground (`--surface-ground`), flat panels (`--surface-panel`), muted border outlines.
  - Single Chrome Accent: Muted rose is reserved for active focus, selected tabs, cursor, and primary action buttons.
  - Semantic Status Colors: Agent states have reserved tokens: `RUN` (blue/cyan pulse), `INPUT` (amber attention), `DONE` (emerald green), `ERROR` (ruby red). Never use rose for agent status.
- **Touch-First Hit Targets:** Every clickable element on mobile viewports must meet or exceed the 44px minimum touch target size (`--touch-target`).
- **Mobile Touch Key Bar:** The terminal view on touch devices must render the on-screen key bar (Esc, Tab, Ctrl, Alt, Shift, arrows, Enter) without dismissing the soft keyboard.
- **Secret Input Isolation:** Sensitive inputs (passwords, PINs, sudo prompts) must stay in DOM state and never enter undo queues, persistent storage, or unencrypted local storage.
- **Terminal Resize Debouncing:** Debounce the grid's resize events by 50ms to prevent flooding the backend PTY with ConPTY resize requests during window dragging.

---

## 3. Directory Layout Conventions

```
orion/
├── crates/orion-core/src/     # Domain logic, PTY, runtime daemon, agent parsers, Axum gateway
├── crates/orion-ui/src/       # Native GPUI shell: workspace, chat, terminal, diff viewer
├── docs/                      # Technical specs, architecture decision records (ADRs)
└── tests/                     # Integration tests and end-to-end fixtures
```

---

## 4. Verification & Testing Standards

Before submitting any code change, run the gates in [`.github/workflows/ci.yml`](.github/workflows/ci.yml). That workflow is the source of truth for what CI runs; do not keep a copy of its command list here.

1. **Host tool limits:** this workstation is not CI. When the LSP daemon stalls, a stale daemon locks the build, or cargo runs out of memory, apply the local workarounds in [`docs/agents/host-tool-constraints.md`](docs/agents/host-tool-constraints.md).
2. **Named tests:** when an acceptance criterion or evidence file names a test ID, confirm the filter matches a real test with [`scripts/check-named-tests.ps1`](scripts/check-named-tests.ps1) before trusting a pass.
3. **Smoke Proof:** Any change touching agent launching or PTY streaming must verify that the spawned process is an identified AI agent CLI and that stdin/stdout/resize function properly.

---

## Agent skills
### Issue tracker

Issues and specs live in GitHub Issues for `noemarch/orion`. See `docs/agents/issue-tracker.md`.
### Triage labels

The default five-role triage vocabulary: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, and `wontfix`. See `docs/agents/triage-labels.md`.
### Domain docs

Single-context layout using root `GLOSSARY.md` and `docs/adr/`. See `docs/agents/domain.md`.

### Coding standards
When reviewing a diff, apply `CODING_STANDARDS.md`: it states which test assertions count.
