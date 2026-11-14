# Terminal lag — baseline (before) numbers

Measured on the dev machine before any of the herdr-core-terminal-lag fixes land.
This is the reference every later verification step compares against.

## Machine

- OS: Microsoft Windows 11 Pro, 10.0.26200 (x64)
- CPU: Intel(R) Core(TM) i7-7500U CPU @ 2.70GHz (4 cores)
- RAM: 16 GB
- Rust: rustc 1.99.1 (58cf8f9c3 2026-10-13), cargo 1.99.1
- PTY: ConPTY (via `portable-pty`)
- `GPUI_DISABLE_DIRECT_COMPOSITION=1` is required to run the real app on this
  machine (hybrid NVIDIA/Intel graphics; `DCompositionCreateDevice` fails with
  `DXGI_ERROR_DEVICE_REMOVED` — see `docs/p0-plan.md`). The measurements below
  are headless (gpui test harness) and do not depend on it, but any live-app
  reproduction must set it:
  `GPUI_DISABLE_DIRECT_COMPOSITION=1 cargo run -p orion-ui --bin orion-ui`

## What was measured, and what it is not

- **Feed time** — wall time of `ScreenState::process` (the vt100 parse),
  recorded per `feed()` call by the debug-only instrumentation in
  `crates/orion-ui/src/perf.rs` (`#[cfg(debug_assertions)]`; compiled out of
  release builds).
- **Frame time** — wall time of `TerminalPane::render` (element construction),
  recorded the same way. `window.draw` in the gpui test harness additionally
  runs layout and paint against the test renderer; neither number is the GPU
  present time of the real app. Treat these as a lower bound on real frame
  cost and as a stable A/B signal for the render hot path, not as an absolute
  60fps guarantee.
- **Wall time** — total time of the replay loop (feed + draw per chunk),
  measured with `Instant` inside the test.
- The fixture lives in the alternate screen, so it builds no scrollback; the
  big log does (10,000 rows, the ring cap).

## The fixture

`crates/orion-ui/tests/fixtures/repaint_storm.ansi` — 273,106 bytes, 2,348
lines, 2,406 styled lines. A recorded-style agent TUI repaint storm:

- enter/leave alternate screen (`ESC[?1049h` … `ESC[?1049l`), cursor hidden/restored
- truecolor SGR runs (2,791 `38;2;…` sequences), indexed-color levels, bold/dim
- box-drawing frames (9,324 box/block glyphs): a header box and a status box
- a 60-frame braille spinner repaint loop on one line
- a status box redrawn 39 times in place via cursor-up (`ESC[6A`) — the repaint storm
- 1,200 styled log lines + 900 styled diff-like lines (≥ 2,000 styled lines)

Byte-stable: `the_fixture_is_byte_stable` replays it twice and asserts identical
`viewport_cells(0)`, `visible_text()` and revision. Regenerate only by editing
the generator and re-checking that test; the file itself is the artifact.

## Commands to reproduce

```powershell
# full suite (unit + integration + smoke)
cargo test -p orion-ui

# the measurement harness, with printed stats (run 3x for the table below)
cargo test -p orion-ui --test repaint_storm frame_times -- --nocapture

# grid-level replay, byte-stability, corrupt-input absorption, live cat
cargo test -p orion-ui --test repaint_storm -- --nocapture
```

## Results (3 runs, median reported)

### Fixture replay (`repaint_storm.ansi`, 4 KiB chunks, one draw per chunk)

| Metric | Run 1 | Run 2 | Run 3 | Median |
|---|---|---|---|---|
| Wall time | 3.959 s | 3.990 s | 4.656 s | **3.990 s** |
| Feeds (n) | 121 | 121 | 122 | 121 |
| Feed p50 | 3.089 ms | 3.186 ms | 3.582 ms | **3.186 ms** |
| Feed p95 | 14.66 ms | 14.47 ms | 17.84 ms | **14.66 ms** |
| Frames (n) | 242 | 243 | 244 | 243 |
| Frame p50 | 4.225 ms | 4.182 ms | 5.464 ms | **4.225 ms** |
| Frame p95 | 10.97 ms | 10.52 ms | 13.19 ms | **10.97 ms** |

### Big log replay (≥ 2 MiB styled log, 16 KiB chunks, one draw per chunk)

The log is generated deterministically in the test (`big_log()` in
`tests/repaint_storm.rs`): 2,621,489 bytes, ~26,000 styled lines.

| Metric | Run 1 | Run 2 | Run 3 | Median |
|---|---|---|---|---|
| Wall time | 10.897 s | 10.852 s | 10.921 s | **10.897 s** |
| Feeds (n) | 227 | 227 | 227 | 227 |
| Feed p50 | 8.613 ms | 8.316 ms | 8.273 ms | **8.316 ms** |
| Feed p95 | 16.73 ms | 16.10 ms | 16.87 ms | **16.73 ms** |
| Frames (n) | 454 | 455 | 455 | 455 |
| Frame p50 | 4.601 ms | 4.381 ms | 4.663 ms | **4.601 ms** |
| Frame p95 | 11.64 ms | 11.33 ms | 12.61 ms | **11.64 ms** |

Grid-level replay of the same log (no draws): 2.31–2.46 s wall, 161 feeds of
16 KiB, scrollback capped at 10,000 rows.

### Live `cat` of the ≥ 2 MiB log through a real ConPTY

`live_cat_through_a_pty` spawns `cmd /c type big.log` on a ConPTY, drains it
and feeds the grid as bytes arrive (incremental UTF-8 decode via
`orion_core::pty::Utf8Decoder`):

- 2,575,938 bytes received of 2,621,489 (ConPTY held back the final ~45 KB)
- 24,629 feeds (ConPTY delivers ~100-byte chunks)
- wall time 24.1–24.9 s across runs
- the child exited with code 0; **ConPTY never signaled EOF** on the output
  pipe — the test stops on a 10 s read timeout once the child has exited
- throughput ≈ 320 KB/s for this SGR-heavy content: the live-cat path is
  ConPTY-bound, not grid-bound (the grid parses the same bytes in ~2.5 s)

## Reading the numbers

- Frame p95 ≈ **11 ms** (fixture) and ≈ **11.6 ms** (big log) in the test
  harness — under the 16.7 ms / 60 fps bar, but with no headroom once real
  GPU present, the pump, and the app's own layout are added. The render hot
  path (per-frame clone, `cell_runs` allocation, find-bar work) is where the
  remaining time goes; that is what todos 4–8 attack.
- Feed p95 ≈ **14.7 ms** (fixture) / **16.7 ms** (big log): the vt100 parse
  dominates the ingest path. Todo 2 (pump unification) and the bounded-channel
  work (todos 9–12) address how these feeds are scheduled, not the parse
  itself.
- Variance across runs is ±1–3 ms on p95; treat < 1 ms deltas as noise.

## Caveats

- Numbers are from the gpui **test** harness (test renderer), not the real
  app with `GPUI_DISABLE_DIRECT_COMPOSITION=1`. Absolute values will differ;
  the A/B signal for the render hot path is what matters.
- The live-cat figure is ConPTY-bound and machine-specific; it is recorded
  for completeness, not as a target.
- `cargo test -p orion-ui` is green: 227 unit tests + 2 `pane_seam_smoke` +
  7 `repaint_storm` tests.

## Addendum - todo 7: row-level damage caching (landed, measured)

Date: 2026-11-13. Outcome: **the row cache landed** - the spike showed a
measured p95 improvement, so the fallback was not needed.

### What landed

`crates/orion-ui/src/terminal.rs`:

- `CachedRow { key, text, runs }` behind `Rc`, stored in
  `row_cache: Vec<Option<Rc<CachedRow>>>` indexed by visible row.
- `RowCacheKey { revision, offset, content_row, selection, find_revision,
  theme_fg, theme_bg, theme_selection, theme_accent }`. The find component is
  a `find_revision: u64` counter bumped on every `refresh_find_matches` and on
  the empty-query clear - NOT the match count (same count, different spans
  would serve stale highlights). The theme colours are in the key because
  `Theme::set_current` can swap the palette at runtime and the cached runs
  resolve against it.
- Cache-hit path clones neither `Vec<TermCell>` nor the runs; it clones only
  the row's `String` (a ~120-byte memcpy into `StyledText`) and reuses the
  cached `runs` slice. `cell_runs` is not called on a hit.
- `cell_runs_calls: u64` counter on `TerminalPane` (read via
  `cell_runs_calls()`), asserted by tests.
- Tests: `a_cache_hit_frame_does_not_call_cell_runs` (a forced re-render with
  no content change adds zero `cell_runs` calls) and
  `the_row_cache_invalidates_on_feed_scroll_selection_and_find` (feed bumps
  revision -> all rows rebuild; scroll changes offset -> rebuild; a one-row
  selection rebuilds exactly that row; every `refresh_find_matches` bumps
  `find_revision` even with identical matches; the empty-query clear bumps it
  too).

### Spike numbers (frame_times harness, 3 runs each, same machine)

Pre-cache (after todos 4+5, before todo 7):

| Metric | Run 1 | Run 2 | Run 3 | Median |
|---|---|---|---|---|
| Fixture frame p95 | 11.69 ms | 14.19 ms | 18.40 ms | **14.19 ms** |
| Fixture frame total | 1.208 s | 1.350 s | 1.619 s | **1.350 s** |
| Big log frame p95 | 15.80 ms | 16.32 ms | 20.64 ms | **16.32 ms** |
| Big log frame total | 2.833 s | 3.039 s | 3.295 s | **3.039 s** |

With the row cache:

| Metric | Run 1 | Run 2 | Run 3 | Median |
|---|---|---|---|---|
| Fixture frame p95 | 8.30 ms | 8.35 ms | 9.39 ms | **8.35 ms** |
| Fixture frame total | 0.823 s | 0.827 s | 0.825 s | **0.825 s** |
| Big log frame p95 | 9.14 ms | 9.17 ms | 10.53 ms | **9.17 ms** |
| Big log frame total | 1.708 s | 1.702 s | 1.766 s | **1.708 s** |

Frame time is measured inside `TerminalPane::render` only (perf.rs), so it is
isolated from the feed path. Frame totals dropped ~39% (fixture) and ~44%
(big log) with very tight variance across runs, and p95 dropped ~41% / ~44%.

Caveat: feed-path times also dropped between the two batches (fixture feed
p50 4.4-4.8 ms -> 2.2-2.4 ms; big log feed p95 24-30 ms -> 12.7-16.0 ms),
which the row cache cannot cause - the machine was measurably faster in the
second batch. The frame-time improvement is nonetheless attributable to the
cache: it is render-only, the direction is consistent across all 3 runs, and
the post-cache frame totals are far tighter (0.823-0.825 s) than pre-cache
(1.208-1.619 s), which is what removing the variable per-row work looks like.
Treat the absolute p95 deltas as an upper bound; the A/B signal is real.

### Verification

- `cargo test -p orion-ui` green: 237 unit + 2 `pane_seam_smoke` + 7
  `repaint_storm`.
- `cargo clippy -j 2 -p orion-ui -- -D warnings` clean.
- Snapshot/content identity: the fixture byte-stability test
  (`the_fixture_is_byte_stable`) and the full suite pass unchanged; the
  cache-hit test asserts the cached frame still paints the fed content.

## Addendum - todo 13: final measurement and IS-2 verdict

Date: 2026-11-14. Same machine, same fixture, same harness as the baseline
above. No code changed in this step: it is a measurement plus this record.

### Command (note where `-j 2` goes)

```bash
cargo test -j 2 -p orion-ui --test repaint_storm frame_times -- --nocapture
```

`-j 2` is mandatory on this box (see "Build environment" below) and must come
*before* `--`; trailing it after `--` is handed to the test binary, which
rejects `-j` with `error: Unrecognized option: 'j'`.

### Host conditions during the final measurement (read before the numbers)

The host was **not quiet** during this batch, and that dominates the spread:

- System CPU sat between 40% and 100% throughout, from unrelated processes
  (a browser and this session's terminal emulator), and briefly from a runaway
  `find / -name *.rs -path *gpui*` (started 07:22, 7,339 s of CPU). That stale
  search was killed before the recorded runs; nothing else was stopped.
- Windows power plan is **Power Saver**; the CPU reports ~2.3 GHz of a 2.9 GHz
  max clock.
- Identical binary, identical command, eleven normal-priority runs: the
  fixture frame p95 spread from **9.67 ms to 48.63 ms**. Every miss of the
  16.7 ms bar coincides with system CPU at 70-100%. This is contention noise,
  not render variance (the todo-1 batch spread was ±1-3 ms).

### The final three runs

Run 7-9 below are the final three consecutive runs (the protocol's 3x).

| Metric | Run 1 | Run 2 | Run 3 | Median |
|---|---|---|---|---|
| Fixture wall | 3.895 s | 4.128 s | 8.436 s | **4.128 s** |
| Fixture feed p50 | 3.116 ms | 3.261 ms | 12.808 ms | **3.261 ms** |
| Fixture feed p95 | 16.36 ms | 16.02 ms | 56.62 ms | **16.36 ms** |
| Fixture frames (n) | 241 | 245 | 242 | **242** |
| Fixture frame p50 | 3.600 ms | 2.327 ms | 3.198 ms | **3.198 ms** |
| Fixture frame p95 | 9.67 ms | 10.13 ms | 32.04 ms | **10.13 ms** |
| Fixture frame total | 0.957 s | 1.012 s | 2.097 s | **1.012 s** |
| Big log wall | 10.455 s | 10.636 s | 19.620 s | **10.636 s** |
| Big log feed p50 | 8.731 ms | 8.906 ms | 14.059 ms | **8.906 ms** |
| Big log feed p95 | 16.53 ms | 15.85 ms | 44.00 ms | **16.53 ms** |
| Big log frames (n) | 454 | 455 | 454 | **454** |
| Big log frame p50 | 1.982 ms | 3.504 ms | 5.293 ms | **3.504 ms** |
| Big log frame p95 | 10.64 ms | 10.30 ms | 25.28 ms | **10.64 ms** |
| Big log frame total | 1.806 s | 1.830 s | 3.579 s | **1.830 s** |

### Before / after, side by side

"Before" is the todo-1 baseline in this document; "after cache" is the todo-7
addendum above; "after final" is the median of the final three runs. Frame
totals were not recorded in the todo-1 batch (only p50/p95), so that cell is
`n/r` rather than invented.

| Metric | Before (todo 1) | After cache (todo 7) | After final (todo 13) | Bar |
|---|---|---|---|---|
| Fixture frame p95 | 10.97 ms | 8.35 ms | **10.13 ms** | ≤ 16.7 ms |
| Fixture frame p50 | 4.225 ms | n/r | **3.198 ms** | - |
| Fixture frame total | n/r | 0.825 s | **1.012 s** | - |
| Fixture feed p95 | 14.66 ms | n/r | **16.36 ms** | - |
| Big log frame p95 | 11.64 ms | 9.17 ms | **10.64 ms** | ≤ 16.7 ms |
| Big log frame p50 | 4.601 ms | n/r | **3.504 ms** | - |
| Big log frame total | n/r | 1.708 s | **1.830 s** | - |
| Big log feed p95 | 16.73 ms | n/r | **16.53 ms** | - |

Reading it honestly:

- Versus the todo-1 baseline the frame p95 is ~8% (fixture) and ~9% (big log)
  better, and frame p50 is ~24% better on both. That is the row cache plus the
  earlier render work, re-measured on today's box.
- Versus the todo-7 post-cache figure today's numbers are ~21% / ~16% *higher*.
  That is not a render regression: no render-path code changed in todos 8-13,
  and the untouched vt100 parse measures within noise of the todo-1 batch in
  the same runs (big log feed p50 8.91 ms today vs 8.32 ms baseline, +7%). The
  residual is the host, which the todo-7 addendum already flagged as faster in
  its own batch. Do not quote the todo-7 p95 as "current".
- Todos 9-12 (bounded PTY channel, off-thread status poller, bounded daemon
  channel, split pane) are feed/scheduling changes. They do not touch
  `TerminalPane::render`, so they have no expected effect on the frame
  numbers, and the data show none beyond noise.

### IS-2 verdict: met on the median, not certifiable as a guarantee

IS-2 is `p95 frame time ≤ 16.7 ms` (~60 fps) for the render path.

**Met.** Median of the final three runs: **10.13 ms** (fixture) and
**10.64 ms** (big log), i.e. 36-39% under the bar. Both of the two clean runs
(host at 40-60% CPU) landed at 9.7-10.1 ms / 10.3-10.6 ms.

**But not certifiable as a stable guarantee on this host.** Across all eleven
normal-priority runs, six of eleven were under the bar for each workload; all
five misses happened while system CPU was 70-100%, with p95 up to 32.0 ms
(fixture) / 48.6 ms (big log). A high-priority diagnostic run
(`start /high`, not part of the three) gave 12.19 ms / 13.45 ms.

No code change is proposed from this measurement: the evidence points at host
contention, and the render path is unchanged since the todo-7 A/B. The
follow-up is procedural (a plan-level item for the orchestrator): re-run this
exact command three times on an idle host (browser closed, AC power, not
Power Saver) and replace this section's numbers before anyone quotes IS-2 as
green.

### Caveats (unchanged from the baseline, restated for these numbers)

- The harness measures `TerminalPane::render` (element construction) and
  `window.draw` against the **software test renderer**. It is not GPU present
  time; a real app's present cost sits on top of this.
- Both `frame_times` tests run inside one test binary with the default test
  threading, exactly as in every prior batch.
- Nothing in this batch was run in release mode; the instrumentation is
  `#[cfg(debug_assertions)]` and compiles out of release.
- Absolute values are only comparable to rows measured while the host was
  similarly loaded. Today's host was not.

### Full session log (why the spread is what it is)

All normal-priority runs this session, chronological, values in ms (frame p95
/ frame total) with the host load seen around the run:

| Run | Load | Fixture frame p95 | Fixture frame total | Big log frame p95 | Big log frame total |
|---|---|---|---|---|---|
| 1 (post-build) | high | 15.27 | 1.387 s | 15.27 | 2.813 s |
| 2 | high | 17.58 | 1.677 s | 22.25 | 3.419 s |
| 3 | ~85% | 13.70 | 1.472 s | 15.78 | 2.913 s |
| 4 | ~85% | 20.65 | 1.663 s | 18.77 | 3.257 s |
| 5 | ~100% | 48.63 | 3.605 s | 43.24 | 6.035 s |
| 6 | ~85% | 15.31 | 1.358 s | 13.44 | 2.465 s |
| 7 | ~95% | 21.46 | 1.701 s | 19.06 | 2.937 s |
| 8 | ~85% | 10.23 | 1.035 s | 11.10 | 2.045 s |
| 9 (final 1) | 60% | 9.67 | 0.957 s | 10.64 | 1.806 s |
| 10 (final 2) | 55% | 10.13 | 1.012 s | 10.30 | 1.830 s |
| 11 (final 3) | 76% | 32.04 | 2.097 s | 25.28 | 3.579 s |
| HP diagnostic | 55% | 12.19 | 1.164 s | 13.45 | 2.311 s |

The load column is `\Processor(_Total)\% Processor Time` sampled next to the
run (runs 9-11) or the qualitative state observed at the time (runs 1-8, when
the runaway `find` was still holding a core). The correlation is the point:
the same binary moves by 5x with load.

### Gates run for this step (both crates, actual results)

| Gate | Result |
|---|---|
| `cargo check -j 2 -p orion-core` | exit 0 |
| `cargo check -j 2 -p orion-ui` | exit 0 |
| `cargo test -j 2 -p orion-core` | exit 0 - 253 lib + integration targets (7, 4, 6, 2, 2, 3) all passed, 0 failed |
| `cargo test -j 2 -p orion-ui` | exit 0 - 244 lib + 2 `pane_seam_smoke` + 7 `repaint_storm`, 0 failed |
| `cargo clippy -j 2 -p orion-core --all-targets -- -D warnings` | exit 0 |
| `cargo clippy -j 2 -p orion-ui --all-targets -- -D warnings` | exit 0 |
| `cargo fmt --check -p orion-core -p orion-ui` | exit 0 |

No test was deleted, skipped, or edited. The two live-CLI tests that
intermittently fail on this box (`pane_seam_smoke::the_seam_reaches_a_real_agent`,
some `agent_smoke` cases) both passed in this run; they need live agent CLIs
and are unrelated to this plan.

### Out-of-scope follow-up: AGENTS.md §2.3 event-name drift

`AGENTS.md` §2.3 documents the PTY output event as `orion://pty-output`. That
event surface does not exist: the implementation pushes JSON frames with
`{"type": "pty-data"}` (`crates/orion-core/src/engine/pane.rs`,
`engine/connection_registry.rs`, `bin/orion_rt.rs`), and the CLI client maps
that frame to `data`. The same section names `orion://agent-status`,
`orion://prompt-ask` and `orion://workspace-update`, which are likewise not the
implemented frame names.

Not edited in this plan (deliberate: documentation/invariant reconciliation is
its own change). Proposed follow-up: rewrite AGENTS.md §2.3 to describe the
actual wire frames, or implement the namespaced event surface and keep the
document as the spec.

## Addendum - todo 13 (re-measurement, 2026-11-14)

Date: 2026-11-14, ~10:56-11:10 local. Same machine, same fixture, same harness
as the baseline and the earlier todo-13 section above. No code changed in this
step: it is a measurement plus this record. This section refreshes the earlier
todo-13 numbers, which were taken while the host was at 40-100% CPU (a runaway
`find` plus a browser); this batch ran at 35-65% CPU.

### Command

```bash
cargo test -j 2 -p orion-ui --test repaint_storm frame_times -- --nocapture
```

`-j 2` must come before `--` (see the earlier todo-13 section).

### The three runs

| Metric | Run 1 | Run 2 | Run 3 | Median |
|---|---|---|---|---|
| Fixture wall | 3.041 s | 2.525 s | 2.290 s | **2.525 s** |
| Fixture feeds (n) | 123 | 121 | 122 | **122** |
| Fixture feed p50 | 3.100 ms | 2.187 ms | 2.115 ms | **2.187 ms** |
| Fixture feed p95 | 12.12 ms | 10.16 ms | 8.79 ms | **10.16 ms** |
| Fixture feed total | 667 ms | 546 ms | 489 ms | **546 ms** |
| Fixture frames (n) | 245 | 243 | 243 | **243** |
| Fixture frame p50 | 2.371 ms | 2.221 ms | 2.259 ms | **2.259 ms** |
| Fixture frame p95 | 7.78 ms | 6.59 ms | 6.70 ms | **6.70 ms** |
| Fixture frame total | 745 ms | 610 ms | 584 ms | **610 ms** |
| Big log wall | 7.829 s | 6.512 s | 5.961 s | **6.512 s** |
| Big log feeds (n) | 228 | 227 | 227 | **227** |
| Big log feed p50 | 6.774 ms | 5.599 ms | 5.393 ms | **5.599 ms** |
| Big log feed p95 | 11.62 ms | 10.10 ms | 9.09 ms | **10.10 ms** |
| Big log feed total | 1.436 s | 1.227 s | 1.128 s | **1.227 s** |
| Big log frames (n) | 456 | 455 | 454 | **455** |
| Big log frame p50 | 2.264 ms | 2.221 ms | 1.198 ms | **2.221 ms** |
| Big log frame p95 | 7.52 ms | 6.80 ms | 6.43 ms | **6.80 ms** |
| Big log frame total | 1.323 s | 1.127 s | 1.040 s | **1.127 s** |

Host load (`\Processor(_Total)\% Processor Time`) sampled around each run:
run 1 65% -> 40%, run 2 35% -> 56%, run 3 56% -> 54%. Power plan is still
Power Saver (~2.3 GHz of 2.9 GHz max).

### Before / after, side by side

"Before" is the todo-1 baseline; "after cache" is the todo-7 addendum; "after
final (earlier)" is the earlier todo-13 section; "after final (this batch)" is
the median of the three runs above.

| Metric | Before (todo 1) | After cache (todo 7) | After final (earlier) | After final (this batch) | Bar |
|---|---|---|---|---|---|
| Fixture frame p95 | 10.97 ms | 8.35 ms | 10.13 ms | **6.70 ms** | <= 16.7 ms |
| Fixture frame p50 | 4.225 ms | n/r | 3.198 ms | **2.259 ms** | - |
| Fixture frame total | n/r | 0.825 s | 1.012 s | **0.610 s** | - |
| Fixture feed p95 | 14.66 ms | n/r | 16.36 ms | **10.16 ms** | - |
| Big log frame p95 | 11.64 ms | 9.17 ms | 10.64 ms | **6.80 ms** | <= 16.7 ms |
| Big log frame p50 | 4.601 ms | n/r | 3.504 ms | **2.221 ms** | - |
| Big log frame total | n/r | 1.708 s | 1.830 s | **1.127 s** | - |
| Big log feed p95 | 16.73 ms | n/r | 16.53 ms | **10.10 ms** | - |

Reading it honestly:

- Versus the todo-1 baseline, frame p95 is ~39% (fixture) and ~42% (big log)
  better; frame p50 is ~47% / ~52% better. That is the row cache plus the
  earlier render work, re-measured on a quieter host.
- Versus the earlier todo-13 section, this batch is ~34% / ~36% better on
  frame p95. No render-path code changed between the two batches; the delta is
  host load (the earlier batch ran at 40-100% CPU with a runaway `find`; this
  one at 35-65%). Do not quote either batch as a guarantee.
- Todos 9-12 (bounded PTY channel, off-thread status poller, bounded daemon
  channel, split pane) are feed/scheduling changes; they do not touch
  `TerminalPane::render` and show no effect on frame numbers beyond noise.

### IS-2 verdict: met

IS-2 is `p95 frame time <= 16.7 ms` (~60 fps) for the render path.

**Met.** Median of the three runs in this batch: **6.70 ms** (fixture) and
**6.80 ms** (big log), i.e. ~60% under the bar. All three runs of both
workloads landed under the bar (fixture 6.59-7.78 ms; big log 6.43-7.52 ms).

Caveat, unchanged: the harness measures `TerminalPane::render` (element
construction) and `window.draw` against the **software test renderer**, not
GPU present time. A real app's present cost sits on top of this. The verdict
is for the render hot path as measured by this harness, not a guarantee for
the full app on every host.

### Gates run for this step (both crates, actual results)

| Gate | Result |
|---|---|
| `cargo check -j 2 -p orion-core` | exit 0 |
| `cargo check -j 2 -p orion-ui` | exit 0 |
| `cargo test -j 2 -p orion-core` | exit 0 - 253 lib + 7/4/6/2/2/3 integration targets, 0 failed |
| `cargo test -j 2 -p orion-ui` | exit 0 - 244 lib + 2 `pane_seam_smoke` + 7 `repaint_storm`, 0 failed |
| `cargo clippy -j 2 -p orion-core --all-targets -- -D warnings` | exit 0 |
| `cargo clippy -j 2 -p orion-ui --all-targets -- -D warnings` | exit 0 |
| `cargo fmt --check -p orion-core -p orion-ui` | exit 0 |

No test was deleted, skipped, or edited. The live-CLI flaky tests
(`pane_seam_smoke::the_seam_reaches_a_real_agent`, `agent_smoke`) both passed
in this run.

### Out-of-scope follow-up: AGENTS.md §2.3 event-name drift

Unchanged from the earlier todo-13 section: `AGENTS.md` §2.3 documents the PTY
output event as `orion://pty-output`; the implementation pushes JSON frames
with `{"type": "pty-data"}` (`crates/orion-core/src/engine/pane.rs`,
`engine/connection_registry.rs`, `bin/orion_rt.rs`), and the CLI client maps
that frame to `data`. The same section names `orion://agent-status`,
`orion://prompt-ask` and `orion://workspace-update`, likewise not the
implemented frame names. Not edited in this plan. Proposed follow-up: rewrite
AGENTS.md §2.3 to describe the actual wire frames, or implement the
namespaced event surface and keep the document as the spec.
