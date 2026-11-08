# Agent matrix

Orion drives agent CLIs discovered on `PATH`. Every pane is a dedicated agent
instance; Orion has no generic shell tabs.

| Agent | Wired | Chat lens | Approvals | Tasks | Notes |
|---|---|---|---|---|---|
| Claude | yes | yes | yes | subagents | permission dialogs + questions |
| Codex | yes | yes | yes | — | queued-question count |
| OpenCode | yes | yes | — | — | 1.x + 2.x stores |
| Omo | yes | yes | — | yes | task DAG |
| Omp | yes | yes | — | — | senpi family |
| Pi | yes | yes | — | — | `/tree` abandoned paths |
| Hermes | yes | yes | — | — | `state.db`, status inferred |
| Grok | no | — | — | — | deferred (M5) |
| Cursor | no | — | — | — | deferred (M5) |
| Agy | no | — | — | — | deferred (M5) |
| Devin | no | — | — | — | deferred (M5) |

Agents marked "deferred" are not yet wired; their panes still run the CLI and
show the live terminal, but the structured chat lens and status inference are
not available for them yet.
