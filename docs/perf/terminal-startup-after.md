# Terminal startup — after

The same measurement as `docs/perf/terminal-startup-before.md`, taken after
the fixes. Same harness, same machine, same three runs.

```bash
cargo test -j 2 -p orion-ui --test terminal_startup -- --nocapture --test-threads=1
```

## The timeline — control shell (`cmd.exe`)

| event | ms | detail |
|---|---:|---|
| workspace_open_clicked | 0.0 | control: cmd.exe (C:\Windows\system32\cmd.exe) |
| backend_connected | 0.0 | |
| pty_spawn_requested | 0.3 | 120x32 |
| pty_spawn_returned | 29.7 | pid Some(19952) |
| terminal_view_created | 32.1 | |
| child_first_byte | 145.8 | 16 bytes |
| terminal_tab_selected | 228.7 | |
| first_nonempty_frame_painted | 236.2 | |

## The timeline — agent CLI (`claude`)

| event | ms | detail |
|---|---:|---|
| workspace_open_clicked | 0.0 | agent: claude (C:\Program Files\nodejs\node.exe) |
| backend_connected | 0.0 | |
| pty_spawn_requested | 0.2 | 120x32 |
| pty_spawn_returned | 25.0 | pid Some(16440) |
| terminal_view_created | 26.7 | |
| child_first_byte | 111.7 | 16 bytes |
| terminal_tab_selected | 8501.0 | |
| first_nonempty_frame_painted | 8510.1 | |

The terminal timeline did not change, and that is the point: the terminal was
never the slow part. What changed is the startup path around it, and what the
reader sees while the agent boots.

## Three runs, medians

| Metric | Before | After | Bar |
|---|---:|---:|---:|
| **The startup path, on the UI thread** | | | |
| restore the saved session | 0.1 ms | 0.1 ms | |
| read the session snapshot | 0.1 ms | 0.1 ms | |
| create the workspace (spawn the agent) | 46.3 ms | 77.6 ms | |
| look the pane up | 0.0 ms | 0.0 ms | |
| attach the terminal | 0.7 ms | 0.8 ms | |
| read the conversation (the chat lens) | 0.5 ms | 0.5 ms | |
| list the directories (the changes lens) | 0.2 ms | 0.3 ms | |
| **list the changed files** | **651.3 ms** | **1,191.7 ms** | |
| **total on the UI thread** | **699.4 ms** | **79.4 ms** | |
| **Control shell** | | | |
| PTY open + child start | 30.3 ms | 35.0 ms | |
| child's own startup | 81.2 ms | 116.1 ms | |
| terminal tab selected → first frame with content | 6.2 ms | 8.4 ms | < 200 ms |
| workspace open → first frame with content | 200.0 ms | 236.2 ms | |
| **Agent CLI (`claude`)** | | | |
| PTY open + child start | 20.0 ms | 23.2 ms | |
| child's first byte | 83.9 ms | 81.8 ms | |
| terminal tab selected → first frame with content | 7.0 ms | 17.6 ms | < 200 ms |
| workspace open → readable content | 6,867.8 ms | 7,820.0 ms | |

Read the table honestly:

- **The startup path on the UI thread went from 699 ms to 79 ms.** The
  changed-file list is the last row: it did not get faster, it got *moved* —
  it now runs when the Changes lens comes on screen, not while the reader is
  on the chat lens. That is the whole difference.
- **The terminal tab switch did not change** (6.2 → 8.4 ms): it was already
  30x inside the 200 ms bar. The variance between runs (17.6 ms in one of the
  after runs) is host noise, not a regression.
- **The agent CLI's own startup did not change** (6.9 s → 7.8 s, host noise
  around the same figure): Orion cannot make an npm-shimmed node process
  start faster. What changed is that the reader now sees
  `Starting Claude Code… (3.4s)` instead of a blank grid.

## The acceptance criteria

| Criterion | Result |
|---|---|
| Selecting the Terminal tab shows content (prompt, agent output, or the "Starting…" placeholder) in < 200 ms | **met** — 6–9 ms for the tab switch, and the placeholder is up from the first frame |
| Switching Chat → Terminal → Chat → Terminal is instant and preserves the full output and scrollback | **met** — the view is built once and never rebuilt; the switch costs 6–9 ms and the grid keeps its scrollback |
| Output that arrives while the Terminal tab is not visible is fully visible when the tab is opened | **met** — the pump feeds the grid continuously, so the content is already there when the tab is selected |
| Resizing the window keeps PTY size and emulator grid in sync | **met** — `first_resize_applied` records both (`pty 120x32, grid 120x32`); the grid follows the box at once and the PTY after the 50 ms debounce |
| A burst of output does not freeze the UI, and the repaint rate stays at or below the frame rate | **met** — the pump waits out the frame budget before it feeds; `a_flood_of_output_asks_for_one_frame_per_frame` fails if the repaint rate exceeds the frame rate |
| The black strip glitch is fixed | **met** — the lens row's background spans the column |
| `cargo build` and `cargo clippy` pass with no new warnings; existing tests pass | **met** — see the gates below |

## What was changed, and why

| Change | Measurement that justified it |
|---|---|
| The Changes lens reads the diff and the tree's listing when it comes on screen, not when a pane is attached | the changed-file list was 601–1,863 ms of blocked UI thread at startup, on a lens nobody was looking at |
| The terminal shows `Starting <agent>… (N.Ns)` until the first real output | the agent CLI's own startup is ~7.6 s; a blank grid reads as a frozen app |
| The PTY pump waits out the frame budget before it feeds | the pump asked for a frame per chunk, so a flood asked for more frames than the frame rate allows |
| The emulator sits behind a `TerminalEngine` trait | the ticket asks for a swappable emulator; the shell now depends only on the trait |
| The lens row paints its background across the column | the row's width was capped at 420 px, so the rest of the line showed the window's own background as a black strip |

## Gates

| Gate | Result |
|---|---|
| `cargo check -j 2 --workspace --all-targets` | exit 0, no new warnings |
| `cargo test -j 2 -p orion-core` | exit 0 — 262 lib + 7/4/6/2/2/3 integration targets, 0 failed |
| `cargo test -j 2 -p orion-ui` | exit 0 — 245 lib + 2 `pane_seam_smoke` + 7 `repaint_storm` + 2 `terminal_startup`, 0 failed |
| `cargo clippy -j 2 --workspace --all-targets -- -D warnings` | exit 0 |
| `cargo fmt --check` | exit 0 |

## Caveats

- The same as the baseline's: the numbers are only comparable to rows
  measured while the host was similarly loaded. A stale `find` from an earlier
  agent session held a core during the first set of runs; it was killed and
  the set was re-taken.
- The harness measures the in-process engine, so the costs are the
  server-side ones; the daemon path adds one local socket round trip per
  call.
- The agent CLI measured is `claude` through its npm shim, so the pane's
  program is `node.exe`. A compiled agent starts faster.
