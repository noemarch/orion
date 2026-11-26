# Sidebar state and project visibility design

Date: 2026-11-25
Status: approved design, awaiting spec review
Scope: the native GPUI sidebar in `crates/orion-ui`, plus one wire field in `crates/orion-core`

## Goal and non-goals

Make the open project and the state of every agent visible in the sidebar at a glance, and never report "idle" when the Orion runtime is unreachable. The visual reference is Orca, but the Orion hierarchy is project first, then sessions; bot avatars, tasks, automations, and external-channel threads are dropped.

Non-goals:

- No change to agent launching, PTY streaming, process supervision, or the wire protocol version.
- No change to which panes exist, how they are created, or how shells are selected (`POST /api/pane/shell` stays the only shell entry point).
- No migration to gpui-kit `Sidebar`/`Tree`: ADR 0004 records why the custom entity-backed virtualized view is required for two-line rows.
- No new telemetry, no new persistence keys, no change to theme tokens' values.
- No attempt to infer a *last-activity* time. Duration means time since session start, per the resolved question.

## Scope inventory (verified)

| Concern | File | Current state |
| --- | --- | --- |
| Sidebar view, rows, status dots | `crates/orion-ui/src/sidebar.rs` | Custom `WorkspaceSidebar`, virtualized rows; `HEADER_ROW_PX=30`, `PANE_ROW_PX=48` |
| Session-row data | `crates/orion-ui/src/agents.rs` | `AgentRow { selected, .. }` built from snapshot panes |
| State → word/colour mapping | `crates/orion-ui/src/session_status.rs` | `SessionState` projection; no offline state |
| State lifecycle + aggregate | `crates/orion-core/src/session_state.rs` | `SessionState`, `aggregate()`, `working_count()` already implemented, unused by UI |
| Pane snapshot fields | `crates/orion-core/src/engine/pane.rs` | `info()`/`agent_info()` carry `pane_kind`, `agent_kind`, `shell_path`, `cwd`, `started_at` exists on `Pane` but is not serialized |
| Runtime status | `crates/orion-ui/src/app.rs`, `crates/orion-core/src/runtime/client.rs` | One-time `notice` at `app.rs:516`; `RuntimeClient.alive` private; `RemotePane::status()` guesses `Idle` on failure |
| Tab strip | `crates/orion-ui/src/app.rs` | `render_tab_strip()` `1684-1856`; 44px suffix buttons inside `TabBar.small()` |
| Footer icons | `crates/orion-ui/src/sidebar.rs` | `render_bottom_bar()` `1417-1441` already uses `action_button()` with tooltips |

## Approved approach

Evolve the existing `WorkspaceSidebar` in place. Derive all displayed state from core `SessionState`; add exactly one field (`started_at`) to the pane snapshot; add one explicit `runtime_online` flag to the sidebar, fed by the app and by poll failures. Rejected: a separate derived view-model entity (redundant with `SessionState`), and a gpui-kit `Sidebar` migration (ADR 0004).

## Data model and offline

- Extend the pane snapshot in `engine/pane.rs` `info()` and `agent_info()` with `started_at` serialized as an RFC 3339 UTC string (chrono's default serde representation). Add the matching field to `protocol::PaneInfo` so the typed wire vocabulary stays in sync. No protocol-version bump; additive fields are ignored by older readers.
- Define in the UI a view enum `RowState { Working, Idle, None, Offline }` plus `since: Option<DateTime<Utc>>`. Map:
  - `SessionState::Starting | Running | NeedsInput` → `Working`
  - `SessionState::Idle | Exited { .. }` → `Idle`
  - shell pane / no agent → `None`
  - `runtime_online == false` → `Offline` for every row, regardless of cached state
- `WorkspaceSidebar` gains `runtime_online: bool`, default `true`. `AppModel` passes false when `connect_or_start_runtime` falls back to the in-process engine (`app.rs:516`); `refresh_session()` (`sidebar.rs:1156`) sets it false when a runtime request fails and true again on the first successful snapshot. Never read state from `RemotePane::status()` for display: it returns `Idle` on failure.

## Row layout

- Pane row (`TreeRow::Pane`, `sidebar.rs:844-978`) keeps `PANE_ROW_PX = 48`. Line 1: fixed-width state slot (use `MARK_SIZE_PX` = 22px, contents centred), then the title. Line 2: indented status/path text.
  - Working: spinning arc in the status-working colour (`theme.status_working`, pulsed via `status_color`), text `working · <elapsed>`. The brief says "accent color", but `AGENTS.md` reserves rose for focus and gives agent states their own status tokens, so rose is not used here.
  - Idle: solid muted dot (`theme.status_idle`), text `idle · <elapsed>`
  - No agent (shell): empty fixed-width slot, text = middle-truncated path
  - Offline: warning triangle, text `offline`. Use an existing gpui-kit warning/alert `IconName` verified against 0.9.1; if none exists, draw a small triangle in `theme.warning_amber`.
- `<elapsed>` is `Utc::now() - started_at`, formatted as `2m`, `14m`, `1h`. No `started_at` yet → omit the suffix rather than guess. Use the existing pulse timer for working rows.
- Add `truncate_middle(s, max_chars)` in `sidebar.rs`: keep a head and tail, insert `…` between them. Derive `max_chars` from the row width, `density_scale()`, and the secondary font size; fall back to a conservative constant in rail mode. Preserve Windows drive prefixes and the final path component.
- Keep `.truncate()` for titles (end truncation is correct there); only paths use middle truncation.

## Project row

- `TreeRow::Header` (`sidebar.rs:730-843`) derives its indicator with `SessionState::aggregate()` over the project's agent sessions:
  - offline → warning triangle + `offline`
  - any `Working` → spinner + `working`, plus `working_count` beside the spinner *only when the project has more than one agent*
  - else any agent → solid muted dot + `idle`
  - else → empty fixed-width slot, no text
- The derived value is computed in `visible_rows()`; never stored on `WorkspaceEntry`. The brief's ASCII example shows `⟳ 2` with one working agent; change 3 (count = working agents) is normative, so that example renders `⟳ 1`.
- The open project (the one containing the focused pane) gets `theme.selected_bg()` on its header row and no border. This replaces the header's current selection treatment.

## Chrome fixes

- Rose (`theme.accent_emphasis`) is reserved for focus, never for a selected row or an agent state. The selected session row keeps only a filled `theme.selected_bg()` background; the rose focus ring remains the single pink border on the focused row. Neither the project header nor the terminal pane paints a rose border.
- Both `...` buttons (project row and session row) are rendered only on hover/focus. `+` stays always visible. Extend the existing hover gating in `render_row_controls()`.
- Footer tooltips already exist through `action_button()`; verify they render with clear labels ("Chat Lens", "Terminal Lens") and adjust copy only if needed.
- Add a warning triangle beside "Orion" in `render_titlebar()` when `runtime_online == false`, with a tooltip naming the unreachable runtime.
- Fix the clipped tab in `render_tab_strip()`: give the tab bar enough vertical room for its 44px suffix buttons, or size those buttons down for this strip only. Verify at both density settings and in narrow/compact mode.

## Verification plan

- Unit tests: `SessionState::aggregate`/`working_count` already have core tests; add UI tests for the `RowState` mapping (including offline override), `truncate_middle`, working-count visibility, and empty slot for shells.
- Gates per AGENTS.md: `cargo check -p orion-ui`, `cargo test -p orion-ui`, `cargo clippy -p orion-ui -- -D warnings`, `cargo fmt --check`, and `cargo check -p orion-core` for the snapshot field. Record the known host limits from the modal spec (all-target UI test OOM; `repaint_storm::live_cat_through_a_pty` hang) as environment notes if they recur.
- Visual check: capture the sidebar at wide and narrow widths, with one working agent, one idle agent, one shell, and offline; confirm fixed slot alignment, no clipped tab, and one pink border.

## Risks and mitigations

- Risk: `started_at` on the wire changes snapshot shape. Mitigation: additive field only, no version bump; verify both in-process and remote runtime paths parse it.
- Risk: poll failure flips `runtime_online` false on a transient error. Mitigation: only a failed request flips it; the first successful snapshot restores it immediately; no guessed states are shown in between.
- Risk: middle truncation breaks on non-ASCII or very short paths. Mitigation: char-based head/tail on `chars()`, return the original string when it already fits, never panic on small maxima.
- Risk: removing the terminal pane's pink border loses focus indication. Mitigation: keep the focused session row's border and verify the main area still shows a distinct active lens/tab.
