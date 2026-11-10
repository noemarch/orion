# Orbit UI Port Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port the Orbit UI to the native GPUI `orion-ui` shell so Orion looks and behaves like Orbit at desktop sizes, with no React/DOM/CSS/WebView in the shipped binary.

**Architecture:** Keep `orion-core::Engine` as the only data seam. Build GPUI views that mirror Orbit component boundaries — App shell, Sidebar workspaces, Agent roster, TabStrip, PaneCanvas, Chat, Composer, Terminal, Changes, Settings/dialogs — and drive them from Engine session/transcript projections. Record the desktop-shell ADR and gpui-kit spike before shell code lands.

**Tech Stack:** Rust, GPUI 0.2, Serde JSON snapshot projections, existing `orion-core` Engine HTTP seam, Orbit `DESIGN.md`/`src/styles.css` as the visual/token reference.

**Spec:** `docs/superpowers/specs/2026-11-10-orbit-ui-port-design.md`

## File Structure

- Modify: `crates/orion-ui/src/lib.rs` — register new modules only.
- Create: `crates/orion-ui/src/shell_model.rs` — pure shell model: breakpoint, sidebar width, header model, section state, lens segment state.
- Create: `crates/orion-ui/src/agents.rs` — pure Agent roster projection from session snapshot.
- Modify: `crates/orion-ui/src/theme.rs` — complete Orbit token table and token tests.
- Modify: `crates/orion-ui/src/sidebar.rs` — Orbit sidebar grid: Workspaces + Agents, rail, empty/loading/error states.
- Modify: `crates/orion-ui/src/app.rs` — Orbit root shell: header, tab strip, lens switcher, pane canvas, modal host.
- Modify: `crates/orion-ui/src/chat.rs`, `crates/orion-ui/src/chat_entities.rs`, `crates/orion-ui/src/prompt_card.rs` — Orbit Chat turns, work rows, tool cards, composer, prompt card states.
- Modify: `crates/orion-ui/src/terminal.rs` — Orbit Terminal toolbar, key bar, find, input, resize behavior.
- Modify: `crates/orion-ui/src/files_view.rs`, `crates/orion-ui/src/file_reader.rs`, `crates/orion-ui/src/diff_view.rs`, `crates/orion-ui/src/files_entities.rs` — Orbit Changes split: tree, reader, diff.
- Modify: `crates/orion-ui/src/settings.rs` — Orbit Settings sections, focus trap, Escape close, focus return.
- Create: `docs/adr/desktop-shell-ownership.md` — Tauri retired vs launcher/tray decision.
- Create: `docs/gpui-kit-spike.md` — build and rendered-component evidence with adopt/defer recommendation.
- Attach to #7: dark 1280×800 comparison screenshot and narrow-breakpoint screenshot.

## Global Constraints

- Orbit reference is `C:\herdr-plugins-project\orbit` plus the live app at `http://127.0.0.1:7317/?ws=w2Q-orion&pane=w2Q%3ApA&view=terminal`.
- No React, DOM, CSS, or WebView in the shipped native binary.
- Engine is the only data seam; views do not fabricate Orbit data.
- Rose marks the live edge only: focus, selected lens, selected row, terminal cursor, user action.
- Agent states use status tokens, never rose.
- Missing Orbit backend routes or fields become explicit Orbit-style unavailable/error states.
- The desktop-shell ADR and gpui-kit spike record must exist before shell code lands.
- Visual acceptance is at 1280×800 dark plus the narrow breakpoint, compared with running Orbit at the same size/theme/density.

## Review Focus

- **Empty session on first launch:** expect Orbit’s inviting empty state, not a blank shell or 404.  
  Test: `shell_model::empty_session_renders_inviting_empty_state` in Task 5.
- **Snapshot missing an Orbit field or route:** expect plain-language unavailable state, not panic or invented data.  
  Test: `agents::missing_agent_fields_are_unavailable_not_defaulted` in Task 4.
- **Large transcript or terminal scrollback:** expect capped rendering and preserved lens state when switching.  
  Test: `chat_entities::large_transcript_caps_rendered_turns` in Task 6.
- **Narrow window below 900px:** expect rail sidebar, 44px targets, no overlapping main column.  
  Test: `shell_model::narrow_window_uses_rail_with_touch_targets` in Task 2.
- **Theme/density mismatch with running Orbit:** expect comparison screenshots to name the actual theme/density; no ad-hoc view colors.  
  Test: `theme::orbit_token_table_matches_design_doc` in Task 3.

---

### Task 1: Prerequisites — ADR and gpui-kit spike

**Files:**
- Create: `docs/adr/desktop-shell-ownership.md`
- Create: `docs/gpui-kit-spike.md`
- Test: manual doc review plus build/render evidence

**Interfaces:**
- Consumes: #7 acceptance criteria; Orbit reference repo.
- Produces: `docs/adr/desktop-shell-ownership.md`; `docs/gpui-kit-spike.md`.

- [ ] **Step 1: Write the ADR decision**

```markdown
# Desktop Shell Ownership

Decision: The native `orion-ui` binary is the desktop application. Tauri is [retired / reduced to launcher and tray host] as checked by the spike and release review.

Consequences: the release pipeline ships the GPUI binary as the desktop app; any retained Tauri role is launcher/tray only and does not render the UI.
```

- [ ] **Step 2: Run the gpui-kit spike**

Run: `cargo add gpui-kit` in a temporary spike crate or branch-only dependency, render one Button and one Dialog, run on Windows with the pinned toolchain.

Expected: the spike records build success/failure, rendered component evidence, and an explicit adopt/defer recommendation in `docs/gpui-kit-spike.md`.

- [ ] **Step 3: Commit prerequisites**

```bash
git add docs/adr/desktop-shell-ownership.md docs/gpui-kit-spike.md
git commit -m "docs: record desktop shell ADR and gpui-kit spike"
```

### Task 2: Shell model — breakpoints, widths, header, lens state

**Files:**
- Create: `crates/orion-ui/src/shell_model.rs`
- Modify: `crates/orion-ui/src/lib.rs`
- Test: unit tests in `shell_model.rs`

**Interfaces:**
- Consumes: window width in px; `Lens` from `app.rs`; session snapshot header facts.
- Produces: `ShellLayout { sidebar_width_px: f32, rail: bool }`, `HeaderModel { title: String, workspace: String, connection: String }`, `lens_segment_state(lens: Lens) -> (bool, bool, bool)`.

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn narrow_window_uses_rail_with_touch_targets() {
    let layout = ShellLayout::for_width(800.0);
    assert!(layout.rail);
    assert_eq!(layout.sidebar_width_px, 64.0);
    assert!(layout.touch_target_px >= 44.0);
}

#[test]
fn desktop_window_uses_orbit_sidebar_width() {
    let layout = ShellLayout::for_width(1280.0);
    assert!(!layout.rail);
    assert_eq!(layout.sidebar_width_px, 280.0);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p orion-ui shell_model`

Expected: FAIL, module does not exist.

- [ ] **Step 3: Minimal implementation**

```rust
pub struct ShellLayout { pub sidebar_width_px: f32, pub rail: bool, pub touch_target_px: f32 }

impl ShellLayout {
    pub fn for_width(width: f32) -> Self {
        if width < 900.0 {
            Self { sidebar_width_px: 64.0, rail: true, touch_target_px: 44.0 }
        } else {
            Self { sidebar_width_px: 280.0, rail: false, touch_target_px: 44.0 }
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p orion-ui shell_model`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/orion-ui/src/shell_model.rs crates/orion-ui/src/lib.rs
git commit -m "feat(ui): add Orbit shell layout model"
```

### Task 3: Theme — complete Orbit token table

**Files:**
- Modify: `crates/orion-ui/src/theme.rs`
- Test: unit tests in `theme.rs`

**Interfaces:**
- Consumes: Orbit `DESIGN.md` token table and `src/styles.css` `:root` values.
- Produces: `Theme` fields for primary hover/text/tint, shadows, syntax colors, scrollbar lane, overlay scrims.

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn orbit_token_table_matches_design_doc() {
    let theme = Theme::dark();
    assert_eq!(theme.primary_hover, gpui::rgb(0xa3185a));
    assert_eq!(theme.primary_text, gpui::rgb(0xf2eef5));
    assert_eq!(theme.syntax_keyword, gpui::rgb(0xff8fa8));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p orion-ui theme::tests::orbit_token_table_matches_design_doc`

Expected: FAIL, fields do not exist.

- [ ] **Step 3: Minimal implementation**

Add the missing fields to `Theme`, initialize them in both dark and light constructors from Orbit’s table, and bind existing views to tokens rather than local colors.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p orion-ui theme::tests::orbit_token_table_matches_design_doc`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/orion-ui/src/theme.rs
git commit -m "feat(ui): complete Orbit theme tokens"
```

### Task 4: Agents roster projection

**Files:**
- Create: `crates/orion-ui/src/agents.rs`
- Modify: `crates/orion-ui/src/lib.rs`
- Test: unit tests in `agents.rs`

**Interfaces:**
- Consumes: session snapshot `panes`, `tabs`, `workspaces`.
- Produces: `AgentRow { pane_id: String, title: String, context: String, status: AgentStatus, selected: bool }`, `agent_rows(snapshot: &Value) -> Vec<AgentRow>`.

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn missing_agent_fields_are_unavailable_not_defaulted() {
    let snapshot = serde_json::json!({ "panes": [{ "pane_id": "p1" }], "tabs": [], "workspaces": [] });
    let rows = agent_rows(&snapshot);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "p1");
    assert_eq!(rows[0].status, AgentStatus::Unknown);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p orion-ui agents::tests::missing_agent_fields_are_unavailable_not_defaulted`

Expected: FAIL, module does not exist.

- [ ] **Step 3: Minimal implementation**

Project panes in snapshot order, derive title/context from label/title/cwd/workspace/tab facts, preserve `Unknown` for absent statuses, and mark only `focused_pane_id` as selected.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p orion-ui agents`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/orion-ui/src/agents.rs crates/orion-ui/src/lib.rs
git commit -m "feat(ui): project Orbit agent roster rows"
```

### Task 5: Root shell — header, tab strip, lens switcher, sidebar sections

**Files:**
- Modify: `crates/orion-ui/src/app.rs`
- Modify: `crates/orion-ui/src/sidebar.rs`
- Test: unit tests in `shell_model.rs` and `sidebar.rs`

**Interfaces:**
- Consumes: `ShellLayout`, `HeaderModel`, `agent_rows`, existing `WorkspaceSidebar::workspaces()` and `tabs()`.
- Produces: Orbit root shell with one Workspaces section, one Agents section, header, horizontal tab strip, compact segmented lens switcher.

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn empty_session_renders_inviting_empty_state() {
    let model = HeaderModel::empty();
    assert_eq!(model.title, "Orbit");
    assert_eq!(model.workspace, "No workspace");
    assert_eq!(model.connection, "Connecting…");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p orion-ui shell_model::tests::empty_session_renders_inviting_empty_state`

Expected: FAIL, `HeaderModel::empty` does not exist.

- [ ] **Step 3: Minimal implementation**

Render header from `HeaderModel`; render Workspaces and Agents as separate sections with one head each; render tabs horizontally with click-to-focus; render the lens switcher as a compact segmented control with no full-width rose bar; constrain the main column with `flex_1`, `min_w`, and overflow clipping.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p orion-ui shell_model sidebar`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/orion-ui/src/app.rs crates/orion-ui/src/sidebar.rs crates/orion-ui/src/shell_model.rs
git commit -m "feat(ui): render Orbit shell chrome"
```

### Task 6: Chat parity — turns, work rows, tool cards, composer, prompt card

**Files:**
- Modify: `crates/orion-ui/src/chat.rs`
- Modify: `crates/orion-ui/src/chat_entities.rs`
- Modify: `crates/orion-ui/src/prompt_card.rs`
- Test: unit tests in `chat_entities.rs` and `chat.rs`

**Interfaces:**
- Consumes: conversation response from `/api/pane/conversation`; prompt state from `Pane::prompt`.
- Produces: Orbit turn/part rendering, composer submit state, prompt card blocking state.

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn large_transcript_caps_rendered_turns() {
    let turns = (0..500).map(|i| ChatTurn { id: i.to_string(), role: ChatRole::User, text: "x".to_string() }).collect::<Vec<_>>();
    let rendered = cap_turns(turns, 200);
    assert_eq!(rendered.len(), 200);
    assert_eq!(rendered[0].id, "300");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p orion-ui chat_entities::tests::large_transcript_caps_rendered_turns`

Expected: FAIL, `cap_turns` does not exist.

- [ ] **Step 3: Minimal implementation**

Cap rendered turns to the most recent 200, render user turns as neutral raised cards, render work/tool/thinking rows as collapsible Orbit rows, add composer send/stop/pending/failed states, and keep the prompt card blocking until answered.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p orion-ui chat`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/orion-ui/src/chat.rs crates/orion-ui/src/chat_entities.rs crates/orion-ui/src/prompt_card.rs
git commit -m "feat(ui): match Orbit chat lens"
```

### Task 7: Terminal parity — toolbar, key bar, find, input, resize

**Files:**
- Modify: `crates/orion-ui/src/terminal.rs`
- Test: unit tests in `terminal.rs`

**Interfaces:**
- Consumes: `Pane` grid, key bar extras from settings, pane write/key APIs.
- Produces: Orbit Terminal lens with key bar, find, input, selection, scrollback, resize behavior.

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn resize_debounce_delay_is_50ms() {
    assert_eq!(RESIZE_DEBOUNCE_MS, 50);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p orion-ui terminal::tests::resize_debounce_delay_is_50ms`

Expected: FAIL, constant does not exist.

- [ ] **Step 3: Minimal implementation**

Add `RESIZE_DEBOUNCE_MS = 50`, route resize through it, keep key bar/find/selection/scrollback inside the Terminal lens, and theme the grid and cursor from terminal tokens.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p orion-ui terminal`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/orion-ui/src/terminal.rs
git commit -m "feat(ui): match Orbit terminal lens"
```

### Task 8: Changes parity — tree, reader, diff split

**Files:**
- Modify: `crates/orion-ui/src/files_view.rs`
- Modify: `crates/orion-ui/src/file_reader.rs`
- Modify: `crates/orion-ui/src/diff_view.rs`
- Modify: `crates/orion-ui/src/files_entities.rs`
- Test: unit tests in `files_entities.rs` and `diff_view.rs`

**Interfaces:**
- Consumes: Engine file/diff routes; pane cwd.
- Produces: Orbit Changes split with tree/list on one side and reader/diff on the other.

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn diff_patch_classifies_context_lines() {
    assert_eq!(classify_patch_line(" context"), PatchKind::Context);
    assert_eq!(classify_patch_line("+added"), PatchKind::Add);
    assert_eq!(classify_patch_line("-removed"), PatchKind::Remove);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p orion-ui files_entities::tests::diff_patch_classifies_context_lines`

Expected: FAIL, function does not exist.

- [ ] **Step 3: Minimal implementation**

Ensure the Changes lens renders file tree and diff/reader as a split view, preserves scroll and selection when inactive, and uses Orbit file/diff token colors.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p orion-ui files_entities diff_view`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/orion-ui/src/files_view.rs crates/orion-ui/src/file_reader.rs crates/orion-ui/src/diff_view.rs crates/orion-ui/src/files_entities.rs
git commit -m "feat(ui): match Orbit changes lens"
```

### Task 9: Settings, dialogs, menus — focus and persistence

**Files:**
- Modify: `crates/orion-ui/src/settings.rs`
- Test: unit tests in `settings.rs`

**Interfaces:**
- Consumes: `Preferences`, Engine preferences route.
- Produces: Orbit Settings modal sections with focus trap, Escape close, and focus return.

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn settings_defaults_match_orbit_defaults() {
    let prefs = Preferences::default();
    assert_eq!(prefs.theme.as_wire(), "dark");
    assert_eq!(prefs.density.as_wire(), "comfortable");
    assert_eq!(prefs.key_bar_extras, vec!["alt".to_string()]);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p orion-ui settings::tests::settings_defaults_match_orbit_defaults`

Expected: PASS already if defaults match; if FAIL, update defaults to Orbit’s values.

- [ ] **Step 3: Minimal implementation**

Add missing Orbit Settings sections supported by Engine preferences; persist on change; trap focus in the modal; Escape closes; focus returns to the opener.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p orion-ui settings`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/orion-ui/src/settings.rs
git commit -m "feat(ui): match Orbit settings modal"
```

### Task 10: Empty, loading, error, and backend-gap states

**Files:**
- Modify: `crates/orion-ui/src/app.rs`
- Modify: `crates/orion-ui/src/sidebar.rs`
- Modify: `crates/orion-ui/src/chat.rs`
- Test: unit tests in `shell_model.rs`, `sidebar.rs`, `chat.rs`

**Interfaces:**
- Consumes: `HeaderModel::empty`, `agent_rows`, conversation fetch errors.
- Produces: Orbit plain-language states with retry where actionable.

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn conversation_fetch_failure_is_plain_language_with_retry() {
    let message = conversation_error_message(404);
    assert_eq!(message, "Couldn't load the conversation. Try again.");
    assert!(message.contains("Try again"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p orion-ui chat::tests::conversation_fetch_failure_is_plain_language_with_retry`

Expected: FAIL, helper does not exist.

- [ ] **Step 3: Minimal implementation**

Replace status-code-only messages with Orbit-style one-sentence errors and retry controls; keep empty states inviting and singular; never show the same waiting state twice.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p orion-ui shell_model sidebar chat`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/orion-ui/src/app.rs crates/orion-ui/src/sidebar.rs crates/orion-ui/src/chat.rs
git commit -m "feat(ui): add Orbit empty and error states"
```

### Task 11: Visual acceptance and smoke gate

**Files:**
- Attach to #7: `orion-1280x800-dark.png`, `orion-narrow-rail.png`, Orbit comparison notes.
- Test: manual visual verification plus existing test suites.

**Interfaces:**
- Consumes: completed Tasks 2–10.
- Produces: screenshot evidence and smoke result on #7.

- [ ] **Step 1: Capture screenshots**

Run the native app at 1280×800 dark and at the narrow breakpoint; capture Orbit at the same sizes/theme/density from `http://127.0.0.1:7317/?ws=w2Q-orion&pane=w2Q%3ApA&view=terminal`.

Expected: screenshots show no overlap, no full-width rose bar, one Workspaces section, one Agents section, horizontal tab strip, compact lens switcher.

- [ ] **Step 2: Run the smoke gate**

Run: launch real Claude through the native app; verify chat load, terminal input/output, resize, prompt approval, file/diff reading, settings persistence, and process-tree cleanup on close.

Expected: no panic, no missing-route log, no orphan agent process tree.

- [ ] **Step 3: Comment evidence on #7**

Post screenshot links, theme/density used, spike/ADR links, and any backend-gap notes.

- [ ] **Step 4: Commit only evidence notes if needed**

```bash
git add docs/gpui-kit-spike.md
git commit -m "docs: record Orbit visual comparison evidence"
```

### Task 12: Final verification, review, and ticket closure

**Files:**
- Modify: none unless verification finds a defect.
- Test: full workspace test suite, clippy, fmt, code review.

**Interfaces:**
- Consumes: all previous tasks.
- Produces: verified branch ready to close #7.

- [ ] **Step 1: Run full verification**

Run: `cargo test --workspace`  
Run: `cargo clippy --workspace --all-targets -- -D warnings`  
Run: `cargo fmt --all --check`

Expected: all pass.

- [ ] **Step 2: Code review**

Use the `code-review` skill on the branch diff since `HEAD` of the `pure-rust-ui` branch start used for #7.

Expected: Standards and Spec reports recorded; fix any hard violations before closure.

- [ ] **Step 3: Close #7**

Comment the verification summary, screenshot links, ADR link, gpui-kit spike outcome, and commit range; then close #7.
