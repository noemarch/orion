//! Main application view with engine integration.

use gpui::prelude::*;
use gpui::{
    div, px, App, Application, Context, Entity, Render, Size, Subscription, Window, WindowBounds,
    WindowOptions,
};
use orion_core::runtime::{PaneHandle, Runtime, RuntimeClient, RuntimeStream};
use orion_core::{Engine, FrameSink, Role};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::chat::{ChatEvent, ChatView};
use crate::diff_view::ChangesView;
use crate::files_view::FileTree;
use crate::new_workspace::{recent_directory, NewWorkspaceEvent, NewWorkspaceModal};
use crate::pane_seam::seam_of;
use crate::settings::{KeyBarExtras, SettingsModal};
use crate::shell_model::{
    lens_segment_state, HeaderModel, LensBook, ShellLayout, FILES_SURFACE_WIDTH_WIDE,
};
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
    /// The runtime the shell talks to: the engine in this process, or one in
    /// the daemon. The shell holds the runtime, not the terminals.
    engine: Arc<dyn Runtime>,
    lens_book: LensBook,
    /// The pane whose lens is active; the switcher writes here.
    focused_pane: Option<String>,
    /// The pane currently attached to the main lens views.
    attached_pane: Option<String>,
    /// Increments per attach so an old PTY pump stops feeding a replaced pane.
    attach_generation: Arc<AtomicU64>,
    /// Increments per split attach so the split's own PTY pump stops feeding a
    /// replaced pane.
    split_attach_generation: Arc<AtomicU64>,
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

/// The runtime session this shell belongs to.
///
/// `ORION_RUNTIME_SESSION` names an isolated runtime, which is how a second
/// shell and the tests keep out of each other's way.
fn runtime_session() -> String {
    std::env::var("ORION_RUNTIME_SESSION")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "default".to_string())
}

/// The runtime the shell talks to.
///
/// The runtime is another process: the terminals it owns outlive this window.
/// When no runtime is running, one is started and waited for.
async fn connect_or_start_runtime(session: &str) -> Result<Arc<dyn Runtime>, String> {
    if let Some(runtime) = try_connect_runtime(session).await {
        return Ok(runtime);
    }
    start_runtime(session)?;
    // The runtime needs a moment to bind its socket.
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if let Some(runtime) = try_connect_runtime(session).await {
            return Ok(runtime);
        }
    }
    Err("the runtime did not answer".to_string())
}

/// A runtime that answers, if one is listening.
async fn try_connect_runtime(session: &str) -> Option<Arc<dyn Runtime>> {
    let stream = RuntimeStream::connect(session).await.ok()?;
    // The handshake writes a frame and waits for the answer, so it belongs on a
    // blocking thread rather than an async worker.
    let client = tokio::task::spawn_blocking(move || RuntimeClient::new(Box::new(stream)))
        .await
        .ok()?
        .ok()?;
    Some(Arc::new(client) as Arc<dyn Runtime>)
}

/// Starts a runtime for `session`.
///
/// The runtime is detached: it must outlive the shell that started it, or
/// closing the shell would take the terminals with it.
fn start_runtime(session: &str) -> Result<(), String> {
    let mut command = runtime_command()?;
    command.arg("daemon");
    command.env("ORION_RUNTIME_SESSION", session);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("could not start the runtime: {error}"))
}

/// The command that starts a runtime.
///
/// The runtime lives in the `orion` binary, which sits beside the shell's own
/// binary; `PATH` is the fallback.
fn runtime_command() -> Result<std::process::Command, String> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(if cfg!(windows) { "orion.exe" } else { "orion" });
            if candidate.exists() {
                return Ok(std::process::Command::new(candidate));
            }
        }
    }
    Ok(std::process::Command::new("orion"))
}

/// Capacity of the PTY output channel between the attach sink (PTY reader
/// thread) and the pump. flume has no byte-aware bound, so this is a
/// message-count bound: 256 chunks. Chunks are PTY frames, typically a few
/// KiB; even if every chunk were 64 KiB (the pump's per-tick feed cap), the
/// queue holds at most ~16 MiB. The producer blocks when the channel is
/// full (herdr-core rule: block, never drop) — that is intended backpressure
/// on the PTY reader thread and never touches the UI thread.
const PTY_CHANNEL_CAPACITY: usize = 256;

/// The connection id the split canvas's terminal attaches with.
///
/// The main pane owns `"gpu-terminal"`. The split needs an id of its own:
/// a detach removes the attachment by key, and the split's key must never
/// take the main pane's attachment away.
const SPLIT_CONNECTION: &str = "gpu-terminal-split";

/// How often the status line is refreshed from the pane.
const STATUS_POLL_MS: u64 = 500;

/// The PTY output pump shared by the main pane attach and the focused
/// runtime terminal. Drains the channel in 64 KiB ticks so a flood of output
/// does not block the UI thread for the whole chunk, feeds the grid, and
/// notifies only when the screen revision changed. A newer attach bumps
/// `attach_generation`, which stops this pump from feeding.
fn spawn_pty_pump(
    terminal_pane: Entity<TerminalPane>,
    pty_rx: flume::Receiver<String>,
    generation: Arc<AtomicU64>,
    cx: &mut App,
) {
    let mine = generation.fetch_add(1, Ordering::SeqCst) + 1;
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
                    // Feed at most ~64 KiB per pump tick so a flood of output
                    // does not block the UI thread for the whole chunk.
                    const CHUNK_LIMIT: usize = 64 * 1024;
                    let before = view.revision();
                    let mut fed = first.len();
                    view.feed(&first);
                    while fed < CHUNK_LIMIT {
                        match pty_rx.try_recv() {
                            Ok(more) => {
                                fed += more.len();
                                view.feed(&more);
                            }
                            Err(_) => break,
                        }
                    }
                    if view.revision() != before {
                        cx.notify();
                    }
                });
            });
        }
        // Final notify to flush any pending repaint.
        let _ = cx.update(|cx| {
            terminal_pane.update(cx, |_view, cx| {
                cx.notify();
            });
        });
    })
    .detach();
}

/// Attach the split canvas's terminal view to one pane and start its pump.
///
/// The split watches the pane's output; it does not own the pane's PTY.
/// The attach uses the role the main pane already sends (`Role::Interact`),
/// so the daemon's connection role does not change and the main pane keeps
/// its input, and it asks to keep the size, so the attach-time resize is
/// suppressed and the PTY keeps the size it already had. The view observes:
/// its render follows its own box and never resizes the PTY.
fn attach_split_terminal(
    view: &Entity<TerminalPane>,
    pane: Arc<dyn PaneHandle>,
    generation: &Arc<AtomicU64>,
    cx: &mut App,
) -> bool {
    // The split's own channel and pump: the split's output must not reach
    // the main pane's grid, and its backpressure must not stall the main
    // pane's reader.
    let (pty_tx, pty_rx) = flume::bounded::<String>(PTY_CHANNEL_CAPACITY);
    let sink: FrameSink = Arc::new(move |frame: Value| {
        if frame["type"] == "pty-data" {
            if let Some(data) = frame["data"].as_str() {
                // Blocks the PTY reader thread when the pump falls behind;
                // that is the intended backpressure.
                let _ = pty_tx.send(data.to_string());
            }
        }
    });
    if pane
        .attach(SPLIT_CONNECTION, sink, Role::Interact, 120, 32, true)
        .is_err()
    {
        return false;
    }
    view.update(cx, |view, cx| {
        view.set_pane(pane);
        view.set_observe(true);
        cx.notify();
    });
    spawn_pty_pump(view.clone(), pty_rx, Arc::clone(generation), cx);
    true
}

/// Let the split canvas go: the pane keeps no split attachment and the
/// split's pump stops feeding the grid it was reading.
fn detach_split_terminal(pane: Option<&Arc<dyn PaneHandle>>, generation: &Arc<AtomicU64>) {
    if let Some(pane) = pane {
        pane.detach(SPLIT_CONNECTION);
    }
    // The pump stops at its next chunk: the generation it claimed is stale.
    generation.fetch_add(1, Ordering::SeqCst);
}

/// The status-line poller. `pane.status()` is a synchronous socket
/// round-trip on the daemon path (30s timeout), so it never runs on the UI
/// thread: a dedicated thread asks the pane what it is doing every
/// `STATUS_POLL_MS` and pushes the answer into a one-slot channel; the async
/// task applies it to the view. A stale line is fine; a frozen UI is not.
/// On error the thread keeps the last value on screen (the view is only
/// updated with a fresh answer). A newer attach bumps `attach_generation`,
/// which stops this poller the same way it stops the pump.
fn spawn_status_poller(
    terminal_pane: Entity<TerminalPane>,
    pane: Arc<dyn orion_core::runtime::PaneHandle>,
    generation: Arc<AtomicU64>,
    cx: &mut App,
) {
    // The pump claims the generation before this runs; read it, never bump it.
    let mine = generation.load(Ordering::SeqCst);
    let weak = terminal_pane.downgrade();
    // One slot: a slow UI drops intermediate answers, never the latest.
    let (tx, rx) = flume::bounded::<String>(1);
    let thread_generation = Arc::clone(&generation);
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(std::time::Duration::from_millis(STATUS_POLL_MS));
            if thread_generation.load(Ordering::SeqCst) != mine {
                break;
            }
            // Blocking round-trip on the daemon path; this thread exists so
            // the UI thread never waits on it.
            let status = pane.status().as_wire().to_string();
            // The receiver is gone when the view is gone; stop the thread.
            if tx.send(status).is_err() {
                break;
            }
        }
    });
    cx.spawn(async move |cx| {
        while let Ok(status) = rx.recv_async().await {
            if generation.load(Ordering::SeqCst) != mine {
                break;
            }
            let applied = cx.update(|cx| {
                weak.update(cx, |view, cx| {
                    if view.set_status_line(status) {
                        cx.notify();
                    }
                })
                .is_ok()
            });
            // The view is gone (or the app is shutting down): stop polling.
            if !applied.unwrap_or(false) {
                break;
            }
        }
    })
    .detach();
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
        let session = runtime_session();
        let runtime = tokio::runtime::Runtime::new()
            .ok()
            .and_then(|rt| rt.block_on(connect_or_start_runtime(&session)).ok());
        // Without a runtime of its own the shell keeps the engine in this
        // process, and the reader is told the terminals will not survive it.
        let (engine, notice) = match runtime {
            Some(engine) => (engine, None),
            None => (
                Arc::new(Engine::new()) as Arc<dyn Runtime>,
                Some(
                    "The Orion runtime is unavailable, so terminals will end with Orion. Start `orion daemon` and reopen Orion."
                        .to_string(),
                ),
            ),
        };

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
            split_attach_generation: Arc::new(AtomicU64::new(0)),
            editing_tab: None,
            split_chat_view,
            split_terminal_pane,
            split_attached_pane: None,
            split_file_tree,
            split_changes_view,
            subscriptions: Vec::new(),
            focus_handle: cx.focus_handle(),
            notice,
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
        engine.restore();
        self.workspace_sidebar
            .update(cx, |view, _cx| view.refresh());

        let snapshot = engine.snapshot_value();
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
        let engine = Arc::clone(&self.engine);
        if self.attached_pane.as_deref() == Some(pane_id) {
            return;
        }
        let Some(pane) = engine.pane_by_id(pane_id) else {
            return;
        };
        // Stop streaming the pane we are leaving; its pump also stops on the
        // generation check below.
        if let Some(previous) = self.attached_pane.take() {
            if let Some(previous_pane) = engine.pane_by_id(&previous) {
                previous_pane.detach("gpu-terminal");
            }
        }
        let engine = Arc::clone(&engine);

        // The attach sink runs on the PTY reader thread, so it can only push
        // into a channel; the async task below drains that channel, feeds the
        // grid, and notifies the view so GPUI repaints.
        let (pty_tx, pty_rx) = flume::bounded::<String>(PTY_CHANNEL_CAPACITY);
        let sink: FrameSink = Arc::new(move |frame: Value| {
            if frame["type"] == "pty-data" {
                if let Some(data) = frame["data"].as_str() {
                    // Blocks the PTY reader thread when the pump falls
                    // behind; that is the intended backpressure.
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
        self.chat_view.update(cx, |view, cx| {
            let seam = seam_of(&pane);
            view.attach(Some(Arc::clone(&engine)), seam, pane_id.to_string(), cx);
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
        let terminal_pane = self.terminal_pane.clone();
        spawn_pty_pump(terminal_pane.clone(), pty_rx, Arc::clone(&generation), cx);
        spawn_status_poller(terminal_pane, pane, generation, cx);
    }

    /// Point the split canvas at one pane and start its own PTY pump.
    ///
    /// The split is a second lens over a second pane. It attaches with its own
    /// connection id and its own pump, so it never takes the main pane's
    /// attachment away and never feeds the main pane's grid.
    fn attach_split_pane(&mut self, cx: &mut App, pane_id: &str) {
        if self.split_attached_pane.as_deref() == Some(pane_id) {
            return;
        }
        let engine = Arc::clone(&self.engine);
        // The pane the split is leaving keeps no split attachment.
        if let Some(previous) = self.split_attached_pane.replace(pane_id.to_string()) {
            if let Some(previous_pane) = engine.pane_by_id(&previous) {
                previous_pane.detach(SPLIT_CONNECTION);
            }
        }
        let Some(pane) = engine.pane_by_id(pane_id) else {
            return;
        };
        self.split_chat_view.update(cx, |view, cx| {
            let seam = seam_of(&pane);
            view.attach(Some(Arc::clone(&engine)), seam, pane_id.to_string(), cx);
        });
        self.split_file_tree.update(cx, |view, cx| {
            view.attach(
                cx,
                Arc::clone(&self.engine),
                Arc::clone(&pane),
                pane_id.to_string(),
            );
        });
        self.split_changes_view.update(cx, |view, _cx| {
            view.attach(
                Arc::clone(&self.engine),
                Arc::clone(&pane),
                pane_id.to_string(),
            );
        });
        // The split's terminal watches the pane: the attach keeps the PTY's
        // size, and the view never resizes it.
        let generation = Arc::clone(&self.split_attach_generation);
        attach_split_terminal(&self.split_terminal_pane, pane, &generation, cx);
    }

    /// Let the split canvas go: the pane keeps no split attachment and the
    /// split's pump stops feeding the grid it was reading.
    fn detach_split_pane(&mut self) {
        let Some(previous) = self.split_attached_pane.take() else {
            return;
        };
        let engine = Arc::clone(&self.engine);
        let pane = engine.pane_by_id(&previous);
        detach_split_terminal(pane.as_ref(), &self.split_attach_generation);
    }

    fn set_lens(&mut self, lens: Lens, cx: &mut impl gpui::AppContext) {
        self.lens_book.set(self.focused_pane.as_deref(), lens);
        match lens {
            // The chat is the lens the reader is looking at: their next word
            // belongs in its box.
            Lens::Chat => {
                self.chat_view.update(cx, |view, _cx| view.take_focus());
            }
            // The terminal takes the keyboard too: the reader's keystrokes go
            // to the agent without a click first.
            Lens::Terminal => {
                self.terminal_pane.update(cx, |view, _cx| view.take_focus());
            }
            Lens::Changes => {}
        }
    }

    /// What the chat asks for: the Terminal Lens has the whole output, and
    /// the Changes Lens opens the file. The split canvas answers for its own
    /// pane, not for the focused one.
    fn handle_chat_event(&mut self, event: &ChatEvent, pane: ChatPane, cx: &mut App) {
        match event {
            ChatEvent::ShowTerminal => match pane {
                ChatPane::Focused => self.set_lens(Lens::Terminal, cx),
                ChatPane::Split => {
                    if let Some(split_pane) = self.split_attached_pane.clone() {
                        self.lens_book.set(Some(&split_pane), Lens::Terminal);
                    }
                }
            },
            ChatEvent::FocusTerminal { id } => {
                // A runtime terminal is not a session pane: the daemon's
                // `pane-open` verb finds it in the runtime registry, and its
                // output reaches the shell as `pty-data` frames carrying that
                // id. The Terminal Lens takes it; the chat keeps its own pane,
                // because a runtime terminal has no conversation to read.
                if let Some(pane) = self.engine.pane_by_id(id.as_str()) {
                    let (pty_tx, pty_rx) = flume::bounded::<String>(PTY_CHANNEL_CAPACITY);
                    let sink: FrameSink = Arc::new(move |frame: Value| {
                        if frame["type"] == "pty-data" {
                            if let Some(data) = frame["data"].as_str() {
                                // Blocks the PTY reader thread when the pump
                                // falls behind; intended backpressure.
                                let _ = pty_tx.send(data.to_string());
                            }
                        }
                    });
                    if pane
                        .attach("gpu-terminal", sink, Role::Interact, 120, 32, false)
                        .is_ok()
                    {
                        let terminal_pane = self.terminal_pane.clone();
                        terminal_pane.update(cx, |view, cx| {
                            view.set_pane(Arc::clone(&pane));
                            cx.notify();
                        });
                        // The session pane is not what the Terminal Lens shows
                        // any more, so the lens must attach it again when the
                        // reader goes back to the agent.
                        self.attached_pane = None;
                        // The pump: the focused terminal's output goes to the
                        // grid, and the generation stops the pane left behind.
                        let generation = Arc::clone(&self.attach_generation);
                        spawn_pty_pump(terminal_pane.clone(), pty_rx, Arc::clone(&generation), cx);
                        spawn_status_poller(terminal_pane, pane, generation, cx);
                    }
                }
                self.set_lens(Lens::Terminal, cx);
            }
            ChatEvent::OpenFile(path) => {
                match pane {
                    ChatPane::Focused => self.set_lens(Lens::Changes, cx),
                    ChatPane::Split => {
                        if let Some(split_pane) = self.split_attached_pane.clone() {
                            self.lens_book.set(Some(&split_pane), Lens::Changes);
                        }
                    }
                }
                // The tree does the revealing, not the reader alone: it expands
                // the file's ancestor folders, selects the row and opens the
                // file, so the tree and the reader agree on what is shown.
                let tree = match pane {
                    ChatPane::Focused => self.file_tree.clone(),
                    ChatPane::Split => self.split_file_tree.clone(),
                };
                tree.update(cx, |tree, cx| tree.reveal(cx, path.clone()));
            }
        }
    }

    /// Runs one lifecycle command through the Route Table. A failure becomes
    /// the shell's one-line notice instead of a silent no-op.
    fn call_route(&mut self, path: &str, body: &Value) -> Option<Value> {
        let engine = Arc::clone(&self.engine);
        let (status, answer) = engine.http("POST", path, body);
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
        let engine = Arc::clone(&self.engine);
        let (status, body) = engine.http("GET", "/api/preferences", &Value::Null);
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
                        this.set_lens(Lens::Chat, cx);
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
                        this.set_lens(Lens::Terminal, cx);
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
                        this.set_lens(Lens::Changes, cx);
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
        let window_w = f32::from(window.bounds().size.width);
        let layout = ShellLayout::for_width(window_w);
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
        // The Chat lens reads the agent's whole transcript on its poll, so it
        // only reads it while it is the lens on screen.
        let chat_active = self.current_lens() == Lens::Chat;
        self.chat_view
            .update(cx, |view, cx| view.set_visible(cx, chat_active));
        let prefs = self.settings.update(cx, |view, _cx| view.prefs().clone());
        Theme::set_current(Theme::for_mode(prefs.theme, prefs.density));
        let extras = KeyBarExtras::from_preferences(&prefs);
        self.terminal_pane
            .update(cx, |view, _cx| view.set_key_bar_extras(extras));
        // The terminal's font: the family the reader configured, measured by
        // the terminal itself.
        let terminal_font = prefs.terminal_font.clone();
        let terminal_font_size = prefs.terminal_font_size;
        self.terminal_pane.update(cx, |view, _cx| {
            view.set_font_family(&terminal_font);
            view.set_font_size(terminal_font_size);
        });
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
            // The split is gone: its attachment and its pump go with it.
            self.detach_split_pane();
        }
        let split_changes_active = split_second
            .as_deref()
            .is_some_and(|pane_id| self.lens_book.lens_for(Some(pane_id)) == Lens::Changes);
        self.split_file_tree
            .update(cx, |view, cx| view.set_visible(cx, split_changes_active));
        self.split_changes_view
            .update(cx, |view, _cx| view.set_visible(split_changes_active));
        let split_chat_active = split_second
            .as_deref()
            .is_some_and(|pane_id| self.lens_book.lens_for(Some(pane_id)) == Lens::Chat);
        self.split_chat_view
            .update(cx, |view, cx| view.set_visible(cx, split_chat_active));
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
                Lens::Chat => div()
                    .id("main-content")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .child(self.chat_view.clone()),
                Lens::Terminal => div()
                    .id("main-content")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .child(self.terminal_pane.clone()),
                Lens::Changes => {
                    // This lens is two surfaces: Files (tree, splitter, reader)
                    // and Changes (list, splitter, diff). Wide they sit side by
                    // side with a fixed Files column so the diff keeps readable
                    // width; narrow they stack, so neither surface is hidden
                    // and each keeps its own inner split. Each view is told the
                    // width its surface really receives so its splitter clamps
                    // against real room.
                    let mut main_room = window_w - sidebar_w;
                    if split_second.is_some() {
                        // The split canvas shows a second pane beside this one;
                        // each half gets half the main column.
                        main_room /= 2.0;
                    }
                    let files_room = if layout.rail {
                        main_room
                    } else {
                        FILES_SURFACE_WIDTH_WIDE
                    };
                    let changes_room = if layout.rail {
                        main_room
                    } else {
                        main_room - FILES_SURFACE_WIDTH_WIDE
                    };
                    self.file_tree
                        .update(cx, |view, _cx| view.set_available_width(files_room));
                    self.changes_view
                        .update(cx, |view, _cx| view.set_available_width(changes_room));
                    let files_surface = div()
                        .id("changes-left")
                        .flex()
                        .flex_col()
                        .child(self.file_tree.clone());
                    let changes_surface = div()
                        .id("changes-right")
                        .flex()
                        .flex_col()
                        .min_w(px(0.0))
                        .child(self.changes_view.clone());
                    let canvas = div().id("main-content").flex().flex_1();
                    if layout.rail {
                        canvas
                            .flex_col()
                            .child(files_surface.flex_1().min_h(px(0.0)))
                            .child(changes_surface.flex_1().min_h(px(0.0)))
                    } else {
                        canvas
                            .flex_row()
                            .child(
                                files_surface
                                    .w(px(FILES_SURFACE_WIDTH_WIDE))
                                    .flex_shrink_0(),
                            )
                            .child(changes_surface.flex_1())
                    }
                }
            };
            if let Some(second_id) = split_second.clone() {
                // The split's own lenses, its own attachment and its own pump.
                self.attach_split_pane(cx, &second_id);
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
                    Lens::Changes => {
                        // Same two-surface layout as the main canvas, inside
                        // this half of the split, with the half's width pushed
                        // to both views.
                        let split_room = (window_w - sidebar_w) / 2.0;
                        let files_room = if layout.rail {
                            split_room
                        } else {
                            FILES_SURFACE_WIDTH_WIDE
                        };
                        let changes_room = if layout.rail {
                            split_room
                        } else {
                            split_room - FILES_SURFACE_WIDTH_WIDE
                        };
                        self.split_file_tree
                            .update(cx, |view, _cx| view.set_available_width(files_room));
                        self.split_changes_view
                            .update(cx, |view, _cx| view.set_available_width(changes_room));
                        let files_surface = div()
                            .id("split-changes-left")
                            .flex()
                            .flex_col()
                            .child(self.split_file_tree.clone());
                        let changes_surface = div()
                            .id("split-changes-right")
                            .flex()
                            .flex_col()
                            .min_w(px(0.0))
                            .child(self.split_changes_view.clone());
                        let canvas = div().id("pane-split-second-changes").flex().flex_1();
                        if layout.rail {
                            canvas
                                .flex_col()
                                .child(files_surface.flex_1().min_h(px(0.0)))
                                .child(changes_surface.flex_1().min_h(px(0.0)))
                        } else {
                            canvas
                                .flex_row()
                                .child(
                                    files_surface
                                        .w(px(FILES_SURFACE_WIDTH_WIDE))
                                        .flex_shrink_0(),
                                )
                                .child(changes_surface.flex_1())
                        }
                    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use orion_core::agents::LaunchSpec;
    use orion_core::engine::pane::{Pane, PaneConfig, PaneEvent};
    use orion_core::protocol::{AgentKind, TerminalSize};
    use orion_core::runtime::{FrameConnection, PROTOCOL_VERSION};
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;

    #[gpui::test]
    fn the_pump_feeds_chunks_and_notifies_only_on_revision_change(cx: &mut gpui::TestAppContext) {
        let view = cx.update(|app| app.new(TerminalPane::new));
        let (tx, rx) = flume::bounded::<String>(PTY_CHANNEL_CAPACITY);
        let generation = Arc::new(AtomicU64::new(0));
        let notifies = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&notifies);
        let _subscription = cx.update(|app| {
            app.observe(&view, move |_view, _app| {
                counter.fetch_add(1, Ordering::SeqCst);
            })
        });
        cx.update(|app| {
            spawn_pty_pump(view.clone(), rx, Arc::clone(&generation), app);
        });
        for i in 0..100 {
            tx.send(format!("chunk {i}\r\n"))
                .expect("the pump is listening");
        }
        drop(tx);
        cx.run_until_parked();

        let revision = cx.read(|app| view.read(app).revision());
        assert!(
            revision >= 100,
            "every chunk advanced the revision: {revision}"
        );
        let notifies = notifies.load(Ordering::SeqCst);
        assert!(notifies >= 1, "the pump notified at least once");
        assert!(
            notifies <= 100,
            "the pump notified at most once per chunk: {notifies}"
        );
    }

    #[test]
    fn the_pty_channel_blocks_the_producer_at_capacity() {
        // A paused consumer (the pump is not draining) must not grow the
        // queue without bound: the producer blocks at the capacity bound.
        let (tx, _rx) = flume::bounded::<String>(PTY_CHANNEL_CAPACITY);
        for i in 0..PTY_CHANNEL_CAPACITY {
            tx.try_send(format!("chunk {i}"))
                .expect("the channel accepts up to its capacity");
        }
        assert!(tx.is_full(), "the channel is full at capacity");
        assert!(
            tx.try_send("one more".to_string()).is_err(),
            "the producer is blocked at capacity, not dropped"
        );
        // With the consumer paused, a blocking send waits instead of
        // growing the queue; the timeout proves it was waiting.
        assert!(
            tx.send_timeout("one more".to_string(), std::time::Duration::from_millis(50))
                .is_err(),
            "a blocked producer waits for the consumer instead of growing the queue"
        );
    }

    #[gpui::test]
    fn a_stale_pump_stops_feeding_when_the_generation_bumps(cx: &mut gpui::TestAppContext) {
        let view = cx.update(|app| app.new(TerminalPane::new));
        let (tx, rx) = flume::bounded::<String>(PTY_CHANNEL_CAPACITY);
        let generation = Arc::new(AtomicU64::new(0));
        cx.update(|app| {
            spawn_pty_pump(view.clone(), rx, Arc::clone(&generation), app);
        });
        tx.send("first\r\n".to_string())
            .expect("the pump is listening");
        cx.run_until_parked();
        let after_first = cx.read(|app| view.read(app).revision());
        assert!(after_first >= 1, "the first chunk was fed: {after_first}");

        // A newer attach owns the view now; the stale pump must stop feeding.
        generation.fetch_add(1, Ordering::SeqCst);
        tx.send("stale\r\n".to_string())
            .expect("the pump is listening");
        drop(tx);
        cx.run_until_parked();

        let after_stale = cx.read(|app| view.read(app).revision());
        assert_eq!(
            after_first, after_stale,
            "a stale pump must not feed the grid"
        );
    }

    /// What the split asked of the pane when it attached.
    #[derive(Clone)]
    struct SplitAttach {
        connection: String,
        role: Role,
        keep_size: bool,
    }

    /// A pane that records what the split asked of it and can emit output.
    struct SplitPane {
        cwd: std::path::PathBuf,
        attaches: Arc<Mutex<Vec<SplitAttach>>>,
        detaches: Arc<Mutex<Vec<String>>>,
        sink: Arc<Mutex<Option<FrameSink>>>,
    }

    impl SplitPane {
        /// The pane's output, as the PTY reader thread would deliver it.
        fn emit(&self, text: &str) {
            let sink = self.sink.lock().expect("the sink's lock").clone();
            if let Some(sink) = sink {
                sink(json!({
                    "type": "pty-data",
                    "pane_id": "split-pane",
                    "data": text,
                }));
            }
        }
    }

    impl PaneHandle for SplitPane {
        fn id(&self) -> &str {
            "split-pane"
        }

        fn cwd(&self) -> &std::path::Path {
            &self.cwd
        }

        fn write(&self, _data: &str) -> orion_core::Result<()> {
            Ok(())
        }

        fn keys(&self, _keys: &[String]) -> orion_core::Result<()> {
            Ok(())
        }

        fn interrupt(&self) -> orion_core::Result<()> {
            Ok(())
        }

        fn resize(&self, _cols: u16, _rows: u16) -> orion_core::Result<()> {
            Ok(())
        }

        fn clear(&self) {}

        fn status(&self) -> orion_core::protocol::AgentStatus {
            orion_core::protocol::AgentStatus::Idle
        }

        fn prompt(&self) -> Option<orion_core::prompt::DetectedPrompt> {
            None
        }

        fn attach(
            &self,
            connection: &str,
            sink: FrameSink,
            role: Role,
            _cols: u16,
            _rows: u16,
            keep_size: bool,
        ) -> orion_core::Result<()> {
            self.attaches
                .lock()
                .expect("the log's lock")
                .push(SplitAttach {
                    connection: connection.to_string(),
                    role,
                    keep_size,
                });
            // The pane keeps the connection: its output goes on reaching the
            // split until the split lets go of it.
            *self.sink.lock().expect("the sink's lock") = Some(sink);
            self.emit("split-live-output\r\n");
            Ok(())
        }

        fn detach(&self, connection: &str) {
            self.detaches
                .lock()
                .expect("the log's lock")
                .push(connection.to_string());
            // The connection keeps its sink: a detach must stop the split's
            // pump even when the connection still delivers.
        }

        fn detach_all(&self, connection: &str) {
            self.detach(connection);
        }
    }

    /// The split watches the pane: it takes the output, never the size.
    #[gpui::test]
    fn the_split_watches_the_pane_without_taking_its_size(cx: &mut gpui::TestAppContext) {
        let attaches = Arc::new(Mutex::new(Vec::new()));
        let detaches = Arc::new(Mutex::new(Vec::new()));
        let pane = Arc::new(SplitPane {
            cwd: std::path::PathBuf::from("."),
            attaches: Arc::clone(&attaches),
            detaches,
            sink: Arc::new(Mutex::new(None)),
        });
        let handle: Arc<dyn PaneHandle> = pane.clone();
        let view = cx.update(|app| app.new(TerminalPane::new));
        let generation = Arc::new(AtomicU64::new(0));
        cx.update(|app| {
            assert!(
                attach_split_terminal(&view, handle, &generation, app),
                "the split attaches"
            );
        });

        let attaches = attaches.lock().expect("the log's lock").clone();
        assert_eq!(attaches.len(), 1, "the split attaches once");
        let attach = &attaches[0];
        assert_eq!(
            attach.connection, SPLIT_CONNECTION,
            "the split attaches under its own connection id"
        );
        assert_ne!(
            attach.connection, "gpu-terminal",
            "the split never takes the main pane's attachment"
        );
        assert_eq!(
            attach.role,
            Role::Interact,
            "the split sends the role the main pane sends: the main pane keeps the keyboard"
        );
        assert!(
            attach.keep_size,
            "the split asks to keep the pane's size: the attach does not resize it"
        );

        // The split's view shows the pane's output.
        cx.run_until_parked();
        let text = cx.read(|app| view.read(app).visible_text());
        assert!(
            text.contains("split-live-output"),
            "the split shows the pane's output: {text:?}"
        );
    }

    /// The split lets go of the pane: its attachment goes and its pump stops.
    #[gpui::test]
    fn detaching_the_split_stops_its_pump(cx: &mut gpui::TestAppContext) {
        let attaches = Arc::new(Mutex::new(Vec::new()));
        let detaches = Arc::new(Mutex::new(Vec::new()));
        let pane = Arc::new(SplitPane {
            cwd: std::path::PathBuf::from("."),
            attaches: Arc::clone(&attaches),
            detaches: Arc::clone(&detaches),
            sink: Arc::new(Mutex::new(None)),
        });
        let handle: Arc<dyn PaneHandle> = pane.clone();
        let view = cx.update(|app| app.new(TerminalPane::new));
        let generation = Arc::new(AtomicU64::new(0));
        cx.update(|app| {
            assert!(
                attach_split_terminal(&view, handle, &generation, app),
                "the split attaches"
            );
        });
        cx.run_until_parked();
        let fed = cx.read(|app| view.read(app).revision());
        assert!(fed > 0, "the split's pump fed the view: {fed}");

        // The split goes: the pane keeps no split attachment...
        let handle: Arc<dyn PaneHandle> = pane.clone();
        detach_split_terminal(Some(&handle), &generation);
        assert_eq!(
            detaches.lock().expect("the log's lock").clone(),
            vec![SPLIT_CONNECTION.to_string()],
            "the split lets go of the pane under its own connection id"
        );

        // ...and its pump stops feeding the grid it was reading.
        pane.emit("late output\r\n");
        cx.run_until_parked();
        let after = cx.read(|app| view.read(app).revision());
        assert_eq!(fed, after, "a detached split's pump must not feed the grid");
    }

    /// A real pane in this process: the split's rule is about a real PTY's
    /// size, so the test uses one.
    fn real_pane() -> Option<Arc<Pane>> {
        let command = if cfg!(windows) { "more.com" } else { "cat" };
        let spec = LaunchSpec::for_command(command, &[]).ok()?;
        let events: Arc<dyn Fn(PaneEvent) + Send + Sync> = Arc::new(|_| {});
        let pane = Pane::spawn(
            PaneConfig {
                id: "split-pane".into(),
                workspace_id: "w".into(),
                tab_id: "t".into(),
                agent: AgentKind::Claude,
                label: "test".into(),
                cwd: std::env::temp_dir(),
                spec,
                args: Vec::new(),
                size: TerminalSize {
                    cols: 120,
                    rows: 32,
                    pixel_width: 0,
                    pixel_height: 0,
                },
            },
            events,
        )
        .ok()?;
        Some(Arc::new(pane))
    }

    /// The agent's PTY keeps the size it had when the split opens.
    #[gpui::test]
    fn the_split_attach_leaves_the_pane_the_size_it_had(cx: &mut gpui::TestAppContext) {
        let Some(pane) = real_pane() else {
            eprintln!("skipped: no command to hold a terminal open on this machine");
            return;
        };
        // The main pane owns the size: it tells the agent how big it is.
        pane.attach(
            "gpu-terminal",
            Arc::new(|_| {}),
            Role::Interact,
            80,
            24,
            false,
        )
        .expect("the main pane attaches");
        let before = pane.size();
        assert_eq!(
            before,
            (24, 80),
            "the main pane sized the agent: {before:?}"
        );

        let view = cx.update(|app| app.new(TerminalPane::new));
        let generation = Arc::new(AtomicU64::new(0));
        let handle: Arc<dyn PaneHandle> = pane.clone();
        cx.update(|app| {
            assert!(
                attach_split_terminal(&view, handle, &generation, app),
                "the split attaches"
            );
        });
        assert_eq!(
            pane.size(),
            before,
            "the split did not resize the agent's PTY"
        );
    }

    /// A connection that records what the shell sends and answers what the
    /// runtime would answer.
    #[derive(Clone)]
    struct RecordingConnection {
        sent: Arc<Mutex<Vec<Value>>>,
        inbox: Arc<Mutex<std::sync::mpsc::Receiver<Value>>>,
        outbox: std::sync::mpsc::Sender<Value>,
    }

    impl FrameConnection for RecordingConnection {
        fn read_frame(&mut self) -> std::io::Result<Option<Value>> {
            let inbox = self.inbox.lock().expect("the inbox's lock");
            match inbox.recv() {
                Ok(frame) => Ok(Some(frame)),
                Err(_) => Ok(None),
            }
        }

        fn write_frame(&mut self, frame: &Value) -> std::io::Result<()> {
            self.sent
                .lock()
                .expect("the log's lock")
                .push(frame.clone());
            let answer = match frame.get("type").and_then(Value::as_str) {
                Some("health") => Some(json!({
                    "ok": true,
                    "version": "0",
                    "protocol": PROTOCOL_VERSION,
                })),
                Some("pane-open") => Some(json!({
                    "id": frame.get("id").cloned().unwrap_or(Value::Null),
                    "ok": true,
                    "cwd": ".",
                })),
                _ => None,
            };
            if let Some(answer) = answer {
                let _ = self.outbox.send(answer);
            }
            Ok(())
        }

        fn clone_box(&self) -> Box<dyn FrameConnection> {
            Box::new(self.clone())
        }
    }

    /// The split leaves the runtime's connection interactive.
    ///
    /// The role belongs to the connection, not to the pane: a split that sent
    /// `observe` would turn the main pane read-only on the daemon path.
    #[gpui::test]
    fn the_split_keeps_the_runtime_connection_interactive(cx: &mut gpui::TestAppContext) {
        let (outbox, inbox) = std::sync::mpsc::channel::<Value>();
        let sent = Arc::new(Mutex::new(Vec::new()));
        let connection = RecordingConnection {
            sent: Arc::clone(&sent),
            inbox: Arc::new(Mutex::new(inbox)),
            outbox,
        };
        let client = RuntimeClient::new(Box::new(connection)).expect("the runtime answers");
        let pane = client.pane_by_id("pane-2").expect("the pane is open");

        let view = cx.update(|app| app.new(TerminalPane::new));
        let generation = Arc::new(AtomicU64::new(0));
        cx.update(|app| {
            assert!(
                attach_split_terminal(&view, pane, &generation, app),
                "the split attaches"
            );
        });

        let sent = sent.lock().expect("the log's lock").clone();
        let role = sent
            .iter()
            .find(|frame| frame["type"] == "role")
            .expect("the split sends a role");
        assert_eq!(
            role["mode"], "interact",
            "the split sends the role the main pane sends: the main pane keeps the keyboard"
        );
        let attach = sent
            .iter()
            .find(|frame| frame["type"] == "attach")
            .expect("the split attaches");
        assert_eq!(attach["pane_id"], "pane-2");
        assert_eq!(
            attach["keep_size"], true,
            "the split keeps the pane's size: the attach does not resize it"
        );
    }
}
