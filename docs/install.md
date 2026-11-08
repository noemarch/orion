# Installing Orion

Orion is a local-first agent orchestrator: a native desktop app (Tauri v2) and a
headless `orion serve` binary that serves the same UI over HTTP/WebSocket.

## Prerequisites

- Rust toolchain (stable) and `cargo`.
- `bun` (package manager for the React frontend).
- The agent CLIs you want to drive, already authenticated on the host:
  `claude`, `codex`, `opencode`, `cursor`, `grok`, `omp`, `omo`, `pi`, `agy`,
  `hermes`, `devin`. Orion discovers them on `PATH`; missing ones are simply not
  offered.

## Desktop build

```bash
bun install
bun run build
cargo build --release -p orion-core
cargo tauri build   # or: cargo build -p orion-desktop
```

The installer lands under `src-tauri/target/release/bundle/` (NSIS + MSI on
Windows). Run it; Orion starts in the system tray and opens the main window.

## Headless build

```bash
cargo build --release -p orion-core --bin orion
```

See [`orion-serve-packaging.md`](orion-serve-packaging.md) for running
`orion serve`, the systemd unit, and Tailscale exposure.

## First run

On first start Orion imports preferences from `~/.config/herdr-web-ui/preferences.json`
if present (one-shot, never overwriting later changes) and writes its own state
under `%APPDATA%\Orion` (Windows) / `$XDG_DATA_HOME/orion` (Linux).

## Uninstall

Remove the application and delete `%APPDATA%\Orion` (or the XDG data dir) to
clear saved sessions, preferences and paired devices.
