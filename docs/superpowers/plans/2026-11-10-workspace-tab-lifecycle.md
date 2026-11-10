# Workspace and Tab Lifecycle (#8)

## Goal
The native shell manages workspaces and tabs through the Engine Route Table:
create a workspace (folder + agent), create/rename/close tabs, close panes,
switch workspace/tab to change the active pane while preserving each pane's
lens, surface agent states, restore the last selection, and show the empty
state when the last workspace closes.

## Tasks

1. **TDD: lens book** — per-pane lens selection (`shell_model::LensBook`).
2. **TDD: new-workspace model** — recent directory + agent choices
   (`new_workspace` pure helpers).
3. **New Workspace flow** — folder browser + agent picker modal that posts
   `POST /api/workspace/create`, then refreshes and focuses the new pane.
4. **Startup restore** — `Engine::restore()` first; otherwise open the default
   workspace through `/api/workspace/create` (no direct `spawn_pane`).
5. **Active-pane following** — re-attach lens views when the focused pane
   changes (workspace/tab switch, create, close).
6. **Lens per pane** — lens switcher writes to the active pane; split body
   reads its own pane lens.
7. **Sidebar lifecycle** — `+` in Workspaces header, close `×` per workspace,
   written agent status words; emit events to the app.
8. **Empty state** — "New Workspace" action.
9. **Tab create** — use the workspace's agent instead of hardcoded `claude`.
10. **Tests + verification** — `cargo test --workspace`,
    `cargo clippy --workspace --all-targets -- -D warnings`,
    `cargo fmt --all --check`, then `code-review`.
