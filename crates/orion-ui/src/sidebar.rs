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
use crate::terminal_tree::{status_token, terminal_rows, TerminalRow, TerminalSource};
use crate::theme::Theme;
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    avatar::Avatar, button::Button, button::ButtonCustomVariant, button::ButtonVariants, Icon,
    Sizable, ThemeStyled, WindowExt,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use gpui_kit::{div, px, Context, FocusHandle, Render, Styled, Window};
use orion_core::protocol::AgentStatus;
use orion_core::runtime::Runtime;
use orion_core::session_state::SessionState;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

// Spacing scale from DESIGN.md §4 (4px base). The Rust theme exposes density
// scaling but no spacing tokens, so keep the scale named and local instead of
// repeating bare literals across the view. Radii are the §4 chip/control tokens.
const SPACE_1: f32 = 4.0; // --space-1: tight icon and control gaps
const SPACE_2: f32 = 8.0; // --space-2: row and menu gaps
const SPACE_3: f32 = 12.0; // --space-3: control padding
const SPACE_4: f32 = 16.0; // --space-4: section padding
const RADIUS_SM: f32 = 6.0; // --radius-sm: chips and inner controls
const TOUCH_TARGET: f32 = 44.0; // --touch-target: coarse-pointer minimum
const ROW_SUB_INDENT: f32 = 28.0; // avatar (20) + row gap (8)

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

/// The sidebar view. State is the last session snapshot; `refresh()` swaps
/// it in when the wire body changes (stale rows are swept with it).
pub struct WorkspaceSidebar {
    engine: Option<Arc<dyn Runtime>>,
    snapshot: Option<Value>,
    last_body: Option<String>,
    /// Where the runtime's terminals come from, once the shell wires it up.
    terminal_source: Option<Arc<dyn TerminalSource>>,
    /// The runtime's last reported terminals (`{"terminals": [...]}`).
    terminals: Option<Value>,
    last_terminals: Option<String>,
    focus_handle: FocusHandle,
    row_seq: usize,
    pulse_phase: u32,
    rail: bool,
}

impl WorkspaceSidebar {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let view = Self {
            engine: None,
            snapshot: None,
            last_body: None,
            terminal_source: None,
            terminals: None,
            last_terminals: None,
            focus_handle: cx.focus_handle(),
            row_seq: 0,
            pulse_phase: 0,
            rail: false,
        };
        view.start_poll(cx);
        view.start_pulse(cx);
        view
    }

    fn next_row_id(&mut self) -> usize {
        let id = self.row_seq;
        self.row_seq += 1;
        id
    }

    pub fn attach(&mut self, engine: Arc<dyn Runtime>) {
        self.engine = Some(engine);
        self.refresh();
    }

    /// Where the sidebar reads the runtime's terminals from.
    ///
    /// The runtime is a separate process the shell has not wired up yet; until
    /// it is, the section stays hidden rather than showing an empty header.
    pub fn attach_terminals(&mut self, source: Arc<dyn TerminalSource>) {
        self.terminal_source = Some(source);
        self.refresh_terminals();
    }

    pub fn set_rail(&mut self, rail: bool) {
        self.rail = rail;
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

    /// Returns whether either source actually changed, so the poll can notify
    /// only on a real update instead of relying on the pulse to repaint.
    pub fn refresh(&mut self) -> bool {
        let session_changed = self.refresh_session();
        let terminals_changed = self.refresh_terminals();
        session_changed || terminals_changed
    }

    fn refresh_session(&mut self) -> bool {
        let Some(engine) = self.engine.clone() else {
            return false;
        };
        let (status, body) = engine.http("GET", "/api/session", &Value::Null);
        if status != 200 {
            return false;
        }
        let encoded = body.to_string();
        if self.last_body.as_deref() == Some(encoded.as_str()) {
            return false;
        }
        self.last_body = Some(encoded);
        self.snapshot = body.get("snapshot").cloned();
        true
    }

    /// The terminal source is polled on the same tick as the session. A source
    /// that cannot answer, or an empty runtime, is an empty list, not an error.
    fn refresh_terminals(&mut self) -> bool {
        let Some(source) = self.terminal_source.clone() else {
            return false;
        };
        let body = source.terminals();
        let encoded = body.as_ref().map(Value::to_string);
        if self.last_terminals == encoded {
            return false;
        }
        self.last_terminals = encoded;
        self.terminals = body;
        true
    }

    /// The runtime's terminals as tree rows; empty when nothing is attached.
    fn runtime_terminals(&self) -> Vec<TerminalRow> {
        self.terminals
            .as_ref()
            .map(terminal_rows)
            .unwrap_or_default()
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
        if let Some(terminals) = self.terminals.as_ref() {
            let rows = terminals
                .as_array()
                .or_else(|| terminals.get("terminals").and_then(Value::as_array));
            if let Some(rows) = rows {
                if rows.iter().any(|row| {
                    status_token(row.get("status").and_then(Value::as_str))
                        .and_then(AgentStatus::from_wire)
                        .is_some_and(is_animating)
                }) {
                    return true;
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

    /// The 40px titlebar: the brand mark, the "Orion" wordmark and two trailing decorative glyphs.
    fn render_titlebar(&self, theme: &Theme, scale: f32) -> impl IntoElement {
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
            .child(
                div()
                    .text_size(px(14.0 * scale))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(theme.text_strong)
                    .child(if self.rail {
                        String::new()
                    } else {
                        "Orion".to_string()
                    }),
            )
            .child(div().flex_1())
            // Decorative only: the real sidebar toggle is `header-sidebar-toggle`
            // in app.rs. These mirror the reference titlebar glyphs and carry no
            // click handler or visibility state of their own.
            .child(
                Icon::new(IconName::Menu)
                    .size(px(15.0 * scale))
                    .text_color(theme.text_muted),
            )
            .child(
                Icon::new(IconName::PanelLeft)
                    .size(px(15.0 * scale))
                    .text_color(theme.text_muted),
            )
    }

    /// The 44px search row: magnifier, placeholder and three trailing tools.
    /// The square-plus tool is the live New Workspace control; the other two
    /// are the reference's quiet affordances.
    fn render_search_row(
        &self,
        theme: &Theme,
        scale: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id("sidebar-search")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(SPACE_2 * scale))
            .h(px(TOUCH_TARGET))
            .flex_shrink_0()
            .px(px(SPACE_4 * scale))
            .child(
                Icon::new(IconName::Search)
                    .size(px(15.0 * scale))
                    .text_color(theme.text_muted),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .text_size(px(13.0 * scale))
                    .text_color(theme.token_text_dim())
                    .child(if self.rail {
                        String::new()
                    } else {
                        "Search".to_string()
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE_1 * scale))
                    .child(
                        Icon::new(IconName::Settings2)
                            .size(px(15.0 * scale))
                            .text_color(theme.text_muted),
                    )
                    .child(
                        Button::new("new-workspace-button")
                            .tooltip("New workspace")
                            .ghost()
                            .icon(IconName::Plus)
                            .min_h(px(TOUCH_TARGET))
                            .min_w(px(TOUCH_TARGET))
                            .on_click(cx.listener(|_this, _, _, cx| {
                                cx.emit(SidebarEvent::NewWorkspace);
                                cx.notify();
                            })),
                    )
                    .child(
                        Icon::new(IconName::SquareTerminal)
                            .size(px(15.0 * scale))
                            .text_color(theme.text_muted),
                    ),
            )
    }

    /// A workspace rendered as a rounded session card: agent mark and project
    /// on top, title and cwd/agent below, status word and dot on the right.
    /// Clicking the card focuses its pane; the close control stops propagation.
    fn render_workspace_row(
        &mut self,
        ws: &WorkspaceEntry,
        theme: &Theme,
        scale: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_chosen = ws.focused;
        let session_running = matches!(ws.status, AgentStatus::Working | AgentStatus::Blocked);
        let sidebar_entity = cx.entity();
        let session_label = ws.label.clone();
        let mark = ws
            .agent
            .chars()
            .next()
            .unwrap_or('?')
            .to_ascii_uppercase()
            .to_string();
        let pane_id = ws.focus_pane.clone();
        let ws_id = ws.id.clone();
        let project = if self.rail {
            String::new()
        } else {
            ws.agent.clone()
        };
        let title = if self.rail {
            String::new()
        } else {
            ws.label.clone()
        };
        let sub = if self.rail {
            String::new()
        } else {
            ws.cwd.clone().unwrap_or_else(|| ws.agent.clone())
        };
        let status_area = Self::status_area(ws.status, self.rail, theme);
        let mut row = Button::new(self.next_row_id())
            .custom(
                ButtonCustomVariant::new(cx)
                    .color(if is_chosen {
                        theme.selected_bg().into()
                    } else {
                        Theme::transparent().into()
                    })
                    .hover(if is_chosen {
                        theme.selected_bg().into()
                    } else {
                        theme.hover_bg().into()
                    }),
            )
            .w_full()
            .h(px(88.0 * scale))
            .px(px(theme.space_3()))
            .py(px(theme.space_3()))
            .rounded(px(theme.radius_card()))
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(px(theme.space_1()))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(theme.space_2()))
                            .child(Self::avatar_chip(&mark, theme, scale))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .truncate()
                                    .text_size(px(theme.font_ui_secondary()))
                                    .text_color(theme.token_text_muted())
                                    .child(project),
                            )
                            .child(status_area)
                            .child(if self.rail {
                                div().id(self.next_row_id()).into_any_element()
                            } else {
                                Button::new(self.next_row_id())
                                    .text()
                                    .child(
                                        div()
                                            .text_size(px(11.0 * scale))
                                            .text_color(theme.text_faint)
                                            .child("×"),
                                    )
                                    .tooltip("Close session")
                                    .min_h(px(TOUCH_TARGET))
                                    .min_w(px(TOUCH_TARGET))
                                    .on_click(cx.listener(move |_this, _, window, cx| {
                                        cx.stop_propagation();
                                        if session_running {
                                            let pane_id = ws_id.clone();
                                            let sidebar = sidebar_entity.clone();
                                            let label = session_label.clone();
                                            window.open_alert_dialog(cx, move |alert, _, _| {
                                                let sidebar = sidebar.clone();
                                                let pane_id = pane_id.clone();
                                                alert
                                                    .title("Close running session?")
                                                    .description(format!(
                                                        "{label} is still running. Closing it will terminate the agent."
                                                    ))
                                                    .confirm()
                                                    .ok_text("Close")
                                                    .on_ok(move |_, _, app| {
                                                        sidebar.update(app, |_view, view_cx| {
                                                            view_cx.emit(
                                                                SidebarEvent::CloseWorkspace(
                                                                    pane_id.clone(),
                                                                ),
                                                            );
                                                        });
                                                        true
                                                    })
                                            });
                                        } else {
                                            cx.emit(SidebarEvent::CloseWorkspace(ws_id.clone()));
                                        }
                                        cx.notify();
                                    }))
                                    .into_any_element()
                            })
                            .child(
                                div()
                                    .size(px(8.0))
                                    .rounded_full()
                                    .bg(Self::status_color(ws.status, theme, self.pulse_phase))
                                    .id(self.next_row_id()),
                            ),
                    )
                    .child(
                        div()
                            .w_full()
                            .truncate()
                            .text_size(px(theme.font_ui_body()))
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(theme.text_primary())
                            .child(title),
                    )
                    .child(
                        div()
                            .w_full()
                            .truncate()
                            .text_size(px(theme.font_ui_secondary()))
                            .text_color(theme.token_text_muted())
                            .child(sub),
                    ),
            );

        if let Some(pane_id) = pane_id {
            row = row.on_click(cx.listener(move |this, _, _, cx| {
                this.focus_pane(&pane_id);
                cx.emit(SidebarEvent::PaneFocused(pane_id.clone()));
                cx.notify();
            }));
        }
        row
    }

    /// An agent rendered as a session card: project, title and context split
    /// across three lines, written status word and a non-rose status dot.
    /// Clicking focuses the agent's pane.
    fn render_agent_row(
        &mut self,
        agent: &AgentRow,
        duplicates: &HashSet<(String, String)>,
        theme: &Theme,
        scale: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let pane_id = agent.pane_id.clone();
        let label = session_status::row_label(
            &agent.agent_name,
            &agent.title,
            duplicates.contains(&(agent.workspace_id.clone(), agent.agent_name.clone())),
        );
        let mark = label
            .chars()
            .next()
            .unwrap_or('?')
            .to_ascii_uppercase()
            .to_string();
        let (project, sub) = match agent.context.split_once(" · ") {
            Some((project, rest)) => (project.to_string(), rest.to_string()),
            None => (agent.context.clone(), String::new()),
        };
        let (project, sub) = if self.rail {
            (String::new(), String::new())
        } else {
            (project, sub)
        };
        let title = if self.rail { String::new() } else { label };
        let status_area = Self::status_area(agent.status, self.rail, theme);
        Button::new(self.next_row_id())
            .custom(
                ButtonCustomVariant::new(cx)
                    .color(if agent.selected {
                        theme.selected_bg().into()
                    } else {
                        Theme::transparent().into()
                    })
                    .hover(if agent.selected {
                        theme.selected_bg().into()
                    } else {
                        theme.hover_bg().into()
                    }),
            )
            .w_full()
            .h(px(88.0 * scale))
            .px(px(theme.space_3()))
            .py(px(theme.space_3()))
            .rounded(px(theme.radius_card()))
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(px(theme.space_1()))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(theme.space_2()))
                            .child(Self::avatar_chip(&mark, theme, scale))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .truncate()
                                    .text_size(px(theme.font_ui_secondary()))
                                    .text_color(theme.token_text_muted())
                                    .child(project),
                            )
                            .child(status_area)
                            .child(
                                div()
                                    .size(px(8.0))
                                    .rounded_full()
                                    .bg(Self::status_color(agent.status, theme, self.pulse_phase))
                                    .id(self.next_row_id()),
                            ),
                    )
                    .child(
                        div()
                            .w_full()
                            .truncate()
                            .text_size(px(theme.font_ui_body()))
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(theme.text_primary())
                            .child(title),
                    )
                    .child(
                        div()
                            .w_full()
                            .truncate()
                            .text_size(px(theme.font_ui_secondary()))
                            .text_color(theme.token_text_muted())
                            .child(sub),
                    ),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.focus_pane(&pane_id);
                cx.emit(SidebarEvent::PaneFocused(pane_id.clone()));
                cx.notify();
            }))
    }

    /// The small rounded-square avatar chip that leads every session card.
    fn avatar_chip(mark: &str, theme: &Theme, scale: f32) -> impl IntoElement {
        div()
            .flex_none()
            .rounded(px(theme.radius_control()))
            .bg(theme.hover_bg())
            .p(px(theme.space_1()))
            .child(
                Avatar::new()
                    .name(mark.to_string())
                    .with_size(px(16.0 * scale)),
            )
    }

    /// The trailing status of a session card: a tinted "Waiting" pill when the
    /// agent is blocked, otherwise the plain status word in dim text.
    fn status_area(status: AgentStatus, rail: bool, theme: &Theme) -> gpui_kit::AnyElement {
        let state = session_status::state_of(status);
        let word = session_status::badge_word(state);
        if state == SessionState::NeedsInput {
            div()
                .flex_none()
                .px(px(theme.space_2()))
                .py(px(theme.space_1()))
                .rounded(px(theme.radius_control()))
                .bg(theme.status_blocked_tint)
                .text_size(px(theme.font_ui_secondary()))
                .text_color(session_status::badge_color(state, theme, 0))
                .child(if rail {
                    String::new()
                } else {
                    word.to_string()
                })
                .into_any_element()
        } else {
            div()
                .flex_none()
                .text_size(px(theme.font_ui_secondary()))
                .text_color(theme.token_text_dim())
                .child(if rail {
                    String::new()
                } else {
                    word.to_string()
                })
                .into_any_element()
        }
    }

    /// A terminal rendered as a quiet one-line row in the flat session list.
    fn render_terminal_row(
        &mut self,
        row: &TerminalRow,
        theme: &Theme,
        scale: f32,
    ) -> impl IntoElement {
        let label = terminal_label(row);
        let status = status_token(row.status.as_deref()).and_then(AgentStatus::from_wire);
        let badge = status.map(|status| Self::status_color(status, theme, self.pulse_phase));
        div()
            .id(self.next_row_id())
            .flex()
            .flex_row()
            .items_center()
            .gap(px(SPACE_2 * scale))
            .min_h(px(TOUCH_TARGET))
            .pl(px((14.0 + row.depth as f32 * SPACE_3) * scale))
            .pr(px(14.0 * scale))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .text_size(px(12.0 * scale))
                    .text_color(theme.text_primary())
                    .child(if self.rail { String::new() } else { label }),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .children(badge.map(|color| div().size(px(8.0)).rounded_full().bg(color))),
            )
    }

    fn render_status_section(
        &self,
        working: &[&AgentRow],
        settled: &[&AgentRow],
        duplicates: &HashSet<(String, String)>,
        theme: &Theme,
        scale: f32,
    ) -> impl IntoElement {
        let mut section = div()
            .id("sidebar-status-section")
            .flex()
            .flex_col()
            .flex_shrink_0();

        for (name, rows, expandable) in [("Working", working, true), ("Settled", settled, false)] {
            let chevron = if expandable {
                Icon::new(IconName::ChevronRight)
                    .size(px(13.0 * scale))
                    .text_color(theme.text_faint)
                    .into_any_element()
            } else {
                div().into_any_element()
            };
            section = section.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE_2 * scale))
                    .h(px(TOUCH_TARGET))
                    .px(px(SPACE_4 * scale))
                    .text_size(px(12.0 * scale))
                    .text_color(theme.text_muted)
                    .child(if self.rail {
                        String::new()
                    } else {
                        name.to_string()
                    })
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(px(12.0 * scale))
                            .text_color(theme.token_text_dim())
                            .child(rows.len().to_string()),
                    )
                    .child(chevron),
            );

            for agent in rows {
                let state = session_status::state_of(agent.status);
                let label = session_status::row_label(
                    &agent.agent_name,
                    &agent.title,
                    duplicates.contains(&(agent.workspace_id.clone(), agent.agent_name.clone())),
                );
                let mark = label
                    .chars()
                    .next()
                    .unwrap_or('?')
                    .to_ascii_uppercase()
                    .to_string();
                section = section.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(SPACE_2 * scale))
                        .h(px(TOUCH_TARGET))
                        .pl(px(ROW_SUB_INDENT * scale))
                        .pr(px(SPACE_4 * scale))
                        .text_size(px(12.0 * scale))
                        .text_color(theme.text)
                        .child(Self::avatar_chip(&mark, theme, scale))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .truncate()
                                .text_color(theme.token_text_muted())
                                .child(if self.rail { String::new() } else { label }),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(11.0 * scale))
                                .text_color(session_status::badge_color(
                                    state,
                                    theme,
                                    self.pulse_phase,
                                ))
                                .child(if self.rail {
                                    String::new()
                                } else {
                                    session_status::badge_word(state).to_string()
                                }),
                        ),
                );
            }
        }
        section
    }

    /// The 40px bottom icon bar.
    fn render_bottom_bar(&self, theme: &Theme, scale: f32) -> impl IntoElement {
        div()
            .id("sidebar-bottom-bar")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(SPACE_4 * scale))
            .h(px(40.0))
            .flex_shrink_0()
            .px(px(SPACE_4 * scale))
            .child(
                Icon::new(IconName::SquareTerminal)
                    .size(px(15.0 * scale))
                    .text_color(theme.token_text_dim()),
            )
            .child(
                Icon::new(IconName::LayoutDashboard)
                    .size(px(15.0 * scale))
                    .text_color(theme.token_text_dim()),
            )
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

/// What a terminal row shows: its name, else its program, else its id.
fn terminal_label(row: &TerminalRow) -> String {
    if let Some(name) = row.name.as_deref().filter(|name| !name.is_empty()) {
        return name.to_string();
    }
    if !row.program.is_empty() {
        return row.program.clone();
    }
    row.id.clone()
}

impl Render for WorkspaceSidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current();
        let scale = theme.density_scale();
        let workspaces = self.workspaces();
        let agents = self.snapshot.as_ref().map(agent_rows).unwrap_or_default();
        let terminal_entries = self.runtime_terminals();

        let duplicates = session_status::duplicate_names(&agents);
        let states: Vec<SessionState> = agents
            .iter()
            .map(|agent| session_status::state_of(agent.status))
            .collect();
        let (working, settled): (Vec<&AgentRow>, Vec<&AgentRow>) =
            agents.iter().partition(|agent| {
                session_status::group(session_status::state_of(agent.status))
                    == session_status::StatusGroup::Working
            });
        debug_assert_eq!(
            orion_core::session_state::working_count(&states),
            working.len()
        );
        debug_assert_eq!(
            orion_core::session_state::settled_count(&states),
            settled.len()
        );

        let mut sessions = div()
            .id("sidebar-sessions")
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .pb(px(SPACE_2 * scale))
            .children(
                workspaces
                    .iter()
                    .map(|ws| self.render_workspace_row(ws, &theme, scale, cx)),
            )
            .children(
                agents
                    .iter()
                    .map(|agent| self.render_agent_row(agent, &duplicates, &theme, scale, cx)),
            );

        // The terminal section stays conditional on non-empty real data.
        if !terminal_entries.is_empty() {
            sessions = sessions.children(
                terminal_entries
                    .iter()
                    .map(|row| self.render_terminal_row(row, &theme, scale)),
            );
        }

        if workspaces.is_empty() {
            sessions = sessions.child(
                div()
                    .px(px(SPACE_4 * scale))
                    .py(px(SPACE_2 * scale))
                    .text_size(px(12.0 * scale))
                    .text_color(theme.text_faint)
                    .child(if self.rail {
                        String::new()
                    } else {
                        "No workspaces yet — open a workspace to begin.".to_string()
                    }),
            );
        }

        div()
            .id("workspace-sidebar")
            .when(self.focus_handle.is_focused(window), |el| {
                el.focus_ring_style(window, cx)
            })
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.panel_bg())
            .rounded(px(theme.radius_panel()))
            .child(self.render_titlebar(&theme, scale))
            .child(self.render_search_row(&theme, scale, cx))
            .child(sessions)
            .child(self.render_status_section(&working, &settled, &duplicates, &theme, scale))
            .child(self.render_bottom_bar(&theme, scale))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::rgb;

    crate::orion_test!(
        fn sidebar_renders_empty_state_without_panic(cx: &mut gpui_kit::TestAppContext) {
            use gpui_kit::{point, size};
            let cx = cx.add_empty_window();
            cx.draw(point(px(0.), px(0.)), size(px(264.), px(800.)), |_, cx| {
                cx.new(WorkspaceSidebar::new).into_any_element()
            });
        }
    );

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
}
