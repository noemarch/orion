# F5 — Final verification consolidation (2026-11-18)

## Goal outcome
The Orion GPUI shell now has a one-click sidebar collapse and git/review-column hide with header restore controls; application-facing controls are gpui-kit components; spacing/layout follow Quiet Slate; and the reported slowdown was investigated with a fresh baseline, with only confirmed causes fixed.

## Scenario contract — results
| Scenario | Result | Evidence |
|---|---|---|
| S1 sidebar full hide + restore, zero width not rail | PASS | `G:/Temp/tmp/opencode/orion-ulw-yi8RAZ/evidence/F3/orion-sidebar-collapsed2.png` — sidebar div absent, header reads "Show sidebar"; OS-level click log |
| S2 git/review pane hide + restore, two-column | PASS | `.../F3/review-hidden-clean.png` — review column absent, header reads "Show review", Changes lens tab present |
| S3 all clickable divs migrated; grid Element kept | PASS | grep: no `cursor_pointer()` anywhere; `terminal_grid.rs` custom `Element` retained |
| S4 narrow rail unchanged; hidden stays hidden | PASS (code + unit) | `shell_model.rs::ShellLayout::for_width` unchanged; `sidebar_visible` independent of rail; no narrow screenshot captured (QA focus contention) |
| S5 no adjacent regressions | PASS | theme gating, pane pruning/attach, diff drag, chat scroll, send/disabled, process cleanup — all gates + targeted tests green |
| S6 perf evidence-led | PASS | `docs/perf/ui-baseline-2026-11-17.md` + `ui-post-change-2026-11-17.md`; terminal harness refuted as cause; app-shell fixes labeled structural, not measured |

## F1 quality gates (all exit 0)
- `cargo fmt --check` — 0
- `cargo check -p orion-ui` — 0; `cargo check -p orion-core` — 0
- `cargo clippy -p orion-ui -- -D warnings` — 0; `cargo clippy -p orion-core -- -D warnings` — 0
- `cargo test -p orion-ui --lib` — 267 passed
- `cargo test -p orion-ui --test repaint_storm` — 7 passed
- `cargo test -p orion-ui --test pane_seam_smoke` — 2 passed
- `cargo test -p orion-ui --test terminal_startup` — 2 passed
- `cargo test -p orion-core` — lib 264 passed + all integration suites passed

## F2 behavior/render tests
`orion_test!` GPUI render-presence tests remain in `app.rs` (7 probes) and other views; no pixel snapshots added. New pure tests: `shell_model` visibility helpers (16 boolean combinations), `diff.rs` combined rev-parse parse + real-SHA assertions.

## F3 visual QA
Native GPUI app launched on the real 1920x1080 desktop with `GPUI_DISABLE_DIRECT_COMPOSITION=1`. Both toggles verified by OS-level clicks + screenshots. Verdict GOOD, one N/A (narrow screenshot), no `[defect]` findings. Details in the RUN_TMP `evidence/F3/visual-qa-verdict.md`.

## F4 perf re-measure
3 post-change runs; all metrics inside baseline noise band → "no measurable difference" in the terminal harness. App-shell improvements are structural with code-inspection evidence; measuring them needs a real-app frame benchmark not available here. No fix reverted.

## Reviewer gate
- Round 1: REQUEST_CHANGES — one blocker: combined `git rev-parse` applied `--abbrev-ref` to both HEAD args, so `head` held the branch name instead of the SHA (`core/engine/diff.rs`).
- Fix: reordered to `--show-toplevel HEAD --abbrev-ref HEAD`, parser field order swapped to match, test strengthened to assert `head == repo.head()`, `head != branch`, 40-char SHA.
- Round 2: APPROVE — all five review points confirmed; one non-blocking stale docstring also corrected.
- Final gates after fix: fmt 0, check core 0, diff tests 9 passed.

## Global / worktree hygiene
- Zero commits created (HEAD still `502c97f`).
- `docs/herd-ui-reference.html` — untracked, unmodified, unstaged (read-only design target).
- Perf reports are untracked artifacts, not staged.
- QA app process cleaned (`taskkill /PID 1572 /F`; `tasklist` confirms none).

## Known limitations (honest)
- No narrow-window (<900px) screenshot; rail behavior covered by pure tests only.
- Native GPUI surface has no pixel-diff score; evidence is screenshots + interaction log.
- Terminal harness does not render the app shell, so the perf fixes' user-visible gain is not numerically measured.
- `app.rs` visual-cleanup agent produced no distinct visual diff beyond Waves 2–3; spacing tokenization landed in sidebar/chat/composer/review_pane/diff_view.
