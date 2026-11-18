# UI performance baseline — current gpui-kit shell (2026-11-17)

Fresh baseline taken on the current UI (post 2026-11-16 gpui-kit migration + 2026-11-17 Quiet Canvas theme), BEFORE any fix in plan `orion-ui-gpui-kit-perf-cleanup`.

## Machine / toolchain
- OS: Windows 11 Pro 10.0.26200 (x64); CPU i7-7500U @ 2.70 GHz (4 cores)
- rustc 1.100.0 (0ef1bc386 2026-11-09); cargo 1.100.0
- Harness: `cargo test -j 2 -p orion-ui --test repaint_storm frame_times -- --nocapture`
- `frame_times` module and `perf.rs` are `#[cfg(debug_assertions)]`; this run is a debug build, so instrumentation is active. A release build emits no frame stats.

## What this harness measures (and what it does NOT)
- It renders `TerminalPane` alone in a `TestWindow` (`repaint_storm.rs:285-317`) and records feed time (`ScreenState::process`) and frame time (`TerminalPane::render`) via `orion_ui::perf`.
- It does **not** render `OrionApp`, so it does not exercise the app-shell render path (`app.rs:1317-1420`): per-frame `Theme::set_current` + `sync_gpui_kit` (`:1382-1383`), whole-session `snapshot()` clones (`:1328,:1360`), pane-id `Vec` rebuild (`:1335-1349`), workspace-list rebuild (`:1398-1415`), chat turn `to_vec()` (`chat.rs:994`), or the pane/git polls.
- Treat numbers as a stable A/B signal for the terminal render hot path, not as the full-app frame cost.

## Results — 5 runs, median reported

Fixture replay (`repaint_storm.ansi`, 4 KiB chunks, one draw per chunk):
| Metric | R1 | R2 | R3 | R4 | R5 | Median | Range |
|---|---|---|---|---|---|---|---|
| Wall | 2.930s | 2.490s | 1.965s | 2.008s | 1.660s | **2.008s** | 1.660–2.930 |
| Feed p50 | 5.686ms | 5.074ms | 3.138ms | 3.607ms | 2.787ms | **3.607ms** | 2.787–5.686 |
| Feed p95 | 23.255ms | 22.832ms | 16.011ms | 17.575ms | 13.785ms | **17.575ms** | 13.785–23.255 |
| Frame p50 | 4.265ms | 2.923ms | 3.902ms | 2.253ms | 0.569ms | **2.923ms** | 0.569–4.265 |
| Frame p95 | 19.825ms | 16.032ms | 11.406ms | 11.529ms | 9.115ms | **11.529ms** | 9.115–19.825 |

Big log replay (2.62 MiB styled log, 16 KiB chunks, one draw per chunk):
| Metric | R1 | R2 | R3 | R4 | R5 | Median | Range |
|---|---|---|---|---|---|---|---|
| Wall | 7.384s | 6.690s | 6.163s | 6.351s | 5.119s | **6.351s** | 5.119–7.384 |
| Feed p50 | 10.516ms | 9.082ms | 9.431ms | 9.721ms | 8.002ms | **9.431ms** | 8.002–10.516 |
| Feed p95 | 22.645ms | 20.770ms | 16.477ms | 18.902ms | 13.785ms | **18.902ms** | 13.785–22.645 |
| Frame p50 | 4.265ms | 2.923ms | 4.109ms | 2.322ms | 0.702ms | **2.923ms** | 0.702–4.265 |
| Frame p95 | 15.882ms | 14.941ms | 11.591ms | 12.036ms | 9.748ms | **12.036ms** | 9.748–15.882 |

All 10 test runs passed (`stats.frames`/`stats.feeds` non-empty). Errors: none.

## Variance / limiter (benchmark-checklist)
- Frame p95 range 9.1–19.8 ms is much wider than the stale doc's ±1–3 ms; runs trend faster with warmup (R1 slowest), so host contention + warmup dominate. A delta under ~5 ms on p95 is NOT measurable in this harness. Any later "improvement" claim must exceed this band or be reported as inconclusive.
- Limiter for feed path: vt100 parse (already documented). Limiter for the harness overall: test-renderer layout/paint + host CPU contention on 4 shared cores.

## Comparison to stale baseline (2026-11-14) — is the terminal path the regression?
| Metric | 2026-11-14 median | 2026-11-17 median | Verdict |
|---|---|---|---|
| Fixture frame p95 | 10.97ms | 11.53ms | within noise — no measurable change |
| Big-log frame p95 | 11.64ms | 12.04ms | within noise — no measurable change |
| Fixture feed p95 | 14.66ms | 17.58ms | up but inside this run's own range — inconclusive |
| Big-log feed p95 | 16.73ms | 18.90ms | up but inside this run's own range — inconclusive |
| Fixture wall | 3.990s | 2.008s | faster (rustc 1.99→1.100 + row cache) |

**The terminal render/feed path is REFUTED as the source of the reported degradation.** Its frame p95 is unchanged within noise. The row-cache and one-repaint-per-frame work from 2026-11-14/15 is intact and effective.

## Cause classification (for waves 3–4)

CONFIRMED by code inspection, on repeated paths, fixable with low risk (impact on full-app frame time NOT measurable in this harness — will re-measure at F4 and report honestly):
1. **App-shell per-frame work** — `app.rs:1382` `Theme::set_current(Theme::for_mode(prefs.theme, prefs.density))` constructs a full `Theme` and locks the global `CURRENT` mutex every frame; `:1383` `sync_gpui_kit` locks `KIT_SYNCED` every frame (cheap early-return once synced, but still a lock). Introduced in `3ae1178` (set_current, 2026-11-09) and `a7f0ae5` (sync, 2026-11-16). `Theme::current()` at `theme.rs:417-422` locks `std::sync::Mutex` on all ~81 call sites (26 diff_view, 19 file_reader, 12 app, 10 chat).
2. **Full session JSON clones** — `app.rs:1328` and `:1360` `view.snapshot()` clone the whole session to read focused pane + live pane ids; pane-id `Vec<String>` rebuilt `:1335-1349`; workspace list rebuilt `:1398-1415` — all every frame.
3. **Whole-transcript clone** — `chat.rs:994` `self.model.turns().to_vec()` clones every turn before capping to 200; zero-copy `recent()` exists unused at `chat_entities.rs:31-36`.
4. **Per-row paint allocation** — `terminal_grid.rs:467-472` builds a fresh `Vec<TextRun>` and clones `row.text` per visible row per paint.
5. **Git subprocess burst on poll** — `diff_view.rs:592-604` 5s poll; `orion-core/engine/diff.rs` spawns ~7 `git` commands per `files()` refresh (`:60,:67,:71,:79,:98,:152,:221-225`).
6. **Offscreen transcript disk read on UI thread** — `chat.rs:350-367` 5s poll, comment at `:355-357` acknowledges this.
7. **Exit-watcher polling threads** — `pane.rs:165-195` (250ms), `runtime/terminal.rs:111-127` (100ms), `registry.rs:564` (100ms), `app.rs:333-379` (500ms status socket round-trip).

NOT MEASURABLE in this headless harness (no live GPU present): real GPU present time, the ConPTY pump, native window layout. A live-app reproduction needs `GPUI_DISABLE_DIRECT_COMPOSITION=1` per `docs/perf/terminal-lag-baseline.md:13-18`.

## Gate decision
Wave 1 PASSES. Terminal render path is not the regression; proceed to wave 2. All 7 app-shell causes above are CONFIRMED to exist on repeated paths and are authorized for the low-risk fixes in plan tasks 8–14. F4 will re-run this exact harness; because the harness does not cover the app shell, any full-app claim will be labeled structural, not measured.
