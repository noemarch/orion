# GPUI Kit Adoption Plan — Native Desktop UI

> **Status:** Plan only — not started. Created 2026-11-08.
> **Scope:** `crates/orion-ui` — the native GPUI desktop shell that will eventually
> replace the Tauri + React WebView UI (`src-tauri` + `src/`).
> **Rule:** Every time a UI component is needed for the native desktop app, pick it
> from the [gpui-kit component catalog](https://gpui-kit.com/component) first. Only
> build a custom control when no gpui-kit component fits and document why.

## 1. Why gpui-kit

Orion's native UI already prototypes with raw [`gpui`](https://gpui.rs) 0.2 in
`crates/orion-ui` (see `docs/p0-plan.md`, P0 complete). Raw GPUI gives layout and
rendering primitives but no styled component library. Every button, sidebar, dialog,
table, and dropdown would have to be hand-styled and re-styled for light/dark themes.

[gpui-kit](https://gpui-kit.com/) (Longbridge, Apache-2.0) is the component layer
built on GPUI. Its `gpui-component` layer ships 75+ styled, accessible components
with light/dark theme support out of the box — exactly what Orion's "Quiet Slate"
design system needs. Adopting it means:

- Buttons, sidebars, tabs, dialogs, notifications, tables, trees, editors, charts,
  diffs, docks, command palettes, settings forms come from one coherent library.
- Theme tokens come from gpui-kit's `Theme` component instead of hand-written CSS
  variables.
- AccessKit roles/names/states are built in (matches Orion's accessibility goal in
  `docs/architecture.md`).
- GPUI Fast retained rendering is available via the `gpui-fast` feature.

Licensing: gpui-kit software source and code examples are Apache-2.0; documentation
prose and illustrations are CC BY 4.0. Adapted component code must be credited in
`THIRD_PARTY_NOTICES.md` (per `src/AGENTS.md`).

## 2. Dependency setup

Replace the raw `gpui` dependency in `crates/orion-ui/Cargo.toml` with `gpui-kit`:

```toml
[dependencies]
gpui-kit = { version = "0.9.0", features = ["gpui-fast"] }
orion-core = { path = "../orion-core" }
flume = "0.11"
tokio = { workspace = true }
tracing = { workspace = true }
serde_json = { workspace = true }
portable-pty = { workspace = true }
```

```toml
[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.59", features = ["Win32_Foundation", "Win32_Security", "Win32_System_JobObjects", "Win32_System_Threading"] }
```

- `gpui-kit = "0.9.0"` pulls in GPUI, GPUI Base, the styled `gpui-component` library,
  and default icon assets.
- Application code uses `use gpui_kit::*;` and components via
  `gpui_kit::component::{...}`.
- `gpui-fast` selects GPUI Fast's retained rendering/layout engine for the whole
  dependency graph (recommended for a desktop app; see gpui-kit Getting Started).
- Do **not** add a direct `gpui` dependency alongside `gpui-kit` (avoids mixing
  engine snapshots).
- macOS text rendering needs `font-kit` on `gpui-pre-platform`, already enabled by
  the `gpui-kit` dependency.
- Startup: `gpui_kit::application().with_assets(assets::Assets).run(|cx| { init(cx); ... })`.

Version pin: use `v0.9.0` (latest on gpui-kit.com) and keep the Cargo requirement
compatible with the pinned Rust 1.86.0 in the workspace `Cargo.toml`.

## 3. Component mapping (Orion need → gpui-kit component)

Whenever the native desktop app needs a UI control, the first choice is the matching
gpui-kit component. Mapping:

| Orion UI area | Current implementation | gpui-kit component (pick first) | Catalog page |
|---|---|---|---|
| Workspace sidebar | Custom `WorkspaceSidebar` (raw gpui) | `Sidebar` | /component/sidebar |
| Tab strip / tabbed panes | Custom tab strip | `Tabs` | /component/tabs |
| Terminal pane | Custom VT grid (`TerminalPane`) | Keep custom (VT grid is not a kit component); theme it via `Theme` | — |
| Agent status dots | Custom colored `div` | `Badge` (or keep small dot + `Badge` for labels) | /component/badge |
| Button / primary actions | Custom `div` + handlers | `Button` (`primary`, `label`, `on_click`) | /component/button |
| Chat messages / rows | React chat lens | `Message`, `MessageScroller`, `Bubble` | /component/message, /component/message-scroller, /component/bubble |
| Approval / prompt cards | React approval cards | `Dialog`, `AlertDialog`, `Sheet` | /component/dialog, /component/alert-dialog, /component/sheet |
| Settings modal | React settings | `Settings`, `Form`, `Field`, `Input`, `Switch`, `Checkbox`, `RadioGroup` | /component/settings, /component/form, /component/input, /component/switch, /component/checkbox, /component/radio |
| Notifications (needs input / done) | React droplets | `Notification` | /component/notification |
| Command palette / quick actions | — | `Command` | /component/command |
| File tree / worktree view | React FileTree | `Tree` | /component/tree |
| Data tables (agent roster, etc.) | — | `DataTable`, `Table` | /component/data-table, /component/table |
| Diff / change preview | React diff | `Diff` | /component/diff |
| Dockable panels / splits | React layout | `Dock` | /component/dock |
| Virtualized long lists | — | `VirtualList`, `List` | /component/virtual-list, /component/list |
| Code editor (markdown/diff) | React editor | `Editor` | /component/editor |
| Charts (agent metrics) | — | `Chart` | /component/chart |
| Calendar / date picker | — | `Calendar`, `DatePicker` | /component/calendar, /component/date-picker |
| Time input | — | `TimeField` | /component/time-field |
| Avatar (agent / user) | — | `Avatar` | /component/avatar |
| Tooltips / help hints | — | `Tooltip` | /component/tooltip |
| Progress / spinners | — | `Progress`, `Spinner` | /component/progress, /component/spinner |
| Kbd key labels (key bar) | React key bar | `Kbd` | /component/kbd |
| Image / attachment preview | — | `Image`, `Attachment` | /component/image, /component/attachment |
| Theming (Quiet Slate) | CSS variables | `Theme` | /component/theme |
| Root window provider (dialogs, sheets, notifications) | — | `Root` | /component/root |
| Scrollable containers | — | `Scrollable` | /component/scrollable |
| Resizable panels / splits | — | `Resizable` | /component/resizable |
| Collapsible sections | — | `Collapsible` | /component/collapsible |
| GroupBox / labeled sections | — | `GroupBox` | /component/group-box |
| Divider / separator | — | `Marker` (conversation separator) | /component/marker |
| Skeleton loading states | — | `Skeleton` | /component/skeleton |
| Tag / label chips | — | `Tag` | /component/tag |
| TextView (markdown/HTML) | — | `TextView` | /component/text-view |
| Textarea | — | `Textarea` | /component/textarea |
| Select / Combobox | — | `Select`, `Combobox` | /component/select, /component/combobox |
| NumberInput / OtpInput / ColorPicker / Speech | — | matching component | /component/* |
| Carousel / Plot / Stepper / Questionnaire / Slider / Switch / Toggle / Toolbar / TitleBar / StatusBar / Popover / HoverCard / Focus Trap / Clipboard | as needed | matching component | /component/* |

**Standing rule:** before writing any custom `div`-based control in `crates/orion-ui`,
search https://gpui-kit.com/component. If a component exists, adapt it to the Quiet
Slate tokens via `Theme`/`ActiveTheme`; if none fits, build custom and record the
justification in the PR description.

## 4. Design system integration

Map Orion's "Quiet Slate" tokens (from `docs/architecture.md` § Design system) onto
gpui-kit's `Theme`:

- Ground: near-black plum (`--surface-ground`) → `theme.background`
- Panels: flat (`--surface-panel`) → panel variants
- Borders: muted outlines → border tokens
- Accent: single muted rose for focus/selection → primary/focus token (rose reserved
  for focus, never for agent status)
- Agent status colors: `RUN` blue/cyan, `INPUT` amber, `DONE` emerald, `ERROR` ruby —
  apply via `Badge`/custom status dots, **never** rose.
- Touch targets: ≥44px on mobile viewports (`--touch-target`).
- Terminal resize: debounce xterm/GPUI resize by 50ms (ConPTY resize flood guard).

## 5. Adoption phases

The existing `docs/orion-pure-rust-plan.md` phases (P0–P6) already describe migrating
the UI from React to GPUI. This plan re-scopes those phases to use **gpui-kit**
components instead of raw `gpui` primitives.

### P0 — Spike (complete, raw gpui)
`crates/orion-ui` exists with a raw-`gpui` sidebar + terminal pane wired to
`orion_core::Engine`. Gate: GPU-rendered Claude TUI on Windows, 60fps resize, no
WebView. **All gates green** (see `docs/p0-plan.md`).

### P1 — Rebase `crates/orion-ui` onto gpui-kit (1 week)
- Swap `gpui` 0.2 → `gpui-kit` 0.9.0 in `crates/orion-ui/Cargo.toml`.
- Update imports: `use gpui_kit::*;`, `gpui_kit::component::{...}`.
- Add `init(cx)` + `with_assets(assets::Assets)` in `main.rs`/`app.rs`.
- Rebuild `WorkspaceSidebar` using `gpui-kit::component::Sidebar` + `Tabs` + `Badge`.
- Rebuild `TerminalPane` as a custom VT grid (kept) themed through `Theme`/`ActiveTheme`.
- Gate: `cargo build -p orion-ui`, `cargo clippy -p orion-ui -D warnings`, app
  launches with `GPUI_DISABLE_DIRECT_COMPOSITION=1` on Windows.

### P2 — Shell chrome + navigation (1–2 weeks)
- `Root` window provider for dialogs/sheets/notifications.
- `TitleBar`, `Toolbar`, `StatusBar` for window chrome.
- `Nav`/sidebar integration with workspaces/tabs/panes from `orion_core::Engine`.
- Gate: sidebar opens/closes workspaces, tab switching renders the right pane.

### P3 — Chat lens + prompt/approval cards (2–3 weeks)
- `Message`, `MessageScroller`, `Bubble` for chat rows (work/thinking/result).
- `Dialog`/`AlertDialog`/`Sheet` for approval cards and permission prompts.
- `Button` for approve/deny/actions.
- Gate: a real Claude session streams chat rows; an approval dialog blocks until
  answered (matches `engine::prompt` behavior).

### P4 — Settings, notifications, file tree, diffs (2 weeks)
- `Settings`/`Form`/`Input`/`Switch`/`Checkbox`/`RadioGroup` for settings.
- `Notification` for needs-input/done alerts (native notifications, no telemetry).
- `Tree` for file/worktree view.
- `Diff` for change previews.
- Gate: settings open and persist; notification fires on agent `done`.

### P5 — Advanced data + editor (2 weeks)
- `DataTable`/`Table` for agent roster / machine roster.
- `Editor` for markdown/diff/code viewing (replaces highlight.js/KaTeX path).
- `Chart`/`Plot` for metrics.
- `Command` palette for quick actions.
- `VirtualList` for long scrollback/message lists.
- Gate: roster table renders hundreds of rows; editor opens a file with syntax
  highlighting.

### P6 — Remove the web layer (1 week)
- Delete `src/` (React) and `shared/*.ts`; move wire types into Rust (`protocol.rs`
  already exists).
- Drop `bun`, `vite`, `tsc`, `package.json` from release pipeline.
- If PWA retained: build GPUI for `wasm32-unknown-unknown` and serve from `orion
  serve` (gpui-kit has a WebAssembly backend).
- Gate: `cargo test --workspace`, `cargo clippy -D warnings`, desktop app starts
  with no WebView.

## 6. Verification gates (per milestone)

Before a milestone is done:

1. `cargo fmt --check`
2. `cargo clippy -p orion-ui --all-targets -- -D warnings`
3. `cargo test -p orion-ui --all-targets`
4. `cargo build -p orion-ui --bin orion-ui`
5. On Windows: `GPUI_DISABLE_DIRECT_COMPOSITION=1 orion-ui.exe` launches and renders
   stably; resize at 60fps; no WebView in the binary.
6. Any new component is sourced from https://gpui-kit.com/component (or justified).

## 7. What is lost / kept

- **Lost:** HeroUI components, React ecosystem, Orbit's exact pixels, the existing
  React component test suite (replaced by gpui-kit snapshot/UI tests).
- **Kept:** `orion-core` engine, PTY, agents, transcripts, prompt detection, store,
  all Rust tests, the `Engine::dispatch` seam, the frozen `shared/protocol.ts`
  contract (until P6).

## 8. Attribution

gpui-kit code examples and adapted components are Apache-2.0 (Longbridge).
Documentation prose/illustrations adapted from gpui-kit.com are CC BY 4.0 — credit
GPUI Kit, link to https://gpui-kit.com/component, and indicate changes in
`THIRD_PARTY_NOTICES.md`.
