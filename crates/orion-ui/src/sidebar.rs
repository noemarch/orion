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
use crate::theme::{Theme, DOT_SIZE_PX, ICON_SIZE_PX, TOUCH_TARGET_PX};
use gpui_kit::assets::IconName;
use gpui_kit::base::{v_virtual_list, VirtualListScrollHandle};
use gpui_kit::component::{button::Button, button::ButtonVariants, Icon, ThemeStyled};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::test::TestSupportExt;
use gpui_kit::*;
use gpui_kit::{div, px, Context, FocusHandle, Render, Styled, Window};
use orion_core::protocol::AgentStatus;
use orion_core::runtime::Runtime;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::rc::Rc;
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
const HEADER_ROW_PX: f32 = 30.0;
const PANE_ROW_PX: f32 = 48.0;
const MONO: &str = "JetBrains Mono";

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

enum TreeRow {
    Header {
        workspace: WorkspaceEntry,
        pane_count: usize,
        collapsed: bool,
    },
    Pane {
        agent: AgentRow,
        subtitle: String,
        brand: IconName,
    },
}

/// The sidebar view. State is the last session snapshot; `refresh()` swaps
/// it in when the wire body changes (stale rows are swept with it).
pub struct WorkspaceSidebar {
    engine: Option<Arc<dyn Runtime>>,
    snapshot: Option<Value>,
    last_body: Option<String>,
    focus_handle: FocusHandle,
    pulse_phase: u32,
    collapsed_workspaces: HashSet<String>,
    scroll: VirtualListScrollHandle,
}

impl WorkspaceSidebar {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let view = Self {
            engine: None,
            snapshot: None,
            last_body: None,
            focus_handle: cx.focus_handle().tab_stop(true),
            pulse_phase: 0,
            collapsed_workspaces: HashSet::new(),
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

    /// The snapshot body (`snapshot` object) backing the current render.
    pub fn snapshot(&self) -> Option<Value> {
        self.snapshot.clone()
    }

    /// Opaque token that changes only when the session body changes. The app
    /// compares it on sidebar notifies to skip cache rebuilds on pulse ticks.
    pub fn snapshot_fingerprint(&self) -> Option<&str> {
        self.last_body.as_deref()
    }

    fn visible_rows(&self) -> Vec<TreeRow> {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return Vec::new();
        };
        let agents = agent_rows(snapshot);
        let panes = snapshot.get("panes").and_then(Value::as_array);
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
                .collect();
            let collapsed = self.collapsed_workspaces.contains(&ws.id);
            rows.push(TreeRow::Header {
                workspace: ws,
                pane_count: group.len(),
                collapsed,
            });
            if collapsed {
                continue;
            }
            for agent in group {
                let subtitle = pane_field(panes, &agent.pane_id, "cwd")
                    .or_else(|| pane_field(panes, &agent.pane_id, "shell_path"))
                    .or_else(|| pane_field(panes, &agent.pane_id, "shell"))
                    .unwrap_or_default()
                    .to_string();
                let brand = if pane_field(panes, &agent.pane_id, "pane_kind") == Some("shell") {
                    IconName::SquareTerminal
                } else {
                    IconName::Bot
                };
                rows.push(TreeRow::Pane {
                    agent: agent.clone(),
                    subtitle,
                    brand,
                });
            }
        }
        rows
    }

    fn toggle_workspace(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.collapsed_workspaces.remove(id) {
            self.collapsed_workspaces.insert(id.to_string());
        }
        cx.notify();
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
            .size(px(DOT_SIZE_PX))
            .rounded_full();
        match status {
            AgentStatus::Idle => base
                .border_1()
                .border_color(theme.status_idle)
                .into_any_element(),
            _ => base
                .bg(Self::status_color(status, theme, self.pulse_phase))
                .into_any_element(),
        }
    }

    fn render_tree_row(
        &self,
        row: &TreeRow,
        rail: bool,
        theme: &Theme,
        scale: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        match row {
            TreeRow::Header {
                workspace,
                pane_count,
                collapsed,
            } => {
                let id = workspace.id.clone();
                let chevron = if *collapsed {
                    IconName::ChevronRight
                } else {
                    IconName::ChevronDown
                };
                let count = format!(
                    "{pane_count} {}",
                    if *pane_count == 1 { "pane" } else { "panes" }
                );
                div()
                    .id(format!("sidebar-workspace-{}", workspace.id))
                    .test_support()
                    .h(px(HEADER_ROW_PX))
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE_2 * scale))
                    .px(px(SPACE_2 * scale))
                    .role(gpui_kit::Role::Button)
                    .aria_expanded(!collapsed)
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_workspace(&id, cx)))
                    .child(
                        Icon::new(chevron)
                            .size(px(12.0))
                            .text_color(theme.token_text_muted()),
                    )
                    .child(
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
                    .child(
                        div()
                            .id(format!("sidebar-workspace-{}-count", workspace.id))
                            .test_support()
                            .aria_label(count)
                            .flex_none()
                            .text_size(px(theme.font_ui_secondary()))
                            .text_color(theme.token_text_muted())
                            .child(if rail {
                                String::new()
                            } else {
                                pane_count.to_string()
                            }),
                    )
                    .child(
                        Button::new(format!("sidebar-workspace-{}-create", workspace.id))
                            .ghost()
                            .icon(IconName::Plus)
                            .tooltip("New pane")
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.stop_propagation();
                                cx.emit(SidebarEvent::NewWorkspace);
                                cx.notify();
                            })),
                    )
            }
            TreeRow::Pane {
                agent,
                subtitle,
                brand,
            } => {
                let pane_id = agent.pane_id.clone();
                let needs_input = matches!(agent.status, AgentStatus::Blocked);
                div()
                    .id(format!("sidebar-pane-{}", agent.pane_id))
                    .test_support()
                    .h(px(PANE_ROW_PX))
                    .w_full()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .gap(px(SPACE_1 * scale))
                    .px(px(SPACE_3 * scale))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.focus_pane(&pane_id);
                        cx.emit(SidebarEvent::PaneFocused(pane_id.clone()));
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(SPACE_2 * scale))
                            .child(self.status_indicator(&agent.pane_id, agent.status, theme))
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
                            .child(
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
                            }),
                    )
                    .child(
                        div()
                            .id(format!("sidebar-pane-{}-subtitle", agent.pane_id))
                            .test_support()
                            .aria_label(subtitle.clone())
                            .w_full()
                            .min_w(px(0.0))
                            .truncate()
                            .font_family(MONO)
                            .text_size(px(theme.font_ui_secondary()))
                            .text_color(theme.token_text_muted())
                            .child(if rail {
                                String::new()
                            } else {
                                subtitle.clone()
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

    /// The 40px titlebar: the brand mark, the "Orion" wordmark and two trailing decorative glyphs.
    fn render_titlebar(&self, rail: bool, theme: &Theme, scale: f32) -> impl IntoElement {
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
                    .child(if rail {
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
                    .size(theme.scaled_icon_size())
                    .text_color(theme.text_muted),
            )
            .child(
                Icon::new(IconName::PanelLeft)
                    .size(theme.scaled_icon_size())
                    .text_color(theme.text_muted),
            )
    }

    /// The 44px search row: magnifier, placeholder and three trailing tools.
    /// The row itself is decorative — it has no text input and no click or
    /// focus handler; search lives in the command palette (DESIGN.md:997).
    /// The square-plus tool is the live New Workspace control; the other two
    /// are the reference's quiet affordances.
    fn render_search_row(
        &self,
        rail: bool,
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
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .text_size(px(13.0 * scale))
                    .text_color(theme.token_text_dim())
                    .child(if rail {
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
                        Icon::new(IconName::Settings)
                            .size(theme.scaled_icon_size())
                            .text_color(theme.text_muted),
                    )
                    .child(
                        Button::new("new-workspace-button")
                            .tooltip("New workspace")
                            .ghost()
                            .icon(IconName::Plus)
                            .min_h(px(TOUCH_TARGET_PX))
                            .min_w(px(TOUCH_TARGET_PX))
                            .on_click(cx.listener(|_this, _, _, cx| {
                                cx.emit(SidebarEvent::NewWorkspace);
                                cx.notify();
                            })),
                    )
                    .child(
                        Icon::new(IconName::SquareTerminal)
                            .size(theme.scaled_icon_size())
                            .text_color(theme.text_muted),
                    ),
            )
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
                    .size(theme.scaled_icon_size())
                    .text_color(theme.token_text_dim()),
            )
            .child(
                Icon::new(IconName::LayoutDashboard)
                    .size(theme.scaled_icon_size())
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

impl Render for WorkspaceSidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current();
        let scale = theme.density_scale();
        let rail = ShellLayout::for_width(f32::from(window.bounds().size.width)).rail;
        let rows = self.visible_rows();
        let empty = rows.is_empty();
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
                    move |this, range, _window, cx| {
                        range
                            .filter_map(|ix| rows.get(ix))
                            .map(|row| {
                                this.render_tree_row(row, rail, &theme, scale, cx)
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
                        .px(px(SPACE_4 * scale))
                        .py(px(SPACE_2 * scale))
                        .text_size(px(12.0 * scale))
                        .text_color(theme.text_faint)
                        .child(if rail {
                            String::new()
                        } else {
                            "No workspaces yet — open a workspace to begin.".to_string()
                        }),
                )
            });

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
            .child(self.render_titlebar(rail, &theme, scale))
            .child(self.render_search_row(rail, &theme, scale, cx))
            .child(tree)
            .child(self.render_bottom_bar(&theme, scale))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::rgb;
    use gpui_kit::test::{TestQueryExt, TestWindowExt};
    use orion_core::runtime::PaneHandle;
    use orion_core::FrameSink;

    struct TreeFixture {
        snapshot: Value,
    }

    impl Runtime for TreeFixture {
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
            let (root, cx) = cx.add_window_view(|_, cx| WorkspaceSidebar::new(cx));
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
            let runtime: Arc<dyn Runtime> = Arc::new(TreeFixture {
                snapshot: status_snapshot(),
            });
            let (root, cx) = cx.add_window_view(|_, cx| WorkspaceSidebar::new(cx));
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
    ) {
        let runtime: Arc<dyn Runtime> = Arc::new(TreeFixture {
            snapshot: tree_snapshot(),
        });
        let (root, cx) = cx.add_window_view(|_, cx| WorkspaceSidebar::new(cx));
        cx.update(|_, app| root.update(app, |view, _cx| view.attach(Arc::clone(&runtime))));
        (root, cx)
    }

    crate::orion_test!(
        fn workspaces_group_their_panes_and_collapse_together(cx: &mut gpui_kit::TestAppContext) {
            let (_root, cx) = open_tree_fixture(cx);
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
            let _ = cx.update(|window, _| window.find_by_label("2 panes"));
            let _ = cx.update(|window, _| window.find_by_label("1 pane"));

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
            let _ = cx.update(|window, _| window.find_by_label("2 panes"));

            cx.update(|window, cx| window.click("sidebar-workspace-ws-1", cx));
            assert!(
                cx.update(|window, _| window.try_find("sidebar-pane-p1").is_some()),
                "the header expands its group again"
            );
        }
    );

    crate::orion_test!(
        fn pane_rows_name_status_and_show_a_monospace_subtitle(cx: &mut gpui_kit::TestAppContext) {
            let (_root, cx) = open_tree_fixture(cx);
            cx.update(|window, cx| window.render_frame(cx));

            for label in ["Working", "Waiting", "Done", "/work/orion", "/bin/bash"] {
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
            let runtime: Arc<dyn Runtime> = Arc::new(TreeFixture {
                snapshot: tree_snapshot(),
            });
            let (root, cx) = cx.add_window_view(|_, cx| MountHarness {
                sidebar: cx.new(WorkspaceSidebar::new),
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
            let (_root, cx) = open_tree_fixture(cx);
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
}
