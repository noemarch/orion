# Installing Orion

Orion is a local-first agent orchestrator: a native GPUI desktop app and a
headless `orion serve` binary that exposes the same runtime over HTTP/WebSocket.

## Prerequisites

- Rust toolchain (stable) and `cargo`.
- The agent CLIs you want to drive, already authenticated on the host:
  `claude`, `codex`, `opencode`, `cursor`, `grok`, `omp`, `omo`, `pi`, `agy`,
  `hermes`, `devin`. Orion discovers them on `PATH`; missing ones are simply not
  offered.

## Desktop build

```bash
cargo build --release -p orion-ui
```

The app is the `orion-ui` binary (`target/release/orion-ui.exe` on Windows). Run
it directly, or copy it to wherever you keep your applications.

## Headless build

```bash
cargo build --release -p orion-core --bin orion
```

See [`orion-serve-packaging.md`](orion-serve-packaging.md) for running
`orion serve`, the systemd unit, and Tailscale exposure.

## First run

On first start Orion imports preferences from any existing predecessor client
if present (one-shot, never overwriting later changes) and writes its own state
under `%APPDATA%\Orion` (Windows) / `$XDG_DATA_HOME/orion` (Linux).

## Uninstall

Remove the application and delete `%APPDATA%\Orion` (or the XDG data dir) to
clear saved sessions, preferences and paired devices.
