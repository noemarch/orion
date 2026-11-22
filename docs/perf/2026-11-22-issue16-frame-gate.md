# Issue #16 acceptance gate: repaint performance and no WebView

Recorded 2026-11-22 from the workspace build. This file distinguishes what was actually measured from what is only designed.

## Measured: headless repaint regression trace

`crates/orion-ui/tests/repaint_storm.rs` replays two workloads through the real `TerminalPane` and GPUI's headless test renderer. `crates/orion-ui/src/perf.rs` records per-frame durations; `orion_ui::perf::summarize` computes p50/p95.

Command:

```powershell
cargo test -j 2 -p orion-ui --test repaint_storm frame_times -- --nocapture
```

Result (this host, after the #16 changes):

| Workload | Frames | Frame p50 | Frame p95 | Rows reshaped/frame avg |
|---|---|---|---|---|
| Fixture replay | 134 | 13.5 ms | 39.8 ms | 12 |
| Big log (>=2 MiB) | 322 | 8.4 ms | 22.6 ms | 15 |

Both tests pass their assertions (frames and feeds are non-empty; the row-cache trace is printed). The tests are **regression traces, not a 60fps gate**: the harness uses GPUI's software test renderer and does not draw the app shell (see `docs/perf/ui-post-change-2026-11-17.md`). An earlier attempt asserted an absolute 16.7ms target and 25ms ceiling against this harness; it could never pass (p95 was 88ms under load) and was removed rather than loosened. A test that cannot pass is not evidence.

## Measured: live 60fps resize

The live app has application instrumentation behind `ORION_TERMINAL_DEBUG=perf` (`terminal_grid.rs` prints each grid paint; `terminal.rs:1379` prints rows reshaped), a 16ms repaint coalescing budget (`app.rs:222-224`) and a 50ms PTY resize debounce (`terminal.rs:51`). The target is p95 frame time <= 16.7ms (`docs/perf/terminal-lag-baseline.md:295`).

A Windows GUI-subsystem binary does not expose stderr to the launching shell reliably, so two debug-only seams were added for the measurement: `ORION_INITIAL_LENS=terminal` forces the Terminal lens on startup, and `ORION_PERF_LOG=<path>` appends one `paint_us=<microseconds>` line per grid paint (both are `cfg(debug_assertions)` and env-gated, so they cost nothing in release).

Procedure: start `orion daemon` in an isolated `ORION_DATA_DIR` and `ORION_RUNTIME_SESSION`, create a shell pane with `orion-rt spawn-agent --name perfshell -- cmd.exe /c ...`, launch `orion-ui.exe` with that session plus the two debug env vars, resize the window 12 times between 1280x800 and 1400x880, then read the log.

Result (this host, 11 real paint samples captured in `2026-11-22-issue16-live-paint-samples.txt`):

| Metric | Value | Budget | Verdict |
|---|---|---|---|
| p50 paint | 6.69 ms | 16.7 ms | PASS |
| p95 paint | 7.08 ms | 16.7 ms | PASS |
| max paint | 7.08 ms | 16.7 ms | PASS |
| samples | 11 | >= 8 | PASS |

This is app-instrumented terminal-grid paint time during a live resize, not an independent GPU present-rate measurement. GPUI has no Windows headless renderer, so a present-rate probe is not available on this host; the measured paint cost is the dominant app-side cost and is comfortably inside one 60fps frame.

## Gate that does pass: no WebView / no web assets

`scripts/check_no_webview.ps1` inspects the built binary (Latin-1 and UTF-16) and the filesystem only — never `Cargo.lock`/`Cargo.toml`, which contain known false positives (`tauri-winrt-notification` is a toast-notification crate; `gpui-fast-web` is GPUI's wasm target).

Command and result:

```powershell
powershell -NoProfile -File scripts\check_no_webview.ps1 -Binary G:\Temp\rust\target\debug\orion-ui.exe
# no-webview gate passed: ... has no WebView/web-runtime markers and no web asset tree exists.
```

Initial markers `wry` and `CEF` false-positived on coincidental byte sequences in a 84MB Rust binary; the markers were replaced with unambiguous runtime identifiers (`WebView2Loader.dll`, `Microsoft.Web.WebView2`, `WebView2Environment`, `webkit2gtk`, `libwebkit2gtk`, `WKWebView`, `wry::`, `dioxus::`). The check also rejects `package.json`, `index.html`, `wwwroot`, `dist`, `public`, `assets/web` at the repo root.

CI wiring exists in `.github/workflows/ci.yml` (after `cargo test --workspace`) and `.github/workflows/release.yml` (after the release build). Those CI steps have not themselves been executed on GitHub; only the local script run above is verified here.
