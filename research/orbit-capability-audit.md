# Orbit → Orion capability audit

## Destination scope

Match Orbit's feature surface while Orion stays standalone (owns PTYs/processes/transcripts, no herdr). This audit covers: plugins/actions, integrations, remote-machine setup flow, slash commands, plan/background-tasks, file/diff viewer, voice dictation, PWA key bar, push alerts.

## Method

Compared Orbit `shared/protocol.ts`, `server/`, `src/` against Orion `crates/orion-core/src/engine.rs`, `src/shared/protocol.ts`, `src/`. "Wired" = backend route implemented in engine.rs; "frontend-ready" = `src/` UI exists but has no backend route.

## Findings

### 1. Agent introspect (chat lens)

| Agent | Orbit chat lens | Orion protocol kind | Orion introspect reader | Status |
|---|---|---|---|---|
| Claude | native store | Claude | claude.rs | wired |
| Codex | native store | Codex | codex.rs | wired |
| OpenCode | 1.x + 2.x stores | OpenCode | opencode.rs | wired |
| Omo | task DAG | Omo | omo.rs | wired |
| Omp | senpi family | Omp | (none yet) | — |
| Pi | state.db | Pi | pi.rs | wired |
| Hermes | state.db | Hermes | hermes.rs | wired |
| Devin | by name/session | Devin | (none yet) | — |
| Grok | updates.jsonl | Grok | grok.rs | wired |
| Cursor | state.vscdb | Cursor | cursor.rs | wired |
| Antigravity (agy) | native ledger + presence lock | Antigravity | **(none)** | **gap** |

Orion already has `AgentKind.Antigravity` in `crates/orion-core/src/protocol.rs` (research ticket #2 located agy storage at `~/.gemini/antigravity-cli/brain/<session>/.system_generated/logs/transcript_full.jsonl` with presence-lock evidence), but no introspect reader exists and `docs/agent-matrix.md` still marks Agy deferred.

### 2. Plugins / actions (Orbit: `plugin.list` + `plugin.action.list`/`plugin.action.invoke`)

| Capability | Orbit route | Orion backend (engine.rs) | Orion frontend (`src/`) | Status |
|---|---|---|---|---|
| List plugins + actions | `GET /api/plugins/actions` | **missing** | `pluginRun.ts`, `CommandPalette.tsx`, `paletteSearch.ts`, `SettingsDialog.tsx` | **gap: frontend-ready, backend missing** |
| Run one plugin action | `POST /api/plugin/action` | **missing** | `pluginRun.ts` (calls `api.runPluginAction`) | **gap: frontend-ready, backend missing** |
| Poll action status/log | `GET /api/plugin/action?plugin_id=&log_id=` | **missing** | `pluginRun.ts` | **gap: frontend-ready, backend missing** |

Orion's `shared/protocol.ts` already defines `PluginActions`, `PluginAction`, `PluginActionRequest`, `PluginActionResult`, `PluginActionContext` identically to Orbit's. The frontend consumes them via `useMachineApi` → `api.fetchPluginActions` / `runPluginAction` / `fetchPluginActionStatus`. engine.rs has no matching match arms.

### 3. Integrations

| Capability | Orbit route | Orion backend | Status |
|---|---|---|---|
| List integrations | `GET /api/integrations` (herdr's `integration.list`) | `GET /api/integrations` (Orion's `integration.list` via `AgentRegistry::discover_all`) | wired |

Engine.rs line 662 implements `self.integrations()`. This is present, though the semantics differ (herdr integration.list vs Orion's AgentRegistry discovery).

### 4. Remote-machine setup flow

| Capability | Orbit route | Orion backend | Status |
|---|---|---|---|
| List machines | `GET /api/machines` | `GET /api/machines` | wired (local machine only) |
| Start remote setup | `POST /api/machines/setup` | **missing** | **gap** |
| Setup job status | `GET/POST/DELETE /api/machines/setup/:job_id` | **missing** | **gap** |
| Pair device (local) | `POST /api/devices/pair` | `POST /api/devices/pair` | wired |

Orion's `shared/machines.ts` defines `SetupJob`, `SetupRequest`, `SetupAction`, `SetupProgress`, `MachineAction` (connect/update_bridge/setup/bridge_conflict) and `src/lib/machines.ts` imports them, but engine.rs exposes no `/api/machines/setup` route — only `GET /api/machines` returning the local machine.

### 5. Slash commands

| Capability | Orbit route | Orion backend | Orion frontend | Status |
|---|---|---|---|---|
| Agent slash commands | `GET /api/pane/commands?pane_id=` | **missing** | `compose.ts` (`slashCommandReplacement`, `COMMAND_GROUPS` incl. `plugin`), `mentions.ts` | **gap: frontend-ready, backend missing** |

### 6. Plan / background-tasks

| Capability | Orbit route | Orion backend | Status |
|---|---|---|---|
| Plan/task list | `GET /api/pane/plan` | — | — | unchecked (not yet compared) |
| OmO tasks | `GET /api/pane/omo-tasks` | `GET /api/pane/omo-tasks` | wired (engine.rs line 688) | wired |

### 7. File / diff viewer

| Capability | Orbit | Orion | Status |
|---|---|---|---|
| File viewer | native viewer | `FileViewer.tsx`, `RepoDiffViewer.tsx`, `TextFileView.tsx`, `DiffWorkerPool.tsx` | wired |
| Repo diff | `GET /api/workspace/diff/files`, `/diff/file` | same routes | wired |

### 8. Voice dictation

| Capability | Orbit | Orion | Status |
|---|---|---|---|
| Dictation (chat + terminal) | SpeechRecognition/OpenAI key | `shared/voice.ts`, `src/lib/voice.ts`, `VoiceInput.tsx`, settings fields | wired |

### 9. PWA key bar / touch targets

| Capability | Orbit | Orion | Status |
|---|---|---|---|
| Touch key bar | mobile key bar (Esc, Tab, Ctrl, Alt, Shift, Enter, arrows) | `KeyBar.tsx`, `KeyBar.css`, `KeyBarSettings.tsx`, `--touch-target: 44px` | wired |

### 10. Push alerts

| Capability | Orbit | Orion | Status |
|---|---|---|---|
| Web push (RFC 8291) | `GET/POST/DELETE /api/push/subscribe`, `/api/push/test` | same routes + `PushPayload` | wired |

## Summary

| Area | Orion status vs Orbit |
|---|---|
| Agent chat lens | Near — missing only Antigravity reader (Agy deferred) |
| Plugins / actions | **Gap** — frontend complete, backend routes absent |
| Integrations | Wired |
| Remote-machine setup | **Gap** — types exist, backend routes absent |
| Slash commands | **Gap** — frontend complete, backend route absent |
| Plan / background-tasks | OmO tasks wired; plan not yet compared |
| File / diff viewer | Wired |
| Voice dictation | Wired |
| Key bar / touch targets | Wired |
| Push alerts | Wired |

## Decision pending

Whether to implement the four missing backend routes (`/api/plugins/actions`, `/api/plugin/action` POST+GET, `/api/machines/setup`, `/api/pane/commands`) in `crates/orion-core/src/engine.rs` so the already-built frontend plugin-action UI works against Orion's own engine instead of herdr's. That is the concrete step that closes the biggest gap toward "same as Orbit, not bounded by herdr."
