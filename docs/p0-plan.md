# P0 Spike — GPUI sidebar + terminal pane

Plan: `docs/orion-pure-rust-plan.md` § P0. Keep browser PWA via wasm (`gpui-web`), but make the shell as native Rust as possible.

Exit gate: screenshot of a GPUI-rendered Claude TUI on Windows, 60fps resize, no WebView.

## Steps
- [x] Step 1: Create `crates/orion-ui` crate, add to workspace, add `gpui` dependency
- [x] Step 2: Verify `gpui` compiles on Windows (minimal window app)
- [x] Step 3: Build the workspace sidebar view (workspace list, tab strip, agent marks)
- [x] Step 4: Build the terminal pane view (grid, scrollback, xterm integration via portable-pty)
- [x] Step 5: Wire `orion-core::Engine` to the GPUI app (open pane → start PTY → stream output via attach)
- [ ] Step 6: Screenshot gate — render Claude TUI, resize at 60fps, confirm no WebView in binary
- [ ] Runtime: GPUI platform DLL issue on Windows (entry point not found) — needs `gpui` platform DLL in PATH or static linking

## Notes
- `orion-ui` depends on `orion-core` (PTY/engine seam) — no cycle since `orion-core` knows nothing of GPUI.
- PWA parity: `gpui-web` target compiles the same UI to wasm served by `orion serve`.
- Native-first: desktop shell is pure Rust + GPUI; Tauri/WebView2 remains only if GPUI proves unworkable on Windows.
