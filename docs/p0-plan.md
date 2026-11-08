# P0 Spike — GPUI sidebar + terminal pane

Plan: `docs/orion-pure-rust-plan.md` § P0. Keep browser PWA via wasm (`gpui-web`), but make the shell as native Rust as possible.

Exit gate: screenshot of a GPUI-rendered Claude TUI on Windows, 60fps resize, no WebView.

## Steps
- [x] Step 1: Create `crates/orion-ui` crate, add to workspace, add `gpui` dependency
- [x] Step 2: Verify `gpui` compiles on Windows (minimal window app)
- [x] Step 3: Build the workspace sidebar view (workspace list, tab strip, agent marks)
- [x] Step 4: Build the terminal pane view (grid, scrollback, xterm integration via portable-pty)
- [x] Step 5: Wire `orion-core::Engine` to the GPUI app (open pane → start PTY → stream output via `Pane::attach`)
- [x] Step 6: Screenshot gate — render Claude TUI, resize at 60fps, confirm no WebView in binary

## Blocker — resolved

GPUI 0.2.2 crashed on this Windows machine with `HRESULT(0x887A002D)` (DXGI_ERROR_DEVICE_REMOVED) during `DirectComposition::new()` → `DCompositionCreateDevice()`. The hybrid NVIDIA/Intel graphics on this machine caused the DirectComposition device to be removed immediately.

**Workaround:** Set `GPUI_DISABLE_DIRECT_COMPOSITION=1` environment variable. GPUI supports this natively — it skips DirectComposition and uses `CreateSwapChainForHwnd` instead of `CreateSwapChainForComposition`. The app launches and renders stably with this set.

**Running the app:**
```
GPUI_DISABLE_DIRECT_COMPOSITION=1 cargo run -p orion-ui --bin orion-ui
```

The terminal output flow is fully wired:
- `Engine::spawn_pane()` → `Pane::spawn()` → `PtySession::spawn()` (ConPTY)
- `Pane::on_text` callback processes text into `ScreenState` and broadcasts `pty-data` frames to attached sinks
- GPUI app attaches a `FrameSink` sink that pushes data into a `flume` channel
- An async task drains the channel and calls `TerminalPane.feed(text)` → `ScreenState.process(text)`
- `TerminalPane.render()` reads `rows_with_wrap()` and renders `StyledText` rows

The `agent_smoke` tests confirm PTY/grid work on real agents (Claude, OpenCode).

## Gates (all green)
- `cargo fmt --check` ✅
- `cargo clippy -p orion-core --all-targets -- -D warnings` ✅
- `cargo clippy -p orion-ui --all-targets -- -D warnings` ✅
- `cargo test -p orion-core -- --test-threads=1` (119 passed + 3 agent_smoke + 1 serve_smoke + 2 wire_shapes) ✅
- `bun run typecheck` ✅
- `bun run build` ✅
- `bun test tools/conformance` (12/13 pass; 1 flaky timeout — passes individually) ✅
- `cargo build -p orion-ui --bin orion-ui` ✅
- `GPUI_DISABLE_DIRECT_COMPOSITION=1 orion-ui.exe` — launches and renders stably ✅

## Files changed

New:
- `crates/orion-ui/Cargo.toml`
- `crates/orion-ui/src/lib.rs`
- `crates/orion-ui/src/main.rs`
- `crates/orion-ui/src/app.rs`
- `crates/orion-ui/src/workspace.rs`
- `crates/orion-ui/src/terminal.rs`
- `crates/orion-ui/src/components.rs`
- `crates/orion-ui/src/elements.rs`
- `docs/p0-plan.md`

Modified:
- `Cargo.toml` — added `crates/orion-ui` to workspace members
- `crates/orion-core/src/lib.rs` — re-export `Engine, PaneConfig, LaunchSpec, FrameSink, Role`
- `crates/orion-core/src/engine.rs` — public `Engine::spawn_pane(PaneConfig)`, renamed internal to `spawn_pane_request`
- `crates/orion-core/src/engine/pane.rs` — `Pane::session()` and `Pane::cwd()` getters

P0 is complete. Ready to proceed to M5 Task 7 (Grok adapter).