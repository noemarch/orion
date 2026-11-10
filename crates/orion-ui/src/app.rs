//! Main application view with engine integration.

use gpui::prelude::*;
use gpui::{
    div, px, App, Application, Context, Entity, Render, Size, Subscription, Window, WindowBounds,
    WindowOptions,
};
use orion_core::{Engine, FrameSink, Role};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::chat::{ChatEvent, ChatView};
use crate::diff_view::ChangesView;
use crate::files_view::FileTree;
use crate::new_workspace::{recent_directory, NewWorkspaceEvent, NewWorkspaceModal};
use crate::settings::{KeyBarExtras, SettingsModal};
use crate::shell_model::{lens_segment_state, HeaderModel, LensBook, ShellLayout};
use crate::sidebar::SidebarEvent;
use crate::terminal::TerminalPane;
use crate::theme::Theme;
use crate::workspace::WorkspaceSidebar;

/// Which chat asked the shell for something: the focused pane's chat, or the
/// split canvas's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ChatPane {
    Focused,
    Split,
}

/// The lenses over the same pane: chat-first by default, terminal one click away,
/// changes one click away like the terminal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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
    new_workspace: Entity<NewWorkspaceModal>,
    engine: Arc<Engine>,
    lens_book: LensBook,
    /// The pane whose lens is active; the switcher writes here.
    focused_pane: Option<String>,
    /// The pane currently attached to the main lens views.
    attached_pane: Option<String>,
    /// Increments per attach so an old PTY pump stops feeding a replaced pane.
    attach_generation: Arc<AtomicU64>,
    editing_tab: Option<(String, String)>,
    split_chat_view: Entity<ChatView>,
    split_terminal_pane: Entity<TerminalPane>,
    split_attached_pane: Option<String>,
    split_file_tree: Entity<FileTree>,
    split_changes_view: Entity<ChangesView>,
    subscriptions: Vec<Subscription>,
    /// Keyboard focus for the shell chrome (tab rename).
    focus_handle: gpui::FocusHandle,
    /// The last lifecycle command that failed, shown as one plain sentence.
    notice: Option<String>,
}

impl OrionApp {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let workspace_sidebar = cx.new(WorkspaceSidebar::new);
        let terminal_pane = cx.new(TerminalPane::new);
        let chat_view = cx.new(ChatView::new);
        let file_tree = cx.new(FileTree::new);
        let changes_view = cx.new(ChangesView::new);
        let settings = cx.new(SettingsModal::new);
        let new_workspace = cx.new(NewWorkspaceModal::new);
        let split_chat_view = cx.new(ChatView::new);
        let split_terminal_pane = cx.new(TerminalPane::new);
        let split_file_tree = cx.new(FileTree::new);
        let split_changes_view = cx.new(ChangesView::new);
        let engine = Arc::new(Engine::new());

        let mut app = Self {
            workspace_sidebar,
            terminal_pane,
            chat_view,
            file_tree,
            changes_view,
            settings,
            new_workspace,
            engine,
            lens_book: LensBook::new(),
            focused_pane: None,
            attached_pane: None,
            attach_generation: Arc::new(AtomicU64::new(0)),
            editing_tab: None,
            split_chat_view,
            split_terminal_pane,
            split_attached_pane: None,
            split_file_tree,
            split_changes_view,
            subscriptions: Vec::new(),
            focus_handle: cx.focus_handle(),
            notice: None,
        };

        // Lifecycle clicks are events, so the shell re-renders at once instead
        // of waiting for the next sidebar poll.
        let sidebar_events = app.workspace_sidebar.clone();
        let modal_events = app.new_workspace.clone();
        let sidebar_sub = cx.subscribe(&sidebar_events, |this, _, event: &SidebarEvent, cx| {
            match event {
                SidebarEvent::NewWorkspace => this.open_new_workspace(cx),
                SidebarEvent::CloseWorkspace(workspace_id) => {
                    this.call_route(
                        "/api/workspace/close",
                        &json!({ "workspace_id": workspace_id }),
                    );
                    this.workspace_sidebar
                        .update(cx, |view, _cx| view.refresh());
                }
                SidebarEvent::PaneFocused(pane_id) => {
                    this.focused_pane = Some(pane_id.clone());
                    this.attach_pane(cx, pane_id);
                }
            }
            cx.notify();
        });
        let modal_sub = cx.subscribe(&modal_events, |this, _, event: &NewWorkspaceEvent, cx| {
            let NewWorkspaceEvent::Created(pane_id) = event;
            this.workspace_sidebar
                .update(cx, |view, _cx| view.refresh());
            this.focused_pane = Some(pane_id.clone());
            this.attach_pane(cx, pane_id);
            cx.notify();
        });
        // The chat asks the shell for the Terminal Lens or for a file: the
        // lens switch and the file viewer live in the app root.
        let chat_events = app.chat_view.clone();
        let split_chat_events = app.split_chat_view.clone();
        let chat_sub = cx.subscribe(&chat_events, |this, _, event: &ChatEvent, cx| {
            this.handle_chat_event(event, ChatPane::Focused, cx);
            cx.notify();
        });
        let split_chat_sub = cx.subscribe(&split_chat_events, |this, _, event: &ChatEvent, cx| {
            this.handle_chat_event(event, ChatPane::Split, cx);
            cx.notify();
        });
        app.subscriptions = vec![sidebar_sub, modal_sub, chat_sub, split_chat_sub];

        app.start_session(cx);
        app
    }

    /// Attach the singletons, restore the saved session, and otherwise open a
    /// default workspace through the Route Table.
    fn start_session(&mut self, cx: &mut App) {
        let engine = Arc::clone(&self.engine);
        self.workspace_sidebar
            .update(cx, |view, _cx| view.attach(Arc::clone(&engine)));
        self.settings
            .update(cx, |view, _cx| view.attach(Arc::clone(&engine)));
        self.new_workspace
            .update(cx, |view, _cx| view.attach(Arc::clone(&engine)));

        // Put the user back where they were before starting a fresh agent.
        self.engine.restore();
        self.workspace_sidebar
            .update(cx, |view, _cx| view.refresh());

        let snapshot = self.engine.snapshot_value();
        let first_pane = snapshot
            .get("panes")
            .and_then(Value::as_array)
            .and_then(|panes| panes.first())
            .and_then(|pane| pane.get("pane_id"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let focused = snapshot
            .get("focused_pane_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or(first_pane);
        if let Some(pane_id) = focused {
            self.attach_pane(cx, &pane_id);
        } else {
            self.open_default_workspace(cx);
        }
    }

    /// First run: open the current folder with Claude through
    /// `POST /api/workspace/create`. If the agent is missing the shell stays in
    /// its empty state and the New Workspace flow is the way in.
    fn open_default_workspace(&mut self, cx: &mut App) {
        let cwd = std::env::current_dir().unwrap_or_default();
        let body = json!({ "cwd": cwd.to_string_lossy(), "agent": { "kind": "claude" } });
        let response = self.call_route("/api/workspace/create", &body);
        self.workspace_sidebar
            .update(cx, |view, _cx| view.refresh());
        let Some(response) = response else {
            return;
        };
        if let Some(pane_id) = response
            .get("pane_id")
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            self.attach_pane(cx, &pane_id);
        }
    }

    /// Point every lens at one pane and start its PTY pump. Re-attaching is
    /// cheap and keeps workspace/tab switches showing the active pane.
    fn attach_pane(&mut self, cx: &mut App, pane_id: &str) {
        if self.attached_pane.as_deref() == Some(pane_id) {
            return;
        }
        let Some(pane) = self.engine.pane_by_id(pane_id) else {
            return;
        };
        // Stop streaming the pane we are leaving; its pump also stops on the
        // generation check below.
        if let Some(previous) = self.attached_pane.take() {
            if let Some(previous_pane) = self.engine.pane_by_id(&previous) {
                previous_pane.detach("gpu-terminal");
            }
        }
        let engine = Arc::clone(&self.engine);

        // The attach sink runs on the PTY reader thread, so it can only push
        // into a channel; the async task below drains that channel, feeds the
        // grid, and notifies the view so GPUI repaints.
        let (pty_tx, pty_rx) = flume::unbounded::<String>();
        let sink: FrameSink = Arc::new(move |frame: Value| {
            if frame["type"] == "pty-data" {
                if let Some(data) = frame["data"].as_str() {
                    let _ = pty_tx.send(data.to_string());
                }
            }
        });
        if pane
            .attach("gpu-terminal", sink, Role::Interact, 120, 32, false)
            .is_err()
        {
            return;
        }
        self.attached_pane = Some(pane_id.to_string());

        self.terminal_pane.update(cx, |view, cx| {
            view.set_pane(Arc::clone(&pane));
            cx.notify();
        });
        // The chat lens shares the same pane: conversation fetches go through
        // the wire seam, prompt answers go straight to the in-process pane.
        self.chat_view.update(cx, |view, _cx| {
            view.attach(Arc::clone(&engine), Arc::clone(&pane), pane_id.to_string());
        });
        // The Changes lens shares the same pane: the file tree starts at
        // the pane's cwd, the diff runs from the pane's repository.
        self.file_tree.update(cx, |view, cx| {
            view.attach(
                cx,
                Arc::clone(&engine),
                Arc::clone(&pane),
                pane_id.to_string(),
            );
        });
        self.changes_view.update(cx, |view, _cx| {
            view.attach(Arc::clone(&engine), Arc::clone(&pane), pane_id.to_string());
        });

        let generation = Arc::clone(&self.attach_generation);
        let mine = generation.fetch_add(1, Ordering::SeqCst) + 1;
        let terminal_pane = self.terminal_pane.clone();
        cx.spawn(async move |cx| {
            while let Ok(first) = pty_rx.recv_async().await {
                // A newer attach owns the view now; stop feeding it.
                if generation.load(Ordering::SeqCst) != mine {
                    break;
                }
                let generation = Arc::clone(&generation);
                let _ = cx.update(|cx| {
                    if generation.load(Ordering::SeqCst) != mine {
                        return;
                    }
                    terminal_pane.update(cx, |view, cx| {
                        view.feed(&first);
                        while let Ok(more) = pty_rx.try_recv() {
                            view.feed(&more);
                        }
                        cx.notify();
                    });
                });
            }
        })
        .detach();
    }

    fn set_lens(&mut self, lens: Lens) {
        self.lens_book.set(self.focused_pane.as_deref(), lens);
    }

    /// What the chat asks for: the Terminal Lens has the whole output, and
    /// the Changes Lens opens the file. The split canvas answers for its own
    /// pane, not for the focused one.
    fn handle_chat_event(&mut self, event: &ChatEvent, pane: ChatPane, cx: &mut App) {
        match event {
            ChatEvent::ShowTerminal => match pane {
                ChatPane::Focused => self.set_lens(Lens::Terminal),
                ChatPane::Split => {
                    if let Some(split_pane) = self.split_attached_pane.clone() {
                        self.lens_book.set(Some(&split_pane), Lens::Terminal);
                    }
                }
            },
            ChatEvent::OpenFile(path) => {
                match pane {
                    ChatPane::Focused => self.set_lens(Lens::Changes),
                    ChatPane::Split => {
                        if let Some(split_pane) = self.split_attached_pane.clone() {
                            self.lens_book.set(Some(&split_pane), Lens::Changes);
                        }
                    }
                }
                let tree = match pane {
                    ChatPane::Focused => self.file_tree.clone(),
                    ChatPane::Split => self.split_file_tree.clone(),
                };
                let reader = tree.read(cx).file_reader().clone();
                reader.update(cx, |reader, _cx| reader.open(path.clone()));
            }
        }
    }

    /// Runs one lifecycle command through the Route Table. A failure becomes
    /// the shell's one-line notice instead of a silent no-op.
    fn call_route(&mut self, path: &str, body: &Value) -> Option<Value> {
        let (status, answer) = self.engine.http("POST", path, body);
        if status == 200 {
            self.notice = None;
            return Some(answer);
        }
        self.notice = Some(crate::new_workspace::error_message(
            &answer,
            "That action could not be completed.",
        ));
        None
    }

    /// Typing while a tab label is being edited renames it through
    /// `POST /api/tab/rename`; Escape cancels.
    fn handle_editing_key(&mut self, ev: &gpui::KeyDownEvent, cx: &mut Context<Self>) {
        let Some((tab_id, text)) = self.editing_tab.clone() else {
            return;
        };
        let mut draft = text;
        let modifiers = &ev.keystroke.modifiers;
        let command_modifier = modifiers.control || modifiers.alt || modifiers.platform;
        match crate::shell_model::apply_text_key(
            &mut draft,
            &ev.keystroke.key,
            ev.keystroke.key_char.as_deref(),
            command_modifier,
        ) {
            crate::shell_model::TextEdit::Cancel => {
                self.editing_tab = None;
            }
            crate::shell_model::TextEdit::Commit => {
                let label = draft.trim().to_string();
                if !label.is_empty() {
                    self.call_route(
                        "/api/tab/rename",
                        &json!({ "tab_id": tab_id, "label": label }),
                    );
                    self.workspace_sidebar
                        .update(cx, |view, _cx| view.refresh());
                }
                self.editing_tab = None;
            }
            crate::shell_model::TextEdit::Changed => {
                self.editing_tab = Some((tab_id, draft));
            }
            crate::shell_model::TextEdit::Ignored => return,
        }
        cx.notify();
    }

    fn current_lens(&self) -> Lens {
        self.lens_book.lens_for(self.focused_pane.as_deref())
    }

    /// Seed the New Workspace picker from the most recent folder.
    fn open_new_workspace(&mut self, cx: &mut App) {
        let (focused_cwd, any_cwd) = self.workspace_sidebar.update(cx, |view, _cx| {
            let workspaces = view.workspaces();
            let focused = workspaces
                .iter()
                .find(|workspace| workspace.focused)
                .or_else(|| workspaces.first());
            (
                focused.and_then(|workspace| workspace.cwd.clone()),
                workspaces
                    .iter()
                    .find_map(|workspace| workspace.cwd.clone()),
            )
        });
        let (status, body) = self.engine.http("GET", "/api/preferences", &Value::Null);
        let explicit = if status == 200 {
            body.get("settings")
                .and_then(|settings| settings.get("recentDirectory"))
                .and_then(Value::as_str)
                .map(str::to_string)
        } else {
            None
        };
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_default();
        let start = recent_directory(
            explicit.as_deref(),
            focused_cwd.as_deref(),
            any_cwd.as_deref(),
            &home,
        );
        let start = if start.is_empty() { None } else { Some(start) };
        self.new_workspace.update(cx, |view, cx| {
            view.open_at(start);
            cx.notify();
        });
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
                    title: "Orion".to_string(),
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
        let (chat_active, terminal_active, changes_active) =
            lens_segment_state(self.current_lens());
        let focused_pane_id = self.workspace_sidebar.update(cx, |view, _cx| {
            view.snapshot().and_then(|s| {
                s.get("focused_pane_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
        });
        let focused_pane_id_zoom = focused_pane_id.clone();
        div()
            .id("lens-toggle")
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
                div()
                    .id("pane-zoom")
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(theme.surface_elevated)
                    .text_color(theme.text)
                    .child("Zoom")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(pane_id) = focused_pane_id_zoom.clone() {
                            this.call_route(
                                "/api/pane/zoom",
                                &json!({ "pane_id": pane_id, "mode": "toggle" }),
                            );
                        }
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("pane-close")
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(theme.surface_elevated)
                    .text_color(theme.text)
                    .child("Close")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(pane_id) = focused_pane_id.clone() {
                            this.call_route("/api/pane/close", &json!({ "pane_id": pane_id }));
                        }
                        this.workspace_sidebar
                            .update(cx, |view, _cx| view.refresh());
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
        let (tabs, ws_id, ws_agent) = sidebar.update(cx, |view, _cx| {
            let workspaces = view.workspaces();
            let focused = workspaces
                .iter()
                .find(|w| w.focused)
                .or_else(|| workspaces.first());
            focused
                .map(|ws| (view.tabs(&ws.id), ws.id.clone(), ws.agent.clone()))
                .unwrap_or_default()
        });
        let scale = theme.density_scale();
        let ws_id_for_new = ws_id.clone();
        let ws_agent_for_new = ws_agent.clone();
        let next_tab_number = tabs.iter().map(|tab| tab.number).max().unwrap_or(0) + 1;
        let new_tab_label = format!("Tab {next_tab_number}");
        div()
            .id("tab-strip")
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_3()
            .py_1()
            .bg(theme.surface_panel)
            .children(tabs.iter().map(|tab| {
                let is_active = tab.focused;
                let pane_id = tab
                    .active_pane
                    .clone()
                    .or_else(|| tab.pane_ids.first().cloned());
                let pane_id_for_split = pane_id.clone();
                let tab_id = tab.id.clone();
                let tab_id_for_edit = tab_id.clone();
                let tab_label_for_edit = tab.label.clone();
                let editing = self
                    .editing_tab
                    .as_ref()
                    .is_some_and(|(id, _)| id == &tab_id);
                let edit_text = if editing {
                    let draft = self
                        .editing_tab
                        .as_ref()
                        .map(|(_, text)| text.clone())
                        .unwrap_or_default();
                    // A visible caret marks the inline rename mode.
                    format!("{draft}▏")
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
                    .child(
                        div()
                            .id(self.next_row_id())
                            .px_1()
                            .text_size(px(12.0 * scale))
                            .text_color(theme.text_dim)
                            .child("✎")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.editing_tab =
                                    Some((tab_id_for_edit.clone(), tab_label_for_edit.clone()));
                                this.focus_handle.focus(window);
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
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                if let Some(pane_id) = pane_id_for_split.clone() {
                                    this.call_route(
                                        "/api/pane/split",
                                        &json!({
                                            "pane_id": pane_id,
                                            "direction": "right",
                                        }),
                                    );
                                    this.workspace_sidebar
                                        .update(cx, |view, _cx| view.refresh());
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
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.call_route("/api/tab/close", &json!({ "tab_id": tab_id }));
                                this.workspace_sidebar
                                    .update(cx, |view, _cx| view.refresh());
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
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.call_route(
                            "/api/tab/create",
                            &json!({
                                "workspace_id": ws_id_for_new,
                                "agent": { "kind": ws_agent_for_new },
                                "label": new_tab_label,
                            }),
                        );
                        this.workspace_sidebar
                            .update(cx, |view, _cx| view.refresh());
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
        self.workspace_sidebar
            .update(cx, |view, _cx| view.set_rail(layout.rail));

        // Follow the pane the session now focuses (workspace/tab switch,
        // create, close). Lifecycle clicks arrive as events above.
        let (focused_pane, live_panes) = self.workspace_sidebar.update(cx, |view, _cx| {
            let snapshot = view.snapshot();
            let focused = snapshot.as_ref().and_then(|snapshot| {
                snapshot
                    .get("focused_pane_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            });
            let live = snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.get("panes"))
                .and_then(Value::as_array)
                .map(|panes| {
                    panes
                        .iter()
                        .filter_map(|pane| {
                            pane.get("pane_id")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            (focused, live)
        });
        // Panes that left the Session do not keep a lens choice alive.
        self.lens_book.retain_panes(&live_panes);
        self.focused_pane = focused_pane;
        if let Some(pane_id) = self.focused_pane.clone() {
            self.attach_pane(cx, &pane_id);
        }

        // The Changes lens' 2s polls run only while it is active; views are
        // preserved between lens switches (entities stay mounted above).
        let changes_active = self.current_lens() == Lens::Changes;
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
        let split_panes = self.workspace_sidebar.update(cx, |view, _cx| {
            let workspaces = view.workspaces();
            let focused = workspaces
                .iter()
                .find(|w| w.focused)
                .or_else(|| workspaces.first());
            let Some(ws) = focused else {
                return Vec::new();
            };
            let tabs = view.tabs(&ws.id);
            let focused_tab = tabs.iter().find(|t| t.focused).or_else(|| tabs.first());
            match focused_tab {
                Some(tab) if tab.layout != "single" && tab.pane_ids.len() >= 2 => {
                    tab.pane_ids.clone()
                }
                _ => Vec::new(),
            }
        });
        // The split canvas always shows the pane the main canvas is not:
        // otherwise focusing the second pane paints it twice.
        let split_second = if split_panes.len() >= 2 {
            split_panes
                .iter()
                .find(|pane_id| Some(pane_id.as_str()) != self.focused_pane.as_deref())
                .cloned()
                .or_else(|| split_panes.get(1).cloned())
        } else {
            None
        };
        if split_second.is_none() {
            self.split_attached_pane = None;
        }
        let split_changes_active = split_second
            .as_deref()
            .is_some_and(|pane_id| self.lens_book.lens_for(Some(pane_id)) == Lens::Changes);
        self.split_file_tree
            .update(cx, |view, cx| view.set_visible(cx, split_changes_active));
        self.split_changes_view
            .update(cx, |view, _cx| view.set_visible(split_changes_active));
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
                .child(
                    div()
                        .id("empty-new-workspace")
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .bg(Theme::current().primary)
                        .text_color(Theme::current().primary_text)
                        .child("New Workspace")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.open_new_workspace(cx);
                            cx.notify();
                        })),
                )
        } else {
            let inner = match self.current_lens() {
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
            };
            if let Some(second_id) = split_second.clone() {
                if self.split_attached_pane.as_deref() != Some(second_id.as_str()) {
                    self.split_attached_pane = Some(second_id.clone());
                    if let Some(second_pane) = self.engine.pane_by_id(&second_id) {
                        self.split_chat_view.update(cx, |view, _cx| {
                            view.attach(
                                Arc::clone(&self.engine),
                                Arc::clone(&second_pane),
                                second_id.clone(),
                            );
                        });
                        self.split_terminal_pane
                            .update(cx, |view, _cx| view.set_pane(Arc::clone(&second_pane)));
                        self.split_file_tree.update(cx, |view, cx| {
                            view.attach(
                                cx,
                                Arc::clone(&self.engine),
                                Arc::clone(&second_pane),
                                second_id.clone(),
                            );
                        });
                        self.split_changes_view.update(cx, |view, _cx| {
                            view.attach(
                                Arc::clone(&self.engine),
                                Arc::clone(&second_pane),
                                second_id.clone(),
                            );
                        });
                    }
                }
                let second_body = match self.lens_book.lens_for(Some(second_id.as_str())) {
                    Lens::Chat => div()
                        .id("pane-split-second-chat")
                        .flex()
                        .flex_col()
                        .flex_1()
                        .child(self.split_chat_view.clone()),
                    Lens::Terminal => div()
                        .id("pane-split-second-terminal")
                        .flex()
                        .flex_col()
                        .flex_1()
                        .child(self.split_terminal_pane.clone()),
                    Lens::Changes => div()
                        .id("pane-split-second-changes")
                        .flex()
                        .flex_row()
                        .flex_1()
                        .child(
                            div()
                                .id("split-changes-left")
                                .flex()
                                .flex_col()
                                .flex_1()
                                .child(self.split_file_tree.clone()),
                        )
                        .child(
                            div()
                                .id("split-changes-right")
                                .flex()
                                .flex_col()
                                .flex_1()
                                .child(self.split_changes_view.clone()),
                        ),
                };
                div()
                    .id("pane-split-canvas")
                    .flex()
                    .flex_row()
                    .flex_1()
                    .child(inner)
                    .child(second_body)
            } else {
                inner
            }
        };
        let notice = self.notice.clone();
        div()
            .id("app")
            .flex()
            .flex_row()
            .size_full()
            .track_focus(&self.focus_handle)
            .on_key_down(
                cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| this.handle_editing_key(ev, cx)),
            )
            .child(
                div()
                    .id("sidebar")
                    .flex()
                    .flex_col()
                    .w(px(sidebar_w))
                    .child(self.workspace_sidebar.clone()),
            )
            .child(
                div()
                    .id("main-column")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.0))
                    .child(self.render_header(cx))
                    // The tab strip and lens switcher act on a selected pane;
                    // with no workspace they would be dead chrome.
                    .children(
                        has_workspaces.then(|| self.render_lens_toggle(cx).into_any_element()),
                    )
                    .children(has_workspaces.then(|| self.render_tab_strip(cx).into_any_element()))
                    .children(notice.map(|message| {
                        div()
                            .id("command-notice")
                            .px_3()
                            .py_1()
                            .text_size(px(11.0))
                            .text_color(Theme::current().indicator_blocked)
                            .child(message)
                            .into_any_element()
                    }))
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
            .child(
                if self.new_workspace.update(cx, |view, _cx| view.is_open()) {
                    div()
                        .id("new-workspace-host")
                        .absolute()
                        .inset_0()
                        .child(self.new_workspace.clone())
                } else {
                    div().id("no-new-workspace-modal")
                },
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
            |_window, cx| cx.new(OrionApp::new),
        )
        .unwrap();
    });
}
