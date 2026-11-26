# Sidebar State And Project Visibility Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the open project and each session's agent state visible in the Orion sidebar at a glance, with offline overriding every cached state.

**Architecture:** Keep the custom entity-backed virtualized sidebar from ADR 0004. Derive presentation state from core `SessionState` in a new UI helper, add one field (`started_at`) to the pane wire snapshot, and feed a single `runtime_online` flag from `AppModel` plus poll failures. No new component, no protocol version bump.

**Tech Stack:** Rust 1.92+, GPUI / gpui-kit 0.9.1, serde_json, chrono, cargo.

## Global Constraints

- Do not commit. Leave all changes in the worktree for user review.
- Pure Rust: no WebView, no JavaScript, no new dependencies.
- Never show `idle` when the runtime is unreachable: `RowState::Offline` overrides all cached states.
- Agent state colors never use rose (`theme.accent_emphasis`); rose is only focus/selection.
- Touch targets stay at `TOUCH_TARGET_PX` (44px).
- Gates after each task's code change: `cargo check -p orion-ui`, `cargo clippy -p orion-ui -- -D warnings`, `cargo fmt --check`. Task 1 additionally runs `cargo check -p orion-core`.
- Follow existing naming: `sidebar.rs`, `session_status.rs`; no new module for the row enum beyond `session_status.rs`.

---

### Task 1: Expose `started_at` on the pane snapshot

**Files:**
- Modify: `crates/orion-core/src/engine/pane.rs:557-606` (`info`, `agent_info`)
- Modify: `crates/orion-core/src/protocol.rs:336-350` (`PaneInfo`)
- Test: `crates/orion-core/src/engine/pane.rs` existing test module

**Interfaces:**
- Consumes: `Pane.started_at: DateTime<Utc>` (`pane.rs:114`).
- Produces: snapshot keys `started_at` (RFC 3339 string) on both `panes[]` and `agents[]`; `protocol::PaneInfo.started_at: String`.

- [ ] **Step 1: Add the field to the typed PaneInfo struct**

In `crates/orion-core/src/protocol.rs`, add after `pub cwd: String,`:
```rust
    /// RFC 3339 UTC time this pane's process was started.
    pub started_at: String,
```
Update every struct literal that constructs `PaneInfo` in the crate (search `PaneInfo {`; add `started_at: String::new()` where no live pane exists, e.g. fixtures). Do not invent a default inside the struct.

- [ ] **Step 2: Write the failing core test**

Add to the `pane.rs` test module:
```rust
    #[test]
    fn snapshot_carries_started_at() {
        let pane = test_pane(); // existing helper in this module
        let info = pane.info(false);
        assert!(info
            .get("started_at")
            .and_then(Value::as_str)
            .is_some_and(|s| chrono::DateTime::parse_from_rfc3339(s).is_ok()));
        let agent = pane.agent_info(false);
        assert_eq!(agent.get("started_at"), info.get("started_at"));
    }
```
If the test module has no `test_pane()` helper, use the existing constructor used by neighbouring tests; do not add a new fixture abstraction.

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p orion-core snapshot_carries_started_at`
Expected: FAIL — field missing from JSON.

- [ ] **Step 4: Add the field to both snapshot builders**

In `pane.rs` `info()` and `agent_info()`, insert after `"cwd": self.cwd.to_string_lossy(),`:
```rust
            "started_at": self.started_at.to_rfc3339(),
```
Both use the same `self.started_at`, so the values match.

- [ ] **Step 5: Run the test and core gates**

Run: `cargo test -p orion-core snapshot_carries_started_at && cargo check -p orion-core && cargo fmt --check`
Expected: PASS, clean check, clean format.

---

### Task 2: Add the presentation state enum and derived helpers

**Files:**
- Modify: `crates/orion-ui/src/session_status.rs` (add enum + helpers after `state_from_wire`)
- Test: `crates/orion-ui/src/session_status.rs` test module

**Interfaces:**
- Consumes: `AgentStatus` (`protocol.rs`), `SessionState::aggregate`/`is_working` (`session_state.rs`), `pane_kind` wire string.
- Produces:
  - `pub enum RowState { Working, Idle, None, Offline }`
  - `pub fn row_state(raw: Option<&str>, is_shell: bool, runtime_online: bool) -> RowState`
  - `pub fn aggregate(states: &[RowState]) -> RowState`
  - `pub fn working_count(states: &[RowState]) -> usize`
  - `pub fn elapsed_label(started_at: Option<&str>, now: DateTime<Utc>) -> Option<String>`

- [ ] **Step 1: Write failing tests for mapping, aggregate, count, elapsed**

Add to `session_status.rs` tests:
```rust
    #[test]
    fn row_state_maps_shell_idle_working_and_offline() {
        assert_eq!(row_state(Some("working"), false, true), RowState::Working);
        assert_eq!(row_state(Some("idle"), false, true), RowState::Idle);
        assert_eq!(row_state(Some("idle"), true, true), RowState::None);
        assert_eq!(row_state(Some("working"), false, false), RowState::Offline);
        assert_eq!(row_state(None, false, true), RowState::Working); // Unknown -> Starting
    }

    #[test]
    fn aggregate_prioritises_offline_then_working_then_idle() {
        use RowState::*;
        assert_eq!(aggregate(&[Working, Idle, None]), Working);
        assert_eq!(aggregate(&[Idle, None]), Idle);
        assert_eq!(aggregate(&[None]), None);
        assert_eq!(aggregate(&[Working, Offline]), Offline);
    }

    #[test]
    fn working_count_counts_only_working_rows() {
        use RowState::*;
        assert_eq!(working_count(&[Working, Working, Idle, None]), 2);
    }

    #[test]
    fn elapsed_label_uses_minutes_and_hours() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-25T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(
            elapsed_label(Some("2026-11-25T11:58:00Z"), now),
            Some("2m".to_string())
        );
        assert_eq!(elapsed_label(None, now), None);
    }
```
`chrono` is already a dependency of the workspace; add `use chrono::{DateTime, Utc};` at the top of `session_status.rs` when implementing.

- [ ] **Step 2: Verify the tests fail to compile**

Run: `cargo test -p orion-ui session_status::tests --lib`
Expected: FAIL — unresolved names.

- [ ] **Step 3: Implement the helpers**

Insert after `state_from_wire`:
```rust
/// What the sidebar may paint in a row's state slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowState {
    Working,
    Idle,
    /// A shell pane, or a project with no agent open.
    None,
    /// The runtime is unreachable; no cached state may be trusted.
    Offline,
}

pub fn row_state(raw: Option<&str>, is_shell: bool, runtime_online: bool) -> RowState {
    if !runtime_online {
        return RowState::Offline;
    }
    if is_shell {
        return RowState::None;
    }
    if state_from_wire(raw).is_working() {
        RowState::Working
    } else {
        RowState::Idle
    }
}

pub fn aggregate(states: &[RowState]) -> RowState {
    if states.contains(&RowState::Offline) {
        return RowState::Offline;
    }
    let core: Vec<SessionState> = states
        .iter()
        .filter_map(|state| match state {
            RowState::Working => Some(SessionState::Running),
            RowState::Idle => Some(SessionState::Idle),
            RowState::None | RowState::Offline => None,
        })
        .collect();
    if core.is_empty() {
        return RowState::None;
    }
    if SessionState::aggregate(&core).is_working() {
        RowState::Working
    } else {
        RowState::Idle
    }
}

pub fn working_count(states: &[RowState]) -> usize {
    states.iter().filter(|state| **state == RowState::Working).count()
}

pub fn elapsed_label(started_at: Option<&str>, now: DateTime<Utc>) -> Option<String> {
    let started = chrono::DateTime::parse_from_rfc3339(started_at?).ok()?;
    let seconds = now.signed_duration_since(started.with_timezone(&Utc)).num_seconds().max(0);
    if seconds < 60 {
        Some(format!("{seconds}s"))
    } else if seconds < 3600 {
        Some(format!("{}m", seconds / 60))
    } else {
        Some(format!("{}h", seconds / 3600))
    }
}
```

- [ ] **Step 4: Run tests and gates**

Run: `cargo test -p orion-ui session_status::tests --lib && cargo check -p orion-ui && cargo clippy -p orion-ui -- -D warnings && cargo fmt --check`
Expected: PASS, clean.

---

### Task 3: Add middle truncation and runtime-online state to the sidebar

**Files:**
- Modify: `crates/orion-ui/src/sidebar.rs` (constants near 49-56, struct at 136-152, `new` at 155-194, `refresh_session` at 1156-1171)
- Test: `crates/orion-ui/src/sidebar.rs` test module

**Interfaces:**
- Consumes: nothing from Task 2 yet.
- Produces: `truncate_middle(s: &str, max_chars: usize) -> String` (private, tested in-module); `WorkspaceSidebar::set_runtime_online(&mut self, online: bool)`; `WorkspaceSidebar::runtime_online: bool`.

- [ ] **Step 1: Write failing tests for truncation**

Add to `sidebar.rs` tests:
```rust
    #[test]
    fn truncate_middle_keeps_both_ends() {
        assert_eq!(truncate_middle(r"C:\rust-projects\orion", 12), r"C:\…\orion");
        assert_eq!(truncate_middle("short", 12), "short");
        assert_eq!(truncate_middle("abcdef", 3), "a…f");
    }
```
Expect an edge case: with `max_chars = 3`, head=1 tail=1 plus ellipsis = 3. Implement exactly that.

- [ ] **Step 2: Verify the test fails**

Run: `cargo test -p orion-ui sidebar::tests::truncate_middle_keeps_both_ends --lib`
Expected: FAIL — unresolved function.

- [ ] **Step 3: Implement the helper**

Near the constants at `sidebar.rs:49-56`, add:
```rust
fn truncate_middle(s: &str, max_chars: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars {
        return s.to_string();
    }
    if max_chars <= 1 {
        return "…".chars().take(max_chars).collect();
    }
    let inner = max_chars - 1;
    let head = inner / 2;
    let tail = inner - head;
    let mut out: String = chars[..head].iter().collect();
    out.push('…');
    out.extend(chars[chars.len() - tail..].iter());
    out
}
```
Check the expected value: `C:\rust-projects\orion` has 23 chars; `max_chars = 12` → inner 11, head 5, tail 6 → `C:\ru…projects` not the test's `C:\…\orion`. Fix the test to the real output for 12 (`C:\ru…s\orion` needs exact count), or choose `max_chars = 8` → inner 7, head 3, tail 4 → `C:\…orion`. Use this test instead:
```rust
    #[test]
    fn truncate_middle_keeps_both_ends() {
        assert_eq!(truncate_middle("short", 12), "short");
        assert_eq!(truncate_middle("abcdef", 3), "a…f");
        assert_eq!(truncate_middle("abcdefgh", 5), "ab…gh");
        assert_eq!(truncate_middle("C:\\rust-projects\\orion", 8), "C:\\…orion");
    }
```
Verify each assertion by counting `char`s before running. No path-specific split logic: this is char-based, so drive prefixes work.

- [ ] **Step 4: Add `runtime_online` to the sidebar struct and constructor**

Change `pub struct WorkspaceSidebar` at `sidebar.rs:136`:
```rust
pub struct WorkspaceSidebar {
    engine: Option<Arc<dyn Runtime>>,
    runtime_online: bool,
    snapshot: Option<Value>,
    // ... unchanged fields
}
```
In `new()`'s struct literal (line 174), add `runtime_online: true,` after `engine: None,`.
Add the setter after `attach()` (line 199):
```rust
    pub fn set_runtime_online(&mut self, online: bool) {
        if self.runtime_online != online {
            self.runtime_online = online;
            cx.notify();
        }
    }
```
`set_runtime_online` needs a context to notify: use `pub fn set_runtime_online(&mut self, online: bool, cx: &mut Context<Self>)` and pass `cx` from callers.

- [ ] **Step 5: Flip offline on failed polls**

Replace `refresh_session()` at `sidebar.rs:1156-1171` with:
```rust
    fn refresh_session(&mut self) -> bool {
        let Some(engine) = self.engine.clone() else {
            return self.runtime_online;
        };
        let (status, body) = engine.http("GET", "/api/session", &Value::Null);
        if status != 200 {
            self.runtime_online = false;
            return true; // notify so offline marks paint
        }
        self.runtime_online = true;
        let encoded = body.to_string();
        if self.last_body.as_deref() == Some(encoded.as_str()) {
            return false;
        }
        self.last_body = Some(encoded);
        self.snapshot = body.get("snapshot").cloned();
        true
    }
```

- [ ] **Step 6: Run tests and gates**

Run: `cargo test -p orion-ui sidebar::tests::truncate_middle_keeps_both_ends --lib && cargo check -p orion-ui && cargo clippy -p orion-ui -- -D warnings && cargo fmt --check`
Expected: PASS, clean.

---

### Task 4: Feed offline state from the app and render the header warning

**Files:**
- Modify: `crates/orion-ui/src/app.rs:505-525` (runtime match), `crates/orion-ui/src/sidebar.rs:1264-1319` (`render_titlebar`)
- Test: `crates/orion-ui/src/sidebar.rs` test module

**Interfaces:**
- Consumes: `WorkspaceSidebar::set_runtime_online(bool, &mut Context<Self>)` from Task 3.
- Produces: sidebar header warning mark bound to `runtime_online`.

- [ ] **Step 1: Write a failing test for the header warning id**

In `sidebar.rs` tests, following the existing draw-test pattern (search `draw` in this test module):
```rust
    #[test]
    fn titlebar_shows_warning_when_offline() {
        let (mut window, cx) = test_context();
        let mut view = test_sidebar(&mut window, &cx);
        view.runtime_online = false;
        assert!(offline_header_mark(&view, &mut window, &cx));
    }
```
If the test module uses a different harness, mirror exactly the neighbouring draw test's setup and assert on the presence of the `sidebar-offline-warning` element id via `TestSupportExt`. Extract the predicate into a small helper `fn offline_header_mark(&self, window, cx) -> bool` only if the harness cannot assert on rendered elements directly.

- [ ] **Step 2: Verify the test fails**

Run: `cargo test -p orion-ui sidebar::tests::titlebar_shows_warning_when_offline --lib`
Expected: FAIL — missing element id.

- [ ] **Step 3: Set the flag at app startup**

In `app.rs` after the `(engine, notice)` match (line 523), before `let mut app = Self {`:
```rust
        let runtime_online = notice.is_none();
        workspace_sidebar.update(cx, |view, cx| view.set_runtime_online(runtime_online, cx));
```
`workspace_sidebar` is created earlier in `new()` (search its `cx.new` binding and use that exact variable name). Do not change `notice` or the banner text.

- [ ] **Step 4: Render the warning triangle**

In `render_titlebar()` (`sidebar.rs:1264-1319`), insert after the logo div (ends line 1294):
```rust
            .when(!self.runtime_online, |el| {
                el.child(
                    Icon::new(IconName::Warning)
                        .size(theme.scaled_icon_size())
                        .text_color(theme.warning_amber)
                        .tooltip("Orion runtime is unreachable"),
                )
            })
```
Verify `IconName::Warning` exists in gpui-kit 0.9.1 via `grep` in the dependency source; if it is named differently (`Alert`, `TriangleAlert`), use the existing name verbatim. Do not introduce a hand-drawn icon if a kit icon exists.

- [ ] **Step 5: Run tests and gates**

Run: `cargo test -p orion-ui sidebar::tests::titlebar_shows_warning_when_offline --lib && cargo check -p orion-ui && cargo clippy -p orion-ui -- -D warnings && cargo fmt --check`
Expected: PASS, clean.

---

### Task 5: Render state slots and status text on session rows

**Files:**
- Modify: `crates/orion-ui/src/sidebar.rs` (`TreeRow` at 115-126, `visible_rows` at 228-282, `render_tree_row` pane branch at 844-978, `status_indicator` at 578-600)
- Test: `crates/orion-ui/src/sidebar.rs` test module

**Interfaces:**
- Consumes: `RowState`, `elapsed_label` (Task 2); `truncate_middle` and `runtime_online` (Task 3).
- Produces: `TreeRow::Pane { agent, subtitle, brand, state, status_line }` and `TreeRow::Header { workspace, pane_count, collapsed, state, working, agent_count }` used by Task 6.

- [ ] **Step 1: Extend the row enum and write a failing derivation test**

```rust
enum TreeRow {
    Header {
        workspace: WorkspaceEntry,
        pane_count: usize,
        collapsed: bool,
        state: session_status::RowState,
        working: usize,
        agent_count: usize,
    },
    Pane {
        agent: AgentRow,
        subtitle: String,
        brand: IconName,
        state: session_status::RowState,
        status_line: String,
    },
}
```

Test:
```rust
    #[test]
    fn visible_rows_derives_state_and_status_line() {
        // Build a snapshot with one working agent (started 2m ago), one idle
        // agent, and one shell pane; use the same JSON shape as neighbouring
        // sidebar tests.
        let rows = visible_rows_from_snapshot(&snapshot, "", true);
        let panes: Vec<_> = rows.iter().filter_map(|row| match row {
            TreeRow::Pane { state, status_line, .. } => Some((*state, status_line.clone())),
            _ => None,
        }).collect();
        assert!(panes.iter().any(|(s, line)| *s == RowState::Working && line.starts_with("working · 2m")));
        assert!(panes.iter().any(|(s, line)| *s == RowState::None && line.contains('\\')));
    }
```
Add `visible_rows_from_snapshot` as a tiny test-only constructor if the harness cannot attach a snapshot to `WorkspaceSidebar`; never duplicate `visible_rows` logic in the test.

- [ ] **Step 2: Verify the test fails**

Run: `cargo test -p orion-ui sidebar::tests::visible_rows_derives_state_and_status_line --lib`
Expected: FAIL — fields missing.

- [ ] **Step 3: Derive state in `visible_rows()`**

After building `group` (line 250), compute:
```rust
            let states: Vec<session_status::RowState> = group
                .iter()
                .map(|agent| {
                    let is_shell = pane_field(panes, &agent.pane_id, "pane_kind") == Some("shell");
                    let raw = pane_field(panes, &agent.pane_id, "agent_status");
                    session_status::row_state(raw, is_shell, self.runtime_online)
                })
                .collect();
            let agent_count = group
                .iter()
                .filter(|agent| pane_field(panes, &agent.pane_id, "pane_kind") != Some("shell"))
                .count();
            let header_state = session_status::aggregate(&states);
            let working = session_status::working_count(&states);
```
Pass these into `TreeRow::Header` at line 255. Then inside the pane loop (line 263), use the precomputed state by index, and build:
```rust
                let started_at = pane_field(panes, &agent.pane_id, "started_at");
                let elapsed = session_status::elapsed_label(
                    started_at,
                    chrono::Utc::now(),
                );
                let status_line = match state {
                    session_status::RowState::Working => format!(
                        "working{}",
                        elapsed.map(|e| format!(" · {e}")).unwrap_or_default()
                    ),
                    session_status::RowState::Idle => format!(
                        "idle{}",
                        elapsed.map(|e| format!(" · {e}")).unwrap_or_default()
                    ),
                    session_status::RowState::None => truncate_middle(&subtitle, 8),
                    session_status::RowState::Offline => "offline".to_string(),
                };
```
Pass `state` and `status_line` into `TreeRow::Pane`.

- [ ] **Step 4: Render the fixed state slot and status line**

In the pane branch (`sidebar.rs:844-978`), replace the `status_indicator` child at line 909 with a fixed-width slot:
```rust
                            .child(
                                div()
                                    .id(format!("sidebar-pane-{}-state", agent.pane_id))
                                    .test_support()
                                    .w(px(MARK_SIZE_PX))
                                    .flex_none()
                                    .flex()
                                    .justify_center()
                                    .child(match state {
                                        session_status::RowState::Working => self
                                            .status_indicator(&agent.pane_id, AgentStatus::Working, theme),
                                        session_status::RowState::Idle => self
                                            .status_indicator(&agent.pane_id, AgentStatus::Idle, theme),
                                        session_status::RowState::None => div().into_any_element(),
                                        session_status::RowState::Offline => Icon::new(IconName::Warning)
                                            .size(px(ICON_SIZE_PX))
                                            .text_color(theme.warning_amber)
                                            .into_any_element(),
                                    }),
                            )
```
Replace the subtitle child (lines 959-977) with the status line:
```rust
                    .child(
                        div()
                            .id(format!("sidebar-pane-{}-status-line", agent.pane_id))
                            .test_support()
                            .aria_label(status_line.clone())
                            .flex_none()
                            .h(px(theme.font_ui_secondary() + 4.0))
                            .w_full()
                            .min_w(px(0.0))
                            .truncate()
                            .font_family(MONO)
                            .text_size(px(theme.font_ui_secondary()))
                            .text_color(theme.token_text_muted())
                            .child(if rail { String::new() } else { status_line.clone() }),
                    )
```
Remove the now-unused `subtitle` field binding from the render pattern only if the compiler warns; keep the field on the row for path truncation.

- [ ] **Step 5: Keep the pulse driven by working rows**

`has_animating_status()` (`sidebar.rs:1212`) already reads `agent_status` from raw snapshot rows and pulses for `Working | Blocked`. No change needed. Confirm offline rows do not pulse because no status is animated when the snapshot is stale.

- [ ] **Step 6: Run tests and gates**

Run: `cargo test -p orion-ui sidebar::tests --lib && cargo check -p orion-ui && cargo clippy -p orion-ui -- -D warnings && cargo fmt --check`
Expected: PASS, no unused-field warnings.

---

### Task 6: Render the project aggregate and working count

**Files:**
- Modify: `crates/orion-ui/src/sidebar.rs` (`render_tree_row` header branch at 730-843)
- Test: `crates/orion-ui/src/sidebar.rs` test module

**Interfaces:**
- Consumes: `TreeRow::Header { state, working, agent_count, .. }` from Task 5.
- Produces: project row indicator + count, visible when collapsed.

- [ ] **Step 1: Write failing tests for count visibility**

```rust
    #[test]
    fn header_count_hidden_for_single_agent() {
        let label = header_count(1, 1);
        let hidden = header_count(1, 2);
        assert_eq!(label, String::new());
        assert_eq!(hidden, "1");
    }
```
Define `fn header_count(working: usize, agent_count: usize) -> String` in `sidebar.rs`:
```rust
fn header_count(working: usize, agent_count: usize) -> String {
    if agent_count <= 1 {
        String::new()
    } else {
        working.to_string()
    }
}
```

- [ ] **Step 2: Verify the test fails**

Run: `cargo test -p orion-ui sidebar::tests::header_count_hidden_for_single_agent --lib`
Expected: FAIL — unresolved helper.

- [ ] **Step 3: Replace the pane count with the aggregate**

In the header branch, delete the `count` string at lines 744-747 and add a fixed-width indicator before the chevron:
```rust
                let count = header_count(*working, *agent_count);
```
Insert a state slot as the first `.child(...)` after `.aria_selected(workspace.focused)`:
```rust
                    .child(
                        div()
                            .id(format!("sidebar-workspace-{}-state", workspace.id))
                            .test_support()
                            .w(px(MARK_SIZE_PX))
                            .flex_none()
                            .flex()
                            .justify_center()
                            .child(match state {
                                session_status::RowState::Working => self
                                    .status_indicator(&workspace.id, AgentStatus::Working, theme),
                                session_status::RowState::Idle => self
                                    .status_indicator(&workspace.id, AgentStatus::Idle, theme),
                                session_status::RowState::None => div().into_any_element(),
                                session_status::RowState::Offline => Icon::new(IconName::Warning)
                                    .size(px(ICON_SIZE_PX))
                                    .text_color(theme.warning_amber)
                                    .into_any_element(),
                            }),
                    )
```
Replace the count child (lines 815-828) with:
```rust
                    .child(
                        div()
                            .id(format!("sidebar-workspace-{}-count", workspace.id))
                            .test_support()
                            .aria_label(count.clone())
                            .flex_none()
                            .text_size(px(theme.font_ui_secondary()))
                            .text_color(theme.token_text_muted())
                            .child(if rail { String::new() } else { count }),
                    )
```
Keep the `+` button and `render_row_controls` as-is.

- [ ] **Step 4: Make the open project's background explicit**

The existing `.when(workspace.focused, |el| el.bg(theme.selected_bg()))` at line 768 already satisfies the filled-background requirement. Leave it; add a test that `workspace.focused` produces `selected_bg` using the neighbouring draw-test harness.

- [ ] **Step 5: Run tests and gates**

Run: `cargo test -p orion-ui sidebar::tests --lib && cargo check -p orion-ui && cargo clippy -p orion-ui -- -D warnings && cargo fmt --check`
Expected: PASS, clean.

---

### Task 7: Chrome fixes — one pink border, hover-only menus, tooltips, tab clip

**Files:**
- Modify: `crates/orion-ui/src/sidebar.rs:618-717` (row controls), `sidebar.rs:877-893` (selected row)
- Modify: `crates/orion-ui/src/app.rs:1684-1856` (tab strip)
- Test: `crates/orion-ui/src/sidebar.rs` and `app.rs` test modules

**Interfaces:**
- Consumes: nothing new.
- Produces: one rose border in the sidebar, hover-only `Ellipsis`, unclipped tab strip.

- [ ] **Step 1: Write a failing test for hover-only menu**

Following the neighbouring hover test pattern:
```rust
    #[test]
    fn row_menu_hidden_until_hover() {
        // Render a pane row with hovered_row = None; assert the
        // "sidebar-pane-...-menu" Button is invisible, then set hovered_row and
        // assert it is visible.
    }
```
Assert via the existing `TestSupportExt` element-id lookup used in `sidebar.rs` tests.

- [ ] **Step 2: Verify the test fails**

Run: `cargo test -p orion-ui sidebar::tests::row_menu_hidden_until_hover --lib`
Expected: FAIL — menu currently always visible.

- [ ] **Step 3: Gate the Ellipsis button on hover/focus**

In `render_row_controls()` (`sidebar.rs:630-658`), wrap the Ellipsis `Button` in:
```rust
                div().when(!hovered, |el| el.invisible()).child(
                    Button::new(format!("{prefix}-menu"))
                        // ... unchanged body
                )
```
Keep the close button's existing `!hovered` gating unchanged. If the menu is open (`menu_open`), force visibility so the popover remains reachable: change the condition to `.when(!hovered && !menu_open, |el| el.invisible())`.

- [ ] **Step 4: Remove the duplicate pink border**

In `render_tree_row` pane branch, change lines 877-881 from:
```rust
                    .when(selected, |el| {
                        el.bg(theme.selected_bg())
                            .border_1()
                            .border_color(theme.accent_emphasis)
                    })
```
to:
```rust
                    .when(selected, |el| el.bg(theme.selected_bg()))
```
Keep only the rose focus ring inside `.when(focused, ...)` (lines 882-893). This leaves exactly one rose border on the focused row.

- [ ] **Step 5: Fix the clipped tab strip**

In `app.rs` `render_tab_strip()`, change the suffix buttons' `min_h(px(TOUCH_TARGET_PX))` to `min_h(px(TOUCH_TARGET_PX * 0.75))` for the three tab suffix buttons (lines 1761, 1780, 1807) and the new-tab button (1837), and add `.h(px(TOUCH_TARGET_PX))` to the `TabBar` itself (after `.py_1()` at line 1724). This gives the bar a fixed 44px height while its buttons fit inside at 33px. Add a draw test asserting the tab strip's bounds contain the suffix button bounds at both density scales.

- [ ] **Step 6: Verify footer tooltips**

Read `render_bottom_bar()` (`sidebar.rs:1417-1441`); both buttons already pass `tooltip` through `action_button()` (`1245-1262`). Add a test asserting the tooltip strings "Chat Lens" and "Terminal Lens" are present in the rendered footer. Change copy only if the test shows a mismatch.

- [ ] **Step 7: Run tests and gates**

Run: `cargo test -p orion-ui sidebar::tests --lib && cargo check -p orion-ui && cargo clippy -p orion-ui -- -D warnings && cargo fmt --check`
Expected: PASS, clean.

---

### Task 8: Full verification and visual evidence

**Files:**
- No source changes; evidence under `docs/visual/` if screenshots are captured.

**Interfaces:**
- Consumes: all prior tasks.
- Produces: a verification record in the final response.

- [ ] **Step 1: Run all gates**

Run in order:
```bash
cargo check -p orion-core
cargo check -p orion-ui
cargo test -p orion-core
cargo test -p orion-ui --lib
cargo clippy -p orion-ui -- -D warnings
cargo fmt --check
```
Expected: all exit 0. If `cargo test -p orion-ui --lib` hits the documented host rustc OOM, record it as an environment note and run the individual module suites (`session_status::tests`, `sidebar::tests`) instead; do not delete or skip tests.

- [ ] **Step 2: Exercise the real UI**

Run `cargo run --bin orion-ui`. Confirm visually:
  - one working agent shows spinner + `working · <elapsed>`;
  - one idle agent shows muted dot + `idle · <elapsed>`;
  - a shell row shows an empty slot and a middle-truncated path;
  - the project row shows an aggregate, visible when collapsed, with a working count only for multi-agent projects;
  - the open project has a filled header, the focused session row has exactly one pink border;
  - `...` appears only on hover;
  - the tab strip is not clipped;
  - with the runtime stopped, rows and header show the offline warning and never `idle`.

- [ ] **Step 3: Record evidence**

Store screenshots in `docs/visual/screenshots/` using the repo's existing naming convention if visual QA tooling is available; otherwise record observations in the final response. Do not add a new documentation file.
