# Orion: Pure-Rust Migration Plan (Zed-Modeled)

> **Status:** Plan only — not started. Created 2026-11-07 in response to a stakeholder
> demand that Orion be 100% Rust. No code has been changed for this plan.

## Why this exists

Orion is already pure Rust where it matters: the engine (`crates/orion-core`), the Tauri v2
desktop shell (`src-tauri`), and the `orion serve` gateway. The only non-Rust layer is the
UI — `src/` (React 19 + TypeScript) plus `shared/*.ts`, compiled by Vite/Bun into static
assets and rendered in WebView2 (desktop) or served to a browser (PWA).

This plan removes that layer and replaces it with a native Rust UI modeled on Zed.

## Reference architecture (Zed)

| Zed piece | What Orion borrows | Where it lands |
|---|---|---|
| GPUI (retained-mode, GPU UI) | Replaces React + HeroUI + Tailwind | New `crates/orion-ui` |
| `terminal` crate (alacritty grid + portable-pty) | Terminal view + grid | `crates/orion-ui::terminal` (or keep `orion-core::screen` behind a GPUI view) |
| `zed-remote` / `zed-remote-server` | One binary, desktop + headless | Already have `engine::Engine` seam + Axum gateway |
| `text`/`buffer` + tree-sitter | Markdown + syntax highlight | Replaces highlight.js / KaTeX-on-web |
| `zed-diff` / `zed-fs` / `zed-git` | Diffs, file tree, git status | Replaces `@pierre/diffs`, `repo-diff.ts` |
| `gpui_web` (wasm) | Optional browser PWA parity | Only if PWA is retained |

## Fork point: PWA parity

Zed's remote story is a **native client**, not a browser PWA. If Orion keeps the PWA, the
UI must also compile to `wasm32-unknown-unknown` and be served by `orion serve`. If the
stakeholder accepts "native remote client only", the PWA is dropped and `orion serve` keeps
just the engine. This decision gates P5.

## Phases

### P0 — Spike (1 week, M2-style gate)
- Add `gpui` to a new `crates/orion-ui` crate.
- Port one screen end-to-end: workspace sidebar + one terminal pane.
- Exit gate: screenshot of a GPUI-rendered Claude TUI on Windows, 60fps resize, no WebView.

### P1 — Terminal truth (2 weeks)
- Replace `src/components/PaneTerminal.tsx` + xterm.js with a GPUI terminal view.
- Port key bar, scrollback, find, selection.
- Keep `orion-core::pty` / `screen` backend.

### P2 — Chat lens + prompt cards (2–3 weeks)
- Port `ChatView.tsx`, `PromptCard.tsx`, work/thinking rows to GPUI.
- Markdown via a Rust markdown crate; diffs via `similar` (or `zed-diff`).
- Wire `/api/pane/conversation` → GPUI entities.

### P3 — Files, diffs, changes (2 weeks)
- Port `FileTree`, `RepoDiff`, `FileReader` to GPUI views.
- `git2` for status/diff, `notify` for watchers.

### P4 — Layout, tabs, settings, theming (2 weeks)
- Port `App.tsx` layout, tab strip, sidebar, settings modal.
- JSON themes replace CSS variables (Zed model).

### P5 — Remove the web layer (1 week)
- Delete `src/`, `shared/*.ts`; move wire types into Rust (already in `protocol.rs`).
- Drop `bun`, `vite`, `tsc`, `package.json` from release pipeline.
- If PWA retained: build GPUI for wasm and serve it from `orion serve`.

### P6 — Verification (1 week)
- Replace component tests with GPUI snapshot tests.
- `cargo test --workspace`, `cargo clippy -D warnings` green.
- Screenshots, orphan sweep, reopen-resume.

## What is lost / kept

- **Lost:** PWA on phone (unless wasm), HeroUI components, React ecosystem, Orbit's exact
  pixels, the existing component test suite.
- **Kept:** engine, PTY, agents, transcripts, prompt detection, store, all Rust tests.

## Risks

- GPUI on Windows is still maturing (Zed is macOS/Linux-first). P0 must gate this.
- ~10–12 weeks of UI rewrite; highest-risk item in the whole program.
- M3–M6 (worktrees, remote, more agents) deliver user value on the current architecture
  and would be paused.

## Recommendation

Do **not** start now. M1 gate is proven and M2 is complete; M3–M6 deliver user-visible
value on the current split (pure-Rust engine/shell + compiled web UI). If the stakeholder
insists, run P0 as a 1-week spike to size the GPUI Windows story before committing to
P1–P5. The engine/shell are already 100% Rust, which is the part that ships and runs.
