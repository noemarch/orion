//! Main application view with engine integration.

use gpui::prelude::*;
use gpui::{
    div, px, App, Application, Context, Entity, Render, Size, Window, WindowBounds, WindowOptions,
};
use orion_core::{AgentKind, Engine, FrameSink, LaunchSpec, PaneConfig, Role, TerminalSize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

use crate::chat::ChatView;
use crate::diff_view::ChangesView;
use crate::files_view::FileTree;
use crate::settings::{KeyBarExtras, SettingsModal};
use crate::shell_model::{lens_segment_state, HeaderModel, ShellLayout};
use crate::terminal::TerminalPane;
use crate::theme::Theme;
use crate::workspace::WorkspaceSidebar;

/// The lenses over the same pane: chat-first by default, terminal one click away,
/// changes one click away like the terminal.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Lens {
    Chat,
    Terminal,
    Changes,
}

pub struct OrionApp {
    workspace_sidebar: Entity<WorkspaceSidebar>,
    terminal_pane: Entity<TerminalPane>,
    chat_view: Entity<ChatView>,
    file_tree: Entity<FileTree>,
    changes_view: Entity<ChangesView>,
    settings: Entity<SettingsModal>,
    engine: Arc<Engine>,
    lens: Lens,
    editing_tab: Option<(String, String)>,
}

impl OrionApp {
    pub fn new(cx: &mut App) -> Self {
        let workspace_sidebar = cx.new(WorkspaceSidebar::new);
        let terminal_pane = cx.new(TerminalPane::new);
        let chat_view = cx.new(ChatView::new);
        let file_tree = cx.new(FileTree::new);
        let changes_view = cx.new(ChangesView::new);
        let settings = cx.new(SettingsModal::new);
        let engine = Arc::new(Engine::new());

        let mut app = Self {
            workspace_sidebar,
            terminal_pane,
            chat_view,
            file_tree,
            changes_view,
            settings,
            engine,
            lens: Lens::Chat,
            editing_tab: None,
        };

        app.spawn_initial_pane(cx);
        app
    }

    fn spawn_initial_pane(&mut self, cx: &mut App) {
        let terminal_pane = self.terminal_pane.clone();
        let chat_view = self.chat_view.clone();
        let file_tree = self.file_tree.clone();
        let changes_view = self.changes_view.clone();
        let workspace_sidebar = self.workspace_sidebar.clone();
        let settings = self.settings.clone();
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

            let pane_id = pane.id.clone();
            let _ = cx.update(|cx| {
                terminal_pane.update(cx, |view, cx| {
                    view.set_pane(Arc::clone(&pane));
                    cx.notify();
                });
                // The chat lens shares the same pane: conversation fetches go through
                // the wire seam, prompt answers go straight to the in-process pane.
                chat_view.update(cx, |view, _cx| {
                    view.attach(Arc::clone(&engine), Arc::clone(&pane), pane_id.clone());
                });
                // The Changes lens shares the same pane: the file tree starts at
                // the pane's cwd, the diff runs from the pane's repository.
                file_tree.update(cx, |view, cx| {
                    view.attach(cx, Arc::clone(&engine), Arc::clone(&pane), pane_id.clone());
                });
                changes_view.update(cx, |view, _cx| {
                    view.attach(Arc::clone(&engine), Arc::clone(&pane), pane_id.clone());
                });
                workspace_sidebar.update(cx, |view, _cx| {
                    view.attach(Arc::clone(&engine));
                });
                settings.update(cx, |view, _cx| {
                    view.attach(Arc::clone(&engine));
                });
            });

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

    fn set_lens(&mut self, lens: Lens) {
        self.lens = lens;
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current();
        let sidebar = self.workspace_sidebar.clone();
        let header = sidebar.update(cx, |view, _cx| {
            let workspaces = view.workspaces();
            let focused = workspaces
                .iter()
                .find(|w| w.focused)
                .or_else(|| workspaces.first());
            if let Some(ws) = focused {
                HeaderModel {
                    title: "Orbit".to_string(),
                    workspace: ws.label.clone(),
                    connection: "Live".to_string(),
                }
            } else {
                HeaderModel::empty()
            }
        });
        div()
            .id("app-header")
            .flex()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.0))
            .h(px(48.0))
            .px_3()
            .bg(theme.surface_panel)
            .child(
                div()
                    .text_size(px(14.0))
                    .text_color(theme.text_strong)
                    .child(header.title),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.text_dim)
                    .child(header.workspace),
            )
            .child(
                div().flex_1().flex().flex_row().justify_end().child(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme.text_dim)
                        .child(header.connection),
                ),
            )
            .child(
                div()
                    .id("header-settings")
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(theme.surface_elevated)
                    .text_color(theme.text)
                    .child("Settings")
                    .on_click(cx.listener(|this, _, _, cx| {
                        let settings = this.settings.clone();
                        settings.update(cx, |view, cx| {
                            view.set_open(true);
                            cx.notify();
                        });
                        cx.notify();
                    })),
            )
    }

    fn render_lens_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current();
        let (chat_active, terminal_active, changes_active) = lens_segment_state(self.lens);
        div()
            .id("lens-toggle")
            .flex()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .h(px(40.0))
            .px_3()
            .max_w(px(420.0))
            .bg(theme.surface_panel)
            .child(
                div()
                    .id("lens-chat")
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(if chat_active {
                        theme.accent
                    } else {
                        theme.surface_elevated
                    })
                    .text_color(theme.text_strong)
                    .child("Chat")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_lens(Lens::Chat);
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("lens-terminal")
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(if terminal_active {
                        theme.accent
                    } else {
                        theme.surface_elevated
                    })
                    .text_color(theme.text_strong)
                    .child("Terminal")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_lens(Lens::Terminal);
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("lens-changes")
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(if changes_active {
                        theme.accent
                    } else {
                        theme.surface_elevated
                    })
                    .text_color(theme.text_strong)
                    .child("Changes")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_lens(Lens::Changes);
                        cx.notify();
                    })),
            )
            .child(
                div().flex_1().flex().flex_row().justify_end().child(
                    div()
                        .id("settings-button")
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .bg(theme.surface_elevated)
                        .text_color(theme.text)
                        .child("Settings")
                        .on_click(cx.listener(|this, _, _, cx| {
                            let settings = this.settings.clone();
                            settings.update(cx, |view, cx| {
                                view.set_open(true);
                                cx.notify();
                            });
                            cx.notify();
                        })),
                ),
            )
    }

    /// The focused workspace's tabs, above the lens content (TabStrip
    /// behavior): clicking a tab focuses its first pane.
    fn render_tab_strip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current();
        let sidebar = self.workspace_sidebar.clone();
        let (tabs, ws_id) = sidebar.update(cx, |view, _cx| {
            let workspaces = view.workspaces();
            let focused = workspaces
                .iter()
                .find(|w| w.focused)
                .or_else(|| workspaces.first());
            let tabs = focused.map(|ws| view.tabs(&ws.id)).unwrap_or_default();
            let ws_id = focused.map(|ws| ws.id.clone()).unwrap_or_default();
            (tabs, ws_id)
        });
        let scale = theme.density_scale();
        let engine = self.engine.clone();
        let ws_id_for_new = ws_id.clone();
        div()
            .id("tab-strip")
            .flex()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_3()
            .py_1()
            .bg(theme.surface_panel)
            .children(tabs.iter().map(|tab| {
                let is_active = tab.focused;
                let pane_id = tab.pane_ids.first().cloned();
                let pane_id_for_split = pane_id.clone();
                let engine_split = engine.clone();
                let tab_id = tab.id.clone();
                let tab_id_for_edit = tab_id.clone();
                let tab_label_for_edit = tab.label.clone();
                let tab_id_for_key = tab_id.clone();
                let engine_close = engine.clone();
                let engine_rename = engine.clone();
                let editing = self
                    .editing_tab
                    .as_ref()
                    .is_some_and(|(id, _)| id == &tab_id);
                let edit_text = if editing {
                    self.editing_tab
                        .as_ref()
                        .map(|(_, text)| text.clone())
                        .unwrap_or_default()
                } else {
                    tab.label.clone()
                };
                let row = div()
                    .id(self.next_row_id())
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(if is_active {
                        theme.accent_tint
                    } else {
                        Theme::transparent()
                    })
                    .text_color(if is_active {
                        theme.text_strong
                    } else {
                        theme.text_dim
                    })
                    .text_size(px(12.0 * scale))
                    .child(edit_text)
                    .on_key_down(cx.listener(move |this, ev: &gpui::KeyDownEvent, _, cx| {
                        let Some((editing_id, text)) = this.editing_tab.as_mut() else {
                            return;
                        };
                        if editing_id != &tab_id_for_key {
                            return;
                        }
                        if ev.keystroke.key == "escape" {
                            this.editing_tab = None;
                            cx.notify();
                            return;
                        }
                        if ev.keystroke.key == "enter" {
                            let label = text.clone();
                            let _ = engine_rename.http(
                                "POST",
                                "/api/tab/rename",
                                &json!({ "tab_id": tab_id_for_key, "label": label }),
                            );
                            this.editing_tab = None;
                            cx.notify();
                            return;
                        }
                        if ev.keystroke.key == "backspace" {
                            text.pop();
                            cx.notify();
                            return;
                        }
                        if let Some(ch) = ev.keystroke.key_char.clone() {
                            text.push_str(&ch);
                            cx.notify();
                        }
                    }))
                    .child(
                        div()
                            .id(self.next_row_id())
                            .px_1()
                            .text_size(px(12.0 * scale))
                            .text_color(theme.text_dim)
                            .child("✎")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.editing_tab =
                                    Some((tab_id_for_edit.clone(), tab_label_for_edit.clone()));
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .id(self.next_row_id())
                            .px_1()
                            .text_size(px(12.0 * scale))
                            .text_color(theme.text_dim)
                            .child("◫")
                            .on_click(cx.listener(move |_this, _, _, cx| {
                                if let Some(pane_id) = pane_id_for_split.clone() {
                                    let _ = engine_split.http(
                                        "POST",
                                        "/api/pane/split",
                                        &json!({
                                            "pane_id": pane_id,
                                            "direction": "right",
                                        }),
                                    );
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .id(self.next_row_id())
                            .px_1()
                            .text_size(px(12.0 * scale))
                            .text_color(theme.text_dim)
                            .child("×")
                            .on_click(cx.listener(move |_this, _, _, cx| {
                                let _ = engine_close.http(
                                    "POST",
                                    "/api/tab/close",
                                    &json!({ "tab_id": tab_id }),
                                );
                                cx.notify();
                            })),
                    );
                if let Some(pane_id) = pane_id {
                    row.on_click(cx.listener(move |this, _, _, cx| {
                        let sidebar = this.workspace_sidebar.clone();
                        sidebar.update(cx, |view, _cx| view.focus_pane(&pane_id));
                        cx.notify();
                    }))
                } else {
                    row
                }
            }))
            .child(
                div()
                    .id(self.next_row_id())
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(theme.surface_elevated)
                    .text_color(theme.text)
                    .text_size(px(12.0 * scale))
                    .child("+")
                    .on_click(cx.listener(move |_this, _, _, cx| {
                        let _ = engine.http(
                            "POST",
                            "/api/tab/create",
                            &json!({
                                "workspace_id": ws_id_for_new,
                                "agent": { "kind": "claude" },
                                "label": "Claude",
                            }),
                        );
                        cx.notify();
                    })),
            )
    }

    fn next_row_id(&self) -> usize {
        // GPUI element ids must be unique per render; the strip is small and
        // static per frame, so a fixed base plus tab order is enough. The
        // sidebar uses its own counter; ids only need to be unique within
        // their parent element tree.
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        SEQ.fetch_add(1, Ordering::Relaxed)
    }
}

impl Render for OrionApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let layout = ShellLayout::for_width(window.bounds().size.width.into());
        let sidebar_w = layout.sidebar_width_px;
        // The Changes lens' 2s polls run only while it is active; views are
        // preserved between lens switches (entities stay mounted above).
        let changes_active = self.lens == Lens::Changes;
        self.file_tree
            .update(cx, |view, cx| view.set_visible(cx, changes_active));
        self.changes_view
            .update(cx, |view, _cx| view.set_visible(changes_active));
        let prefs = self.settings.update(cx, |view, _cx| view.prefs().clone());
        Theme::set_current(Theme::for_mode(prefs.theme, prefs.density));
        let extras = KeyBarExtras::from_preferences(&prefs);
        self.terminal_pane
            .update(cx, |view, _cx| view.set_key_bar_extras(extras));
        let has_workspaces = self
            .workspace_sidebar
            .update(cx, |view, _cx| !view.workspaces().is_empty());
        let lens_body = if !has_workspaces {
            div()
                .id("main-content")
                .flex()
                .flex_col()
                .flex_1()
                .items_center()
                .justify_center()
                .gap(px(8.0))
                .child(
                    div()
                        .text_size(px(14.0))
                        .text_color(Theme::current().text_strong)
                        .child("No workspace open"),
                )
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(Theme::current().text_dim)
                        .child("Open a workspace to begin."),
                )
        } else {
            match self.lens {
                Lens::Chat => {
                    if let Some(card) = self
                        .chat_view
                        .update(cx, |view, cx| view.prompt_card_entity(cx))
                    {
                        div()
                            .id("main-content")
                            .flex()
                            .flex_col()
                            .flex_1()
                            .child(self.chat_view.clone())
                            .child(card)
                    } else {
                        div()
                            .id("main-content")
                            .flex()
                            .flex_col()
                            .flex_1()
                            .child(self.chat_view.clone())
                    }
                }
                Lens::Terminal => div()
                    .id("main-content")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .child(self.terminal_pane.clone()),
                Lens::Changes => div()
                    .id("main-content")
                    .flex()
                    .flex_row()
                    .flex_1()
                    .child(
                        div()
                            .id("changes-left")
                            .flex()
                            .flex_col()
                            .flex_1()
                            .child(self.file_tree.clone()),
                    )
                    .child(
                        div()
                            .id("changes-right")
                            .flex()
                            .flex_col()
                            .flex_1()
                            .child(self.changes_view.clone()),
                    ),
            }
        };
        div()
            .id("app")
            .flex()
            .flex()
            .flex_row()
            .size_full()
            .child(
                div()
                    .id("sidebar")
                    .flex()
                    .flex()
                    .flex_col()
                    .w(px(sidebar_w))
                    .child(self.workspace_sidebar.clone()),
            )
            .child(
                div()
                    .id("main-column")
                    .flex()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.0))
                    .child(self.render_header(cx))
                    .child(self.render_lens_toggle(cx))
                    .child(self.render_tab_strip(cx))
                    .child(lens_body),
            )
            .child(if self.settings.update(cx, |view, _cx| view.is_open()) {
                div()
                    .id("modal-host")
                    .absolute()
                    .inset_0()
                    .child(self.settings.clone())
            } else {
                div().id("no-modal")
            })
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
