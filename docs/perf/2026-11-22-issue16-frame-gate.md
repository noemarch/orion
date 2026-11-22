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

## Designed but NOT independently measured: live 60fps

The live app has application instrumentation behind `ORION_TERMINAL_DEBUG=perf` (`terminal_grid.rs:849-850` prints each grid paint; `terminal.rs:1379` prints rows reshaped), a 16ms repaint coalescing budget (`app.rs:222-224`) and a 50ms PTY resize debounce (`terminal.rs:51`). The documented target remains p95 frame time <= 16.7ms (`docs/perf/terminal-lag-baseline.md:295`).

A live resize attempt was made: the freshly built app was launched with `ORION_TERMINAL_DEBUG=perf` in an isolated `ORION_DATA_DIR` and driven through 12 resize cycles between 1280x800 and 1400x880. **No `ORION frame: paint` lines were emitted**, because the isolated session had no active terminal pane to paint. Therefore this run does **not** prove 60fps. Claiming otherwise would be dishonest. The closest honest statement: the app's design bounds per-frame work to well under a 60fps budget (the ticket comment measured 239-344us for `viewport_rows` and 17.5us for `snapshot_value`, about 2% of a frame), and the headless trace above shows no row-cache blow-up, but no independent present-rate measurement was obtained on this host.

To complete a true live gate on a host with an active terminal pane: launch with `ORION_TERMINAL_DEBUG=perf`, resize repeatedly, collect stderr, and assert p95 of the `paint` durations <= 16.7ms.

## Gate that does pass: no WebView / no web assets

`scripts/check_no_webview.ps1` inspects the built binary (Latin-1 and UTF-16) and the filesystem only — never `Cargo.lock`/`Cargo.toml`, which contain known false positives (`tauri-winrt-notification` is a toast-notification crate; `gpui-fast-web` is GPUI's wasm target).

Command and result:

```powershell
powershell -NoProfile -File scripts\check_no_webview.ps1 -Binary G:\Temp\rust\target\debug\orion-ui.exe
# no-webview gate passed: ... has no WebView/web-runtime markers and no web asset tree exists.
```

Initial markers `wry` and `CEF` false-positived on coincidental byte sequences in a 84MB Rust binary; the markers were replaced with unambiguous runtime identifiers (`WebView2Loader.dll`, `Microsoft.Web.WebView2`, `WebView2Environment`, `webkit2gtk`, `libwebkit2gtk`, `WKWebView`, `wry::`, `dioxus::`). The check also rejects `package.json`, `index.html`, `wwwroot`, `dist`, `public`, `assets/web` at the repo root.

CI wiring exists in `.github/workflows/ci.yml` (after `cargo test --workspace`) and `.github/workflows/release.yml` (after the release build). Those CI steps have not themselves been executed on GitHub; only the local script run above is verified here.
