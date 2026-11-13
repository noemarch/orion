# Running Other Agents

The page below is what `orion-rt --skill` prints. It is the same text: the CLI embeds this
file, so the two cannot drift apart.

orion-rt: run other agents from this one

You are an agent working in an Orion terminal. `orion-rt` is how you start
another agent, hand it work, wait for it, read what it said, and clean up. It
talks to the runtime daemon that owns the terminals, so it works from any
terminal on this machine in the same runtime session.

Every command prints plain text. Exit code 0 means it did what you asked; 1
means it did not, and the reason is on stderr.

THE VERBS

  spawn-agent   start a terminal and give back its id
  list          show the terminals
  send          write text to a terminal
  wait          wait for a report or an exit
  read          print what a terminal printed
  kill          end a terminal and its children
  report        record what a program says about itself

`run` is another name for `spawn-agent`, and `ls` for `list`.

START ANOTHER AGENT

  orion-rt spawn-agent --agent claude --name reviewer
  orion-rt spawn-agent --agent codex --name fixer --cwd ../other-worktree
  orion-rt spawn-agent --name probe -- cmd.exe /C "echo hello"

`spawn-agent` prints the new terminal's id and then its address,
`<socket>:<id>`, and returns at once; the program keeps running. `--agent`
accepts one kind: claude, codex, agy, opencode, cursor, grok, omp, omo, pi,
hermes, devin. Without `--agent`, put the program and its arguments after `--`,
and the program gets exactly those arguments. The new terminal is a child of
the terminal you are running in, when the runtime knows which one that is, and
a child must stay inside its parent's directory. `--bg` prints only the id.

A bash shell rewrites a single-letter switch such as `/C` into a drive path
before the program sees it, and the CLI puts that switch back. In a bash shell,
write `//C` to keep it as it is.

FIND A TERMINAL AGAIN

  orion-rt list

One line per terminal, tab-separated: id, name, parent, status, cwd. An empty
runtime prints `no terminals`. A terminal's address is always `<socket>:<id>`,
and the id alone is enough for every other command.

SEND WORK

  orion-rt send <id> "run the test suite and report what failed"

`send` writes the text to the terminal's stdin and presses Enter. Words after
the id are joined with spaces.

WAIT FOR IT

  orion-rt wait <id> --timeout 600

Returns when the program reports a status or ends, or when the timeout runs out
(default 30 seconds). On a timeout it prints the terminal's address and exits 0:
the program is still running, so do other work and come back. `done` means it
finished its work, `blocked` means it needs you, `error` means it failed.

READ WHAT IT SAID

  orion-rt read <id> --lines 80

Prints the last N lines of the terminal's output (default 40). Read after
`wait` returns, or after a timeout to see how far the program got.

  orion-rt attach <id>

Streams the terminal's new output until it ends. Press Ctrl+C to stop watching;
the terminal keeps running. `attach` prints what arrives while you watch, so use
`read` for what came before.

REPORT BACK

  orion-rt report <id> working
  orion-rt report <id> done
  orion-rt report <id> blocked
  orion-rt report <id> error

Records what a program says about itself. A terminal has no status until
something reports one: no report means no status. The agent that started a
terminal reads this status, so report `working` when you start and `done` when
the work is finished.

CLEAN UP

  orion-rt kill <id>

Ends the terminal and its children. A terminal whose program ends on its own
stays in the table until it is killed; `list` shows its exit code in place of a
status.

WHERE YOU ARE

The daemon gives every terminal three variables:

  RUNTIME_TERMINAL_ID   the id of the terminal you are running in
  RUNTIME_SOCKET        the socket the runtime listens on
  RUNTIME_CLI           the orion-rt binary to call

A terminal started from inside another is its child, and `RUNTIME_TERMINAL_ID`
tells the runtime which parent to record. A child ends with its parent unless
it was asked to outlive it.

A WHOLE JOB

  orion-rt spawn-agent --agent claude --name reviewer
  1a2b3c4d
  address /run/user/1000/orion/runtime/default.sock:1a2b3c4d
  orion-rt send 1a2b3c4d "review src/lib.rs and list what is wrong"
  orion-rt wait 1a2b3c4d --timeout 600
  orion-rt read 1a2b3c4d --lines 80
  orion-rt kill 1a2b3c4d

The runtime session is `default` unless ORION_RUNTIME_SESSION names another.
`orion daemon` starts the daemon when none is running.

`orion-rt --skill` prints this page.
