//! Real-data sidebar (P4): replaces the old static `workspace.rs` entries
//! ("w1"/"t1"/"p1") with the live engine snapshot.
//!
//! `GET /api/session` returns `{snapshot: {workspaces, tabs, panes,
//! focused_pane_id, ...}}` (see `crates/orion-core/src/engine/session.rs`
//! `snapshot_value`): workspaces carry `workspace_id`/`label`/
//! `active_tab_id`/`agent_status`, tabs carry `tab_id`/`workspace_id`/
//! `label`/`agent_status`, panes carry `pane_id`/`workspace_id`/`tab_id`/
//! `label`/`display_agent`/`agent_status`/`focused`. This view polls that
//! seam on a 5s timer (the `ChatView` pattern) and renders one row per
//! workspace — leading agent mark, title, trailing status dot (DESIGN.md
//! §1's single-row sidebar, Quiet Slate theme tokens).
//!
//! Click-to-focus: a workspace focuses its focused/active pane, a tab
//! focuses its first pane, a pane focuses itself — all via
//! `POST /api/pane/focus` with `{pane_id}`. Rose marks the live edge
//! alone: selection uses the neutral `--bg-hover` fill, never a rose wash
//! (DESIGN.md §1); agent states carry the remaining saturated colors and
//! none of them is rose.
//!
//! `workspace.rs` now re-exports this module's `WorkspaceSidebar` so
//! existing imports keep working.

use crate::agents::{agent_rows, AgentRow};
use crate::session_status;
use crate::shell_model::ShellLayout;
use crate::theme::{Theme, DOT_SIZE_PX, ICON_SIZE_PX, MARK_SIZE_PX, TOUCH_TARGET_PX};
use gpui_kit::assets::IconName;
use gpui_kit::base::{v_virtual_list, VirtualListScrollHandle};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{button::Button, button::ButtonVariants, Icon, ThemeStyled, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::test::TestSupportExt;
use gpui_kit::*;
use gpui_kit::{div, px, Context, FocusHandle, Render, Styled, Window};
use orion_core::protocol::AgentStatus;
use orion_core::runtime::Runtime;
use orion_core::session_state::SessionState;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

gpui_kit::actions!(sidebar, [FocusSidebarSearch]);

// Spacing scale from DESIGN.md §4 (4px base). The Rust theme exposes density
// scaling but no spacing tokens, so keep the scale named and local instead of
// repeating bare literals across the view. Radii are the §4 chip/control tokens.
const SPACE_1: f32 = 4.0; // --space-1: tight icon and control gaps
const SPACE_2: f32 = 8.0; // --space-2: row and menu gaps
const SPACE_3: f32 = 12.0; // --space-3: control padding
const SPACE_4: f32 = 16.0; // --space-4: section padding
const RADIUS_SM: f32 = 6.0; // --radius-sm: chips and inner controls
const HEADER_ROW_PX: f32 = 30.0;
const PANE_ROW_PX: f32 = 48.0;
const MONO: &str = "JetBrains Mono";

fn truncate_middle(s: &str, max_chars: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars {
        return s.to_string();
    }
    if max_chars <= 1 {
        return "…".chars().take(max_chars).collect();
    }
    let inner = max_chars - 1;
    if let Some(last_sep) = chars.iter().rposition(|c| *c == '\\' || *c == '/') {
        let tail_len = chars.len() - last_sep;
        if tail_len < inner {
            let head_len = inner - tail_len;
            let mut out: String = chars[..head_len].iter().collect();
            out.push('…');
            out.extend(chars[last_sep..].iter());
            return out;
        }
    }
    let head = inner / 2;
    let tail = inner - head;
    let mut out: String = chars[..head].iter().collect();
    out.push('…');
    out.extend(chars[chars.len() - tail..].iter());
    out
}

/// One workspace row of the sidebar: mark + title + status on a single row.
#[derive(Debug, Clone)]
pub struct WorkspaceEntry {
    pub id: String,
    pub label: String,
    pub agent: String,
    pub status: AgentStatus,
    pub focused: bool,
    /// Pane to focus on click (the workspace's focused pane, else its first).
    pub focus_pane: Option<String>,
    /// The workspace's first pane cwd, used to seed the New Workspace picker.
    pub cwd: Option<String>,
}

/// Lifecycle requests the sidebar raises for the app to run through the
/// Engine Route Table. Emitted as typed events so the app root re-renders
/// immediately; the sidebar never owns the modal or a second copy of state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarEvent {
    NewWorkspace,
    CloseWorkspace(String),
    /// A click focused this pane; the app re-attaches its lens views.
    PaneFocused(String),
    OpenSettings(Option<FocusHandle>),
    ToggleSidebar,
    TerminalLens,
    ChatLens,
    PaneRenamed(String),
    WorkspaceRenamed(String),
    PaneClosed(String),
    PaneCreated(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RowTarget {
    Pane(String),
    Workspace(String),
}

impl gpui_kit::EventEmitter<SidebarEvent> for WorkspaceSidebar {}

/// One tab row (shown under its workspace; also what the app's tab strip
/// reads from the snapshot).
#[derive(Debug, Clone)]
pub struct TabEntry {
    pub id: String,
    pub label: String,
    pub number: u32,
    pub status: AgentStatus,
    pub focused: bool,
    /// Pane ids in the engine's tab order (not HashMap order).
    pub pane_ids: Vec<String>,
    /// The pane the tab last had selected, when it is still open.
    pub active_pane: Option<String>,
    pub layout: String,
}

enum TreeRow {
    Header {
        workspace: WorkspaceEntry,
        collapsed: bool,
        state: session_status::RowState,
        working: usize,
        agent_count: usize,
    },
    Pane {
        agent: AgentRow,
        brand: IconName,
        state: session_status::RowState,
        status_line: String,
    },
}

fn header_count(working: usize, agent_count: usize) -> String {
    if agent_count <= 1 {
        String::new()
    } else {
        working.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TreeCursor {
    Header(String),
    Pane(String),
}

/// The sidebar view. State is the last session snapshot; `refresh()` swaps
/// it in when the wire body changes (stale rows are swept with it).
pub struct WorkspaceSidebar {
    engine: Option<Arc<dyn Runtime>>,
    runtime_connected: bool,
    runtime_online: bool,
    snapshot: Option<Value>,
    last_body: Option<String>,
    focus_handle: FocusHandle,
    search_input: Entity<InputState>,
    _search_subscription: Subscription,
    pulse_phase: u32,
    collapsed_workspaces: HashSet<String>,
    cursor: Option<TreeCursor>,
    open_menu: Option<RowTarget>,
    inline_edit: Option<RowTarget>,
    hovered_row: Option<RowTarget>,
    rename_input: Entity<InputState>,
    _rename_subscription: Subscription,
    scroll: VirtualListScrollHandle,
}

impl WorkspaceSidebar {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let subscription = cx.subscribe(
            &search_input,
            |_this, _input: Entity<InputState>, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            },
        );
        let rename_input = cx.new(|cx| InputState::new(window, cx));
        let rename_subscription = cx.subscribe(
            &rename_input,
            |_this, _input: Entity<InputState>, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            },
        );
        let view = Self {
            engine: None,
            runtime_connected: true,
            runtime_online: true,
            snapshot: None,
            last_body: None,
            focus_handle: cx.focus_handle().tab_stop(true),
            search_input,
            _search_subscription: subscription,
            pulse_phase: 0,
            collapsed_workspaces: HashSet::new(),
            cursor: None,
            open_menu: None,
            inline_edit: None,
            hovered_row: None,
            rename_input,
            _rename_subscription: rename_subscription,
            scroll: VirtualListScrollHandle::new(),
        };
        view.start_poll(cx);
        view.start_pulse(cx);
        view
    }

    pub fn attach(&mut self, engine: Arc<dyn Runtime>) {
        self.engine = Some(engine);
        self.refresh();
    }

    pub fn set_runtime_online(&mut self, online: bool, cx: &mut Context<Self>) {
        self.runtime_connected = online;
        if self.runtime_online != online {
            self.runtime_online = online;
            cx.notify();
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus_handle
    }

    pub fn focus_search(&self, window: &mut Window, cx: &mut App) {
        let handle = self.search_input.read(cx).focus_handle(cx);
        handle.focus(window, cx);
    }

    fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    /// The snapshot body (`snapshot` object) backing the current render.
    pub fn snapshot(&self) -> Option<Value> {
        self.snapshot.clone()
    }

    /// Opaque token that changes only when the session body changes. The app
    /// compares it on sidebar notifies to skip cache rebuilds on pulse ticks.
    pub fn snapshot_fingerprint(&self) -> Option<&str> {
        self.last_body.as_deref()
    }

    fn visible_rows(&self, query: &str) -> Vec<TreeRow> {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return Vec::new();
        };
        let agents = agent_rows(snapshot);
        let panes = snapshot.get("panes").and_then(Value::as_array);
        let needle = query.trim().to_lowercase();
        let searching = !needle.is_empty();
        let mut rows = Vec::new();

        for ws in self.workspaces() {
            let mut pane_ids: Vec<String> = self
                .tabs(&ws.id)
                .into_iter()
                .flat_map(|tab| tab.pane_ids)
                .collect();
            let mut seen = HashSet::new();
            pane_ids.retain(|id| seen.insert(id.clone()));
            let group: Vec<&AgentRow> = pane_ids
                .iter()
                .filter_map(|id| agents.iter().find(|agent| &agent.pane_id == id))
                .filter(|agent| matches_query(agent, panes, &needle))
                .collect();
            if searching && group.is_empty() {
                continue;
            }
            let collapsed = !searching && self.collapsed_workspaces.contains(&ws.id);
            let is_shell = |pane_id: &str| {
                pane_field(panes, pane_id, "pane_kind") == Some("shell")
                    || pane_field(panes, pane_id, "shell_path").is_some()
                    || matches!(
                        pane_field(panes, pane_id, "agent"),
                        Some("powershell" | "pwsh" | "bash" | "cmd" | "zsh")
                    )
            };
            let states: Vec<session_status::RowState> = group
                .iter()
                .map(|agent| {
                    session_status::row_state(
                        pane_field(panes, &agent.pane_id, "agent_status"),
                        is_shell(&agent.pane_id),
                        self.runtime_online,
                    )
                })
                .collect();
            let agent_count = group
                .iter()
                .filter(|agent| !is_shell(&agent.pane_id))
                .count();
            let header_state = session_status::aggregate(&states);
            let working = session_status::working_count(&states);
            rows.push(TreeRow::Header {
                workspace: ws,
                collapsed,
                state: header_state,
                working,
                agent_count,
            });
            if collapsed {
                continue;
            }
            for (agent, state) in group.iter().zip(states.iter()) {
                let subtitle = pane_field(panes, &agent.pane_id, "cwd")
                    .or_else(|| pane_field(panes, &agent.pane_id, "shell_path"))
                    .or_else(|| pane_field(panes, &agent.pane_id, "shell"))
                    .unwrap_or_default()
                    .to_string();
                let brand = if is_shell(&agent.pane_id) {
                    IconName::SquareTerminal
                } else {
                    IconName::Bot
                };
                let elapsed = session_status::elapsed_label(
                    pane_field(panes, &agent.pane_id, "started_at"),
                    chrono::Utc::now(),
                );
                let suffix = elapsed
                    .map(|value| format!(" · session {value}"))
                    .unwrap_or_default();
                let status_line = match state {
                    session_status::RowState::Working => format!("working{suffix}"),
                    session_status::RowState::Idle => format!("idle{suffix}"),
                    session_status::RowState::None => truncate_middle(&subtitle, 10),
                    session_status::RowState::Offline => "offline".to_string(),
                };
                rows.push(TreeRow::Pane {
                    agent: (*agent).clone(),
                    brand,
                    state: *state,
                    status_line,
                });
            }
        }
        rows
    }

    fn row_target(row: &TreeRow) -> (TreeCursor, &str) {
        match row {
            TreeRow::Header { workspace, .. } => (
                TreeCursor::Header(workspace.id.clone()),
                workspace.id.as_str(),
            ),
            TreeRow::Pane { agent, .. } => (
                TreeCursor::Pane(agent.pane_id.clone()),
                agent.workspace_id.as_str(),
            ),
        }
    }

    fn cursor_index(&self, rows: &[TreeRow]) -> Option<usize> {
        let cursor = self.cursor.as_ref()?;
        rows.iter()
            .position(|row| Self::row_target(row).0 == *cursor)
    }

    fn apply_workspace_collapsed(&mut self, id: &str, collapsed: bool) -> bool {
        if self.collapsed_workspaces.contains(id) == collapsed {
            return false;
        }
        if collapsed {
            self.collapsed_workspaces.insert(id.to_string());
        } else {
            self.collapsed_workspaces.remove(id);
        }
        true
    }

    fn toggle_workspace(&mut self, id: &str, cx: &mut Context<Self>) {
        let collapsed = !self.collapsed_workspaces.contains(id);
        if self.apply_workspace_collapsed(id, collapsed) {
            cx.notify();
        }
    }

    fn http(&self, method: &str, path: &str, body: &Value) {
        if let Some(engine) = self.engine.clone() {
            let _ = engine.http(method, path, body);
        }
    }

    fn row_info(&self, target: &RowTarget, query: &str) -> Option<(String, AgentStatus)> {
        let rows = self.visible_rows(query);
        rows.iter().find_map(|row| match (target, row) {
            (RowTarget::Pane(id), TreeRow::Pane { agent, .. }) if id == &agent.pane_id => {
                Some((agent.title.clone(), agent.status))
            }
            (RowTarget::Workspace(id), TreeRow::Header { workspace, .. })
                if id == &workspace.id =>
            {
                Some((workspace.label.clone(), workspace.status))
            }
            _ => None,
        })
    }

    fn set_hovered(&mut self, target: &RowTarget, hovered: bool, cx: &mut Context<Self>) {
        let next = hovered.then(|| target.clone());
        if self.hovered_row != next {
            self.hovered_row = next;
            cx.notify();
        }
    }

    fn open_rename(&mut self, target: RowTarget, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.search_input.read(cx).value().to_string();
        let title = self
            .row_info(&target, &query)
            .map(|(title, _)| title)
            .unwrap_or_default();
        self.rename_input.update(cx, |input, cx| {
            input.set_value(title, window, cx);
            input.select_all(window, cx);
        });
        self.inline_edit = Some(target);
        self.open_menu = None;
        self.rename_input
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();
    }

    fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        self.inline_edit = None;
        self.open_menu = None;
        cx.notify();
    }

    fn commit_rename(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.inline_edit.take() else {
            return;
        };
        let label = self.rename_input.read(cx).value().trim().to_string();
        if !label.is_empty() {
            match &target {
                RowTarget::Pane(id) => {
                    self.http(
                        "POST",
                        "/api/pane/rename",
                        &json!({ "pane_id": id, "label": label }),
                    );
                    cx.emit(SidebarEvent::PaneRenamed(id.clone()));
                }
                RowTarget::Workspace(id) => {
                    self.http(
                        "POST",
                        "/api/workspace/rename",
                        &json!({ "workspace_id": id, "label": label }),
                    );
                    cx.emit(SidebarEvent::WorkspaceRenamed(id.clone()));
                }
            }
            self.refresh();
        }
        self.open_menu = None;
        cx.notify();
    }

    fn perform_close(&mut self, target: &RowTarget, cx: &mut Context<Self>) {
        match target {
            RowTarget::Pane(id) => {
                self.http("POST", "/api/pane/close", &json!({ "pane_id": id }));
                self.refresh();
                cx.emit(SidebarEvent::PaneClosed(id.clone()));
            }
            RowTarget::Workspace(id) => {
                self.http(
                    "POST",
                    "/api/workspace/close",
                    &json!({ "workspace_id": id }),
                );
                self.refresh();
                cx.emit(SidebarEvent::CloseWorkspace(id.clone()));
            }
        }
        cx.notify();
    }

    fn request_close(&mut self, target: RowTarget, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.search_input.read(cx).value().to_string();
        let (label, status) = self
            .row_info(&target, &query)
            .unwrap_or((String::new(), AgentStatus::Unknown));
        let running = session_status::state_of(status).is_working();
        self.open_menu = None;
        if !running {
            self.perform_close(&target, cx);
            return;
        }
        let entity = cx.entity();
        let pane = matches!(target, RowTarget::Pane(_));
        let title = if pane {
            "Close running agent?".to_string()
        } else {
            "Close running session?".to_string()
        };
        let description = if pane {
            "The agent is still running. Closing it will terminate it.".to_string()
        } else {
            format!("{label} is still running. Closing it will terminate the agent.")
        };
        window.open_alert_dialog(cx, move |alert, _, _| {
            let entity = entity.clone();
            let target = target.clone();
            let title = title.clone();
            let description = description.clone();
            alert
                .title(title)
                .description(description)
                .confirm()
                .ok_text("Close")
                .on_ok(move |_, _, app| {
                    entity.update(app, |this, cx| this.perform_close(&target, cx));
                    true
                })
        });
        cx.notify();
    }

    fn create_pane(&mut self, workspace_id: &str, cx: &mut Context<Self>) {
        let workspaces = self.workspaces();
        let workspace = if workspace_id.is_empty() {
            workspaces
                .iter()
                .find(|workspace| workspace.focused)
                .or_else(|| workspaces.first())
        } else {
            workspaces
                .iter()
                .find(|workspace| workspace.id == workspace_id)
        };
        let Some(workspace) = workspace.cloned() else {
            return;
        };
        let next = self
            .tabs(&workspace.id)
            .iter()
            .map(|tab| tab.number)
            .max()
            .unwrap_or(0)
            + 1;
        self.http(
            "POST",
            "/api/tab/create",
            &json!({
                "workspace_id": workspace.id,
                "agent": { "kind": workspace.agent },
                "label": format!("Tab {next}"),
            }),
        );
        self.refresh();
        cx.emit(SidebarEvent::PaneCreated(workspace.id.clone()));
        cx.notify();
    }

    fn move_cursor(&mut self, rows: &[TreeRow], index: usize) {
        let (cursor, _) = Self::row_target(&rows[index]);
        self.cursor = Some(cursor);
        self.scroll.scroll_to_item(index, ScrollStrategy::Top);
    }

    fn handle_tree_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let query = self.search_input.read(cx).value().to_string();
        let rows = self.visible_rows(&query);
        if rows.is_empty() {
            return;
        }
        let searching = !query.trim().is_empty();
        match event.keystroke.key.as_str() {
            "down" => {
                let index = match self.cursor_index(&rows) {
                    Some(current) => (current + 1).min(rows.len() - 1),
                    None => 0,
                };
                self.move_cursor(&rows, index);
                cx.notify();
            }
            "up" => {
                let index = match self.cursor_index(&rows) {
                    Some(current) => current.saturating_sub(1),
                    None => rows.len() - 1,
                };
                self.move_cursor(&rows, index);
                cx.notify();
            }
            "escape" => self.clear_search(window, cx),
            "enter" => {
                let Some(index) = self.cursor_index(&rows) else {
                    return;
                };
                match &rows[index] {
                    TreeRow::Header { workspace, .. } if !searching => {
                        self.toggle_workspace(&workspace.id, cx);
                    }
                    TreeRow::Header { .. } => {}
                    TreeRow::Pane { agent, .. } => {
                        let pane_id = agent.pane_id.clone();
                        self.focus_pane(&pane_id);
                        cx.emit(SidebarEvent::PaneFocused(pane_id));
                        cx.notify();
                    }
                }
            }
            "left" | "right" => {
                let Some(index) = self.cursor_index(&rows) else {
                    return;
                };
                if searching {
                    return;
                }
                let (_, workspace_id) = Self::row_target(&rows[index]);
                let workspace_id = workspace_id.to_string();
                let collapse = event.keystroke.key == "left";
                if !self.apply_workspace_collapsed(&workspace_id, collapse) {
                    return;
                }
                if collapse && matches!(rows[index], TreeRow::Pane { .. }) {
                    self.cursor = Some(TreeCursor::Header(workspace_id));
                }
                cx.notify();
            }
            _ => {}
        }
    }

    fn status_indicator(
        &self,
        pane_id: &str,
        status: AgentStatus,
        theme: &Theme,
    ) -> gpui_kit::AnyElement {
        let base = div()
            .id(format!("sidebar-pane-{pane_id}-status"))
            .test_support()
            .role(gpui_kit::Role::Image)
            .aria_label(session_status::badge_word(session_status::state_of(status)))
            .size(px(MARK_SIZE_PX))
            .flex_none()
            .flex()
            .items_center()
            .justify_center();
        match session_status::state_of(status) {
            SessionState::Starting | SessionState::Running | SessionState::NeedsInput => base
                .child(
                    div()
                        .size(px(DOT_SIZE_PX))
                        .rounded_full()
                        .bg(Self::status_color(status, theme, self.pulse_phase)),
                )
                .into_any_element(),
            _ => base
                .child(
                    div()
                        .size(px(DOT_SIZE_PX))
                        .rounded_full()
                        .bg(theme.status_idle),
                )
                .into_any_element(),
        }
    }

    fn aggregate_indicator(
        &self,
        workspace_id: &str,
        state: session_status::RowState,
        theme: &Theme,
    ) -> gpui_kit::AnyElement {
        match state {
            session_status::RowState::Working => div()
                .id(format!("sidebar-workspace-{workspace_id}-state-status"))
                .test_support()
                .role(gpui_kit::Role::Image)
                .aria_label("Project working")
                .size(px(DOT_SIZE_PX))
                .rounded_full()
                .bg(Self::status_color(
                    AgentStatus::Working,
                    theme,
                    self.pulse_phase,
                ))
                .into_any_element(),
            session_status::RowState::Idle => div()
                .id(format!("sidebar-workspace-{workspace_id}-state-status"))
                .test_support()
                .role(gpui_kit::Role::Image)
                .aria_label("Project idle")
                .size(px(DOT_SIZE_PX))
                .rounded_full()
                .bg(theme.status_idle)
                .into_any_element(),
            session_status::RowState::None => div().into_any_element(),
            session_status::RowState::Offline => Icon::new(IconName::TriangleAlert)
                .size(px(ICON_SIZE_PX))
                .text_color(theme.warning_amber)
                .into_any_element(),
        }
    }

    fn rename_field(&self, id: &str, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id(format!("{id}-rename-input"))
            .test_support()
            .flex_1()
            .min_w(px(0.0))
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                if ev.keystroke.key == "escape" {
                    this.cancel_rename(cx);
                } else if ev.keystroke.key == "enter" {
                    this.commit_rename(cx);
                }
            }))
            .child(Input::new(&self.rename_input).w_full())
    }

    fn render_row_controls(
        &self,
        target: &RowTarget,
        scale: f32,
        reveal: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let prefix = match target {
            RowTarget::Pane(id) => format!("sidebar-pane-{id}"),
            RowTarget::Workspace(id) => format!("sidebar-workspace-{id}"),
        };
        let hovered = self.hovered_row.as_ref() == Some(target);
        let menu_open = self.open_menu.as_ref() == Some(target);
        let revealed = hovered || menu_open || reveal;
        div()
            .absolute()
            .right_0()
            .top_0()
            .h(px(TOUCH_TARGET_PX))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.0 * scale))
            .child(
                div().when(!revealed, |el| el.invisible()).child(
                    Button::new(format!("{prefix}-menu"))
                        .ghost()
                        .icon(IconName::Ellipsis)
                        .tooltip("Actions")
                        .min_h(px(TOUCH_TARGET_PX))
                        .min_w(px(TOUCH_TARGET_PX))
                        .on_click(cx.listener({
                            let target = target.clone();
                            move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.open_menu = if this.open_menu.as_ref() == Some(&target) {
                                    None
                                } else {
                                    Some(target.clone())
                                };
                                cx.notify();
                            }
                        })),
                ),
            )
            .child(
                div().when(!revealed, |el| el.invisible()).child(
                    Button::new(format!("{prefix}-close"))
                        .ghost()
                        .icon(IconName::X)
                        .tooltip("Close")
                        .min_h(px(TOUCH_TARGET_PX))
                        .min_w(px(TOUCH_TARGET_PX))
                        .on_click(cx.listener({
                            let target = target.clone();
                            move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.request_close(target.clone(), window, cx);
                            }
                        })),
                ),
            )
            .when(menu_open, |el| {
                el.child(
                    div()
                        .id(format!("{prefix}-menu-items"))
                        .test_support()
                        .absolute()
                        .top_full()
                        .right_0()
                        .flex()
                        .flex_col()
                        .bg(Theme::current().panel_bg())
                        .child(
                            Button::new(format!("{prefix}-menu-rename"))
                                .ghost()
                                .label("Rename")
                                .min_h(px(TOUCH_TARGET_PX))
                                .min_w(px(TOUCH_TARGET_PX))
                                .on_click(cx.listener({
                                    let target = target.clone();
                                    move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.open_rename(target.clone(), window, cx);
                                    }
                                })),
                        )
                        .child(
                            Button::new(format!("{prefix}-menu-close"))
                                .ghost()
                                .label("Close")
                                .min_h(px(TOUCH_TARGET_PX))
                                .min_w(px(TOUCH_TARGET_PX))
                                .on_click(cx.listener({
                                    let target = target.clone();
                                    move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.request_close(target.clone(), window, cx);
                                    }
                                })),
                        ),
                )
            })
    }

    fn render_tree_row(
        &self,
        row: &TreeRow,
        cursor: Option<&TreeCursor>,
        rail: bool,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let scale = theme.density_scale();
        match row {
            TreeRow::Header {
                workspace,
                collapsed,
                state,
                working,
                agent_count,
            } => {
                let id = workspace.id.clone();
                let editing = self.inline_edit.as_ref() == Some(&RowTarget::Workspace(id.clone()));
                let focused = self.focus_handle.is_focused(window)
                    && cursor == Some(&TreeCursor::Header(id.clone()));
                let chevron = if *collapsed {
                    IconName::ChevronRight
                } else {
                    IconName::ChevronDown
                };
                let count = header_count(*working, *agent_count);
                div()
                    .id(format!("sidebar-workspace-{}", workspace.id))
                    .test_support()
                    .on_hover(cx.listener({
                        let target = RowTarget::Workspace(id.clone());
                        move |this, hovered: &bool, _, cx| {
                            this.set_hovered(&target, *hovered, cx);
                        }
                    }))
                    .h(px(HEADER_ROW_PX))
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE_2 * scale))
                    .pl(px(SPACE_2 * scale))
                    .pr(px(2.0 * TOUCH_TARGET_PX + SPACE_1 * scale))
                    .role(gpui_kit::Role::TreeItem)
                    .aria_expanded(!collapsed)
                    .aria_selected(workspace.focused)
                    .when(workspace.focused, |el| el.bg(theme.selected_bg()))
                    .when(focused, |el| {
                        el.child(
                            div()
                                .id(format!("sidebar-workspace-{id}-focus-ring"))
                                .test_support()
                                .absolute()
                                .inset(px(2.0))
                                .rounded(px(RADIUS_SM))
                                .flex_none()
                                .focus_ring_style(window, cx),
                        )
                    })
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, _, cx| {
                            if this.search_input.read(cx).value().trim().is_empty() {
                                this.toggle_workspace(&id, cx);
                            }
                        }
                    }))
                    .child(
                        div()
                            .id(format!("sidebar-workspace-{}-state", workspace.id))
                            .test_support()
                            .w(px(MARK_SIZE_PX))
                            .flex_none()
                            .flex()
                            .justify_center()
                            .child(self.aggregate_indicator(&workspace.id, *state, theme)),
                    )
                    .child(
                        Icon::new(chevron)
                            .size(px(12.0))
                            .text_color(theme.token_text_muted()),
                    )
                    .when(!editing, |el| {
                        el.child(
                            div()
                                .id(format!("sidebar-workspace-{}-label", workspace.id))
                                .test_support()
                                .aria_label(workspace.label.clone())
                                .flex_1()
                                .min_w(px(0.0))
                                .truncate()
                                .text_size(px(theme.font_ui_secondary()))
                                .text_color(theme.text_primary())
                                .child(if rail {
                                    String::new()
                                } else {
                                    workspace.label.clone()
                                }),
                        )
                    })
                    .when(editing, |el| {
                        el.child(self.rename_field(&format!("sidebar-workspace-{id}"), cx))
                    })
                    .child(
                        div()
                            .id(format!("sidebar-workspace-{}-count", workspace.id))
                            .test_support()
                            .aria_label(if count.is_empty() {
                                String::new()
                            } else {
                                format!("{count} working agents")
                            })
                            .flex_none()
                            .text_size(px(theme.font_ui_secondary()))
                            .text_color(theme.token_text_muted())
                            .child(if rail { String::new() } else { count }),
                    )
                    .child(
                        Button::new(format!("sidebar-workspace-{}-create", workspace.id))
                            .ghost()
                            .icon(IconName::Plus)
                            .tooltip("New pane")
                            .on_click(cx.listener({
                                let workspace_id = id.clone();
                                move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.create_pane(&workspace_id, cx);
                                }
                            })),
                    )
                    .child(self.render_row_controls(
                        &RowTarget::Workspace(id.clone()),
                        scale,
                        focused || rail,
                        cx,
                    ))
            }
            TreeRow::Pane {
                agent,
                brand,
                state,
                status_line,
            } => {
                let pane_id = agent.pane_id.clone();
                let editing = self.inline_edit.as_ref() == Some(&RowTarget::Pane(pane_id.clone()));
                let needs_input = matches!(agent.status, AgentStatus::Blocked);
                let selected = agent.selected;
                let focused = self.focus_handle.is_focused(window)
                    && match cursor {
                        Some(TreeCursor::Pane(id)) => id == &agent.pane_id,
                        Some(TreeCursor::Header(_)) => false,
                        None => selected,
                    };
                let keyboard_reveal = self.focus_handle.is_focused(window)
                    && matches!(cursor, Some(TreeCursor::Pane(id)) if id == &agent.pane_id);
                div()
                    .id(format!("sidebar-pane-{}", agent.pane_id))
                    .test_support()
                    .on_hover(cx.listener({
                        let target = RowTarget::Pane(pane_id.clone());
                        move |this, hovered: &bool, _, cx| {
                            this.set_hovered(&target, *hovered, cx);
                        }
                    }))
                    .role(gpui_kit::Role::Row)
                    .aria_selected(selected)
                    .h(px(PANE_ROW_PX))
                    .w_full()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .gap(px(SPACE_1 * scale))
                    .px(px(SPACE_3 * scale))
                    .when(selected, |el| el.bg(theme.selected_bg()))
                    .when(focused, |el| {
                        el.child(
                            div()
                                .id(format!("sidebar-pane-{}-focus-ring", agent.pane_id))
                                .test_support()
                                .absolute()
                                .inset(px(2.0))
                                .rounded(px(RADIUS_SM))
                                .flex_none()
                                .focus_ring_style(window, cx),
                        )
                    })
                    .on_click(cx.listener({
                        let pane_id = pane_id.clone();
                        move |this, _, _, cx| {
                            this.focus_pane(&pane_id);
                            cx.emit(SidebarEvent::PaneFocused(pane_id.clone()));
                            cx.notify();
                        }
                    }))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(SPACE_2 * scale))
                            .pr(px(96.0 * scale))
                            .child(
                                div()
                                    .id(format!("sidebar-pane-{}-state", agent.pane_id))
                                    .test_support()
                                    .w(px(MARK_SIZE_PX))
                                    .flex_none()
                                    .flex()
                                    .justify_center()
                                    .child(match state {
                                        session_status::RowState::Working
                                        | session_status::RowState::Idle => self.status_indicator(
                                            &agent.pane_id,
                                            agent.status,
                                            theme,
                                        ),
                                        session_status::RowState::None => div().into_any_element(),
                                        session_status::RowState::Offline => {
                                            Icon::new(IconName::TriangleAlert)
                                                .size(px(ICON_SIZE_PX))
                                                .text_color(theme.warning_amber)
                                                .into_any_element()
                                        }
                                    }),
                            )
                            .child(
                                div()
                                    .id(format!("sidebar-pane-{}-brand", agent.pane_id))
                                    .test_support()
                                    .aria_label(agent.agent_name.clone())
                                    .child(
                                        Icon::new(*brand)
                                            .size(px(ICON_SIZE_PX))
                                            .text_color(theme.token_text_muted()),
                                    ),
                            )
                            .when(!editing, |el| {
                                el.child(
                                    div()
                                        .id(format!("sidebar-pane-{}-title", agent.pane_id))
                                        .test_support()
                                        .aria_label(agent.title.clone())
                                        .flex_1()
                                        .min_w(px(0.0))
                                        .truncate()
                                        .text_size(px(theme.font_ui_body()))
                                        .text_color(theme.text_primary())
                                        .child(if rail {
                                            String::new()
                                        } else {
                                            agent.title.clone()
                                        }),
                                )
                            })
                            .when(editing, |el| {
                                el.child(self.rename_field(&format!("sidebar-pane-{pane_id}"), cx))
                            })
                            .when(needs_input, |el| {
                                el.child(
                                    div()
                                        .id(format!("sidebar-pane-{}-badge", agent.pane_id))
                                        .test_support()
                                        .flex_none()
                                        .text_size(px(theme.font_ui_secondary()))
                                        .text_color(theme.state_attention())
                                        .child("Needs Input"),
                                )
                            })
                            .child(self.render_row_controls(
                                &RowTarget::Pane(pane_id.clone()),
                                scale,
                                keyboard_reveal || rail,
                                cx,
                            )),
                    )
                    .child(
                        div()
                            .id(format!("sidebar-pane-{}-subtitle", agent.pane_id))
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
                            .child(if rail {
                                String::new()
                            } else {
                                status_line.clone()
                            }),
                    )
            }
        }
    }

    /// Workspaces of the current snapshot, in engine order. Each entry points
    /// at its active tab's remembered pane, so a click restores the selection
    /// the user left instead of an arbitrary pane.
    pub fn workspaces(&self) -> Vec<WorkspaceEntry> {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return Vec::new();
        };
        let panes = snapshot.get("panes").and_then(Value::as_array);
        let snapshot_tabs = snapshot.get("tabs").and_then(Value::as_array);
        let focused_pane = snapshot.get("focused_pane_id");
        snapshot
            .get("workspaces")
            .and_then(Value::as_array)
            .map(|workspaces| {
                workspaces
                    .iter()
                    .map(|ws| {
                        let id = ws
                            .get("workspace_id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let ws_tabs: Vec<&Value> = snapshot_tabs
                            .map(|tabs| {
                                tabs.iter()
                                    .filter(|tab| {
                                        tab.get("workspace_id").and_then(Value::as_str)
                                            == Some(id.as_str())
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        let active_tab = ws
                            .get("active_tab_id")
                            .and_then(Value::as_str)
                            .and_then(|active| {
                                ws_tabs.iter().copied().find(|tab| {
                                    tab.get("tab_id").and_then(Value::as_str) == Some(active)
                                })
                            })
                            .or_else(|| ws_tabs.first().copied());
                        let pane_ids = active_tab
                            .map(|tab| ordered_pane_ids(tab, panes))
                            .unwrap_or_default();
                        let focus_pane = active_tab
                            .and_then(|tab| {
                                tab.get("active_pane_id")
                                    .and_then(Value::as_str)
                                    .map(str::to_string)
                            })
                            .filter(|pane_id| pane_ids.iter().any(|id| id == pane_id))
                            .or_else(|| pane_ids.first().cloned())
                            .or_else(|| {
                                // Older snapshots only carried pane rows.
                                panes.and_then(|panes| {
                                    panes
                                        .iter()
                                        .filter(|pane| {
                                            pane.get("workspace_id").and_then(Value::as_str)
                                                == Some(id.as_str())
                                        })
                                        .find(|pane| {
                                            pane.get("focused").and_then(Value::as_bool)
                                                == Some(true)
                                        })
                                        .or_else(|| {
                                            panes.iter().find(|pane| {
                                                pane.get("workspace_id").and_then(Value::as_str)
                                                    == Some(id.as_str())
                                            })
                                        })
                                        .and_then(|pane| {
                                            pane.get("pane_id")
                                                .and_then(Value::as_str)
                                                .map(str::to_string)
                                        })
                                })
                            })
                            .or_else(|| focused_pane.and_then(Value::as_str).map(str::to_string));
                        let agent = focus_pane
                            .as_deref()
                            .and_then(|pane_id| pane_field(panes, pane_id, "agent"))
                            .unwrap_or("claude")
                            .to_string();
                        let cwd = focus_pane
                            .as_deref()
                            .and_then(|pane_id| pane_field(panes, pane_id, "cwd"))
                            .map(str::to_string);
                        WorkspaceEntry {
                            focused: ws.get("focused").and_then(Value::as_bool) == Some(true),
                            id,
                            label: ws
                                .get("label")
                                .and_then(Value::as_str)
                                .unwrap_or("Workspace")
                                .to_string(),
                            agent,
                            status: parse_status(ws.get("agent_status").and_then(Value::as_str)),
                            cwd,
                            focus_pane,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Tabs of one workspace, in engine order.
    pub fn tabs(&self, workspace_id: &str) -> Vec<TabEntry> {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return Vec::new();
        };
        let panes = snapshot.get("panes").and_then(Value::as_array);
        snapshot
            .get("tabs")
            .and_then(Value::as_array)
            .map(|tabs| {
                tabs.iter()
                    .filter(|tab| {
                        tab.get("workspace_id").and_then(Value::as_str) == Some(workspace_id)
                    })
                    .map(|tab| {
                        let tab_id = tab
                            .get("tab_id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let pane_ids = ordered_pane_ids(tab, panes);
                        let active_pane = tab
                            .get("active_pane_id")
                            .and_then(Value::as_str)
                            .filter(|pane_id| pane_ids.iter().any(|id| id == pane_id))
                            .map(str::to_string)
                            .or_else(|| pane_ids.first().cloned());
                        TabEntry {
                            focused: tab.get("focused").and_then(Value::as_bool) == Some(true),
                            id: tab_id,
                            label: tab
                                .get("label")
                                .and_then(Value::as_str)
                                .unwrap_or("Tab")
                                .to_string(),
                            number: tab.get("number").and_then(Value::as_u64).unwrap_or(1) as u32,
                            status: parse_status(tab.get("agent_status").and_then(Value::as_str)),
                            pane_ids,
                            active_pane,
                            layout: tab
                                .get("layout")
                                .and_then(Value::as_str)
                                .unwrap_or("single")
                                .to_string(),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Focus a pane through the wire seam (`POST /api/pane/focus`) and tell the
    /// app so it re-attaches its lens views before the next poll.
    pub fn focus_pane(&mut self, pane_id: &str) {
        if let Some(engine) = self.engine.clone() {
            let body = json!({ "pane_id": pane_id });
            let _ = engine.http("POST", "/api/pane/focus", &body);
        }
        self.refresh();
    }

    /// Returns whether the session actually changed, so the poll can notify
    /// only on a real update instead of relying on the pulse to repaint.
    pub fn refresh(&mut self) -> bool {
        self.refresh_session()
    }

    fn refresh_session(&mut self) -> bool {
        let Some(engine) = self.engine.clone() else {
            return self.runtime_online;
        };
        let (status, body) = engine.http("GET", "/api/session", &Value::Null);
        if status != 200 {
            self.runtime_online = false;
            return true;
        }
        self.runtime_online = self.runtime_connected;
        let encoded = body.to_string();
        if self.last_body.as_deref() == Some(encoded.as_str()) {
            return false;
        }
        self.last_body = Some(encoded);
        self.snapshot = body.get("snapshot").cloned();
        true
    }

    fn start_poll(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |view, cx| loop {
            cx.background_executor().timer(Duration::from_secs(5)).await;
            let _ = view.update(cx, |view, cx| {
                if view.refresh() {
                    cx.notify();
                }
            });
            if view.upgrade().is_none() {
                break;
            }
        })
        .detach();
    }

    fn start_pulse(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |view, cx| loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(250))
                .await;
            let _ = view.update(cx, |view, cx| {
                // Only a live status breathes. When nothing is animating, do
                // no work and do not notify: an idle shell must not repaint
                // four times a second.
                if view.has_animating_status() {
                    view.pulse_phase = view.pulse_phase.wrapping_add(1);
                    cx.notify();
                }
            });
            if view.upgrade().is_none() {
                break;
            }
        })
        .detach();
    }

    /// Whether any status the sidebar paints is animating (RUN/INPUT). The
    /// pulse timer notifies only then. Reads the same fields `render` uses,
    /// without building the row vectors.
    fn has_animating_status(&self) -> bool {
        fn is_animating(status: AgentStatus) -> bool {
            matches!(status, AgentStatus::Working | AgentStatus::Blocked)
        }
        let wire_animating = |raw: Option<&str>| {
            raw.and_then(AgentStatus::from_wire)
                .is_some_and(is_animating)
        };
        if let Some(snapshot) = self.snapshot.as_ref() {
            // Workspace cards read `agent_status` off the workspace; agent
            // cards read it off the pane. Both are painted with the pulse.
            for key in ["workspaces", "panes"] {
                if let Some(rows) = snapshot.get(key).and_then(Value::as_array) {
                    if rows
                        .iter()
                        .any(|row| wire_animating(row.get("agent_status").and_then(Value::as_str)))
                    {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Status dot color for a protocol `AgentStatus`. Never rose (DESIGN.md
    /// §1): RUN is the working cyan, INPUT the blocked coral, DONE the
    /// emerald, ERROR the indicator ruby. Live statuses breathe between
    /// alpha 0.55 and 1.0 by `pulse` (DESIGN.md: RUN = blue/cyan pulse).
    fn status_color(status: AgentStatus, theme: &Theme, pulse: u32) -> gpui_kit::Rgba {
        session_status::badge_color(session_status::state_of(status), theme, pulse)
    }

    fn action_button(
        id: &'static str,
        icon: IconName,
        tooltip: &'static str,
        event: SidebarEvent,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        Button::new(id)
            .ghost()
            .icon(icon)
            .tooltip(tooltip)
            .min_h(px(TOUCH_TARGET_PX))
            .min_w(px(TOUCH_TARGET_PX))
            .on_click(cx.listener(move |_this, _, _, cx| {
                cx.emit(event.clone());
                cx.notify();
            }))
    }

    fn render_titlebar(
        &self,
        rail: bool,
        theme: &Theme,
        scale: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id("sidebar-titlebar")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(SPACE_2 * scale))
            .h(px(40.0))
            .flex_shrink_0()
            .px(px(SPACE_4 * scale))
            .child(
                div()
                    .size(px(18.0 * scale))
                    .rounded(px(RADIUS_SM * scale))
                    .bg(theme.accent_emphasis)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .size(px(4.0 * scale))
                            .rounded_full()
                            .bg(theme.surface_canvas),
                    ),
            )
            .when(!self.runtime_online, |el| {
                el.child(
                    Button::new("sidebar-offline-warning")
                        .ghost()
                        .icon(Icon::new(IconName::TriangleAlert).size(theme.scaled_icon_size()))
                        .text_color(theme.warning_amber)
                        .tooltip(
                            "Orion could not start or reach its runtime. Terminals end when Orion closes.",
                        )
                        .min_h(px(TOUCH_TARGET_PX))
                        .min_w(px(TOUCH_TARGET_PX)),
                )
            })
            .child(
                div()
                    .text_size(px(14.0 * scale))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(theme.text_strong)
                    .child(if rail {
                        String::new()
                    } else {
                        "Orion".to_string()
                    }),
            )
            .child(div().flex_1())
            .child(
                Icon::new(IconName::Menu)
                    .size(theme.scaled_icon_size())
                    .text_color(theme.text_muted),
            )
            .child(Self::action_button(
                "sidebar-header-collapse",
                IconName::PanelLeft,
                "Collapse sidebar",
                SidebarEvent::ToggleSidebar,
                cx,
            ))
    }

    fn render_search_row(
        &self,
        rail: bool,
        theme: &Theme,
        scale: f32,
        query: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let searching = !query.trim().is_empty();
        div()
            .id("sidebar-search")
            .test_support()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(SPACE_2 * scale))
            .h(px(TOUCH_TARGET_PX))
            .flex_shrink_0()
            .px(px(SPACE_4 * scale))
            .child(
                Icon::new(IconName::Search)
                    .size(theme.scaled_icon_size())
                    .text_color(theme.text_muted),
            )
            .child(
                div()
                    .id("sidebar-search-input")
                    .test_support()
                    .flex_1()
                    .min_w(px(0.0))
                    .when(!rail, |el| {
                        el.child(Input::new(&self.search_input).w_full())
                    })
                    .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                        if ev.keystroke.key.to_lowercase() == "escape" {
                            this.clear_search(window, cx);
                        }
                    })),
            )
            .when(!rail && searching, |el| {
                el.child(
                    Button::new("sidebar-search-clear")
                        .ghost()
                        .icon(IconName::X)
                        .tooltip("Clear search")
                        .min_h(px(TOUCH_TARGET_PX))
                        .min_w(px(TOUCH_TARGET_PX))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.clear_search(window, cx);
                        })),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE_1 * scale))
                    .child(
                        Button::new("sidebar-settings")
                            .ghost()
                            .icon(IconName::Settings)
                            .tooltip("Settings")
                            .min_h(px(TOUCH_TARGET_PX))
                            .min_w(px(TOUCH_TARGET_PX))
                            .on_click(cx.listener(|this, _, window, cx| {
                                let opener = window
                                    .focused(cx)
                                    .or_else(|| Some(this.focus_handle.clone()));
                                cx.emit(SidebarEvent::OpenSettings(opener));
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("new-workspace-button")
                            .tooltip("New workspace")
                            .ghost()
                            .icon(IconName::Plus)
                            .min_h(px(TOUCH_TARGET_PX))
                            .min_w(px(TOUCH_TARGET_PX))
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.create_pane("", cx);
                            })),
                    )
                    .child(Self::action_button(
                        "sidebar-terminal-toolbar",
                        IconName::SquareTerminal,
                        "Terminal Lens",
                        SidebarEvent::TerminalLens,
                        cx,
                    )),
            )
    }

    /// The 40px bottom icon bar.
    fn render_bottom_bar(&self, scale: f32, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("sidebar-bottom-bar")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(SPACE_4 * scale))
            .h(px(40.0))
            .flex_shrink_0()
            .px(px(SPACE_4 * scale))
            .child(Self::action_button(
                "sidebar-terminal-footer",
                IconName::SquareTerminal,
                "Terminal Lens",
                SidebarEvent::TerminalLens,
                cx,
            ))
            .child(Self::action_button(
                "sidebar-chat-footer",
                IconName::LayoutDashboard,
                "Chat Lens",
                SidebarEvent::ChatLens,
                cx,
            ))
    }
}

fn parse_status(raw: Option<&str>) -> AgentStatus {
    raw.and_then(AgentStatus::from_wire)
        .unwrap_or(AgentStatus::Unknown)
}

/// The tab's pane ids in the engine's order. Snapshots carry `pane_ids`; older
/// ones only had pane rows, so fall back to filtering those by tab id.
fn ordered_pane_ids(tab: &Value, panes: Option<&Vec<Value>>) -> Vec<String> {
    if let Some(ids) = tab.get("pane_ids").and_then(Value::as_array) {
        return ids
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
    }
    let tab_id = tab.get("tab_id").and_then(Value::as_str);
    panes
        .map(|panes| {
            panes
                .iter()
                .filter(|pane| pane.get("tab_id").and_then(Value::as_str) == tab_id)
                .filter_map(|pane| {
                    pane.get("pane_id")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn pane_field<'a>(panes: Option<&'a Vec<Value>>, pane_id: &str, field: &str) -> Option<&'a str> {
    panes?
        .iter()
        .find(|pane| pane.get("pane_id").and_then(Value::as_str) == Some(pane_id))?
        .get(field)?
        .as_str()
}

fn matches_query(agent: &AgentRow, panes: Option<&Vec<Value>>, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if agent.title.to_lowercase().contains(needle) {
        return true;
    }
    ["cwd", "shell_path", "shell"].iter().any(|field| {
        pane_field(panes, &agent.pane_id, field)
            .is_some_and(|value| value.to_lowercase().contains(needle))
    })
}

pub fn focus_search(sidebar: &Entity<WorkspaceSidebar>, window: &mut Window, cx: &mut App) {
    sidebar.update(cx, |view, cx| view.focus_search(window, cx));
}

impl Render for WorkspaceSidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current();
        let scale = theme.density_scale();
        let rail = ShellLayout::for_width(f32::from(window.bounds().size.width)).rail;
        let query = self.search_input.read(cx).value().to_string();
        let rows = self.visible_rows(&query);
        let cursor = self
            .cursor
            .clone()
            .filter(|cursor| rows.iter().any(|row| Self::row_target(row).0 == *cursor));
        let empty = rows.is_empty();
        let searching = !query.trim().is_empty();
        let sizes = Rc::new(
            rows.iter()
                .map(|row| match row {
                    TreeRow::Header { .. } => size(px(0.0), px(HEADER_ROW_PX)),
                    TreeRow::Pane { .. } => size(px(0.0), px(PANE_ROW_PX)),
                })
                .collect::<Vec<_>>(),
        );
        let tree = div()
            .id("sidebar-tree-viewport")
            .test_support()
            .flex_1()
            .min_h(px(0.0))
            .child(
                v_virtual_list(
                    cx.entity(),
                    "sidebar-tree",
                    sizes,
                    move |this, range, window, cx| {
                        range
                            .filter_map(|ix| rows.get(ix))
                            .map(|row| {
                                this.render_tree_row(row, cursor.as_ref(), rail, &theme, window, cx)
                                    .into_any_element()
                            })
                            .collect()
                    },
                )
                .track_scroll(&self.scroll),
            )
            .when(empty, |el| {
                el.child(
                    div()
                        .id("sidebar-empty")
                        .test_support()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(SPACE_2 * scale))
                        .px(px(SPACE_4 * scale))
                        .py(px(SPACE_2 * scale))
                        .text_size(px(12.0 * scale))
                        .text_color(theme.text_faint)
                        .when(searching && !rail, |el| {
                            el.aria_label("No panes match that search.")
                        })
                        .child(if rail {
                            String::new()
                        } else if searching {
                            "No panes match that search.".to_string()
                        } else {
                            "No workspaces yet — open a workspace to begin.".to_string()
                        })
                        .when(searching, |el| {
                            el.child(
                                Button::new("sidebar-empty-clear")
                                    .ghost()
                                    .label("Clear")
                                    .min_h(px(TOUCH_TARGET_PX))
                                    .min_w(px(TOUCH_TARGET_PX))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.clear_search(window, cx);
                                    })),
                            )
                        }),
                )
            });

        div()
            .id("workspace-sidebar")
            .test_support()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.handle_tree_key(event, window, cx);
            }))
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.panel_bg())
            .rounded(px(theme.radius_panel()))
            .child(self.render_titlebar(rail, &theme, scale, cx))
            .child(self.render_search_row(rail, &theme, scale, &query, cx))
            .child(tree)
            .child(self.render_bottom_bar(scale, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::rgb;
    use gpui_kit::test::{TestQueryExt, TestWindowExt};
    use orion_core::runtime::PaneHandle;
    use orion_core::FrameSink;
    use std::sync::Mutex;

    struct TreeFixture {
        state: Mutex<FixtureState>,
    }

    struct FixtureState {
        snapshot: Value,
        calls: Vec<(String, String, Value)>,
    }

    impl TreeFixture {
        fn new(snapshot: Value) -> Self {
            Self {
                state: Mutex::new(FixtureState {
                    snapshot,
                    calls: Vec::new(),
                }),
            }
        }

        fn calls(&self) -> Vec<(String, String, Value)> {
            self.state.lock().expect("fixture state").calls.clone()
        }

        fn focus_pane(&self, pane_id: &str) {
            let mut state = self.state.lock().expect("fixture state");
            let mut workspace_id = None;
            if let Some(panes) = state
                .snapshot
                .get_mut("panes")
                .and_then(Value::as_array_mut)
            {
                for pane in panes {
                    let selected = pane.get("pane_id").and_then(Value::as_str) == Some(pane_id);
                    if selected {
                        workspace_id = pane
                            .get("workspace_id")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                    }
                    pane["focused"] = json!(selected);
                }
            }
            if let Some(workspaces) = state
                .snapshot
                .get_mut("workspaces")
                .and_then(Value::as_array_mut)
            {
                for workspace in workspaces {
                    let selected = workspace_id.as_deref()
                        == workspace.get("workspace_id").and_then(Value::as_str);
                    workspace["focused"] = json!(selected);
                }
            }
            state.snapshot["focused_pane_id"] = json!(pane_id);
        }
    }

    impl Runtime for TreeFixture {
        fn identity(&self) -> Value {
            json!({ "version": "fixture", "protocol": 1 })
        }

        fn http(&self, method: &str, path: &str, body: &Value) -> (u16, Value) {
            let mut state = self.state.lock().expect("fixture state");
            state
                .calls
                .push((method.to_string(), path.to_string(), body.clone()));
            if method == "GET" && path == "/api/session" {
                return (200, json!({ "snapshot": state.snapshot }));
            }
            if method == "POST" && path == "/api/pane/focus" {
                if let Some(pane_id) = body.get("pane_id").and_then(Value::as_str) {
                    drop(state);
                    self.focus_pane(pane_id);
                }
                return (200, Value::Null);
            }
            (200, Value::Null)
        }

        fn pane_by_id(&self, _pane_id: &str) -> Option<Arc<dyn PaneHandle>> {
            None
        }

        fn connect(&self, _sink: FrameSink) -> String {
            "fixture".to_string()
        }

        fn send_to(&self, _connection: &str, _frame: Value) {}

        fn disconnect(&self, _connection: &str) {}

        fn dispatch_from(
            &self,
            _connection: Option<&str>,
            _request: &Value,
        ) -> std::result::Result<Option<Value>, String> {
            Ok(None)
        }

        fn snapshot_value(&self) -> Value {
            self.state.lock().expect("fixture state").snapshot.clone()
        }

        fn restore(&self) {}
    }

    fn tree_snapshot() -> Value {
        json!({
            "workspaces": [
                { "workspace_id": "ws-1", "label": "Orion", "active_tab_id": "tab-1",
                  "focused": true, "agent_status": "idle" },
                { "workspace_id": "ws-2", "label": "Side project", "active_tab_id": "tab-2",
                  "focused": false, "agent_status": "idle" },
            ],
            "tabs": [
                { "tab_id": "tab-1", "workspace_id": "ws-1", "label": "main",
                  "pane_ids": ["p1", "p2"], "active_pane_id": "p1", "focused": true,
                  "agent_status": "working" },
                { "tab_id": "tab-2", "workspace_id": "ws-2", "label": "side",
                  "pane_ids": ["p3"], "active_pane_id": "p3", "focused": false,
                  "agent_status": "done" },
            ],
            "panes": [
                { "pane_id": "p1", "workspace_id": "ws-1", "tab_id": "tab-1",
                  "label": "Build", "display_agent": "claude", "cwd": "/work/orion",
                  "agent_status": "working", "focused": true },
                { "pane_id": "p2", "workspace_id": "ws-1", "tab_id": "tab-1",
                  "label": "Review", "display_agent": "codex", "cwd": "/work/orion/docs",
                  "agent_status": "blocked", "focused": false },
                { "pane_id": "p3", "workspace_id": "ws-2", "tab_id": "tab-2",
                  "label": "Deploy", "display_agent": "opencode", "shell_path": "/bin/bash",
                  "agent_status": "done", "focused": false },
            ],
            "focused_pane_id": "p1",
        })
    }

    crate::orion_test!(
        fn sidebar_renders_empty_state_without_panic(cx: &mut gpui_kit::TestAppContext) {
            use gpui_kit::{point, size};
            let cx = cx.add_empty_window();
            cx.draw(
                point(px(0.), px(0.)),
                size(px(264.), px(800.)),
                |window, cx| {
                    cx.new(|cx| WorkspaceSidebar::new(window, cx))
                        .into_any_element()
                },
            );
        }
    );

    #[::core::prelude::v1::test]
    fn truncate_middle_keeps_both_ends() {
        assert_eq!(truncate_middle("short", 12), "short");
        assert_eq!(truncate_middle("abcdef", 3), "a…f");
        assert_eq!(truncate_middle("abcdefgh", 5), "ab…gh");
        assert_eq!(
            truncate_middle("C:\\rust-projects\\orion", 10),
            "C:\\…\\orion"
        );
    }

    #[::core::prelude::v1::test]
    fn status_parses_wire_names() {
        assert_eq!(parse_status(Some("working")), AgentStatus::Working);
        assert_eq!(parse_status(Some("blocked")), AgentStatus::Blocked);
        assert_eq!(parse_status(Some("done")), AgentStatus::Done);
        assert_eq!(parse_status(Some("error")), AgentStatus::Error);
        assert_eq!(parse_status(Some("idle")), AgentStatus::Idle);
        assert_eq!(parse_status(None), AgentStatus::Unknown);
    }

    #[::core::prelude::v1::test]
    fn status_dot_uses_theme_tokens_not_rose() {
        // DESIGN.md §1: rose marks the live edge alone; agent states never
        // resolve to the accent.
        let theme = Theme::dark();
        for status in [
            AgentStatus::Idle,
            AgentStatus::Working,
            AgentStatus::Blocked,
            AgentStatus::Done,
            AgentStatus::Error,
        ] {
            let color = WorkspaceSidebar::status_color(status, &theme, 8);
            assert_ne!(color, theme.accent, "{:?} must not be rose", status);
            assert_ne!(color, theme.primary, "{:?} must not be rose", status);
        }
        assert_eq!(
            WorkspaceSidebar::status_color(AgentStatus::Working, &theme, 8),
            theme.status_working
        );
        assert_eq!(
            WorkspaceSidebar::status_color(AgentStatus::Working, &theme, 8),
            rgb(0x58a6ff)
        );
    }

    crate::orion_test!(
        fn the_sidebar_renders_a_fixture_snapshot_with_stable_rows(
            cx: &mut gpui_kit::TestAppContext,
        ) {
            use gpui_kit::test::TestWindowExt;
            use orion_core::runtime::PaneHandle;
            use orion_core::FrameSink;
            use std::cell::RefCell;
            use std::rc::Rc;

            struct FixtureRuntime {
                snapshot: Value,
            }

            impl Runtime for FixtureRuntime {
                fn identity(&self) -> Value {
                    json!({ "version": "fixture", "protocol": 1 })
                }

                fn http(&self, method: &str, path: &str, _body: &Value) -> (u16, Value) {
                    if method == "GET" && path == "/api/session" {
                        return (200, json!({ "snapshot": self.snapshot }));
                    }
                    (200, Value::Null)
                }

                fn pane_by_id(&self, _pane_id: &str) -> Option<Arc<dyn PaneHandle>> {
                    None
                }

                fn connect(&self, _sink: FrameSink) -> String {
                    "fixture".to_string()
                }

                fn send_to(&self, _connection: &str, _frame: Value) {}

                fn disconnect(&self, _connection: &str) {}

                fn dispatch_from(
                    &self,
                    _connection: Option<&str>,
                    _request: &Value,
                ) -> std::result::Result<Option<Value>, String> {
                    Ok(None)
                }

                fn snapshot_value(&self) -> Value {
                    self.snapshot.clone()
                }

                fn restore(&self) {}
            }

            let snapshot = json!({
                "workspaces": [{
                    "workspace_id": "ws-1", "label": "Orion", "active_tab_id": "tab-1",
                    "focused": true, "agent_status": "idle"
                }],
                "tabs": [{
                    "tab_id": "tab-1", "workspace_id": "ws-1", "label": "main",
                    "pane_ids": ["p1"], "active_pane_id": "p1", "focused": true,
                    "agent_status": "idle"
                }],
                "panes": [{
                    "pane_id": "p1", "workspace_id": "ws-1", "tab_id": "tab-1",
                    "label": "Build", "display_agent": "claude", "cwd": "/work/orion",
                    "agent_status": "idle", "focused": true
                }],
                "focused_pane_id": "p1",
            });
            let runtime: Arc<dyn Runtime> = Arc::new(FixtureRuntime { snapshot });
            let (root, cx) = cx.add_window_view(|window, cx| WorkspaceSidebar::new(window, cx));
            cx.update(|_, app| {
                root.update(app, |view, _cx| view.attach(Arc::clone(&runtime)));
            });

            cx.update(|window, cx| window.render_frame(cx));
            assert!(
                cx.update(|window, _| window.try_find("sidebar-workspace-ws-1").is_some()),
                "the fixture workspace is on screen"
            );
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p1").is_some()),
                "the fixture pane is on screen"
            );

            cx.update(|window, cx| window.render_frame(cx));
            assert!(
                cx.update(|window, _| window.try_find("sidebar-workspace-ws-1").is_some()),
                "the workspace keeps its identity across renders"
            );
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p1").is_some()),
                "the pane keeps its identity across renders"
            );

            let events = Rc::new(RefCell::new(Vec::new()));
            let collected = events.clone();
            // The subscription must outlive the click, or the event goes nowhere.
            let subscription = cx.update(|_window, app| {
                app.subscribe(&root, move |_entity, event: &SidebarEvent, _cx| {
                    collected.borrow_mut().push(event.clone());
                })
            });
            cx.update(|window, cx| window.click("sidebar-pane-p1", cx));
            assert!(
                events
                    .borrow()
                    .contains(&SidebarEvent::PaneFocused("p1".to_string())),
                "clicking the row reports its pane: {:?}",
                events.borrow()
            );
            drop(subscription);
        }
    );

    crate::orion_test!(
        fn clicking_a_pane_activates_it_and_moves_the_tree_selection(
            cx: &mut gpui_kit::TestAppContext,
        ) {
            let (_root, cx, fixture) = open_tree_fixture(cx);
            cx.update(|window, cx| window.render_frame(cx));

            assert_eq!(
                cx.update(|window, _| window.find("sidebar-pane-p1").selected()),
                Some(true),
                "the focused pane starts selected"
            );
            assert_eq!(
                cx.update(|window, _| window.find("sidebar-pane-p2").selected()),
                Some(false),
                "other panes start unselected"
            );

            cx.update(|window, cx| window.click("sidebar-pane-p3", cx));

            let focus_calls: Vec<_> = fixture
                .calls()
                .into_iter()
                .filter(|(method, path, _)| method == "POST" && path == "/api/pane/focus")
                .collect();
            assert_eq!(
                focus_calls,
                vec![(
                    "POST".to_string(),
                    "/api/pane/focus".to_string(),
                    json!({ "pane_id": "p3" })
                )],
                "the click sends the existing focus route exactly once"
            );
            assert_eq!(
                cx.update(|window, _| window.find("sidebar-pane-p3").selected()),
                Some(true),
                "the activated pane becomes selected"
            );
            assert_eq!(
                cx.update(|window, _| window.find("sidebar-pane-p1").selected()),
                Some(false),
                "the previous pane is no longer selected"
            );
            assert_eq!(
                cx.update(|window, _| window.find("sidebar-workspace-ws-2").selected()),
                Some(true),
                "the activated pane's workspace is evident in the tree"
            );
        }
    );

    crate::orion_test!(
        fn tree_focus_puts_a_ring_inside_the_selected_row_only(cx: &mut gpui_kit::TestAppContext) {
            let (root, cx, _fixture) = open_tree_fixture(cx);
            cx.update(|window, cx| window.render_frame(cx));

            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p1-focus-ring").is_none()),
                "an unfocused tree has no row ring"
            );

            let handle = cx.update(|_, app| root.read(app).focus_handle().clone());
            cx.update(|window, app| handle.focus(window, app));
            cx.update(|window, cx| window.render_frame(cx));

            assert_eq!(
                cx.update(|window, _| window.find("workspace-sidebar").focused()),
                Some(true),
                "the tree holds keyboard focus"
            );
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p1-focus-ring").is_some()),
                "the selected row shows the focus ring"
            );
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p2-focus-ring").is_none()),
                "unselected rows do not show a ring"
            );

            let (row, ring) = cx.update(|window, _| {
                (
                    window.find("sidebar-pane-p1").bounds(),
                    window.find("sidebar-pane-p1-focus-ring").bounds(),
                )
            });
            assert!(
                ring.top() >= row.top()
                    && ring.bottom() <= row.bottom()
                    && ring.left() >= row.left()
                    && ring.right() <= row.right(),
                "the ring stays inside the row, not around the sidebar"
            );
        }
    );

    fn status_snapshot() -> Value {
        let panes = [
            ("p1", "idle"),
            ("p2", "working"),
            ("p3", "blocked"),
            ("p4", "done"),
            ("p5", "error"),
            ("p6", "unknown"),
        ];
        json!({
            "workspaces": [{
                "workspace_id": "ws-1", "label": "Statuses", "active_tab_id": "tab-1",
            }],
            "tabs": [{
                "tab_id": "tab-1", "workspace_id": "ws-1",
                "pane_ids": panes.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            }],
            "panes": panes.iter().map(|(id, status)| json!({
                "pane_id": id, "workspace_id": "ws-1", "tab_id": "tab-1",
                "label": id, "agent_status": status,
            })).collect::<Vec<_>>(),
            "focused_pane_id": "p1",
        })
    }

    crate::orion_test!(
        fn every_status_uses_the_shared_accessible_name(cx: &mut gpui_kit::TestAppContext) {
            let runtime: Arc<dyn Runtime> = Arc::new(TreeFixture::new(status_snapshot()));
            let (root, cx) = cx.add_window_view(|window, cx| WorkspaceSidebar::new(window, cx));
            cx.update(|_, app| root.update(app, |view, _cx| view.attach(Arc::clone(&runtime))));
            cx.update(|window, cx| window.render_frame(cx));

            for label in ["Idle", "Working", "Waiting", "Done", "Error", "Starting"] {
                let _ = cx.update(|window, _| window.find_by_label(label));
            }
        }
    );

    fn open_tree_fixture<'a>(
        cx: &'a mut gpui_kit::TestAppContext,
    ) -> (
        Entity<WorkspaceSidebar>,
        &'a mut gpui_kit::VisualTestContext,
        Arc<TreeFixture>,
    ) {
        let fixture = Arc::new(TreeFixture::new(tree_snapshot()));
        let runtime: Arc<dyn Runtime> = fixture.clone();
        let (root, cx) = cx.add_window_view(|window, cx| WorkspaceSidebar::new(window, cx));
        cx.update(|_, app| root.update(app, |view, _cx| view.attach(runtime)));
        (root, cx, fixture)
    }

    crate::orion_test!(
        fn workspaces_group_their_panes_and_collapse_together(cx: &mut gpui_kit::TestAppContext) {
            let (_root, cx, _fixture) = open_tree_fixture(cx);
            cx.update(|window, cx| window.render_frame(cx));

            for id in [
                "sidebar-workspace-ws-1",
                "sidebar-workspace-ws-2",
                "sidebar-pane-p1",
                "sidebar-pane-p2",
                "sidebar-pane-p3",
            ] {
                assert!(
                    cx.update(|window, _| window.try_find(id).is_some()),
                    "{id} is in the tree"
                );
            }
            let _ = cx.update(|window, _| window.find_by_label("2 working agents"));

            cx.update(|window, cx| window.click("sidebar-workspace-ws-1-create", cx));
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p1").is_some()),
                "the create control does not collapse its group"
            );

            cx.update(|window, cx| window.click("sidebar-workspace-ws-1", cx));
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p1").is_none()),
                "collapsing a workspace hides its panes"
            );
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p2").is_none()),
                "every pane in the group is hidden"
            );
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p3").is_some()),
                "another workspace stays expanded"
            );
            let _ = cx.update(|window, _| window.find_by_label("2 working agents"));

            cx.update(|window, cx| window.click("sidebar-workspace-ws-1", cx));
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p1").is_some()),
                "the header expands its group again"
            );
        }
    );

    crate::orion_test!(
        fn pane_rows_name_status_and_show_a_monospace_subtitle(cx: &mut gpui_kit::TestAppContext) {
            let (_root, cx, _fixture) = open_tree_fixture(cx);
            cx.update(|window, cx| window.render_frame(cx));

            for label in ["Working", "Waiting", "/bin/bash"] {
                let _ = cx.update(|window, _| window.find_by_label(label));
            }
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p2-badge").is_some()),
                "the needs-input badge is present"
            );
            assert!(
                cx.update(|window, _| window.try_find("sidebar-status-section").is_none()),
                "the old Working and Settled sections are gone"
            );
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p1-status").is_some()),
                "the left indicator owns the status"
            );
        }
    );

    struct MountHarness {
        sidebar: Entity<WorkspaceSidebar>,
        mounted: bool,
    }

    impl Render for MountHarness {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .id("mount-harness")
                .when(self.mounted, |el| el.child(self.sidebar.clone()))
        }
    }

    crate::orion_test!(
        fn collapse_survives_sidebar_hide_and_show(cx: &mut gpui_kit::TestAppContext) {
            let runtime: Arc<dyn Runtime> = Arc::new(TreeFixture::new(tree_snapshot()));
            let (root, cx) = cx.add_window_view(|window, cx| MountHarness {
                sidebar: cx.new(|cx| WorkspaceSidebar::new(window, cx)),
                mounted: true,
            });
            let sidebar = cx.update(|_, app| root.read(app).sidebar.clone());
            cx.update(|_, app| {
                sidebar.update(app, |view, _cx| view.attach(Arc::clone(&runtime)));
            });
            cx.update(|window, cx| window.render_frame(cx));

            cx.update(|window, cx| window.click("sidebar-workspace-ws-1", cx));
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p1").is_none()),
                "the header collapsed the group"
            );

            for mounted in [false, true] {
                cx.update(|_, app| {
                    root.update(app, |harness, cx| {
                        harness.mounted = mounted;
                        cx.notify();
                    });
                });
                cx.update(|window, cx| window.render_frame(cx));
            }
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p1").is_none()),
                "the collapse choice survives a hide/show cycle"
            );

            cx.update(|window, cx| window.click("sidebar-workspace-ws-1", cx));
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p1").is_some()),
                "the header still expands the group"
            );
        }
    );

    crate::orion_test!(
        fn tree_rows_use_precomputed_header_and_pane_heights(cx: &mut gpui_kit::TestAppContext) {
            let (_root, cx, _fixture) = open_tree_fixture(cx);
            cx.update(|window, cx| window.render_frame(cx));

            let header = cx.update(|window, _| {
                window
                    .try_find("sidebar-workspace-ws-1")
                    .expect("the header")
                    .bounds()
            });
            let pane = cx.update(|window, _| {
                window
                    .try_find("sidebar-pane-p1")
                    .expect("the pane")
                    .bounds()
            });
            assert_eq!(f32::from(header.size.height), 30.0);
            assert_eq!(f32::from(pane.size.height), 48.0);
        }
    );

    fn type_query(cx: &mut gpui_kit::VisualTestContext, text: &str) {
        cx.update(|window, cx| {
            window.click("sidebar-search-input", cx);
            window.input(text, cx);
        });
    }

    crate::orion_test!(
        fn search_filters_panes_by_title_cwd_and_shell(cx: &mut gpui_kit::TestAppContext) {
            let (_root, cx, _fixture) = open_tree_fixture(cx);
            cx.update(|window, cx| window.render_frame(cx));

            type_query(cx, "BUILD");
            assert!(cx.update(|window, _| window.try_find("sidebar-pane-p1").is_some()));
            assert!(cx.update(|window, _| window.try_find("sidebar-pane-p2").is_none()));
            assert!(cx.update(|window, _| window.try_find("sidebar-pane-p3").is_none()));

            cx.update(|window, cx| window.click("sidebar-search-clear", cx));
            type_query(cx, "orion/docs");
            assert!(cx.update(|window, _| window.try_find("sidebar-pane-p2").is_some()));
            assert!(cx.update(|window, _| window.try_find("sidebar-pane-p1").is_none()));

            cx.update(|window, cx| window.click("sidebar-search-clear", cx));
            type_query(cx, "bash");
            assert!(cx.update(|window, _| window.try_find("sidebar-pane-p3").is_some()));
            assert!(cx.update(|window, _| window.try_find("sidebar-pane-p1").is_none()));
        }
    );

    crate::orion_test!(
        fn search_expands_matching_groups_only_in_the_filtered_view(
            cx: &mut gpui_kit::TestAppContext,
        ) {
            let (_root, cx, _fixture) = open_tree_fixture(cx);
            cx.update(|window, cx| window.render_frame(cx));

            cx.update(|window, cx| window.click("sidebar-workspace-ws-1", cx));
            assert!(cx.update(|window, _| window.try_find("sidebar-pane-p2").is_none()));

            type_query(cx, "review");
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p2").is_some()),
                "a matching group expands while filtering"
            );
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p1").is_none()),
                "non-matching panes stay hidden"
            );
            assert!(
                cx.update(|window, _| window.try_find("sidebar-workspace-ws-2").is_none()),
                "empty groups are hidden"
            );

            cx.update(|window, cx| window.click("sidebar-workspace-ws-1", cx));
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p2").is_some()),
                "a header click cannot change collapse state while filtering"
            );

            cx.update(|window, cx| window.click("sidebar-search-clear", cx));
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p2").is_none()),
                "the stored collapse choice returns unchanged"
            );
        }
    );

    crate::orion_test!(
        fn search_keeps_selection_and_offers_a_clearable_empty_state(
            cx: &mut gpui_kit::TestAppContext,
        ) {
            let (_root, cx, _fixture) = open_tree_fixture(cx);
            cx.update(|window, cx| window.render_frame(cx));
            assert_eq!(
                cx.update(|window, _| window.find("sidebar-pane-p1").selected()),
                Some(true)
            );

            type_query(cx, "deploy");
            assert_eq!(
                cx.update(|window, _| window.find("sidebar-pane-p3").selected()),
                Some(false),
                "a result is not selected automatically"
            );
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p1").is_none()),
                "the selected pane can be filtered out"
            );

            cx.update(|window, cx| window.click("sidebar-search-clear", cx));
            type_query(cx, "nothing-matches");
            let _ = cx.update(|window, _| window.find_by_label("No panes match that search."));
            assert!(
                cx.update(|window, _| window.try_find("sidebar-empty-clear").is_some()),
                "the empty state has a Clear control"
            );

            cx.update(|window, cx| window.click("sidebar-empty-clear", cx));
            assert_eq!(
                cx.update(|window, _| window.find("sidebar-pane-p1").selected()),
                Some(true),
                "clearing restores the original selection"
            );
        }
    );

    crate::orion_test!(
        fn escape_clears_search_and_returns_focus_to_the_tree(cx: &mut gpui_kit::TestAppContext) {
            let (_root, cx, _fixture) = open_tree_fixture(cx);
            cx.update(|window, cx| window.render_frame(cx));

            type_query(cx, "bash");
            assert!(cx.update(|window, _| window.try_find("sidebar-pane-p1").is_none()));

            cx.update(|window, cx| window.press("escape", cx));
            assert!(cx.update(|window, _| window.try_find("sidebar-pane-p1").is_some()));
            assert_eq!(
                cx.update(|window, _| window.find("workspace-sidebar").focused()),
                Some(true),
                "Escape hands focus back to the tree"
            );
        }
    );

    fn focus_tree(root: &Entity<WorkspaceSidebar>, cx: &mut gpui_kit::VisualTestContext) {
        let handle = cx.update(|_, app| root.read(app).focus_handle().clone());
        cx.update(|window, app| handle.focus(window, app));
        cx.update(|window, cx| window.render_frame(cx));
    }

    fn focus_calls(fixture: &TreeFixture) -> Vec<(String, String, Value)> {
        fixture
            .calls()
            .into_iter()
            .filter(|(method, path, _)| method == "POST" && path == "/api/pane/focus")
            .collect()
    }

    fn collect_events(
        root: &Entity<WorkspaceSidebar>,
        cx: &mut gpui_kit::VisualTestContext,
    ) -> (
        std::rc::Rc<std::cell::RefCell<Vec<SidebarEvent>>>,
        Subscription,
    ) {
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let collected = events.clone();
        let subscription = cx.update(|_window, app| {
            app.subscribe(root, move |_entity, event: &SidebarEvent, _cx| {
                collected.borrow_mut().push(event.clone());
            })
        });
        (events, subscription)
    }

    crate::orion_test!(
        fn sidebar_settings_icon_reports_open_settings(cx: &mut gpui_kit::TestAppContext) {
            let (root, cx, _fixture) = open_tree_fixture(cx);
            cx.update(|window, cx| window.render_frame(cx));
            let (events, subscription) = collect_events(&root, cx);

            cx.update(|window, cx| window.click("sidebar-settings", cx));
            assert!(
                events
                    .borrow()
                    .iter()
                    .any(|event| matches!(event, SidebarEvent::OpenSettings(_))),
                "the settings icon asks the app to open Settings"
            );
            assert!(
                events
                    .borrow()
                    .iter()
                    .any(|event| matches!(event, SidebarEvent::OpenSettings(Some(_)))),
                "the settings icon passes its focus so Settings can restore it"
            );
            drop(subscription);
        }
    );

    crate::orion_test!(
        fn sidebar_header_collapse_reports_toggle_sidebar(cx: &mut gpui_kit::TestAppContext) {
            let (root, cx, _fixture) = open_tree_fixture(cx);
            cx.update(|window, cx| window.render_frame(cx));
            let (events, subscription) = collect_events(&root, cx);

            cx.update(|window, cx| window.click("sidebar-header-collapse", cx));
            assert!(
                events.borrow().contains(&SidebarEvent::ToggleSidebar),
                "the header collapse button asks the app to toggle the sidebar"
            );
            drop(subscription);
        }
    );

    crate::orion_test!(
        fn terminal_icons_report_the_terminal_lens(cx: &mut gpui_kit::TestAppContext) {
            let (root, cx, _fixture) = open_tree_fixture(cx);
            cx.update(|window, cx| window.render_frame(cx));
            let (events, subscription) = collect_events(&root, cx);

            for id in ["sidebar-terminal-toolbar", "sidebar-terminal-footer"] {
                cx.update(|window, cx| window.click(id, cx));
            }
            assert_eq!(
                events
                    .borrow()
                    .iter()
                    .filter(|event| **event == SidebarEvent::TerminalLens)
                    .count(),
                2,
                "both terminal icons ask for the Terminal Lens"
            );
            drop(subscription);
        }
    );

    crate::orion_test!(
        fn footer_view_icon_reports_the_chat_lens(cx: &mut gpui_kit::TestAppContext) {
            let (root, cx, _fixture) = open_tree_fixture(cx);
            cx.update(|window, cx| window.render_frame(cx));
            let (events, subscription) = collect_events(&root, cx);

            cx.update(|window, cx| window.click("sidebar-chat-footer", cx));
            assert!(
                events.borrow().contains(&SidebarEvent::ChatLens),
                "the footer view icon asks for the Chat Lens"
            );
            drop(subscription);
        }
    );

    crate::orion_test!(
        fn arrow_keys_move_the_focus_cursor_over_headers_and_panes(
            cx: &mut gpui_kit::TestAppContext,
        ) {
            let (root, cx, _fixture) = open_tree_fixture(cx);
            cx.update(|window, cx| window.render_frame(cx));
            focus_tree(&root, cx);

            cx.update(|window, cx| window.press("down", cx));
            assert!(
                cx.update(|window, _| window
                    .try_find("sidebar-workspace-ws-1-focus-ring")
                    .is_some()),
                "the first Down puts the cursor on the first header"
            );
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p1-focus-ring").is_none()),
                "the first Down does not skip the header"
            );

            cx.update(|window, cx| window.press("down", cx));
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p1-focus-ring").is_some()),
                "Down advances onto the first pane"
            );
            cx.update(|window, cx| window.press("down", cx));
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p2-focus-ring").is_some()),
                "Down walks the visible panes in display order"
            );

            cx.update(|window, cx| window.press("up", cx));
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p1-focus-ring").is_some()),
                "Up moves back one visible row"
            );
            assert!(
                cx.update(|window, _| window
                    .try_find("sidebar-workspace-ws-1-focus-ring")
                    .is_none()),
                "only the cursor row carries a ring"
            );
        }
    );

    crate::orion_test!(
        fn enter_activates_the_focused_pane_and_keeps_focus_in_the_tree(
            cx: &mut gpui_kit::TestAppContext,
        ) {
            use std::cell::RefCell;
            use std::rc::Rc;

            let (root, cx, fixture) = open_tree_fixture(cx);
            cx.update(|window, cx| window.render_frame(cx));
            focus_tree(&root, cx);

            let events = Rc::new(RefCell::new(Vec::new()));
            let collected = events.clone();
            let subscription = cx.update(|_window, app| {
                app.subscribe(&root, move |_entity, event: &SidebarEvent, _cx| {
                    collected.borrow_mut().push(event.clone());
                })
            });

            cx.update(|window, cx| window.press("up", cx));
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p3-focus-ring").is_some()),
                "Up from no cursor parks the cursor on the last visible pane"
            );
            cx.update(|window, cx| window.press("enter", cx));

            assert_eq!(
                focus_calls(&fixture),
                vec![(
                    "POST".to_string(),
                    "/api/pane/focus".to_string(),
                    json!({ "pane_id": "p3" })
                )],
                "Enter activates the pane under the cursor"
            );
            assert!(
                events
                    .borrow()
                    .contains(&SidebarEvent::PaneFocused("p3".to_string())),
                "keyboard activation reports the same event as a click: {:?}",
                events.borrow()
            );
            assert_eq!(
                cx.update(|window, _| window.find("workspace-sidebar").focused()),
                Some(true),
                "keyboard activation leaves focus in the tree"
            );
            assert_eq!(
                cx.update(|window, _| window.find("sidebar-pane-p3").selected()),
                Some(true),
                "the activated pane and its workspace become selected"
            );
            assert_eq!(
                cx.update(|window, _| window.find("sidebar-workspace-ws-2").selected()),
                Some(true),
                "the activated pane's workspace is evident in the tree"
            );
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p3-focus-ring").is_some()),
                "the cursor ring stays on the activated pane"
            );
            drop(subscription);
        }
    );

    crate::orion_test!(
        fn enter_on_a_header_toggles_the_group(cx: &mut gpui_kit::TestAppContext) {
            let (root, cx, _fixture) = open_tree_fixture(cx);
            cx.update(|window, cx| window.render_frame(cx));
            focus_tree(&root, cx);

            cx.update(|window, cx| window.press("down", cx));
            cx.update(|window, cx| window.press("enter", cx));
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p1").is_none()),
                "Enter collapses the group under the cursor"
            );
            assert!(
                cx.update(|window, _| window
                    .try_find("sidebar-workspace-ws-1-focus-ring")
                    .is_some()),
                "the cursor stays on the header"
            );

            cx.update(|window, cx| window.press("enter", cx));
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p1").is_some()),
                "Enter expands the group again"
            );
        }
    );

    crate::orion_test!(
        fn left_and_right_collapse_and_expand_the_group_under_the_cursor(
            cx: &mut gpui_kit::TestAppContext,
        ) {
            let (root, cx, _fixture) = open_tree_fixture(cx);
            cx.update(|window, cx| window.render_frame(cx));
            focus_tree(&root, cx);

            cx.update(|window, cx| window.press("down", cx));
            cx.update(|window, cx| window.press("right", cx));
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p1").is_some()),
                "Right on an expanded group changes nothing"
            );
            assert!(
                cx.update(|window, _| window
                    .try_find("sidebar-workspace-ws-1-focus-ring")
                    .is_some()),
                "an ineffective collapse key leaves the cursor where it was"
            );

            cx.update(|window, cx| window.press("left", cx));
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p1").is_none()),
                "Left collapses the group under the cursor"
            );
            assert!(
                cx.update(|window, _| window
                    .try_find("sidebar-workspace-ws-1-focus-ring")
                    .is_some()),
                "the cursor stays on the header after collapsing"
            );

            cx.update(|window, cx| window.press("left", cx));
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p1").is_none()),
                "Left on an already collapsed group changes nothing"
            );
            assert!(
                cx.update(|window, _| window
                    .try_find("sidebar-workspace-ws-1-focus-ring")
                    .is_some()),
                "an ineffective Left keeps the cursor on the header"
            );

            cx.update(|window, cx| window.press("right", cx));
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p1").is_some()),
                "Right expands the group again"
            );

            cx.update(|window, cx| window.press("down", cx));
            cx.update(|window, cx| window.press("left", cx));
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p1").is_none()),
                "Left on a pane collapses its Workspace"
            );
            assert!(
                cx.update(|window, _| window
                    .try_find("sidebar-workspace-ws-1-focus-ring")
                    .is_some()),
                "the cursor moves to the group it just collapsed"
            );
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p3").is_some()),
                "another workspace stays expanded"
            );
        }
    );

    crate::orion_test!(
        fn navigation_and_activation_respect_the_search_filter(cx: &mut gpui_kit::TestAppContext) {
            let (root, cx, fixture) = open_tree_fixture(cx);
            cx.update(|window, cx| window.render_frame(cx));
            type_query(cx, "bash");
            focus_tree(&root, cx);

            cx.update(|window, cx| window.press("down", cx));
            assert!(
                cx.update(|window, _| window
                    .try_find("sidebar-workspace-ws-2-focus-ring")
                    .is_some()),
                "the cursor walks the filtered header"
            );
            cx.update(|window, cx| window.press("down", cx));
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p3-focus-ring").is_some()),
                "the cursor walks the filtered pane"
            );

            cx.update(|window, cx| window.press("enter", cx));
            assert_eq!(
                focus_calls(&fixture),
                vec![(
                    "POST".to_string(),
                    "/api/pane/focus".to_string(),
                    json!({ "pane_id": "p3" })
                )],
                "Enter activates only the visible pane"
            );
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p1").is_none()),
                "a hidden pane is never activated"
            );
            assert_eq!(
                cx.update(|window, _| window.find("workspace-sidebar").focused()),
                Some(true),
                "focus stays in the tree while filtering"
            );
        }
    );

    fn many_panes_snapshot() -> Value {
        let pane_ids: Vec<String> = (1..=30).map(|i| format!("p{i}")).collect();
        json!({
            "workspaces": [{
                "workspace_id": "ws-1", "label": "Many", "active_tab_id": "tab-1",
                "focused": true, "agent_status": "idle"
            }],
            "tabs": [{
                "tab_id": "tab-1", "workspace_id": "ws-1", "label": "main",
                "pane_ids": pane_ids, "active_pane_id": "p1", "focused": true,
                "agent_status": "idle"
            }],
            "panes": (1..=30).map(|i| json!({
                "pane_id": format!("p{i}"), "workspace_id": "ws-1", "tab_id": "tab-1",
                "label": format!("Pane {i}"), "display_agent": "claude",
                "cwd": format!("/work/{i}"), "agent_status": "idle", "focused": false,
            })).collect::<Vec<_>>(),
            "focused_pane_id": "p1",
        })
    }

    crate::orion_test!(
        fn the_cursor_scrolls_into_view_when_it_reaches_an_unrendered_row(
            cx: &mut gpui_kit::TestAppContext,
        ) {
            let runtime: Arc<dyn Runtime> = Arc::new(TreeFixture::new(many_panes_snapshot()));
            let (root, cx) = cx.add_window_view(|window, cx| WorkspaceSidebar::new(window, cx));
            cx.update(|_, app| root.update(app, |view, _cx| view.attach(runtime)));
            cx.update(|window, cx| window.render_frame(cx));
            focus_tree(&root, cx);

            cx.update(|window, cx| window.press("up", cx));
            cx.update(|window, cx| window.render_frame(cx));
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p30-focus-ring").is_some()),
                "the cursor row is rendered after navigating to it"
            );
        }
    );

    crate::orion_test!(
        fn collapse_keys_do_not_change_hidden_state_while_filtering(
            cx: &mut gpui_kit::TestAppContext,
        ) {
            let (root, cx, _fixture) = open_tree_fixture(cx);
            cx.update(|window, cx| window.render_frame(cx));
            type_query(cx, "orion");
            focus_tree(&root, cx);

            cx.update(|window, cx| window.press("down", cx));
            cx.update(|window, cx| window.press("left", cx));
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p1").is_some())
                    && cx.update(|window, _| window.try_find("sidebar-pane-p2").is_some()),
                "collapse keys have no visible effect while filtering"
            );
            assert!(
                cx.update(|window, _| window
                    .try_find("sidebar-workspace-ws-1-focus-ring")
                    .is_some()),
                "ineffective collapse keys leave the cursor on the header"
            );

            cx.update(|window, cx| window.click("sidebar-search-clear", cx));
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p1").is_some())
                    && cx.update(|window, _| window.try_find("sidebar-pane-p2").is_some()),
                "clearing the query restores the unchanged layout"
            );

            type_query(cx, "orion");
            focus_tree(&root, cx);
            cx.update(|window, cx| window.press("down", cx));
            cx.update(|window, cx| window.press("enter", cx));
            cx.update(|window, cx| window.click("sidebar-search-clear", cx));
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p1").is_some())
                    && cx.update(|window, _| window.try_find("sidebar-pane-p2").is_some()),
                "Enter on a filtered header leaves stored collapse state alone"
            );
        }
    );

    crate::orion_test!(
        fn escape_from_the_tree_clears_search_and_keeps_tree_focus(
            cx: &mut gpui_kit::TestAppContext,
        ) {
            let (root, cx, _fixture) = open_tree_fixture(cx);
            cx.update(|window, cx| window.render_frame(cx));
            type_query(cx, "bash");
            focus_tree(&root, cx);

            cx.update(|window, cx| window.press("escape", cx));
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p1").is_some()),
                "Escape from the tree clears the query"
            );
            assert_eq!(
                cx.update(|window, _| window.find("workspace-sidebar").focused()),
                Some(true),
                "Escape keeps focus in the tree"
            );
        }
    );

    fn route_calls(fixture: &TreeFixture, path: &str) -> Vec<(String, String, Value)> {
        fixture
            .calls()
            .into_iter()
            .filter(|(_, call_path, _)| call_path == path)
            .collect()
    }

    crate::orion_test!(
        fn row_menus_offer_only_rename_and_close(cx: &mut gpui_kit::TestAppContext) {
            let (_root, cx, _fixture) = open_tree_fixture(cx);
            cx.update(|window, cx| window.render_frame(cx));

            for (row, menu) in [
                ("sidebar-pane-p1", "sidebar-pane-p1-menu"),
                ("sidebar-workspace-ws-1", "sidebar-workspace-ws-1-menu"),
            ] {
                assert!(cx.update(|window, _| window.try_find(row).is_some()));
                cx.update(|window, cx| window.hover(row, cx));
                cx.update(|window, cx| window.click(menu, cx));
                assert!(
                    cx.update(|window, _| {
                        window.try_find(format!("{row}-menu-rename")).is_some()
                    }),
                    "the {row} menu offers Rename"
                );
                assert!(
                    cx.update(|window, _| {
                        window.try_find(format!("{row}-menu-close")).is_some()
                    }),
                    "the {row} menu offers Close"
                );
                assert!(
                    cx.update(|window, _| {
                        window.try_find(format!("{row}-menu-duplicate")).is_none()
                    }),
                    "the {row} menu has no Duplicate item"
                );
                assert!(
                    cx.update(|window, _| {
                        window.try_find(format!("{row}-menu-copy-path")).is_none()
                    }),
                    "the {row} menu has no Copy path item"
                );
                assert!(
                    cx.update(|window, _| window.try_find(format!("{row}-drag-handle")).is_none()),
                    "the {row} row has no drag handle"
                );
                cx.update(|window, cx| window.click(menu, cx));
            }
        }
    );

    crate::orion_test!(
        fn inline_rename_commits_through_the_existing_route(cx: &mut gpui_kit::TestAppContext) {
            let (root, cx, fixture) = open_tree_fixture(cx);
            cx.update(|window, cx| window.render_frame(cx));
            let (events, subscription) = collect_events(&root, cx);

            cx.update(|window, cx| window.hover("sidebar-pane-p1", cx));
            cx.update(|window, cx| window.click("sidebar-pane-p1-menu", cx));
            cx.update(|window, cx| window.click("sidebar-pane-p1-menu-rename", cx));
            cx.update(|window, cx| {
                window.click("sidebar-pane-p1-rename-input", cx);
                for _ in 0..5 {
                    window.press("backspace", cx);
                }
                window.input("Renamed Build", cx);
                window.press("enter", cx);
            });

            assert_eq!(
                route_calls(&fixture, "/api/pane/rename"),
                vec![(
                    "POST".to_string(),
                    "/api/pane/rename".to_string(),
                    json!({ "pane_id": "p1", "label": "Renamed Build" })
                )],
                "Enter sends the pane rename through the existing route"
            );
            assert!(
                events
                    .borrow()
                    .contains(&SidebarEvent::PaneRenamed("p1".to_string())),
                "the rename is reported to the app"
            );
            drop(subscription);
        }
    );

    crate::orion_test!(
        fn escape_cancels_inline_rename_without_a_request(cx: &mut gpui_kit::TestAppContext) {
            let (_root, cx, fixture) = open_tree_fixture(cx);
            cx.update(|window, cx| window.render_frame(cx));

            cx.update(|window, cx| window.hover("sidebar-workspace-ws-1", cx));
            cx.update(|window, cx| window.click("sidebar-workspace-ws-1-menu", cx));
            cx.update(|window, cx| window.click("sidebar-workspace-ws-1-menu-rename", cx));
            cx.update(|window, cx| {
                window.click("sidebar-workspace-ws-1-rename-input", cx);
                window.input("Discarded", cx);
                window.press("escape", cx);
            });

            assert!(
                route_calls(&fixture, "/api/workspace/rename").is_empty(),
                "Escape sends no rename request"
            );
            let _ = cx.update(|window, _| window.find_by_label("Orion"));
        }
    );

    crate::orion_test!(
        fn pane_subtitle_occupies_the_second_line(cx: &mut gpui_kit::TestAppContext) {
            let (_root, cx, _fixture) = open_tree_fixture(cx);
            cx.update(|window, cx| window.render_frame(cx));

            let (row, subtitle) = cx.update(|window, _| {
                (
                    window.find("sidebar-pane-p1").bounds(),
                    window.find("sidebar-pane-p1-subtitle").bounds(),
                )
            });
            assert!(
                subtitle.size.height > px(0.0),
                "the subtitle line has a positive height"
            );
            assert!(
                subtitle.top() >= row.top() && subtitle.bottom() <= row.bottom(),
                "the subtitle stays inside the pane row: row={row:?} subtitle={subtitle:?}"
            );
        }
    );

    crate::orion_test!(
        fn offline_overrides_cached_agent_state(cx: &mut gpui_kit::TestAppContext) {
            let (root, cx, _fixture) = open_tree_fixture(cx);
            cx.update(|window, cx| window.render_frame(cx));
            cx.update(|_, app| {
                root.update(app, |view, cx| {
                    view.set_runtime_online(false, cx);
                    view.refresh();
                });
            });
            cx.update(|window, cx| window.render_frame(cx));

            assert!(
                cx.update(|window, _| window.try_find("sidebar-offline-warning").is_some()),
                "the header warns that the runtime is unreachable"
            );
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p1-status").is_none()),
                "a cached working state is not painted while offline"
            );
            assert!(
                cx.update(|window, _| window
                    .try_find("sidebar-workspace-ws-1-state-status")
                    .is_none()),
                "the project aggregate is not painted as idle while offline"
            );
        }
    );

    crate::orion_test!(
        fn the_close_control_is_hidden_until_the_row_is_hovered(cx: &mut gpui_kit::TestAppContext) {
            let (root, cx, _fixture) = open_tree_fixture(cx);
            cx.update(|window, cx| window.render_frame(cx));

            assert_eq!(
                cx.update(|window, _| window.find("sidebar-pane-p1-close").visible()),
                false,
                "the close control is hidden at rest"
            );
            cx.update(|window, cx| window.hover("sidebar-pane-p1", cx));
            assert_eq!(
                cx.update(|window, _| window.find("sidebar-pane-p1-close").visible()),
                true,
                "hovering the row reveals the close control"
            );

            cx.update(|window, cx| window.hover("sidebar-workspace-ws-1", cx));
            focus_tree(&root, cx);
            cx.update(|window, cx| {
                window.press("down", cx);
                window.press("down", cx);
            });
            assert_eq!(
                cx.update(|window, _| window.find("sidebar-pane-p1-menu").visible()),
                true,
                "a real keyboard cursor reveals the row menu without hover"
            );
        }
    );

    crate::orion_test!(
        fn elapsed_status_line_names_session_age(cx: &mut gpui_kit::TestAppContext) {
            let mut snapshot = tree_snapshot();
            let started = chrono::Utc::now() - chrono::Duration::minutes(2);
            snapshot["panes"][0]["started_at"] = json!(started.to_rfc3339());
            let runtime: Arc<dyn Runtime> = Arc::new(TreeFixture::new(snapshot));
            let (root, cx) = cx.add_window_view(|window, cx| WorkspaceSidebar::new(window, cx));
            cx.update(|_, app| root.update(app, |view, _cx| view.attach(runtime)));
            cx.update(|window, cx| window.render_frame(cx));

            let _ = cx.update(|window, _| window.find_by_label("working · session 2m"));
        }
    );

    crate::orion_test!(
        fn closing_a_settled_pane_needs_no_confirmation(cx: &mut gpui_kit::TestAppContext) {
            let (root, cx, fixture) = open_tree_fixture(cx);
            cx.update(|window, cx| window.render_frame(cx));
            let (events, subscription) = collect_events(&root, cx);

            cx.update(|window, cx| window.hover("sidebar-pane-p3", cx));
            cx.update(|window, cx| window.click("sidebar-pane-p3-menu", cx));
            cx.update(|window, cx| window.click("sidebar-pane-p3-menu-close", cx));

            assert_eq!(
                route_calls(&fixture, "/api/pane/close"),
                vec![(
                    "POST".to_string(),
                    "/api/pane/close".to_string(),
                    json!({ "pane_id": "p3" })
                )],
                "a settled pane closes without a dialog"
            );
            assert!(
                events
                    .borrow()
                    .contains(&SidebarEvent::PaneClosed("p3".to_string())),
                "the close is reported to the app"
            );
            drop(subscription);
        }
    );

    crate::orion_test!(
        fn closing_a_running_pane_asks_for_confirmation_first(cx: &mut gpui_kit::TestAppContext) {
            let fixture = Arc::new(TreeFixture::new(tree_snapshot()));
            let runtime: Arc<dyn Runtime> = fixture.clone();
            let (handle, root) = cx.update(|cx| {
                gpui_kit::open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            Default::default(),
                            size(px(264.0), px(800.0)),
                        ))),
                        ..Default::default()
                    },
                    cx,
                    |window, cx| cx.new(|cx| WorkspaceSidebar::new(window, cx)),
                )
                .expect("the sidebar window")
            });
            cx.update(|app| root.update(app, |view, _cx| view.attach(runtime)));
            let _ = cx.update_window(handle.into(), |_, window, cx| window.render_frame(cx));
            let _ = cx.update_window(handle.into(), |_, window, cx| {
                window.hover("sidebar-pane-p1", cx);
                window.click("sidebar-pane-p1-menu", cx)
            });
            let _ = cx.update_window(handle.into(), |_, window, cx| {
                window.click("sidebar-pane-p1-menu-close", cx)
            });
            assert!(
                route_calls(&fixture, "/api/pane/close").is_empty(),
                "a running pane waits for the confirmation instead of closing"
            );
        }
    );

    crate::orion_test!(
        fn closing_a_running_workspace_uses_the_session_confirmation(
            cx: &mut gpui_kit::TestAppContext,
        ) {
            let mut snapshot = tree_snapshot();
            snapshot["workspaces"][0]["agent_status"] = json!("working");
            let fixture = Arc::new(TreeFixture::new(snapshot));
            let runtime: Arc<dyn Runtime> = fixture.clone();
            let (handle, root) = cx.update(|cx| {
                gpui_kit::open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            Default::default(),
                            size(px(264.0), px(800.0)),
                        ))),
                        ..Default::default()
                    },
                    cx,
                    |window, cx| cx.new(|cx| WorkspaceSidebar::new(window, cx)),
                )
                .expect("the sidebar window")
            });
            cx.update(|app| root.update(app, |view, _cx| view.attach(runtime)));
            let _ = cx.update_window(handle.into(), |_, window, cx| window.render_frame(cx));
            let _ = cx.update_window(handle.into(), |_, window, cx| {
                window.hover("sidebar-workspace-ws-1", cx);
                window.click("sidebar-workspace-ws-1-menu", cx)
            });
            let _ = cx.update_window(handle.into(), |_, window, cx| {
                window.click("sidebar-workspace-ws-1-menu-close", cx)
            });
            assert!(
                route_calls(&fixture, "/api/workspace/close").is_empty(),
                "a running workspace waits for the confirmation instead of closing"
            );
        }
    );

    crate::orion_test!(
        fn create_controls_use_the_existing_tab_creation_route(cx: &mut gpui_kit::TestAppContext) {
            let (root, cx, fixture) = open_tree_fixture(cx);
            cx.update(|window, cx| window.render_frame(cx));
            let (events, subscription) = collect_events(&root, cx);

            cx.update(|window, cx| window.click("sidebar-workspace-ws-1-create", cx));
            cx.update(|window, cx| window.click("new-workspace-button", cx));

            let created = route_calls(&fixture, "/api/tab/create");
            assert_eq!(created.len(), 2, "both plus controls create a pane");
            for (_, _, body) in &created {
                assert_eq!(body["workspace_id"], json!("ws-1"));
                assert_eq!(body["agent"]["kind"], json!("claude"));
            }
            assert_eq!(
                events
                    .borrow()
                    .iter()
                    .filter(|event| matches!(event, SidebarEvent::PaneCreated(_)))
                    .count(),
                2,
                "both creations are reported to the app"
            );
            drop(subscription);
        }
    );

    struct ShortcutHarness {
        sidebar: Entity<WorkspaceSidebar>,
        other: FocusHandle,
    }

    impl Render for ShortcutHarness {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .id("shortcut-harness")
                .on_action(cx.listener(|this, _: &FocusSidebarSearch, window, cx| {
                    crate::sidebar::focus_search(&this.sidebar, window, cx);
                }))
                .child(
                    div()
                        .id("other-view")
                        .test_support()
                        .track_focus(&self.other),
                )
                .child(self.sidebar.clone())
        }
    }

    crate::orion_test!(
        fn slash_and_ctrl_k_focus_search_from_another_view(cx: &mut gpui_kit::TestAppContext) {
            cx.update(crate::app::register_search_shortcuts);
            let runtime: Arc<dyn Runtime> = Arc::new(TreeFixture::new(tree_snapshot()));
            let (root, cx) = cx.add_window_view(|window, cx| ShortcutHarness {
                sidebar: cx.new(|cx| WorkspaceSidebar::new(window, cx)),
                other: cx.focus_handle(),
            });
            let sidebar = cx.update(|_, app| root.read(app).sidebar.clone());
            cx.update(|_, app| sidebar.update(app, |view, _cx| view.attach(runtime)));
            cx.update(|window, cx| window.render_frame(cx));

            let other = cx.update(|_, app| root.read(app).other.clone());
            for key in ["/", "secondary-k"] {
                cx.update(|window, app| other.focus(window, app));
                cx.update(|window, cx| window.press(key, cx));
                cx.update(|window, app| {
                    let handle = root
                        .read(app)
                        .sidebar
                        .read(app)
                        .search_input
                        .read(app)
                        .focus_handle(app);
                    assert!(
                        handle.is_focused(window),
                        "{key} focuses the sidebar search input"
                    );
                });
            }
        }
    );
}
