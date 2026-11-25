# Modal revamp visual evidence (issue #26)

Captured 2026-11-24 from the final `orion-ui` binary at commit `e44fdcb`
(`G:\Temp\rust\target\debug\orion-ui.exe`), using OS-level window capture
(`Graphics.CopyFromScreen` via `scripts/capture_issue16.ps1` pattern, isolated
`ORION_DATA_DIR` + preferences theme/density, `GPUI_DISABLE_DIRECT_COMPOSITION=1`).

## Custom overlays — captured

| File | Surface | Size | Theme | Result |
|---|---|---|---|---|
| `orion-modal-workspace-1280x800-dark-comfortable.png` | New workspace | 1280x800 | dark | pass |
| `orion-modal-workspace-1280x800-light-comfortable.png` | New workspace | 1280x800 | light | pass |
| `orion-modal-terminal-1280x800-dark-comfortable.png` | New terminal | 1280x800 | dark | pass |
| `orion-modal-terminal-1280x800-light-comfortable.png` | New terminal | 1280x800 | light | pass |
| `orion-modal-terminal-480x800-dark-comfortable.png` | New terminal | 480x800 | dark | pass |
| `orion-modal-terminal-480x800-light-comfortable.png` | New terminal | 480x800 | light | pass |

Visual review of these captures confirms: one visible close control (header X),
no duplicate footer Cancel, sentence-case titles ("New workspace", "New
terminal"), muted captions (Folder/Agent/Shell), dimmed-but-legible disabled
rows with "Not on PATH", selected row marked, validation error adjacent to the
primary action, panel centred inside the window at both sizes, and both themes
using tokens only. The narrow terminal panels fit inside the 480px window
without clipping.

## Narrow-width Add Workspace — OS-capture blocker

The 480x800 Add Workspace OS capture could not be produced: at that width the
sidebar collapses to an icon rail and the "New workspace" `+` trigger is not
reachable by the capture script's coordinate-driven click (it instead opened
chat/terminal panes). The narrow-width geometry is proven at the render level by
`new_workspace::tests::modal_panel_fits_at_narrow_width` (480px), which asserts
the panel is drawn with positive size and inside both window edges. This is the
same seam the Settings redesign uses for its narrow-width proof. No source
change was made to work around the capture limitation.

## Confirmation dialogs

The two gpui-kit alert dialogs were verified by API audit and code trace only
(`docs/visual/2026-11-24-issue26-confirmation-audit.md`). Both already satisfy sentence-case
titles, outcome-verb "Close" buttons, specific consequence text, and token-only
colors, so no source change was made. OS-level screenshots of the confirmations
were not reachable in the capture harness; this is recorded as an environment
limitation, not a defect.

## Final verification (2026-11-25)

Re-checked at commit `02ac4b9`, with no source change required:

- `modal_shell::tests` covers one close control, Escape dismissal and opener
  restoration, and close-control opener restoration.
- `new_workspace::tests` covers disabled/non-selectable agents, primary-action
  gating, one close control with no Cancel, panel fit at 1280px and 480px, and
  opener restoration on Escape.
- `shell_picker::tests` covers one close control with no Cancel, panel fit at
  1280px and 480px, and opener restoration on Escape.
- `cargo fmt --check`, `cargo check -p orion-core`, `cargo check -p orion-ui`,
  `cargo clippy -p orion-ui -- -D warnings` and `cargo test -p orion-ui --lib`
  (430 passed) are clean; `git diff --stat -- crates/orion-core` is empty.
- Confirmation-dialog OS captures remain unreachable: the fixture session in the
  capture harness contains only a settled shell pane, so no close target is
  running; the pinned toolkit's alert dialog is also not observable through this
  repo's render-level query API. The API audit and copy trace above stand as
  evidence, as the plan permits.
