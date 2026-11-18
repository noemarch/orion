# UI performance — post-change re-measure (2026-11-17)

Comparison against `docs/perf/ui-baseline-2026-11-17.md` (5 runs, same machine/rustc 1.100.0).

## Command
`cargo test -j 2 -p orion-ui --test repaint_storm frame_times -- --nocapture` (3 post-change runs).

## Results — post-change medians vs baseline medians

| Metric | Baseline median | Post median | Post range | Verdict |
|---|---|---|---|---|
| Fixture frame p95 | 11.529 ms | 11.906 ms | 11.484–12.597 | within noise |
| Big-log frame p95 | 12.036 ms | 12.608 ms | 12.540–12.732 | within noise |
| Fixture feed p50 | 3.607 ms | 3.263 ms | 3.214–3.274 | within noise |
| Fixture feed p95 | 17.575 ms | 16.189 ms | 15.361–16.447 | within noise |
| Big-log feed p50 | 9.431 ms | 11.151 ms | 10.454–11.252 | within noise |
| Big-log feed p95 | 18.902 ms | 16.904 ms | 16.012–17.973 | within noise |
| Fixture wall | 2.008 s | 2.105 s | 2.086–2.127 | within noise |
| Big-log wall | 6.351 s | 6.746 s | 6.471–6.979 | within noise |

Baseline frame p95 range was 9.115–19.825 ms (fixture) / 9.748–15.882 ms (big log). Every post-change value sits inside its baseline range. Per benchmark-checklist question 5, a gap smaller than run-to-run variation is no measurable difference.

## Verdict
**No measurable difference** in the harness. Not faster, not slower. The limiter remains the debug test-renderer layout/paint plus host CPU contention on 4 cores; harness variance dominates.

## Honesty caveat (load-bearing)
This harness renders `TerminalPane` in isolation. It does **not** exercise the app-shell render path (`app.rs`), the chat turn list, git polling, or the theme accessor. The Wave 3 fixes removed confirmed per-frame work in those paths:
- `Theme::current()` global mutex → relaxed atomic load (`theme.rs:417-455,526`).
- `Theme::set_current` + `sync_gpui_kit` gated on preference change in `OrionApp::render` (`app.rs:1479-1484`).
- Whole-session `snapshot()` JSON clones + pane/workspace list rebuilds moved out of `render` into event-driven `refresh_shell_cache` (`app.rs:1361-1453`).
- Chat whole-transcript `to_vec()` → borrowed `recent_turns` slice of at most 200 turns (`chat.rs:1001-1016`, `chat_entities.rs:34-42`).
- Terminal-grid per-row `Vec<TextRun>` + `row.text.clone()` → reused scratch buffer + paint-text handle (`terminal_grid.rs:171,390-398,457-496`).
- Git metadata 4 subprocesses → 1 combined `rev-parse` (`core/engine/diff.rs:65-115`), with a deterministic parse test.
- Exit-watcher polling → bounded 25→400 ms exponential backoff (`core/engine/pane.rs:165`, `core/runtime/terminal.rs:111`).
- Git/transcript polls gated on view visibility (`diff_view.rs`, `chat.rs`).

Because the harness cannot see the app shell, these are **structural improvements with code-inspection evidence**, not measured frame-time gains. Claiming a speedup from these numbers would fail benchmark-checklist question 6/7. A real-app frame benchmark (GPUI present + pump + layout) would be needed to measure them, and requires `GPUI_DISABLE_DIRECT_COMPOSITION=1` on this hybrid-GPU machine (`docs/perf/terminal-lag-baseline.md:13-18`).

## Regression check
No post-change metric exceeds its baseline range. No fix was reverted.
