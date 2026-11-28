# Orca three-pane UI — OS-level capture evidence (2026-11-27)

Task 10 of `.omo/plans/orca-three-pane-ui.md`. Full command log, rejection table and DoneClaim are in `.omo/evidence/task-10-orca-three-pane-ui.md`.

Captured from a freshly rebuilt `G:\Temp\rust\target\debug\orion-ui.exe` (built 2026-11-27 14:10:21 from worktree HEAD `0c05b93`) through a parameterized copy of `scripts/capture_issue16.ps1` kept outside the workspace at `G:\Temp\tmp\opencode\task10-capture\capture_task10.ps1`. Final artifacts use `SetForegroundWindow` + `Graphics.CopyFromScreen` on the real window rect with `GPUI_DISABLE_DIRECT_COMPOSITION=1` and an isolated `ORION_DATA_DIR`; `PrintWindow` was diagnostic only. Readiness and the mode switch were detected by pixel-region change, not a fixed sleep.

## Captures

| File | Window | Theme | Right column | Sampled colours | Mean | Verdict |
| --- | --- | --- | --- | --- | --- | --- |
| `screenshots/orca-1280x800-dark-transcript.png` | 1280×800 | dark | open, **Transcript** (read-only transcript view) | 198 | 34.8 | pass |
| `screenshots/orca-1280x800-light-transcript.png` | 1280×800 | light | open, **Transcript** (read-only transcript view) | 237 | 243.3 | pass |
| `screenshots/orca-1280x800-dark-review.png` | 1280×800 | dark | open, **Review** (diff pane) | 265 | 35.0 | pass |
| `screenshots/orca-1000x800-rail.png` | 1000×800 | dark | collapsed at the 1200px breakpoint | 207 | 34.9 | pass |

Counts are distinct sampled colours from a System.Drawing pass over every 6th pixel of the full-resolution PNG; every file is a valid PNG at the exact expected dimensions and non-uniform. The mode button was clicked through the task-8 `right-column-mode-button` control; `MODE_SWITCH_DETECTED=True` in both transcript runs and the button label changed from `Review` to `Transcript`. The functional switch is proven by the task-8 draw test, not by the screenshot alone.

## What the captures show

- Sidebar: `Orion` header, search, `Projects`, the `orion` project and one pane row with `model unavailable`; no host-group section.
- Centre: chat with `No conversation yet`, the composer and its four affordances (`@`, `/`, `Access mode unavailable`, `Model unavailable`).
- Status bar: `0 working · 1 host · 1 panes`, `usage unavailable locally`, `Orion 0.1.0` — locally derived, no fabricated cost.
- At 1000px the right column collapses; the 64px sidebar rail is below the 900px `NARROW_BREAKPOINT_PX` and is covered by `shell_model::tests::narrow_window_uses_rail_with_touch_targets`, not by these captures.

## Deviations recorded

1. First transcript trial clicked absolute coordinates while the app had restored its window to `320,162,1600,962`; it hit a neighbouring editor window and was **rejected**. Fixed by re-asserting `MoveWindow(0,0)` and clicking at window-relative coordinates.
2. `PrintWindow` counts differ from screen-copy counts on every frame; `PrintWindow` is the known-bad diagnostic channel for this GPUI window and produced no final artifact.
3. A 1–5px strip of an always-on-top neighbouring window shows at the far edge of some screen-copy captures; no Orion surface is clipped or replaced.
4. Multimodal `look_at` was unavailable; visual read-back used the image attachment and the System.Drawing pixel-diversity script as the recorded fallback.

## Gates

`cargo run --bin orion-ui` started with no missing-route log; `cargo fmt --check` and `cargo check -p orion-ui` both exit 0 after the documentation-only Quiet Slate / Quiet Canvas naming reconciliation in `theme.rs` and `DESIGN.md` (no token value changed).
