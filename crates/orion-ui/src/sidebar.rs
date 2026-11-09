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

use crate::agents::agent_rows;
use crate::theme::Theme;
use gpui::prelude::*;
use gpui::{div, px, Context, FocusHandle, Render, Styled, StyledText, TextStyle, Window};
use orion_core::protocol::AgentStatus;
use orion_core::Engine;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

/// Sidebar width (matches `app.rs`'s column and `terminal.rs`'s
/// `SIDEBAR_WIDTH_PX`).
pub const SIDEBAR_WIDTH_PX: f32 = 280.0;

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
}

/// One tab row (shown under its workspace; also what the app's tab strip
/// reads from the snapshot).
#[derive(Debug, Clone)]
pub struct TabEntry {
    pub id: String,
    pub label: String,
    pub status: AgentStatus,
    pub focused: bool,
    pub pane_ids: Vec<String>,
}

/// The sidebar view. State is the last session snapshot; `refresh()` swaps
/// it in when the wire body changes (stale rows are swept with it).
pub struct WorkspaceSidebar {
    engine: Option<Arc<Engine>>,
    snapshot: Option<Value>,
    last_body: Option<String>,
    focus_handle: FocusHandle,
    row_seq: usize,
    pulse_phase: u32,
}

impl WorkspaceSidebar {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let view = Self {
            engine: None,
            snapshot: None,
            last_body: None,
            focus_handle: cx.focus_handle(),
            row_seq: 0,
            pulse_phase: 0,
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

    pub fn attach(&mut self, engine: Arc<Engine>) {
        self.engine = Some(engine);
        self.refresh();
    }

    /// The snapshot body (`snapshot` object) backing the current render.
    pub fn snapshot(&self) -> Option<Value> {
        self.snapshot.clone()
    }

    /// Workspaces of the current snapshot, in engine order.
    pub fn workspaces(&self) -> Vec<WorkspaceEntry> {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return Vec::new();
        };
        let panes = snapshot.get("panes").and_then(Value::as_array);
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
                        let ws_panes: Vec<&Value> = panes
                            .map(|p| {
                                p.iter()
                                    .filter(|p| {
                                        p.get("workspace_id").and_then(Value::as_str)
                                            == Some(id.as_str())
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        let focus_pane = ws_panes
                            .iter()
                            .find(|p| p.get("focused").and_then(Value::as_bool) == Some(true))
                            .or_else(|| ws_panes.first())
                            .and_then(|p| {
                                p.get("pane_id").and_then(Value::as_str).map(str::to_string)
                            })
                            .or_else(|| focused_pane.and_then(Value::as_str).map(str::to_string));
                        let agent = ws_panes
                            .first()
                            .and_then(|p| p.get("agent").and_then(Value::as_str))
                            .unwrap_or("claude")
                            .to_string();
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
                        let pane_ids: Vec<String> = panes
                            .map(|p| {
                                p.iter()
                                    .filter(|p| {
                                        p.get("tab_id").and_then(Value::as_str)
                                            == Some(tab_id.as_str())
                                    })
                                    .filter_map(|p| {
                                        p.get("pane_id").and_then(Value::as_str).map(str::to_string)
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        TabEntry {
                            focused: tab.get("focused").and_then(Value::as_bool) == Some(true),
                            id: tab_id,
                            label: tab
                                .get("label")
                                .and_then(Value::as_str)
                                .unwrap_or("Tab")
                                .to_string(),
                            status: parse_status(tab.get("agent_status").and_then(Value::as_str)),
                            pane_ids,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Focus a pane through the wire seam (`POST /api/pane/focus`).
    pub fn focus_pane(&self, pane_id: &str) {
        if let Some(engine) = self.engine.clone() {
            let body = json!({ "pane_id": pane_id });
            let _ = engine.http("POST", "/api/pane/focus", &body);
        }
    }

    pub fn refresh(&mut self) {
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

    fn start_poll(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |view, cx| loop {
            cx.background_executor().timer(Duration::from_secs(2)).await;
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
                .timer(std::time::Duration::from_millis(120))
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
    fn status_color(status: AgentStatus, theme: &Theme, pulse: u32) -> gpui::Rgba {
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
                gpui::Rgba {
                    r: base.r,
                    g: base.g,
                    b: base.b,
                    a: 0.55 + wave * 0.45,
                }
            }
            _ => base,
        }
    }
}

fn parse_status(raw: Option<&str>) -> AgentStatus {
    match raw {
        Some("working") => AgentStatus::Working,
        Some("blocked") => AgentStatus::Blocked,
        Some("done") => AgentStatus::Done,
        Some("error") => AgentStatus::Error,
        _ => AgentStatus::Idle,
    }
}

impl Render for WorkspaceSidebar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current();
        let scale = theme.density_scale();
        let row_h = theme.row_height() * scale;
        let focused_id = self
            .snapshot
            .as_ref()
            .and_then(|s| s.get("focused_pane_id"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let workspaces = self.workspaces();
        let agents = self
            .snapshot
            .as_ref()
            .map(agent_rows)
            .unwrap_or_default();

        let empty = workspaces.is_empty();
        let list = div()
            .flex_col()
            .gap_1()
            .px_2()
            .py_2()
            .children(workspaces.iter().map(|ws| {
                let is_chosen = ws.focused;
                let mark = ws
                    .agent
                    .chars()
                    .next()
                    .unwrap_or('?')
                    .to_ascii_uppercase()
                    .to_string();
                let pane_id = ws.focus_pane.clone();
                let row = div()
                    .id(self.next_row_id())
                    .flex_row()
                    .items_center()
                    .gap(px(8.0 * scale))
                    .h(px(row_h))
                    .px_2()
                    .rounded_md()
                    .bg(if is_chosen {
                        theme.surface_hover
                    } else {
                        Theme::transparent()
                    })
                    .child(
                        div()
                            .flex()
                            .size(px(22.0 * scale))
                            .rounded_md()
                            .items_center()
                            .justify_center()
                            .bg(theme.accent_tint)
                            .text_color(theme.text_strong)
                            .child(mark),
                    )
                    .child(StyledText::new(ws.label.clone()).with_default_highlights(
                        &TextStyle {
                            color: if is_chosen {
                                theme.text_strong.into()
                            } else {
                                theme.text.into()
                            },
                            font_size: gpui::rems(0.875).into(),
                            ..Default::default()
                        },
                        [],
                    ))
                    .child(
                        div().flex_1().flex_row().justify_end().child(
                            div()
                                .size(px(8.0))
                                .rounded_full()
                                .bg(Self::status_color(ws.status, &theme, self.pulse_phase))
                                .id(self.next_row_id()),
                        ),
                    );
                let row = if let Some(pane_id) = pane_id {
                    row.on_click(cx.listener(move |this, _, _, cx| {
                        this.focus_pane(&pane_id);
                        cx.notify();
                    }))
                } else {
                    row
                };
                row
            }));

        let _ = focused_id;
        div()
            .id("workspace-sidebar")
            .track_focus(&self.focus_handle)
            .flex_col()
            .size_full()
            .bg(theme.surface_panel)
            .child(
                div()
                    .flex_row()
                    .items_center()
                    .px_2()
                    .py_1()
                    .text_size(px(11.0 * scale))
                    .text_color(theme.text_dim)
                    .child("WORKSPACES"),
            )
            .child(list)
            .child(
                div()
                    .flex_row()
                    .items_center()
                    .px_2()
                    .py_1()
                    .text_size(px(11.0 * scale))
                    .text_color(theme.text_dim)
                    .child("AGENTS"),
            )
            .child(div().flex_col().gap_1().px_2().py_2().children(
                agents.iter().map(|agent| {
                    let pane_id = agent.pane_id.clone();
                    div()
                        .id(self.next_row_id())
                        .flex_row()
                        .items_center()
                        .gap(px(8.0 * scale))
                        .h(px(row_h))
                        .px_2()
                        .rounded_md()
                        .bg(if agent.selected {
                            theme.surface_hover
                        } else {
                            Theme::transparent()
                        })
                        .child(
                            div()
                                .flex()
                                .size(px(22.0 * scale))
                                .rounded_md()
                                .items_center()
                                .justify_center()
                                .bg(theme.accent_tint)
                                .text_color(theme.text_strong)
                                .child(
                                    agent
                                        .title
                                        .chars()
                                        .next()
                                        .unwrap_or('?')
                                        .to_ascii_uppercase()
                                        .to_string(),
                                ),
                        )
                        .child(
                            div()
                                .text_size(px(12.0 * scale))
                                .text_color(theme.text)
                                .child(agent.title.clone()),
                        )
                        .child(
                            div().flex_1().flex_row().justify_end().child(
                                div()
                                    .size(px(8.0))
                                    .rounded_full()
                                    .bg(Self::status_color(
                                        agent.status,
                                        &theme,
                                        self.pulse_phase,
                                    ))
                                    .id(self.next_row_id()),
                            ),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.focus_pane(&pane_id);
                            cx.notify();
                        }))
                }),
            ))
            .child(div().flex_1().child(if empty {
                div()
                    .px_2()
                    .py_1()
                    .text_size(px(12.0 * scale))
                    .text_color(theme.text_dim)
                    .child("No workspaces yet — open a workspace to begin.")
            } else {
                div()
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::rgb;

    #[test]
    fn status_parses_wire_names() {
        assert_eq!(parse_status(Some("working")), AgentStatus::Working);
        assert_eq!(parse_status(Some("blocked")), AgentStatus::Blocked);
        assert_eq!(parse_status(Some("done")), AgentStatus::Done);
        assert_eq!(parse_status(Some("error")), AgentStatus::Error);
        assert_eq!(parse_status(Some("idle")), AgentStatus::Idle);
        assert_eq!(parse_status(None), AgentStatus::Idle);
    }

    #[test]
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
            rgb(0x7fc4de)
        );
    }
}
