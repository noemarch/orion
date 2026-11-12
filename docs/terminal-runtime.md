# Terminal Runtime Architecture

> The full decision record is in [`docs/adr/terminal-runtime.md`](adr/terminal-runtime.md).
> The agent-facing guide is in [`docs/agent-runtime.md`](agent-runtime.md); the CLI prints that
> same text with `orion-rt --skill`.

## The problem

A terminal is a running program. A program outlives the window that shows it. If the app owns the
process, then closing the app kills the agent in it, and a crash kills it without a goodbye. That
is the opposite of what an orchestrator needs: the agent you started five minutes ago should still
be working after you close the window, and it should still be working when you come back.

So the terminals move out of the app and into a runtime that owns them. The app becomes a client.

## The shape

```
GPUI app (client)                         runtime daemon (separate process)
├─ chat view  ── orchestrator tools ──►   ├─ terminal registry (ids, names, parent/child, status)
├─ terminal view (renders the grid) ◄───  ├─ API server (local socket / named pipe)
└─ sidebar (terminal tree + status)        └─ per terminal: PTY + emulator + scrollback + saved screen
```

Three things follow from this, and they are the whole design:

1. **The daemon owns the processes.** The app owns nothing but the view. Closing the app closes the
   window, not the work.
2. **The socket is local.** The daemon has no permission model: anything that can reach the socket
   can run commands as the user. It is a named pipe on Windows and a unix socket elsewhere, never
   TCP, and only the current user can reach it.
3. **There is one API.** The app, the CLI and the orchestrator's tools are all clients of the same
   daemon. No second code path, no "the app version" of a rule.

## The pieces

### The runtime crate

Everything lives in `crates/orion-core/src/runtime/`:

| module | what it owns |
|---|---|
| `traits.rs` | `Runtime` and `PaneHandle` — the two questions the shell asks |
| `framing.rs` | the wire: a length prefix and a JSON body |
| `client.rs` | `RuntimeClient` — one connection, requests and pushed frames |
| `transport.rs` | the local socket: a named pipe or a unix socket |
| `lock.rs` | one runtime per session |
| `terminal.rs` | `Terminal` — a program, its grid, and what it reported |
| `registry.rs` | `Registry` — the table of terminals, their families and their limits |
| `limits.rs` | `RuntimeLimits` — depth, children, terminals, and where a child may run |
| `daemon.rs` | the server: it answers the verbs and owns the registry |

There is no second crate for this. `orion-core` already held the PTY, the emulator and the pane;
splitting them into a parallel crate would have produced two terminal implementations and two
answers to every question.

### The wire

A frame is a `u32` little-endian byte length followed by that many bytes of UTF-8 JSON. The length
counts the body only. A frame over the cap is refused rather than allocated, because a corrupt
length must not be able to ask for the machine's memory.

One connection carries both kinds of frame. A request carries an `id`; its answer carries the same
`id`. A frame with no `id` was pushed by the daemon and belongs to everybody watching.

The first thing a client sends is `health`. The daemon answers with its version and its protocol
version; a client that speaks another protocol is refused, and the refusal names both versions.

The daemon's verb table is the contract, and it lives in the module docs of `daemon.rs` and
`client.rs` so the two can be read side by side.

### The attach strategy: a snapshot

When a client attaches, the daemon replays the screen and then streams what comes next.

Replaying the raw byte history instead would be simpler, but a bounded history cannot rebuild a
full-screen program that painted more than the ring holds. The daemon therefore keeps the grid,
and sends the screen as ANSI, which a client feeds straight back into its own emulator. The
client's grid is then exactly the daemon's, colours included.

### Status is reported, never guessed

A program says what it is doing. It does so through an OSC escape sequence the daemon reads out of
the byte stream, or through `orion-rt report <state>`. The daemon stores what was reported.

A terminal that has not reported has **no status**. The sidebar draws no badge for it, because the
shell cannot know and will not pretend. This is deliberately different from the agent panes, whose
status is read off the screen; the two answer different questions.

### Parents and children

Every terminal the daemon starts gets `RUNTIME_TERMINAL_ID`, `RUNTIME_SOCKET` and `RUNTIME_CLI` in
its environment. A terminal started from inside another one therefore knows who started it, and
the daemon records the link.

A child ends with its parent, unless it was started with `--outlive-parent`. The parent is told
when a child finishes, fails or reports that it is blocked.

### The limits live in the daemon

A client that asks for too much is refused by the daemon, not by the client, because a client can
be anything:

- a chain may be at most `max_depth` deep (4);
- one terminal may start at most `max_children` others (16);
- one session may hold at most `max_terminals` terminals (64);
- a child runs inside its parent's directory. `..` and symlinks cannot walk it out.

### The shell asks through traits

The shell holds `Arc<dyn Runtime>` and `Arc<dyn PaneHandle>`. `Engine` and `Pane` answer those
questions in-process; `RuntimeClient` and `RemotePane` answer them over the socket. The views do
not change when the answer comes from somewhere else, and the tests keep using the in-process
answer.

## The agent-facing surface

There is no LLM inside Orion. The orchestrator is the agent CLI running in the pane, and the way
it reaches another agent is the `orion-rt` command:

```
orion-rt run --agent claude --name reviewer
orion-rt send <id> "review src/lib.rs"
orion-rt wait <id> --timeout 600
orion-rt read <id> --lines 80
orion-rt report <id> done
orion-rt kill <id>
```

`wait` returns the terminal's address when the timeout runs out rather than blocking forever: the
program is still working, so the caller can do something else and come back.

The full guide is [`docs/agent-runtime.md`](agent-runtime.md). The CLI embeds that file, so the
document an agent reads and the document in the repository are the same text.

## What this does not change

The workspace panes are still agent-only: creating one still requires an agent CLI found on
`PATH`. The runtime registry is a lower layer — a terminal there may run any program, because that
is what an orchestrator needs when it runs a test suite or a linter. The boundary is enforced in
code: `PaneConfig` requires an `AgentKind`, and the runtime's spawn path is a different type.

## What is deliberately not here

- **Holder processes.** A daemon restart currently ends the terminals it held. Phase 6 of the
  issue would give each terminal its own holder so a daemon could be replaced under it.
- **A second code path.** There is one daemon, one API and one set of verbs.
