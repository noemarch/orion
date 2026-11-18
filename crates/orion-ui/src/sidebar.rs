//! Real-data sidebar (P4): replaces the old static `workspace.rs` entries
//! ("w1"/"t1"/"p1") with the live engine snapshot.
//!
//! `GET /api/session` returns `{snapshot: {workspaces, tabs, panes,
//! focused_pane_id, ...}}` (see `crates/orion-core/src/engine/session.rs`
//! `snapshot_value`): workspaces carry `workspace_id`/`label`/
//! `active_tab_id`/`agent_status`, tabs carry `tab_id`/`workspace_id`/
//! `label`/`agent_status`, panes carry `pane_id`/`workspace_id`/`tab_id`/
//! `label`/`display_agent`/`agent_status`/`focused`. This view polls that
//! seam on a 2s timer (the `ChatView` pattern) and renders one row per
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
use crate::terminal_tree::{status_token, terminal_rows, TerminalRow, TerminalSource};
use crate::theme::Theme;
use gpui_kit::assets::IconName;
use gpui_kit::component::{avatar::Avatar, button::Button, button::ButtonVariants, Icon, Sizable};
use gpui_kit::*;
use gpui_kit::{div, px, Context, FocusHandle, Render, Styled, Window};
use orion_core::protocol::AgentStatus;
use orion_core::runtime::Runtime;
use serde_json::{json, Value};
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

    pub fn refresh(&mut self) {
        self.refresh_session();
        self.refresh_terminals();
    }

    fn refresh_session(&mut self) {
        let Some(engine) = self.engine.clone() else {
            return;
        };
        let (status, body) = engine.http("GET", "/api/session", &Value::Null);
        if status != 200 {
            return;
        }
        let encoded = body.to_string();
        if self.last_body.as_deref() == Some(encoded.as_str()) {
            return;
        }
        self.last_body = Some(encoded);
        self.snapshot = body.get("snapshot").cloned();
    }

    /// The terminal source is polled on the same tick as the session. A source
    /// that cannot answer, or an empty runtime, is an empty list, not an error.
    fn refresh_terminals(&mut self) {
        let Some(source) = self.terminal_source.clone() else {
            return;
        };
        let body = source.terminals();
        let encoded = body.as_ref().map(Value::to_string);
        if self.last_terminals == encoded {
            return;
        }
        self.last_terminals = encoded;
        self.terminals = body;
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
            let _ = view.update(cx, |view, _cx| view.refresh());
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
                // Pulse only matters for live statuses; tick the phase so
                // the dots breathe instead of strobing with the 2s poll.
                view.pulse_phase = view.pulse_phase.wrapping_add(1);
                cx.notify();
            });
            if view.upgrade().is_none() {
                break;
            }
        })
        .detach();
    }

    /// Status dot color for a protocol `AgentStatus`. Never rose (DESIGN.md
    /// §1): RUN is the working cyan, INPUT the blocked coral, DONE the
    /// emerald, ERROR the indicator ruby. Live statuses breathe between
    /// alpha 0.55 and 1.0 by `pulse` (DESIGN.md: RUN = blue/cyan pulse).
    fn status_color(status: AgentStatus, theme: &Theme, pulse: u32) -> gpui_kit::Rgba {
        let base = match status {
            AgentStatus::Idle => theme.status_idle,
            AgentStatus::Working => theme.status_working,
            AgentStatus::Blocked => theme.status_blocked,
            AgentStatus::Done => theme.status_done,
            AgentStatus::Error => theme.indicator_blocked,
            AgentStatus::Unknown => theme.indicator_unknown,
        };
        match status {
            AgentStatus::Working | AgentStatus::Blocked => {
                let phase = (pulse % 16) as f32 / 16.0;
                let wave = if phase < 0.5 {
                    phase * 2.0
                } else {
                    2.0 - phase * 2.0
                };
                gpui_kit::Rgba {
                    r: base.r,
                    g: base.g,
                    b: base.b,
                    a: 0.55 + wave * 0.45,
                }
            }
            _ => base,
        }
    }

    /// A 12px platform window light. The four colors come from status and
    /// indicator tokens, so the traffic-light motif stays inside the palette
    /// instead of hardcoding hex in view code.
    fn window_light(color: gpui_kit::Rgba) -> impl IntoElement {
        div().size(px(12.0)).rounded_full().bg(color)
    }

    /// The 48px titlebar: four window lights, the brand mark, the "Orion"
    /// wordmark and two trailing decorative glyphs.
    fn render_titlebar(&self, theme: &Theme, scale: f32) -> impl IntoElement {
        div()
            .id("sidebar-titlebar")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(SPACE_2 * scale))
            .h(px(48.0))
            .flex_shrink_0()
            .px(px(SPACE_4 * scale))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE_2 * scale))
                    .mr(px(SPACE_2 * scale))
                    .child(Self::window_light(theme.indicator_blocked))
                    .child(Self::window_light(theme.warning_amber))
                    .child(Self::window_light(theme.status_done))
                    .child(Self::window_light(theme.text_faint)),
            )
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
            .border_b_1()
            .border_color(theme.border_soft)
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
                    .text_color(theme.text_faint)
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

    /// A quiet section label with a real count badge. The caller passes an
    /// already-blanked label in rail mode.
    fn section_header(label: &str, count: usize, theme: &Theme, scale: f32) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(SPACE_2 * scale))
            .h(px(30.0 * scale))
            .px(px(SPACE_4 * scale))
            .text_size(px(11.0 * scale))
            .text_color(theme.text_muted)
            .child(label.to_string())
            .child(div().flex_1())
            .child(
                div()
                    .px(px(SPACE_2 * scale))
                    .py(px(1.0 * scale))
                    .rounded(px(RADIUS_SM * scale))
                    .bg(theme.badge_bg)
                    .border_1()
                    .border_color(theme.badge_border)
                    .text_color(theme.text_faint)
                    .child(count.to_string()),
            )
    }

    /// A workspace rendered as a reference-style session row: agent mark and
    /// label on top, real cwd/agent below, status dot on the right. Clicking
    /// the row focuses its pane; the close control stops propagation.
    fn render_workspace_row(
        &mut self,
        ws: &WorkspaceEntry,
        theme: &Theme,
        scale: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_chosen = ws.focused;
        let mark = ws
            .agent
            .chars()
            .next()
            .unwrap_or('?')
            .to_ascii_uppercase()
            .to_string();
        let pane_id = ws.focus_pane.clone();
        let ws_id = ws.id.clone();
        let sub = if self.rail {
            String::new()
        } else {
            ws.cwd.clone().unwrap_or_else(|| ws.agent.clone())
        };
        let mut row = Button::new(self.next_row_id())
            .text()
            .w_full()
            .min_h(px(TOUCH_TARGET))
            .px(px(14.0 * scale))
            .py(px(SPACE_2 * scale))
            .border_l_2()
            .border_color(if is_chosen {
                theme.accent_emphasis
            } else {
                Theme::transparent()
            })
            .bg(if is_chosen {
                theme.surface_raised_2
            } else {
                Theme::transparent()
            })
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_1 * scale))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(SPACE_2 * scale))
                            .child(Avatar::new().name(mark).with_size(px(20.0 * scale)))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .truncate()
                                    .text_size(px(13.0 * scale))
                                    .text_color(if is_chosen {
                                        theme.text_strong
                                    } else {
                                        theme.text
                                    })
                                    .child(if self.rail {
                                        String::new()
                                    } else {
                                        ws.label.clone()
                                    }),
                            )
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
                                    .min_h(px(TOUCH_TARGET))
                                    .min_w(px(TOUCH_TARGET))
                                    .on_click(cx.listener(move |_this, _, _, cx| {
                                        cx.stop_propagation();
                                        cx.emit(SidebarEvent::CloseWorkspace(ws_id.clone()));
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
                            .pl(px(ROW_SUB_INDENT * scale))
                            .truncate()
                            .text_size(px(11.0 * scale))
                            .text_color(theme.text_muted)
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

    /// An agent rendered as a session row: title, real context, written status
    /// word and a non-rose status dot. Clicking focuses the agent's pane.
    fn render_agent_row(
        &mut self,
        agent: &AgentRow,
        theme: &Theme,
        scale: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let pane_id = agent.pane_id.clone();
        let mark = agent
            .title
            .chars()
            .next()
            .unwrap_or('?')
            .to_ascii_uppercase()
            .to_string();
        Button::new(self.next_row_id())
            .text()
            .w_full()
            .min_h(px(TOUCH_TARGET))
            .px(px(14.0 * scale))
            .py(px(SPACE_2 * scale))
            .border_l_2()
            .border_color(if agent.selected {
                theme.accent_emphasis
            } else {
                Theme::transparent()
            })
            .bg(if agent.selected {
                theme.surface_raised_2
            } else {
                Theme::transparent()
            })
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_1 * scale))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(SPACE_2 * scale))
                            .child(Avatar::new().name(mark).with_size(px(20.0 * scale)))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .truncate()
                                    .text_size(px(13.0 * scale))
                                    .text_color(if agent.selected {
                                        theme.text_strong
                                    } else {
                                        theme.text
                                    })
                                    .child(if self.rail {
                                        String::new()
                                    } else {
                                        agent.title.clone()
                                    }),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(px(10.0 * scale))
                                    .text_color(theme.text_muted)
                                    .child(if self.rail {
                                        String::new()
                                    } else {
                                        agent.status_word().to_string()
                                    }),
                            )
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
                            .pl(px(ROW_SUB_INDENT * scale))
                            .truncate()
                            .text_size(px(11.0 * scale))
                            .text_color(theme.text_muted)
                            .child(if self.rail {
                                String::new()
                            } else {
                                agent.context.clone()
                            }),
                    ),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.focus_pane(&pane_id);
                cx.emit(SidebarEvent::PaneFocused(pane_id.clone()));
                cx.notify();
            }))
    }

    /// A terminal rendered as a quiet indented row with its real status badge.
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
                    .text_color(theme.text)
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

    /// The reference's bottom side-section: Working and Settled counts derived
    /// from live agent statuses, with real agent titles in the side-item slots.
    /// The screenshot's time labels are fabricated and are never reproduced.
    fn render_status_section(
        &self,
        agents: &[AgentRow],
        working: usize,
        settled: usize,
        theme: &Theme,
        scale: f32,
    ) -> impl IntoElement {
        let mut section = div()
            .id("sidebar-status-section")
            .flex()
            .flex_col()
            .flex_shrink_0()
            .child(
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
                        "Working".to_string()
                    })
                    .child(div().flex_1())
                    .child(
                        div()
                            .px(px(SPACE_2 * scale))
                            .rounded(px(RADIUS_SM * scale))
                            .bg(theme.badge_bg)
                            .border_1()
                            .border_color(theme.badge_border)
                            .text_color(theme.text_faint)
                            .child(working.to_string()),
                    )
                    .child(
                        Icon::new(IconName::ChevronRight)
                            .size(px(13.0 * scale))
                            .text_color(theme.text_faint),
                    ),
            )
            .child(
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
                        "Settled".to_string()
                    })
                    .child(div().flex_1())
                    .child(
                        div()
                            .px(px(SPACE_2 * scale))
                            .rounded(px(RADIUS_SM * scale))
                            .bg(theme.badge_bg)
                            .border_1()
                            .border_color(theme.badge_border)
                            .text_color(theme.text_faint)
                            .child(settled.to_string()),
                    ),
            );

        for agent in agents.iter().take(3) {
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
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .truncate()
                            .child(if self.rail {
                                String::new()
                            } else {
                                agent.title.clone()
                            }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(11.0 * scale))
                            .text_color(theme.text_faint)
                            .child(if self.rail {
                                String::new()
                            } else {
                                agent.status_word().to_string()
                            }),
                    ),
            );
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
            .border_t_1()
            .border_color(theme.border_soft)
            .child(
                Icon::new(IconName::SquareTerminal)
                    .size(px(15.0 * scale))
                    .text_color(theme.text_muted),
            )
            .child(
                Icon::new(IconName::LayoutDashboard)
                    .size(px(15.0 * scale))
                    .text_color(theme.text_muted),
            )
    }
}

fn parse_status(raw: Option<&str>) -> AgentStatus {
    raw.and_then(AgentStatus::from_wire)
        .unwrap_or(AgentStatus::Idle)
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current();
        let scale = theme.density_scale();
        let workspaces = self.workspaces();
        let agents = self.snapshot.as_ref().map(agent_rows).unwrap_or_default();
        let terminal_entries = self.runtime_terminals();

        // Counts come from live agent statuses; no number here is invented.
        let working = agents
            .iter()
            .filter(|a| matches!(a.status, AgentStatus::Working | AgentStatus::Blocked))
            .count();
        let settled = agents
            .iter()
            .filter(|a| matches!(a.status, AgentStatus::Done | AgentStatus::Idle))
            .count();

        let mut sessions = div()
            .id("sidebar-sessions")
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .pb(px(SPACE_2 * scale))
            .child(Self::section_header(
                if self.rail { "" } else { "WORKSPACES" },
                workspaces.len(),
                &theme,
                scale,
            ))
            .children(
                workspaces
                    .iter()
                    .map(|ws| self.render_workspace_row(ws, &theme, scale, cx)),
            )
            .child(Self::section_header(
                if self.rail { "" } else { "AGENTS" },
                agents.len(),
                &theme,
                scale,
            ))
            .children(
                agents
                    .iter()
                    .map(|agent| self.render_agent_row(agent, &theme, scale, cx)),
            );

        // The terminal section stays conditional on non-empty real data.
        if !terminal_entries.is_empty() {
            sessions = sessions
                .child(Self::section_header(
                    if self.rail { "" } else { "TERMINALS" },
                    terminal_entries.len(),
                    &theme,
                    scale,
                ))
                .children(
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
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.surface_canvas)
            .child(self.render_titlebar(&theme, scale))
            .child(self.render_search_row(&theme, scale, cx))
            .child(sessions)
            .child(self.render_status_section(&agents, working, settled, &theme, scale))
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
        assert_eq!(parse_status(None), AgentStatus::Idle);
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
