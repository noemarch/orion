# Orion Domain Glossary

This document establishes the canonical domain vocabulary for the Orion project. Use these terms precisely in code, architecture decisions, tests, and documentation.

---

### Core Domain Entities

#### Engine
The central native Rust orchestrator (`orion-core::engine::Engine`) that coordinates workspaces, tabs, panes, agent process supervision, and client transports (the GPUI shell's frames and the Axum WebSocket/HTTP gateway). Every client request flows through the engine seam.

#### Workspace
A top-level project container bound to a root working directory (`cwd`). A workspace contains one or more tabs, has an active tab, and tracks its own Git branch and worktree metadata.

#### Tab
A tab within a workspace that groups one or more agent panes in a specific layout (single, horizontal split, vertical split).

#### Pane
A dedicated, interactive AI agent instance. Orion strictly enforces an **agent-only invariant**: every pane owns a supervised process tree running an identified agent CLI (`claude`, `codex`, `opencode`, etc.) connected to a native PTY. Orion never runs generic shells (`bash`, `powershell`).
_Avoid_: session row

#### Agent
An identified AI coding assistant CLI discovered on `PATH`. Orion manages credentials and process lifetimes via bring-your-own-subscription, driving the agent's CLI interface directly.

#### Lens
The presentation mode for inspecting an agent pane:
- **Chat Lens:** Structured conversational turns, tool call cards, diff viewers, and prompt approvals.
- **Terminal Lens:** The raw, live PTY terminal grid, drawn by the GPUI shell from the daemon's screen state, with zero state loss.

#### Session
The complete in-memory and persisted runtime state of open workspaces, tabs, panes, focused pane, split ratios, and zoom states.

#### Transcript
The structured conversational history of an agent session (turns, user messages, assistant thinking, tool executions, approval requests). Introspected in read-only mode from agent CLI storage (e.g. SQLite databases, JSONL logs) or captured from PTY streams.

#### Composer
The Chat Lens's message box: the only way the reader drives an agent conversation without the Terminal Lens. It holds the reader's draft, delivers it to the agent, and stops the agent's turn. Its states are **idle**, **pending** (a message is on its way), and **failed** (the last action did not go through; a failed send keeps its words and offers a retry).

#### Prompt Card
The blocking card the Chat Lens shows while an agent's menu is on screen. It carries the agent's question and its numbered options, and it is answered from the keyboard (the option's number) or the pointer. Answering it resumes the agent and removes the card.

#### Pane Seam
The one boundary the Chat Lens uses to reach an agent: deliver a message, stop a turn, answer a menu, and read the agent's state. A live Pane answers it in-process; a test answers it with a recording double, so the lens's behaviour is observable without an agent process.

#### Connection Registry
The subsystem managing connected remote or desktop clients, their WebSocket channels, and fan-out of PTY output and state change events.

#### Frame Dispatch
The message handler responsible for parsing, validating, and routing push frames sent by connected clients over WebSocket/IPC (`attach`, `input`, `submit`, `ping`).

#### Route Table
The HTTP/REST dispatch table mapping inbound `/api/*` requests to their corresponding engine operations.

---

### Modal Presentation

#### Modal Shell
The shared, stateless frame for a custom overlay: scrim, centered panel, header title, one close control, and focus trap. It defines presentation only; modal views own their state.
_Avoid_: Dialog primitive, settings page

#### Close Control
The single visible button in a modal header that dismisses the modal. It is distinct from Escape, which is keyboard dismissal, and from a primary action that commits a result.
_Avoid_: Cancel button, Done button, dismiss button

#### Keyboard Dismissal
Escape closing the topmost modal without acting as a visible control. It complements the close control rather than duplicating it.
_Avoid_: Escape action, close shortcut

#### Opener Focus
The view that had keyboard focus when a modal opened and receives focus back when a dismissal closes it. A successful commit clears it so the newly created pane can take focus.
_Avoid_: saved focus, previous focus handle

#### Confirmation Dialog
A gpui-kit alert dialog that asks whether to close a running agent or session. It keeps kit-owned focus behavior and uses a specific consequence statement and "Close" outcome verb.
_Avoid_: custom modal, alert prompt
