//! Main application view with engine integration.

use crate::terminal::TerminalPane;
use crate::workspace::WorkspaceSidebar;
use gpui::prelude::*;
use gpui::{
    div, px, rems, rgb, rgba, white, App, Application, Context, Entity, Render, Size, Window,
    WindowBounds, WindowOptions,
};
use orion_core::{AgentKind, Engine, FrameSink, LaunchSpec, PaneConfig, Role, TerminalSize};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;

pub struct OrionApp {
    workspace_sidebar: Entity<WorkspaceSidebar>,
    terminal_pane: Entity<TerminalPane>,
    engine: Arc<Engine>,
}

impl OrionApp {
    pub fn new(cx: &mut App) -> Self {
        let workspace_sidebar = cx.new(|cx| WorkspaceSidebar::new(cx));
        let terminal_pane = cx.new(|cx| TerminalPane::new(cx));
        let engine = Arc::new(Engine::new());

        let mut app = Self {
            workspace_sidebar,
            terminal_pane,
            engine,
        };

        // Spawn a pane and connect its output to the terminal
        app.spawn_initial_pane(cx);
        app
    }

    fn spawn_initial_pane(&mut self, cx: &mut App) {
        let terminal_pane = self.terminal_pane.clone();
        let engine = Arc::clone(&self.engine);

        cx.spawn(async move |cx| {
            // Create a pane config
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

            // Spawn the pane through the engine
            let pane = engine.spawn_pane(config).expect("pane spawn");

            // Attach to the pane with a custom sink that forwards to the terminal pane
            let sender = terminal_pane
                .read_with(cx, |view, _| view.sender())
                .unwrap();
            let sink: FrameSink = Arc::new(move |frame: Value| {
                if frame["type"] == "pty-data" {
                    if let Some(data) = frame["data"].as_str() {
                        let _ = sender.send(data.as_bytes().to_vec());
                    }
                }
            });

            let _ = pane
                .attach("gpu-terminal", sink, Role::Interact, 120, 32, false)
                .expect("attach");
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
