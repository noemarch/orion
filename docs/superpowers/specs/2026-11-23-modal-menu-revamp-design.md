# Modal menu revamp design

Date: 2026-11-23
Status: approved design, awaiting spec review
Scope: all five modal/overlay surfaces in the native GPUI shell

## Goal and non-goals

Apply the visual language of the shipped Settings redesign to every modal/overlay surface, starting with Add Workspace. Keep all non-visual behavior intact. This is a layout, styling, and focus-contract pass, not a feature change.

Non-goals:

- No changes to `orion-core`, wire frames, HTTP routes, storage keys, or event payloads.
- No migration of custom overlays to gpui-kit `Settings`; that component is a page/sidebar browser and is wrong for short forms and pickers.
- No new feature, no validation-rule change, no change to which agent or shell can be selected.
- No changes to in-layout controls that are not modals: tab rename, terminal find bar, terminal touch key bar, tooltips, scrollbars, or the empty-state button that opens Add Workspace.

## Scope inventory (verified)

| Surface | File | Current container | Action |
| --- | --- | --- | --- |
| Add Workspace | `crates/orion-ui/src/new_workspace.rs` | custom overlay div + scrim | Rebuild shell around existing form; primary target |
| Shell Picker | `crates/orion-ui/src/shell_picker.rs` | custom overlay div + scrim | Rebuild shell around existing picker |
| Close running pane confirmation | `crates/orion-ui/src/app.rs` (`confirm_close_pane`) | gpui-kit alert dialog | Token/copy pass only |
| Close running session confirmation | `crates/orion-ui/src/sidebar.rs` | gpui-kit alert dialog | Token/copy pass only |
| Settings | `crates/orion-ui/src/settings.rs` | custom overlay + gpui-kit Settings | Reference only; unchanged |

No sheets, popovers, or context menus exist in the UI crate. The two confirmation dialogs are the only kit-owned overlays.

## Approved approach

Use a shared, stateless modal shell for the two custom overlays. Keep gpui-kit's alert dialogs for confirmations and apply only verified theme/token and copy changes. Leave the shipped Settings modal untouched. This was chosen over inline duplication (higher drift risk) and full kit-dialog migration (unverified 0.9.1 APIs and larger behavior risk).

## Architecture

Add `crates/orion-ui/src/modal_shell.rs`, a value-like `RenderOnce`/helper element with no entity state of its own:

- Inputs: element id, sentence-case title, close callback, `&FocusHandle`, `opener_focus`, content element, density-derived width/max-height, and the current `&mut Window`/`&mut App`.
- Renders: scrim (`theme.scrim`), centered panel, header row with title and one 44px X close, hairline separator, content slot, focus trap, Escape handler.
- Does not own `open`, form state, focus handles, or opener focus. Those stay in each modal view entity so no stateful component is created during render.

Ownership changes in the two custom modals:

- `NewWorkspaceModal` keeps its fields and removes only the shell-level rendering concerns. It gains `opener_focus: Option<FocusHandle>`; its `focus_handle` remains the view's handle and is passed to the shell.
- `ShellPickerModal` gets the same `opener_focus` field and uses the shared shell.
- Trigger sites in `app.rs` (sidebar New Workspace event, empty-state button, header New Terminal button) pass `window.focused(cx)` into the open method. If no window-focused value is available at a trigger, pass `None` and the shell simply has no focus target to restore; do not synthesize one.
- Add `open_with_focus` to both modal views. `NewWorkspaceModal::open_with_focus(opener: Option<FocusHandle>, start: Option<String>, cx: &mut App)` stores the opener and calls the existing `open_at`; `ShellPickerModal::open_with_focus(opener: Option<FocusHandle>, default: &str, cwd: Option<String>, cx: &mut App)` stores the opener and calls the existing `open_with_default`. All existing open triggers must be updated to pass `window.focused(cx)` as the opener.
- The focus trap uses the verified gpui-kit 0.9.1 API `.focus_trap(id, &focus_handle)` from `gpui_kit::base::FocusTrapElement`, matching `settings.rs`.

The shell pattern mirrors `settings.rs` shell composition (scrim, centered panel, header, footer rhythm) but does not import `Settings`, `SettingPage`, `SettingGroup`, `SettingItem`, or `SettingField`.

## Visual design

- Panel: centered over scrim; density-scaled width clamped to available window width; comfortable minimum width; content scrolls rather than clipping. Add Workspace currently uses 520px and Shell Picker 420px; the shared shell accepts a per-modal preferred width so these can differ without duplicating layout code.
- Header: single row, sentence-case title left, one right-aligned X close at `TOUCH_TARGET_PX` (44px), muted tooltip and accessible label, hairline below.
- Add Workspace: "Folder" section with current path as muted text, quiet Home/Up buttons, scannable directory rows with stable IDs and chevrons; "Agent" section with radio rows, installed agents selectable, non-installed agents visibly disabled and marked "Not on PATH". Validation error directly above the footer, adjacent to the primary action it blocks.
- Shell Picker: "Shell" section with radio rows and optional path field in vertical layout; error directly above footer.
- Footer: primary action only ("Open workspace", "Open terminal") — the existing footer Cancel buttons are removed as duplicate close controls.
- Tokens: colors from `cx.theme()` / Orion theme tokens only; no raw hex or `rgb`. Spacing, type, and panel dimensions use `theme.density_scale()`; `TOUCH_TARGET_PX` remains a fixed physical minimum. Disabled rows are dimmed but legible; selected rows use a persistent selected surface.
- Confirmation dialogs: sentence-case titles and outcome-verb buttons; no raw literals introduced. Their kit-owned geometry and focus behavior remain. A "token/copy pass" permits changing only strings and theme-token arguments after the 0.9.1 APIs are verified; if the current dialog already satisfies those rules, it stays unchanged and the screenshots are the evidence.

## Behavior and data flow

- Close: header X or Escape only. No scrim click-to-close. This follows the Settings pass, where a scrim mouse handler swallowed clicks inside the panel because GPUI dispatched overlapping absolute children.
- Focus: shared shell traps focus and restores opener focus on close. Each modal view owns `open`, form state, and its focus handle.
- Preserve exactly: HTTP routes and payloads, validation rules, emitted events, selection rules, recent-directory fallback, error meaning, and the `NewWorkspaceEvent::Created` / `ShellPickerEvent::Created` flows in `app.rs`.
- Copy may change case and wording for clarity, but action outcomes and error meaning do not.
- Confirmation dialogs keep gpui-kit alert-dialog primitives and callbacks. Their focus behavior is inherited from the kit and not replaced.

## Verification plan

- Keep existing unit/draw tests green, notably `new_workspace.rs` tests for agent choices, recent-directory fallback, folder browser drawing, and missing-recent-folder recovery; shell picker tests; focus and panel-fit tests.
- Add focused UI assertions: panel bounds at desktop and narrow widths, one close control, focus trapped/restored, non-installed agents disabled and non-interactive, and primary action state.
- Capture fresh light/dark screenshots at wide and narrow widths for Add Workspace and Shell Picker, plus both confirmation dialogs. Store evidence under `docs/visual/screenshots/` using the repo's existing naming convention.
- Gates: `cargo check -p orion-ui`, `cargo test -p orion-ui`, `cargo clippy -p orion-ui -- -D warnings`, `cargo fmt --check`, plus a `cargo check -p orion-core` no-op proof. On this host, known environment limits apply: full all-target `cargo test -p orion-ui` has hit rustc OOM, and `repaint_storm::live_cat_through_a_pty` hangs; individual runnable suites have passed. Record any recurrence as an environment note, not a regression from this change.

## Risks and mitigations

- Risk: focus restoration breaks event-driven open paths. Mitigation: update every trigger to pass `window.focused(cx)` and add an integration assertion for each open path where feasible.
- Risk: removing footer Cancel is perceived as lost behavior. Mitigation: header X and Escape provide the same dismissal outcome; visual QA must confirm the header close is obvious.
- Risk: shared shell becomes settings-specific by accident. Mitigation: shell takes generic content and never imports setting-page types.
- Risk: gpui-kit confirmation APIs differ in 0.9.1. Mitigation: verify against local gpui-kit 0.9.1 source before editing; if an API is unverified, leave that dialog unchanged and report the gap.
- Risk: responsive width clips content. Mitigation: clamp to available window width, allow internal scrolling, and verify at narrow widths in both themes.

## Acceptance criteria

1. All five scoped surfaces use the redesigned visual language or a verified token/copy pass.
2. Custom overlays have one visible close control and Escape dismissal; no scrim close.
3. Focus is trapped while open and restored to the opener on close.
4. All existing HTTP calls, validation rules, events, and selection behavior are unchanged.
5. Light/dark and wide/narrow screenshots exist for both custom overlays and both confirmations.
6. Formatting, clippy, targeted tests, and runnable test suites pass; no `orion-core` diff.
