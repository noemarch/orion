# Issue #24 regression evidence (2026-11-23)

Verification of `noemarch/orion#24` after commits `bfd72c2`, `b9ce9d7` (prior session)
and `c941839`, `a3c2ef4` (this session). Issue is closed with the T6.1 interpretation
note below; this file records what was actually run and observed.

## Implementation status

- T1: one `SessionState` enum in `crates/orion-core/src/session_state.rs`; sidebar
  badges, Working/Settled counters and terminal footer derive from that enum;
  duplicate rows carry agent name + pane title; `NeedsInput` uses amber.
- T2: selecting a file fetches its patch with a spinner; empty, binary, error and
  Retry states explain themselves; bare dash label replaced; `0/4` counter removed.
  This session fixed the last gap: a fresh pane no longer claims "No changes"
  before any fetch, and a list refresh drops a stale selection (`c941839`).
- T3: cursor drawn only when focused and DECTCEM (mode 25) is set; cell backgrounds
  snap to cells that actually have them; explicit monospace font stack + size.
- T4: single `agent · state · cwd` status line; pane UUID only in a hover tooltip.
- T5: tooltips on interactive icon-only buttons; 44px close target; confirmation
  before closing a running process; focus rings on terminal/diff/sidebar;
  New Terminal is a normal button. The sidebar magnifier and the two bottom-left
  icons are decorative `Icon`s with no click handler, not buttons.

## Quality gates (all exit 0)

| Command | Result |
|---|---|
| `cargo fmt --check` | clean |
| `cargo check -p orion-core` | clean |
| `cargo check -p orion-ui` | clean |
| `cargo clippy -p orion-core -- -D warnings` | clean |
| `cargo clippy -p orion-ui -- -D warnings` | clean |
| `cargo test -p orion-ui --lib` | 374 passed, 0 failed (baseline 371) |
| `cargo test -p orion-ui` (full, isolated) | all suites green |
| `cargo test -p orion-core -- --skip <2 agent-CLI smoke>` | 303 unit + integration suites green |
| `cargo test -p orion-core --test shell_first_spawn` | 4 passed |

Two pre-existing failures remain in `crates/orion-core/tests/agent_smoke.rs`
(`claude_draws_a_grid_and_its_tree_dies_with_the_pane`,
`the_http_route_starts_an_agent_and_reads_its_screen`): they depend on real agent CLIs
and also failed before this work. Not touched.

A full parallel `cargo test -p orion-ui` run hit host memory pressure and a transient
200 ms timing threshold in the *control* arm of `terminal_startup.rs` (378 ms). Run
in isolation with the GUI app closed: `cargo test -p orion-ui --test terminal_startup`
passed 2/2; the agent arm showed tab-selected -> first content frame in 9.3 ms.

## T6 checklist

| # | Item | Status | Evidence |
|---|---|---|---|
| 1 | Workspace creates default shell without agent prompt | PASS with caveat (see below) | `shell_first_spawn.rs` proves the agent runs in a shell PTY root; New Terminal -> shell picker -> shell pane recorded in GUI capture |
| 2 | Agent runs inside shell; quit returns to shell prompt | PASS | `an_inner_command_exit_returns_to_the_shell_prompt_in_the_same_pane` (`a3c2ef4`); `shell_smoke` green |
| 3 | Clicking running agent focuses pane, no "That pane is not open" error | PASS | GUI clicks on agent rows; no error banner; core `bootstrap` focus tests green |
| 4 | Clicking workspace tile selects and opens it | PASS | GUI click on workspace tile; `one_workspace_per_folder` green |
| 5 | All sidebar buttons respond to clicks | PASS | GUI: New Workspace modal, Settings modal, New Terminal shell picker, panel toggles; `sidebar` tests green |
| 6 | Header leaves "Connecting…" and shows "Live" | PASS | GUI cold-start capture shows "Live"; `shell_model` tests green |
| 7 | Terminal tab shows content quickly | PASS | GUI terminal-lens capture; `terminal_startup` isolated run 9.3 ms agent arm |

GUI captures were taken from the real `orion-ui` window on the interactive desktop
(1920x1080) and inspected visually. They include cold start, terminal lens, agent
row clicks, New Workspace modal, Settings modal, New Terminal picker and spawned
shell pane. Screenshots and the action log were kept outside the repository in the
run temp directory and removed after verification.

## T6.1 interpretation (agreed with the issue author)

Read literally, "creating a workspace spawns a default shell without asking for an
agent" is not what the code does: cold start selects Claude (`app.rs`), the
create-workspace route requires an agent and rejects a shell, and AGENTS.md forbids
auto-spawning generic shells for agent panes. The item is recorded as satisfied in
the project's shell-first sense: every agent pane's PTY root is a real shell, the
agent runs as an inner command, and an explicit user-invoked shell pane is available
through New Terminal. The literal wording is preserved here so it is not lost.
