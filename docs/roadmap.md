# Orion roadmap

The architecture is described in [`../PLAN.md`](../PLAN.md). This document is the executable
plan: the milestones, what each one proves, and the gate that must pass before the next one
starts. A milestone ships when its definition of done (DoD) is demonstrated on Windows, not when
its code compiles.

## Ground rules

- **Agents only.** Every pane runs an identified AI agent CLI discovered on `PATH`. No generic
  shell, no ad-hoc terminal tab.
- **No external multiplexer.** Orion allocates PTYs, supervises processes and streams sessions
  itself; nothing is installed beside it.
- **One engine, two surfaces.** The desktop shell and the browser gateway call the same
  `Engine::dispatch`; no behaviour exists in only one of them.
- **The client contract is frozen.** `shared/protocol.ts` is the wire contract. A change is a
  protocol revision, not a convenience edit.
- **One UI language for now.** The early build ships English only: `t()` keeps the English string
  as its key, so adding a dictionary later is additive, not a rewrite.
- **Verification is a command.** Every milestone ends with the checks in
  [`AGENTS.md`](../AGENTS.md) §4 passing in CI.

## M0 — Groundwork (done)

**Goal:** a native shell that installs, opens, and moves one frame over IPC, with the porting
rails in place.

- Copied the shipped React client, its shared contract and its assets into the repo; the mock
  frontend is gone.
- Adopted the client's dependency set (React 19.3, Vite 8, HeroUI, patched xterm) with `bun` as
  the package manager.
- Froze the engine wire types into `shared/engine-types.ts`, hand-owned from then on; renamed the
  client socket and RPC helpers; no generated type file and no generator script remains.
- Added `src/lib/transport.ts` with the browser WebSocket pipe and the desktop IPC pipe; the
  session socket is host-agnostic.
- Added `orion_request`, `orion_connect`, `orion_send`, `orion_close` and the engine seam.
- Added the plugins and explicit capabilities (notification, single-instance, autostart,
  window-state, opener, global-shortcut); no wildcard permission.
- Put the process tree in a kill-on-close job object on Windows; a second instance focuses the
  first; closing the window hides to the tray.
- CI: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, `tsc --noEmit`, `bun run
  build`, plus the no-foreign-artifact scan.

**DoD:** the installer runs on Windows; the window opens with the shipped UI; a snapshot frame
arrives over IPC; a 1 MiB byte round trip arrives intact; closing the window hides to tray; the
process exits cleanly.

## M1 — Walking skeleton: Claude end-to-end (L) — **gate**

**Goal:** use Orion daily with a single agent.

- `engine::pty`: spawn, incremental UTF-8 decoder, writer, resize, pause/resume, signal
  escalation, exit watcher, job-object membership.
- `engine::screen`: VT grid, revision counter, capped scrollback ring, rows and attributes,
  selection, find, scroll info.
- `engine::pane`: geometry ownership, attach/observe, keep/release size, replay buffer,
  per-subscription windows.
- `engine::events`: broadcast hub, presence, role frames, stall close, join budgets.
- `engine::dispatch` routes: `snapshot`, `agents`, `pane/read|scroll|keys|input|resize|prompt/answer`,
  `conversation`.
- `engine::introspect::claude`: locate the session log by PID ancestry and session id, tail by
  byte offset, emit invalidations; fall back to scrollback.
- `engine::prompt` (Claude subset): detect permission dialogs, option lists and the input box;
  answer with validated keystrokes.
- Persistence under `%APPDATA%\Orion`, atomic writes.
- Client: terminal lens, chat lens and approval cards live against the engine.
- Tests: spawn a real agent, assert the grid, resize storm, interrupt, orphan sweep; screen and
  prompt fixtures.

**DoD:** pick a repo → the agent starts in a ConPTY → terminal, chat and approvals work → resizing
while dragging behaves → quit Orion → no agent process survives → reopening resumes the
conversation.

**Gate:** if grid-based detection cannot read the agent's TUI reliably on Windows, stop and
re-decide the read path before building breadth.

## M2 — Second agent and the conformance harness (M)

**Goal:** the contract suites pass by construction, not by inspection.

- Port the remaining agent adapters the client knows about, starting with Codex.
- Run the frozen client contract suites against the Rust engine through the HTTP/WS gateway.
- Wire-shape regression: every request and response is serde round-tripped with strict field
  names; a hash check fails if the frozen contract drifts.

## M3 — Workspaces, splits and worktrees (M)

**Goal:** the workspace model the client already draws, owned by the engine.

- Workspaces, tabs, layouts, splits, zoom and focus.
- Git worktree create/remove, diff against the parent branch, safe cleanup.
- Layout persistence across restarts.

## M4 — Remote gateway and mobile PWA (L)

**Goal:** the same engine on a phone, over the local network or a private tailnet.

- Embedded Axum HTTP/WS gateway with the same request and frame shapes as the desktop bridge.
- Pairing: short code, device tokens, constant-time comparison, expiry.
- `tailscale serve --https` integration; `tailscale funnel` is forbidden.
- Touch key bar, resume-after-sleep, draft safety on reconnect.

## M5 — Alerts, notifications and polish (M)

**Goal:** the features that make the app livable rather than impressive.

- Native notifications for `needsinput` and `done`; web push for the PWA.
- Alert droplets, quiet hours, per-agent rules.
- Signed updater: produce signed artifacts, then enable the updater plugin.
- Design pass over the desktop layouts; accessibility and reduced-motion.

## M6 — Release (S)

**Goal:** an installable build for each platform, from CI, with no manual steps.

- Windows MSI/NSIS, macOS DMG, Linux AppImage/deb.
- `orion serve` headless binary.
- Migration notes, first-run onboarding, release automation.
