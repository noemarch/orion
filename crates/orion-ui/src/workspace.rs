//! Workspace sidebar view — lists workspaces, tabs, and panes.

use gpui::prelude::*;
use gpui::{
    div, px, rems, rgb, rgba, white, Context, IntoElement, Render, StyledText, TextStyle, Window,
};

pub struct WorkspaceSidebar {
    workspaces: Vec<WorkspaceEntry>,
    focused_workspace: Option<usize>,
    focused_tab: Option<usize>,
    focused_pane: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct WorkspaceEntry {
    pub id: String,
    pub label: String,
    pub tabs: Vec<TabEntry>,
    pub active_tab: usize,
}

#[derive(Clone, Debug)]
pub struct TabEntry {
    pub id: String,
    pub label: String,
    pub panes: Vec<PaneEntry>,
    pub active_pane: usize,
}

#[derive(Clone, Debug)]
pub struct PaneEntry {
    pub id: String,
    pub agent: String,
    pub label: String,
    pub status: AgentStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    Error,
}

impl WorkspaceSidebar {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            workspaces: vec![WorkspaceEntry {
                id: "w1".to_string(),
                label: "orion".to_string(),
                tabs: vec![TabEntry {
                    id: "t1".to_string(),
                    label: "orion".to_string(),
                    panes: vec![PaneEntry {
                        id: "p1".to_string(),
                        agent: "claude".to_string(),
                        label: "Claude".to_string(),
                        status: AgentStatus::Idle,
                    }],
                    active_pane: 0,
                }],
                active_tab: 0,
            }],
            focused_workspace: Some(0),
            focused_tab: Some(0),
            focused_pane: Some(0),
        }
    }
}

impl Render for WorkspaceSidebar {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("workspace-sidebar")
            .flex_col()
            .size_full()
            .bg(rgb(0x1a1a2e))
            .child(self.render_workspace_list())
    }
}

impl WorkspaceSidebar {
    fn render_workspace_list(&self) -> impl IntoElement {
        div().flex_col().gap_1().px_2().py_2().children(
            self.workspaces
                .iter()
                .enumerate()
                .map(|(i, ws)| self.render_workspace(i, ws)),
        )
    }

    fn render_workspace(&self, index: usize, ws: &WorkspaceEntry) -> impl IntoElement {
        let is_focused = self.focused_workspace == Some(index);
        let tab = &ws.tabs[ws.active_tab];
        let pane = &tab.panes[tab.active_pane];

        div()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .bg(if is_focused { rgb(0x2d2d44) } else { rgba(0) })
                    .rounded(gpui::px(4.0))
                    .child(self.render_agent_mark(&pane.agent, pane.status))
                    .child(StyledText::new(ws.label.clone()).with_default_highlights(
                        &TextStyle {
                            color: white(),
                            font_size: rems(1.).into(),
                            ..Default::default()
                        },
                        [],
                    )),
            )
            .child(self.render_tab_strip(ws, is_focused))
    }

    fn render_tab_strip(&self, ws: &WorkspaceEntry, ws_focused: bool) -> impl IntoElement {
        div()
            .flex_row()
            .gap_1()
            .ml_2()
            .children(ws.tabs.iter().enumerate().map(|(i, tab)| {
                let is_active = i == ws.active_tab && ws_focused;
                div()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .bg(if is_active { rgb(0x3d3d55) } else { rgba(0) })
                    .rounded(gpui::px(3.0))
                    .child(StyledText::new(tab.label.clone()).with_default_highlights(
                        &TextStyle {
                            color: if is_active {
                                white()
                            } else {
                                rgb(0x8888aa).into()
                            },
                            font_size: rems(0.6875).into(), // 11px
                            ..Default::default()
                        },
                        [],
                    ))
            }))
    }

    fn render_agent_mark(&self, _agent: &str, status: AgentStatus) -> impl IntoElement {
        let color = match status {
            AgentStatus::Idle => rgb(0x10b981),    // emerald
            AgentStatus::Working => rgb(0x06b6d4), // cyan
            AgentStatus::Blocked => rgb(0xf59e0b), // amber
            AgentStatus::Done => rgb(0x10b981),    // emerald
            AgentStatus::Error => rgb(0xef4444),   // ruby
        };

        div().size(px(8.0)).rounded_full().bg(color)
    }
}
