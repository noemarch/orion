# Terminal startup — before

The numbers behind issue #20 ("Bug: Slow terminal startup in Orion"), measured
before the fixes. What was measured, how, and what the numbers said.

Every number here comes from `crates/orion-ui/tests/terminal_startup.rs`, the
measurement harness added with this work:

```bash
cargo test -j 2 -p orion-ui --test terminal_startup -- --nocapture --test-threads=1
```

## What the harness measures

Two runs through **the same path the shell uses**: the engine's pane, a real
ConPTY, the `gpu-terminal` attachment, the PTY pump, and the terminal view's
own render.

- The **control** run starts a plain shell (`cmd.exe`). Its own startup is
  small, so what is left in its numbers is Orion's.
- The **agent** run starts an agent CLI (`claude`). The difference between the
  two runs is the agent CLI's own startup cost — the part Orion can only
  mask, not fix.

The terminal is not painted during the run: in the app the terminal element is
not in the tree while another lens is on screen, so the harness mounts it only
when the reader selects the Terminal tab. That makes the "tab selected → first
frame with content" number the app's real tab-switch cost.

The second test times the calls the shell makes **on the UI thread before its
first paint**.

## The machine

- OS: Microsoft Windows 11 Pro, 10.0.26200 (x64)
- CPU: Intel(R) Core(TM) i7-7500U CPU @ 2.70GHz (4 cores), Power Saver
- PTY: ConPTY (via `portable-pty`)
- Rust: 1.99.1

**Host conditions.** A runaway `find / -maxdepth 6 -iname gpui-0.2* -type d`
from an earlier agent session was holding a core (4,679 s of CPU) while these
numbers were taken; it was killed and the set was re-taken. The spread is
still wide: the same binary moves by 10x between a quiet and a loaded host.
Treat a difference under 10 ms as noise, and read the medians, not the
extremes. This is the same caveat the terminal-lag baseline records.

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

- pty reads sampled: 2 (cap 50), bytes read in total: 207
- frames painted: 2

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

- pty reads sampled: 6 (cap 50), bytes read in total: 509
- frames painted: 3

## Three runs, medians

| Metric | Run 1 | Run 2 | Run 3 | Median |
|---|---:|---:|---:|---:|
| **Control** | | | | |
| PTY open + child start | 29.4 ms | 53.9 ms | 35.0 ms | **35.0 ms** |
| child's own startup (spawn returned → first byte) | 116.1 ms | 154.6 ms | 22.3 ms | **116.1 ms** |
| terminal tab selected → first frame with content | 7.5 ms | 14.2 ms | 8.4 ms | **8.4 ms** |
| workspace open → first frame with content | 236.2 ms | 541.7 ms | 109.4 ms | **236.2 ms** |
| **Agent (claude)** | | | | |
| PTY open + child start | 24.8 ms | 23.2 ms | 21.5 ms | **23.2 ms** |
| child's first byte | 86.7 ms | 81.8 ms | 54.7 ms | **81.8 ms** |
| terminal tab selected → first frame with content | 9.0 ms | 17.6 ms | 49.1 ms | **17.6 ms** |
| workspace open → readable content | 8,510 ms | 7,820 ms | 6,584 ms | **7,820 ms** |

Read down the two columns: the control shell is readable in **236 ms**; the
agent CLI is readable in **7.8 s**. The gap — about **7.6 s** — is the agent
CLI's own startup. Orion's own share of the control run is **35 ms** for the
PTY and **8.4 ms** for the tab switch.

## The startup path, on the UI thread

The shell's own calls before the window paints anything. Measured in-process,
so these are the *server-side* costs; the daemon path adds one local socket
round trip per call.

| Call | Run 1 | Run 2 | Run 3 | Median |
|---|---:|---:|---:|---:|
| restore the saved session | 0.1 ms | 0.1 ms | 0.1 ms | **0.1 ms** |
| read the session snapshot | 0.1 ms | 0.1 ms | 0.1 ms | **0.1 ms** |
| create the workspace (spawn the agent) | 51.4 ms | 45.6 ms | 46.3 ms | **46.3 ms** |
| look the pane up | 0.0 ms | 0.0 ms | 0.0 ms | **0.0 ms** |
| attach the terminal | 0.7 ms | 0.7 ms | 0.7 ms | **0.7 ms** |
| read the conversation (the chat lens) | 0.5 ms | 0.5 ms | 0.5 ms | **0.5 ms** |
| list the directories (the changes lens) | 0.4 ms | 0.2 ms | 0.2 ms | **0.2 ms** |
| **list the changed files (the changes lens)** | 601.5 ms | 1,863.4 ms | 651.3 ms | **651.3 ms** |
| **total** | 654.8 ms | 1,910.6 ms | 699.4 ms | **699.4 ms** |

The last two rows are the finding: **the changed-file list is a `git` call,
and it ran while the reader was on the chat lens**, on a lens nobody was
looking at. It was the largest single cost in the whole startup.

## Reproducing

```bash
# the timelines and the startup path, with the numbers printed
cargo test -j 2 -p orion-ui --test terminal_startup -- --nocapture --test-threads=1
```

The harness forces the trace on, so no environment variable is needed. The
same timeline is available in the running app with `ORION_TRACE=1`, which
prints the table when the app closes.
