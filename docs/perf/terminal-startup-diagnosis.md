# Terminal startup — diagnosis

What the timeline in `docs/perf/terminal-startup-before.md` says about the slow
terminal startup, and which of the issue's hypotheses the numbers confirm or
rule out.

The short version: **the terminal was never the slow part.** Switching to the
Terminal tab and painting its first frame with content took **6–9 ms**. The
slow part was (1) a `git` call the shell made on the UI thread at startup, for
a lens nobody was looking at, and (2) the agent CLI's own cold start, which
Orion cannot fix and can only make visible.

## The gaps, largest first

### 1. The changed-file list: 651 ms (median), on the UI thread, for a hidden lens

`ChangesView::attach` and `FileTree::attach` both read their data as soon as
a pane was attached. Attaching happened during the shell's startup, while the
reader was on the chat lens. The changed-file list is a `git` call: measured
at **601–1,863 ms** across three runs, it was the largest single cost in the
startup and it was spent on a lens that was not on screen.

**App overhead.** Fixed by reading the diff when the Changes lens comes back
(`set_visible`), not when a pane is attached.

### 2. The agent CLI's own startup: ~7.6 s

The control shell was readable in **236 ms**; `claude` was readable in
**7,820 ms**. The difference is the agent CLI's own cold start: `claude` is an
npm shim, so the pane's program is `node.exe`, and node has to start, parse
the bundle, and paint a TUI before it says anything.

**Child startup.** Orion cannot make this faster; it can only make it legible.
The terminal now shows `Starting Claude Code… (3.4s)` in the grid until the
first real output arrives.

Note the split inside that 7.8 s: the child's *first byte* arrives at
**82 ms** (a cursor or a spinner), and the first *readable* content at
**7.8 s**. A shell that only counted bytes would have called this fast.

### 3. The PTY opening and the child starting: 23–35 ms

`pty_spawn_requested` → `pty_spawn_returned` costs **23–35 ms**, on both runs.
That is ConPTY: the pseudoconsole and its host process are created before the
child exists.

**App overhead**, but it happens in the daemon, not on the UI thread, and it
is the floor for any PTY on Windows.

### 4. The terminal tab switch: 6–9 ms

`terminal_tab_selected` → `first_nonempty_frame_painted` is **6.2 ms**
(control, before) / **8.4 ms** (control, after) / **7.0 ms** (agent,
before). The bar in the issue is 200 ms.

**Not the problem**, and the measurement says why: the terminal view is built
when the app starts, not when the tab is selected; it is never destroyed on a
tab switch; and the grid is fed in the background while the chat lens is on
screen. Switching to the Terminal tab shows what was already there.

### 5. The workspace creation: 46 ms

`POST /api/workspace/create` opens the PTY and starts the agent: **46 ms**
(median), which is the same ConPTY cost as gap 3, seen from the other side.
It happens in the daemon; the shell waits for the answer.

## The hypotheses, one by one

| # | Hypothesis | Verdict | Evidence |
|---|---|---|---|
| 1 | **Late spawn**: the PTY is only spawned when the Terminal tab is first opened | **ruled out** | `pty_spawn_requested` comes 0.2–0.3 ms after the workspace open, long before `terminal_tab_selected` (at 228–8,501 ms). The pane is spawned when the workspace is created. |
| 2 | **View recreated on tab switch**: the terminal view is destroyed and rebuilt | **ruled out** | `terminal_view_created` appears once, at 32 ms, and the entity lives in the app for its whole life. A tab switch changes which lens is in the element tree; it does not rebuild the view. |
| 3 | **Startup gated on the handshake**: terminal creation waits for "Connecting…" | **ruled out** | `backend_connected` is recorded at 0.0 ms in the harness and the pane spawn follows immediately. The handshake gates the app's *first paint*, not the terminal. |
| 4 | **No output buffering while detached**: bytes produced before the view attaches are dropped | **ruled out** | The pane replays its screen as ANSI on attach (`Pane::attach`), and the grid is fed continuously by the pump whether or not the terminal is on screen. The harness's "tab selected → first frame with content" number (6–9 ms) is the proof: the content was already there. |
| 5 | **Resize round trip**: the spawn waits for layout to report a size | **ruled out** | The pane is created with a default 120×32 and resized afterwards. The timeline shows `pty_spawn_requested` with `120x32` in the detail column. |
| 6 | **Size mismatch**: the PTY size and the grid disagree | **ruled out** | `first_resize_applied` records both sizes (`pty 120x32, grid 120x32`), and the grid is resized in the same step as the PTY (the 50 ms debounce is in the UI, the grid follows the box at once). |
| 7 | **UI-thread work**: PTY reading, VT parsing or process spawn on the UI thread | **ruled out, with one exception** | The PTY reader is a dedicated thread and the parser runs in the pump's task. The exception was the `git` call and the filesystem listing, which ran on the UI thread during attach — fixed by moving them behind the lens. |
| 8 | **Over-eager repaint**: one repaint per chunk instead of per frame | **confirmed** | The pump notified whenever it had fed the grid, so a flood asked for a frame per chunk. Fixed: the pump waits out the frame budget, and a new test fails if the repaint rate exceeds the frame rate. |
| 9 | **Blocking init**: agent detection, PATH scans or directory listing before the terminal can start | **partly confirmed** | The PATH scan happens when the agent is resolved, before the PTY opens — 0.2 ms of the measured 23 ms, and it is in the daemon. The directory listing and the `git` call ran on the UI thread at attach: that is gap 1 above. The `/api/agents` discovery (a PATH scan plus a `--version` probe per agent) runs when the New Workspace picker opens, not at startup. |
| 10 | **Event-driven rendering gap**: the view only redraws on user input | **ruled out** | The pump notifies on every revision change, and the repaint rate is now bounded by the frame budget rather than by the chunk count. |

## What the numbers say is left

| Cost | Whose | Fixable? |
|---|---|---|
| 651 ms — the changed-file list on the UI thread | app overhead | **fixed**: the lens reads it when it comes on screen |
| ~7.6 s — the agent CLI's own cold start | child startup | not fixable; **masked** by the `Starting … (N.Ns)` placeholder |
| 23–35 ms — opening the PTY and starting the child | app overhead (ConPTY) | not worth it: it is the floor for a Windows pseudoconsole |
| 46 ms — the workspace creation round trip | app overhead | the daemon does the work; the shell waits for the answer |
| 6–9 ms — the terminal tab switch | app overhead | already fast |

## Assumptions

- **The in-process engine stands in for the daemon.** The harness uses
  `Engine` directly, so the measured costs are the server-side ones. The
  daemon path adds one local socket round trip per call — sub-millisecond each
  — and the transport's own latency, which is not measured here.
- **The control shell is `cmd.exe`**, launched through the engine's
  `spawn_pane` seam, which the codebase documents as the seam "tests may
  supply a stand-in" through. It runs the same pane, PTY, attach, pump and
  render path as an agent CLI; only the launched program differs. Orion's
  agent-only rule is untouched: no shell is offered to the reader.
- **The agent CLI measured is `claude`**, the npm shim, so the pane's program
  is `node.exe`. A natively compiled agent would start faster; the split
  between "the first byte" and "the first readable content" would narrow.
- **The numbers were taken on a loaded host.** A runaway `find` from an
  earlier agent session was holding a core during the first set; it was
  killed and the set was re-taken. The spread is still 10x between runs.
