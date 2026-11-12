# Terminal Runtime

Decision: the terminals live in a separate runtime process that owns them, and the shell reaches them over a local socket through the same traits the in-process engine answers.

Consequences:

- The runtime daemon (`orion daemon`) owns every terminal's process, grid, scrollback and saved screen. The app owns the view and nothing else, so closing or crashing the app does not kill the agents it started.
- The socket is local only: a named pipe with a current-user-only descriptor on Windows, a unix socket in a user-only directory elsewhere. The runtime is never exposed over TCP, and the state directory is readable only by its owner.
- One runtime serves one session. A second runtime for the same session refuses to start; `ORION_RUNTIME_SESSION` names an isolated runtime, which is how the tests keep out of each other's way.
- The shell asks the runtime through `Runtime` and `PaneHandle`. `Engine` and `Pane` answer in-process, `RuntimeClient` and `RemotePane` answer over the socket, and the views are the same either way.
- The wire is length-prefixed JSON frames over one connection that carries both requests and pushed frames. A client that speaks another protocol version is refused, and the refusal names both versions.
- A client that attaches is sent the screen as ANSI before the live output, so its grid matches the daemon's exactly, colours included. A raw byte ring would not rebuild a full-screen program that painted more than the ring holds.
- A terminal's status is only what the program reported, through an OSC sequence or `orion-rt report`. A terminal that has not reported has no status, and the shell draws no badge for it.
- Every terminal is started with `RUNTIME_TERMINAL_ID`, `RUNTIME_SOCKET` and `RUNTIME_CLI` in its environment, so a terminal knows its parent and can reach the CLI.
- The runtime enforces the limits itself — chain depth, children per parent, terminals per session, and the rule that a child runs inside its parent's directory — so a client cannot ask for more than the session allows.
- The agent-facing surface is the `orion-rt` command, and the guide it prints is `docs/agent-runtime.md`, embedded by the CLI so the two cannot drift apart.
- The workspace panes stay agent-only. The runtime's registry is a lower layer, and a terminal there may run any program.
- The runtime crate is part of `orion-core` (`crates/orion-core/src/runtime/`), not a new set of crates: `orion-core` already held the PTY and the emulator, and a parallel crate would have produced two terminal implementations.
- The emulator stays `vt100`. It is already integrated, it carries truecolor, and Zed's own terminal crates are GPL-3.0, so nothing was copied from them.
