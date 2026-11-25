# Issue #35 — sidebar visual evidence and release gates

Verified 2026-11-25 against the grouped Workspace → Pane tree completed in #28–#34. The final binary was built from the working tree at commit `046ed33` plus the subtitle fix recorded below. Captures use the OS-level screen-copy method from `scripts/capture_issue16.ps1`, with an isolated `ORION_DATA_DIR`, `GPUI_DISABLE_DIRECT_COMPOSITION=1`, and preferences set per capture.

## Captures

| File | Window | Theme | Result |
|---|---|---|---|
| `screenshots/orion-sidebar-wide-dark-comfortable.png` | 1280×800 | dark | pass |
| `screenshots/orion-sidebar-wide-light-comfortable.png` | 1280×800 | light | pass |
| `screenshots/orion-sidebar-compact-dark-comfortable.png` | 1000×800 | dark | pass |
| `screenshots/orion-sidebar-compact-light-comfortable.png` | 1000×800 | light | pass |

All four show the grouped header (`orion`), its visible pane count (`1`), the per-workspace `+` and action menu, a two-line pane row (title `Windows Powe…` and monospace subtitle `C:/rust-projects/orion`), the leading status ring and agent glyph, the rose focus ring on the selected row, and no clipping or overflow at either width. At widths below the 900px `NARROW_BREAKPOINT_PX` the shell intentionally collapses the sidebar to a 64px rail; that layout is covered by `shell_model::tests::narrow_window_uses_rail_with_touch_targets`, not by these captures.

## Defect found and fixed

The first capture round showed only the title line. `sidebar::tests::pane_subtitle_occupies_the_second_line` proved the subtitle element had zero height: the 44px row controls participated in the row's flex layout and squeezed the second line out of the 48px row. The row controls are now absolutely positioned over the row (keeping their 44px hit targets), the subtitle has an explicit line height, and the test asserts it has positive height and stays inside the row. Re-capture confirmed the subtitle is visible in both themes at both widths.

## Parent acceptance audit

| Parent stories | Evidence |
|---|---|
| 1–2, 8: grouping, counts | `workspaces_group_their_panes_and_collapse_together`; captures show header and count |
| 3–5, 36: collapse, persistence, whole-header toggle | `workspaces_group_their_panes_and_collapse_together`, `collapse_survives_sidebar_hide_and_show`, `left_and_right_collapse_and_expand_the_group_under_the_cursor` |
| 6–7: create in chosen/active workspace | `create_controls_use_the_existing_tab_creation_route`; header New Workspace control visible in captures |
| 9–12, 64–66: two-line rows, truncation, fonts | `pane_rows_name_status_and_show_a_monospace_subtitle`, new subtitle-bounds test, captures |
| 13–22: brand glyph, one status indicator, status palette, accessible names | captures; `every_status_uses_the_shared_accessible_name`, `status_dot_uses_theme_tokens_not_rose`, `session_status::tests::*` |
| 23–25, 60–61: selection, focus ring, keyboard/pointer activation | captures; `tree_focus_puts_a_ring_inside_the_selected_row_only`, `enter_activates_the_focused_pane_and_keeps_focus_in_the_tree`, `clicking_a_pane_activates_it_and_moves_the_tree_selection` |
| 26–27, 47–51: hover close, no drag, menu actions | `the_close_control_is_hidden_until_the_row_is_hovered`, `row_menus_offer_only_rename_and_close`, `inline_rename_commits_through_the_existing_route`, `escape_cancels_inline_rename_without_a_request`, `closing_a_settled_pane_needs_no_confirmation`, `closing_a_running_pane_asks_for_confirmation_first`, `closing_a_running_workspace_uses_the_session_confirmation` |
| 28–29: click activates pane and workspace | `clicking_a_pane_activates_it_and_moves_the_tree_selection` |
| 30–40: search, auto-expand, empty state, focus return | `search_filters_panes_by_title_cwd_and_shell`, `search_expands_matching_groups_only_in_the_filtered_view`, `search_keeps_selection_and_offers_a_clearable_empty_state`, `escape_clears_search_and_returns_focus_to_the_tree`, `escape_from_the_tree_clears_search_and_keeps_tree_focus` |
| 41–46: header/footer controls | `sidebar_settings_icon_reports_open_settings`, `sidebar_header_collapse_reports_toggle_sidebar`, `terminal_icons_report_the_terminal_lens`, `footer_view_icon_reports_the_chat_lens`; captures show the header controls |
| 52–59, 62: keyboard cursor and virtualization | `arrow_keys_move_the_focus_cursor_over_headers_and_panes`, `left_and_right_collapse_and_expand_the_group_under_the_cursor`, `enter_on_a_header_toggles_the_group`, `navigation_and_activation_respect_the_search_filter`, `slash_and_ctrl_k_focus_search_from_another_view`, `the_cursor_scrolls_into_view_when_it_reaches_an_unrendered_row` |
| 67–69: tokens, touch targets, layout fit | captures; `pane_rows_name_status_and_show_a_monospace_subtitle`, `theme::tests::*`, menu buttons use `TOUCH_TARGET_PX` |
| 70–72: old sections and identity removed | `pane_rows_name_status_and_show_a_monospace_subtitle` asserts the old sections are gone; IDs derive from workspace/pane identity (ADR 0004) |
| 73–80: entities, stable IDs, ADR, glossary | source audit of `sidebar.rs`; ADR 0004 and `GLOSSARY.md` present |
| 81–84: tests and gates | this record and the gate results below |

## Gate results

| Gate | Result |
|---|---|
| `cargo fmt --check` | pass |
| `cargo check -p orion-core`, `cargo check -p orion-ui` | pass |
| `cargo clippy -p orion-core -- -D warnings`, `cargo clippy -p orion-ui -- -D warnings` | pass |
| `cargo test -p orion-core --lib` | 303 passed |
| `cargo test -p orion-ui --lib` | 426 passed |
| `cargo test -p orion-core` (full) | pass, including doc tests |
| `cargo test -p orion-ui --test terminal_startup` | 2 passed |
| `cargo test -p orion-ui --test repaint_storm` | 7 passed |
| `cargo test -p orion-ui --test pane_seam_smoke` | 1 passed, 1 failed (see note) |

## Environment notes

- `cargo test -p orion-ui --test pane_seam_smoke::the_seam_reaches_a_real_agent` fails on this host: a live `claude` CLI is launched and the interrupt ends it (`the write did not go down the PTY`). The test skips when `claude` is absent, so this is a host/CLI condition, not a suppressed failure.
- Windows has no headless GPUI renderer in the pinned toolkit, so pixel comparison targets are unavailable; the evidence above uses OS-level window capture instead.
- Build parallelism was limited to `CARGO_BUILD_JOBS=1` after Windows commit-limit errors during earlier parallel builds. No test or lint was skipped.
- Worktree check: the change contains only the sidebar row-layout fix, its test, the four captures and this record. No Engine, wire, data-format, process-lifecycle, dependency or unrelated refactor changes.
