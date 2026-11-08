//! Main application view with engine integration.

use gpui::prelude::*;
use gpui::{
    div, px, App, Application, Context, Entity, Render, Size, Window, WindowBounds, WindowOptions,
};
use orion_core::{AgentKind, Engine, FrameSink, LaunchSpec, PaneConfig, Role, TerminalSize};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;

use crate::terminal::TerminalPane;
use crate::workspace::WorkspaceSidebar;

pub struct OrionApp {
    workspace_sidebar: Entity<WorkspaceSidebar>,
    terminal_pane: Entity<TerminalPane>,
    engine: Arc<Engine>,
}

impl OrionApp {
    pub fn new(cx: &mut App) -> Self {
        let workspace_sidebar = cx.new(WorkspaceSidebar::new);
        let terminal_pane = cx.new(TerminalPane::new);
        let engine = Arc::new(Engine::new());

        let mut app = Self {
            workspace_sidebar,
            terminal_pane,
            engine,
        };

        app.spawn_initial_pane(cx);
        app
    }

    fn spawn_initial_pane(&mut self, cx: &mut App) {
        let terminal_pane = self.terminal_pane.clone();
        let engine = Arc::clone(&self.engine);

        cx.spawn(async move |cx| {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let spec = LaunchSpec::for_agent(
                AgentKind::Claude,
                &[
                    "--permission-mode".to_string(),
                    "bypassPermissions".to_string(),
                ],
                None,
                false,
            )
            .expect("valid launch spec");

            let config = PaneConfig {
                id: "pane-1".to_string(),
                workspace_id: "ws-1".to_string(),
                tab_id: "tab-1".to_string(),
                agent: AgentKind::Claude,
                label: "Claude".to_string(),
                cwd: cwd.clone(),
                spec,
                args: vec![
                    "--permission-mode".to_string(),
                    "bypassPermissions".to_string(),
                ],
                size: TerminalSize {
                    rows: 32,
                    cols: 120,
                    pixel_width: 0,
                    pixel_height: 0,
                },
            };

            let pane = engine.spawn_pane(config).expect("pane spawn");

            // The attach sink runs on the PTY reader thread, so it can only push into a
            // channel; the async task below drains that channel, feeds the grid, and
            // notifies the view so GPUI repaints.
            let (pty_tx, pty_rx) = flume::unbounded::<String>();
            let sink: FrameSink = Arc::new(move |frame: Value| {
                if frame["type"] == "pty-data" {
                    if let Some(data) = frame["data"].as_str() {
                        let _ = pty_tx.send(data.to_string());
                    }
                }
            });

            pane.attach("gpu-terminal", sink, Role::Interact, 120, 32, false)
                .expect("attach");

            while let Ok(first) = pty_rx.recv_async().await {
                // If the app is gone, there is nothing to repaint.
                let _ = cx.update(|cx| {
                    terminal_pane.update(cx, |pane, cx| {
                        pane.feed(&first);
                        while let Ok(more) = pty_rx.try_recv() {
                            pane.feed(&more);
                        }
                        cx.notify();
                    });
                });
            }
        })
        .detach();
    }
}

impl Render for OrionApp {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("app")
            .flex_row()
            .size_full()
            .child(
                div()
                    .id("sidebar")
                    .flex_col()
                    .w(px(280.0))
                    .child(self.workspace_sidebar.clone()),
            )
            .child(
                div()
                    .id("main-content")
                    .flex_col()
                    .flex_1()
                    .child(self.terminal_pane.clone()),
            )
    }
}

pub fn run() {
    let app = Application::new();
    app.run(|cx| {
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::centered(Size::new(px(1280.0), px(800.0)), cx)),
                ..Default::default()
            },
            |_window, cx| cx.new(|cx| OrionApp::new(cx)),
        )
        .unwrap();
    });
}
