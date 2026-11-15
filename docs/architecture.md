# Architecture

Orion is a local-first orchestrator with a native Rust core and a native GPUI
desktop shell, delivered both as a desktop app (`orion-ui`) and a headless
`orion serve`.

## Layers

- `crates/orion-core` — domain logic: PTY allocation, process supervision,
  the screen grid (behind a `TerminalEngine` trait, implemented by `vt100`),
  introspection parsers, the runtime daemon that owns every terminal, the
  Axum gateway, persistence.
- `crates/orion-ui` — native GPUI shell: workspaces, the chat lens, the
  terminal lens, the changes lens. No web view, no JavaScript.
- `docs/adr` — architecture decision records, including
  `desktop-shell-ownership.md`, which is why the app is GPUI and pure Rust.

## Invariants

- Every PTY runs an identified agent CLI; no generic shells.
- No external multiplexer; Orion owns PTYs and process trees.
- PTY reads never block a Tokio worker (dedicated thread / `spawn_blocking`).
- Process termination kills the whole tree (Job Object on Windows, process group
  on Unix) with SIGINT → SIGTERM → SIGKILL escalation.
- Agent state files are opened read-only (`SQLITE_OPEN_READONLY`).
- No telemetry; no `tailscale funnel`.
- Terminals live in the runtime daemon; the shell is only a viewer. A client
  that attaches is replayed the screen as ANSI before the live stream, so a
  view can come and go without losing what the agent drew.

## Design system

"Quiet Slate": near-black plum ground, flat panels, muted borders, single rose
accent for focus/selection, semantic status colors (RUN blue/cyan, INPUT amber,
DONE emerald, ERROR ruby). 44px minimum touch targets on mobile.
