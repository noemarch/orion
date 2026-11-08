# Architecture

Orion is a local-first orchestrator with a native Rust core and a React 19
frontend, delivered both as a Tauri v2 desktop app and a headless `orion serve`.

## Layers

- `crates/orion-core` — domain logic: PTY allocation, agent supervision, screen
  grid, introspection parsers, Axum gateway, persistence.
- `src-tauri` — Tauri v2 shell: windows, tray, IPC handlers, capabilities.
- `src` — React 19 frontend (chat lens, terminal lens, workspaces).
- `shared/protocol.ts` — frozen wire contract; `engine-types.ts` mirrors it.

## Invariants

- Every PTY runs an identified agent CLI; no generic shells.
- No external multiplexer; Orion owns PTYs and process trees.
- PTY reads never block a Tokio worker (dedicated thread / `spawn_blocking`).
- Process termination kills the whole tree (Job Object on Windows, process group
  on Unix) with SIGINT → SIGTERM → SIGKILL escalation.
- Agent state files are opened read-only (`SQLITE_OPEN_READONLY`).
- No telemetry; no `tailscale funnel`.

## Design system

"Quiet Slate": near-black plum ground, flat panels, muted borders, single rose
accent for focus/selection, semantic status colors (RUN blue/cyan, INPUT amber,
DONE emerald, ERROR ruby). 44px minimum touch targets on mobile.
