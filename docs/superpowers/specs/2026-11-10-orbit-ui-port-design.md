# Orbit UI Port — Native GPUI Design

Date: 2026-11-10  
Ticket: #7 (shell: native Orion layout and Quiet Slate chrome), part of #5  
Reference: `C:\herdr-plugins-project\orbit`, live Orbit at `http://127.0.0.1:7317/?ws=w2Q-orion&pane=w2Q%3ApA&view=terminal`

## Outcome

Replace the current debug-harness-like native shell with a GPUI implementation of the Orbit UI: same shell information architecture, same component boundaries, same Quiet Slate/Orbit token system, and same interactions at desktop sizes. The result must look and behave like Orbit at 1280×800, without React, DOM, CSS, or WebView in the shipped native binary.

## Approaches considered

### A. Native GPUI component-for-component port (chosen)

Map Orbit components to `orion-ui` views while preserving component seams: App shell, Sidebar workspaces, Agent roster, TabStrip, PaneCanvas, Chat, Composer, Terminal, Changes, dialogs, menus, settings. Data remains Engine-driven; the native UI reads the same session/transcript seams Orbit reads over HTTP/WS-equivalent routes.

Trade-offs: largest implementation effort, but it satisfies the pure-Rust goal, keeps Orbit visual parity checkable at the same window size, and avoids embedding a web stack.

### B. Wrap or reuse Orbit web UI

Rejected for the product. It would be visually fast but reintroduces React/WebView and conflicts with the native binary goal. Orbit’s running app and screenshots may be used only as comparison references.

### C. Shell slice only

Rejected because the request is full Orbit UI parity, not only shell chrome.

## Architecture

- `orion-ui` becomes the desktop application shell. Tauri’s role is resolved separately by the desktop-shell ownership ADR.
- Views mirror Orbit component boundaries rather than current GPUI one-off divs. Shared chrome — sidebar rows, tabs, menus, badges, dialogs, buttons, notices — are small components with token-bound styling.
- The existing GPUI terminal grid remains custom; there is no Orbit component equivalent for the VT grid. It is themed from terminal tokens.
- The gpui-kit spike remains a gating experiment: if gpui-kit builds and renders cleanly on Windows with the pinned toolchain, adopt its component layer for shell chrome; otherwise keep raw GPUI but preserve the same component boundaries and token contract. The outcome is recorded with evidence.
- No web assets, React, or WebView return to the native binary.

## Shell information architecture

Root layout is a desktop two-column shell:

- Sidebar plus main column.
- Header: Orbit brand, focused workspace context, connection state, and primary global actions.
- Sidebar: Workspaces section and Agents section as independent lists. Rows use the Orbit grid: leading mark/monogram, title/context, trailing status. A dim head appears once per section; the roster/workspace boundary follows Orbit’s spacing/hairline treatment.
- Tab strip: horizontal, scrollable, Orbit tab row behavior, visible for one tab too, with selected state and menus.
- Pane canvas: the active lens body with Orbit’s pane controls, split behavior where the current Orion model supports it, and Orbit empty/loading/error states.
- Narrow behavior: sidebar collapses to the desktop rail with 44px targets; docked/drawer behavior follows Orbit where applicable.

## Lenses and component parity

- **Chat:** Orbit turn structure, work/thinking rows, tool cards, diffs, markdown, prompt/approval card, composer, pending/failed notices, empty/loading/error/retry states.
- **Terminal:** live PTY grid, input, key bar, find, selection, scrollback, resize behavior, terminal tokens, and Orbit terminal toolbar/density treatment.
- **Changes:** file tree, changed-file list, file reader, repo diff, split view, and Orbit file/diff presentation.
- **Settings/dialogs/menus:** Orbit settings sections, focus trapping, Escape close, focus return, and plain-language errors.
- **Agents/workspaces:** Orbit sidebar agents list with brand marks, grouping/activity order where Orion data supports it, status badges, background task and plan counts when present.

## Data flow

- Session truth remains `orion-core::engine::Engine`. `orion-ui` reads `/api/session`, conversation, pane, workspace, tab, diff/file, preferences, and prompt routes through the Engine seam.
- Interactive changes use the same route families as the web client: focus, create/rename/close, pane submit, prompt answer, settings save.
- Live updates preserve Orbit’s behavior: polling plus change notifications must not rebuild unchanged transcripts; lens switches preserve scroll, selection, and drafts.
- Backend gaps are explicit. If Orion lacks an Orbit route or field, the native UI shows Orbit-style plain-language unavailable/error states rather than inventing data.

## Theme and tokens

- Orbit `DESIGN.md` and `src/styles.css` are the token reference. Existing Orion Quiet Slate values already mirror the base table; missing tokens are added from the Orbit table, not ad-hoc in views.
- The visual comparison uses the running Orbit app’s current theme and density. Default Orbit behavior should be captured before implementation; if the live app is on Codex default, Codex is the comparison palette, with Quiet Slate retained as a supported theme.
- Rose remains the live edge only: focus, selected lens, selected row, terminal cursor, user action. Agent states use status tokens, never rose.

## ADR and spike prerequisites

Before shell code lands, record:

1. **Desktop-shell ownership ADR** under `docs/adr/`: whether Tauri is retired or reduced to launcher/tray host, and what the release pipeline does.
2. **gpui-kit spike record**: build and rendered component evidence, plus adopt/defer recommendation.

These are prerequisites in the implementation plan, not optional follow-ups.

## Testing and verification

- **View-model seam:** pure projections and layout decisions — sidebar rows, agent roster ordering, tab rows, header model, lens selection, empty/loading/error states — unit-tested without a window.
- **Engine seam:** bootstrap and route behavior remain covered through Engine HTTP/session tests, including conversation 200 and focus/active-tab consistency.
- **GPUI render seam:** narrow tests assert presence and bounds of intentional regions: sidebar, tab strip, lens switcher, composer, key bar, prompt card, settings modal. No pixel snapshots.
- **Visual acceptance:** dark-theme screenshots at 1280×800 compared with running Orbit at the same size/theme/density; narrow-breakpoint screenshot for rail behavior. Screenshots are attached to #7 before closing.
- **Smoke gate:** real Claude launch, no panic or missing-route log, terminal input/output, resize, chat load, prompt approval, file/diff reading, settings persistence, and process-tree cleanup on close.

## Scope controls

- Exact Orbit parity is a product UI goal; it is not satisfied by reusing Orbit’s web implementation.
- Features absent from Orion’s Engine model are not fabricated in the UI; they become explicit unavailable states or backend-gap notes.
- Mobile/PWA-only behaviors, push, voice, QR pairing, remote machine roster, command palette, scheduled prompts, and other follow-up specs remain out of this port unless needed to render the desktop shell correctly.
