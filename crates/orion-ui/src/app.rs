//! Main application view with engine integration.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::toolbar::Toolbar;
use gpui_kit::component::Sizable;
use gpui_kit::component::{Icon, IconName};
use gpui_kit::*;
use gpui_kit::{
    div, px, App, Context, Entity, Render, Size, Subscription, Window, WindowBounds, WindowOptions,
};
use orion_core::runtime::{PaneHandle, Runtime, RuntimeClient, RuntimeStream};
use orion_core::{Engine, FrameSink, Role};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crate::chat::{ChatEvent, ChatView};
use crate::diff_view::ChangesView;
use crate::files_view::FileTree;
use crate::new_workspace::{recent_directory, NewWorkspaceEvent, NewWorkspaceModal};
use crate::pane_seam::seam_of;
use crate::review_pane::ReviewPane;
use crate::settings::{KeyBarExtras, Preferences, SettingsModal};
use crate::shell_model::{
    reference_layout, review_column_visible, sidebar_visible, HeaderModel, LensBook, ShellLayout,
    FILES_SURFACE_WIDTH_WIDE,
};
use crate::sidebar::{SidebarEvent, TabEntry, WorkspaceEntry};
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
    /// The reference layout's right-hand review column, a presentation over the
    /// same [`ChangesView`] the Changes lens renders.
    review_pane: Entity<ReviewPane>,
    /// Whether the review column is wanted; it still collapses when the window
    /// is too narrow or a split pane is up.
    review_open: bool,
    /// Whether the left sidebar is mounted. When false the sidebar div is not
    /// mounted at all (zero width); the entity stays alive so selection and
    /// scroll survive a hide/show cycle. Distinct from the narrow-width rail.
    sidebar_open: bool,
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
    focus_handle: gpui_kit::FocusHandle,
    /// The last lifecycle command that failed, shown as one plain sentence.
    notice: Option<String>,
    /// Shell state derived from the sidebar snapshot. Refreshed on session
    /// events (and once at startup), never in `render`, so the frame path
    /// reads cached values instead of cloning the whole session JSON.
    last_render_prefs: Option<Preferences>,
    cached_live_panes: Vec<String>,
    cached_agent_label: String,
    cached_has_workspaces: bool,
    cached_split_panes: Vec<String>,
    cached_workspaces: Vec<WorkspaceEntry>,
    cached_tabs: Vec<TabEntry>,
    cached_tabs_workspace_id: Option<String>,
    /// Last sidebar session-body token seen by the observer. Pulse ticks leave
    /// it unchanged, so the observer skips the cache rebuild and repaint.
    cached_sidebar_fingerprint: Option<String>,
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
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        // Give the daemon a hidden console rather than no console at all. With
        // DETACHED_PROCESS it has none, so every console child it spawns without
        // explicit flags gets a fresh *visible* window. A hidden console is
        // inherited by those children, so they stay invisible. The daemon still
        // outlives this shell - process lifetime does not follow the parent.
        command.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
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

/// The renderer's frame budget: the grid asks for one repaint per frame, not
/// one per chunk of output.
const FRAME_BUDGET: std::time::Duration = std::time::Duration::from_millis(16);

/// The sink the terminal lens attaches a pane with, and the channel it feeds.
///
/// The sink runs on the PTY reader thread, so it can only push into a
/// channel; the pump drains that channel, feeds the grid, and notifies the
/// view so GPUI repaints. The sender blocks when the pump falls behind: that
/// is the intended backpressure on the reader thread, and it never touches the
/// UI thread.
pub fn pty_sink() -> (flume::Receiver<String>, FrameSink) {
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
    (pty_rx, sink)
}

/// The PTY output pump shared by the main pane attach and the focused
/// runtime terminal. Drains the channel in 64 KiB ticks so a flood of output
/// does not block the UI thread for the whole chunk, feeds the grid, and
/// notifies only when the screen revision changed. A newer attach bumps
/// `attach_generation`, which stops this pump from feeding.
pub fn spawn_pty_pump(
    terminal_pane: Entity<TerminalPane>,
    pty_rx: flume::Receiver<String>,
    generation: Arc<AtomicU64>,
    cx: &mut App,
) {
    let mine = generation.fetch_add(1, Ordering::SeqCst) + 1;
    cx.spawn(async move |cx| {
        // The renderer gets at most one repaint per frame: a flood of output
        // asks for a frame, not a frame per chunk. The wait happens before the
        // feed, so the grid stays at most one frame behind the terminal.
        let mut last_notify = Instant::now() - FRAME_BUDGET;
        while let Ok(first) = pty_rx.recv_async().await {
            // A newer attach owns the view now; stop feeding it.
            if generation.load(Ordering::SeqCst) != mine {
                break;
            }
            let since = last_notify.elapsed();
            if since < FRAME_BUDGET {
                cx.background_executor().timer(FRAME_BUDGET - since).await;
            }
            let generation = Arc::clone(&generation);
            cx.update(|cx| {
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
            last_notify = Instant::now();
        }
        // Final notify to flush any pending repaint.
        cx.update(|cx| {
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
    let (pty_rx, sink) = pty_sink();
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
            if !applied {
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
        let review_pane = cx.new(|cx| ReviewPane::new(changes_view.clone(), cx));
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
            Some(engine) => {
                orion_core::trace::record(orion_core::trace::Event::BackendConnected);
                (engine, None)
            }
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
            review_pane,
            review_open: true,
            sidebar_open: true,
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
            last_render_prefs: None,
            cached_live_panes: Vec::new(),
            cached_agent_label: "the agent".to_string(),
            cached_has_workspaces: false,
            cached_split_panes: Vec::new(),
            cached_workspaces: Vec::new(),
            cached_tabs: Vec::new(),
            cached_tabs_workspace_id: None,
            cached_sidebar_fingerprint: None,
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
            this.refresh_shell_cache(cx);
            cx.notify();
        });
        let modal_sub = cx.subscribe(&modal_events, |this, _, event: &NewWorkspaceEvent, cx| {
            let NewWorkspaceEvent::Created(pane_id) = event;
            this.workspace_sidebar
                .update(cx, |view, _cx| view.refresh());
            this.focused_pane = Some(pane_id.clone());
            this.attach_pane(cx, pane_id);
            this.refresh_shell_cache(cx);
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
        let sidebar_observe = cx.observe(&app.workspace_sidebar, |this, view, cx| {
            // The sidebar notifies on every pulse tick while a status animates.
            // Its session data is unchanged on those ticks, so skip the costly
            // cache rebuild and repaint unless the snapshot actually moved.
            let unchanged =
                view.read(cx).snapshot_fingerprint() == this.cached_sidebar_fingerprint.as_deref();
            if unchanged {
                return;
            }
            this.refresh_shell_cache(cx);
            cx.notify();
        });
        app.subscriptions = vec![
            sidebar_sub,
            modal_sub,
            chat_sub,
            split_chat_sub,
            sidebar_observe,
        ];

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
        self.refresh_shell_cache(cx);

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
        orion_core::trace::begin();
        orion_core::trace::record_with(
            orion_core::trace::Event::WorkspaceOpenClicked,
            Some(format!("claude in {}", cwd.to_string_lossy())),
        );
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
        let (pty_rx, sink) = pty_sink();
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

    fn set_lens(&mut self, lens: Lens, cx: &mut impl gpui_kit::AppContext) {
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
                orion_core::trace::record(orion_core::trace::Event::TerminalTabSelected);
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
                    let (pty_rx, sink) = pty_sink();
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
    fn handle_editing_key(&mut self, ev: &gpui_kit::KeyDownEvent, cx: &mut Context<Self>) {
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

    fn render_window_controls(window: &Window, theme: &Theme) -> impl IntoElement {
        let muted = theme.token_text_muted();
        let hover_bg = theme.hover_bg();
        let hover_fg = theme.text_primary();
        let close_hover_bg = theme.close_hover_bg();
        let close_hover_fg = theme.close_hover_fg();
        let maximize_icon = if window.is_maximized() {
            IconName::WindowRestore
        } else {
            IconName::WindowMaximize
        };

        div()
            .flex()
            .flex_row()
            .h_full()
            .flex_shrink_0()
            .child(
                div()
                    .w(px(46.0))
                    .h_full()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .window_control_area(WindowControlArea::Min)
                    .text_color(muted)
                    .hover(move |s| s.bg(hover_bg).text_color(hover_fg))
                    .child(Icon::new(IconName::WindowMinimize).small()),
            )
            .child(
                div()
                    .w(px(46.0))
                    .h_full()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .window_control_area(WindowControlArea::Max)
                    .text_color(muted)
                    .hover(move |s| s.bg(hover_bg).text_color(hover_fg))
                    .child(Icon::new(maximize_icon).small()),
            )
            .child(
                div()
                    .w(px(46.0))
                    .h_full()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .window_control_area(WindowControlArea::Close)
                    .text_color(muted)
                    .hover(move |s| s.bg(close_hover_bg).text_color(close_hover_fg))
                    .child(Icon::new(IconName::WindowClose).small()),
            )
    }

    fn render_header(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current();
        let workspaces = &self.cached_workspaces;
        let focused = workspaces
            .iter()
            .find(|w| w.focused)
            .or_else(|| workspaces.first());
        let header = if let Some(ws) = focused {
            HeaderModel {
                title: "Orion".to_string(),
                workspace: ws.label.clone(),
                connection: "Live".to_string(),
            }
        } else {
            HeaderModel::empty()
        };
        let sidebar_label = if self.sidebar_open {
            "Hide sidebar"
        } else {
            "Show sidebar"
        };
        let review_label = if self.review_open {
            "Hide review"
        } else {
            "Show review"
        };
        // The kit's toolbar: the title and workspace are plain content, and
        // the flexible spacer pushes the connection and the settings control
        // to the trailing edge.
        Toolbar::new("app-header")
            .h(px(40.0))
            .w_full()
            .px(px(theme.space_3()))
            .gap(px(theme.space_3()))
            .bg(theme.window_bg())
            .content(
                div()
                    .id("header-drag-title")
                    .window_control_area(WindowControlArea::Drag)
                    .text_size(px(theme.font_title()))
                    .text_color(theme.text_primary())
                    .child(header.title),
            )
            .content(
                div()
                    .id("header-drag-workspace")
                    .window_control_area(WindowControlArea::Drag)
                    .min_w(px(0.0))
                    .text_size(px(theme.font_ui_secondary()))
                    .text_color(theme.token_text_dim())
                    .truncate()
                    .child(header.workspace),
            )
            .content(
                div()
                    .id("header-drag-connection")
                    .window_control_area(WindowControlArea::Drag)
                    .flex_1()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .child(
                        div()
                            .text_size(px(theme.font_ui_secondary()))
                            .text_color(theme.token_text_dim())
                            .child(header.connection),
                    ),
            )
            .content(
                Button::new("header-sidebar-toggle")
                    .icon(Icon::new(IconName::PanelLeft))
                    .ghost()
                    .small()
                    .h(px(44.0))
                    .min_h(px(44.0))
                    .min_w(px(44.0))
                    .tooltip(sidebar_label)
                    .accessibility_label(sidebar_label)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar_open = !this.sidebar_open;
                        cx.notify();
                    })),
            )
            .content(
                Button::new("header-review-toggle")
                    .icon(Icon::new(IconName::PanelRight))
                    .ghost()
                    .small()
                    .h(px(44.0))
                    .min_h(px(44.0))
                    .min_w(px(44.0))
                    .tooltip(review_label)
                    .accessibility_label(review_label)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.review_open = !this.review_open;
                        cx.notify();
                    })),
            )
            // content, not child: the toolbar's child() turns a button into a
            // quiet ghost command, and this one keeps its labeled action look.
            .content(
                Button::new("header-settings")
                    .icon(Icon::new(IconName::Settings))
                    .ghost()
                    .small()
                    .h(px(44.0))
                    .min_h(px(44.0))
                    .min_w(px(44.0))
                    .tooltip("Settings")
                    .accessibility_label("Settings")
                    .on_click(cx.listener(|this, _, _, cx| {
                        let settings = this.settings.clone();
                        settings.update(cx, |view, cx| {
                            view.set_open(true);
                            cx.notify();
                        });
                        cx.notify();
                    })),
            )
            .content(Self::render_window_controls(window, &theme))
    }

    /// The compact Chat|Terminal switch of the three-panel layout. The review
    /// column owns the diff there, so the middle column only needs the two
    /// lenses that share the focused pane's live views.
    fn render_middle_switcher(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current();
        // The Changes lens keeps the conversation on screen in this layout, so
        // Chat is the selected tab for it too.
        let lens_index = match self.current_lens() {
            Lens::Terminal => 1,
            Lens::Chat | Lens::Changes => 0,
        };
        let focused_pane_id = self.focused_pane.clone();
        let focused_pane_id_zoom = focused_pane_id.clone();
        div()
            .id("middle-switcher")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .h(px(40.0))
            .px_3()
            .w_full()
            .bg(theme.panel_bg())
            .child(
                TabBar::new("middle-switch")
                    .segmented()
                    .selected_index(lens_index)
                    .child(
                        Tab::new()
                            .debug_selector(|| "middle-chat".into())
                            .label("Chat")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_lens(Lens::Chat, cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        Tab::new()
                            .debug_selector(|| "middle-terminal".into())
                            .label("Terminal")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_lens(Lens::Terminal, cx);
                                cx.notify();
                            })),
                    ),
            )
            .child(div().flex_1())
            .child(
                Button::new("middle-pane-zoom")
                    .label("Zoom")
                    .secondary()
                    .small()
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
                Button::new("middle-pane-close")
                    .label("Close")
                    .secondary()
                    .small()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(pane_id) = focused_pane_id.clone() {
                            this.call_route("/api/pane/close", &json!({ "pane_id": pane_id }));
                        }
                        this.workspace_sidebar
                            .update(cx, |view, _cx| view.refresh());
                        cx.notify();
                    })),
            )
    }

    /// The thin bar under the middle column. Every field comes from the live
    /// sidebar snapshot: the focused workspace's path, label, agent and status,
    /// plus the focused pane id. No token counts or other invented metrics.
    fn render_conversation_status(&self, _cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current();
        let workspaces = &self.cached_workspaces;
        let focused = workspaces
            .iter()
            .find(|workspace| workspace.focused)
            .or_else(|| workspaces.first());
        let label = focused.map(|ws| ws.label.clone()).unwrap_or_default();
        let cwd = focused.and_then(|ws| ws.cwd.clone()).unwrap_or_default();
        let agent = focused.map(|ws| ws.agent.clone()).unwrap_or_default();
        let status = focused
            .map(|ws| ws.status.as_wire().to_string())
            .unwrap_or_default();
        let pane_id = self.focused_pane.clone();
        let location = if cwd.is_empty() { label } else { cwd };
        let mut right_fields: Vec<String> = Vec::new();
        if !agent.is_empty() {
            right_fields.push(agent);
        }
        if !status.is_empty() {
            right_fields.push(status);
        }
        if let Some(pane_id) = pane_id {
            right_fields.push(pane_id);
        }
        let mut right = div()
            .flex_1()
            .min_w(px(0.0))
            .flex()
            .flex_row()
            .items_center()
            .justify_end()
            .overflow_hidden()
            .whitespace_nowrap();
        for (index, field) in right_fields.into_iter().enumerate() {
            if index > 0 {
                right = right.child(" · ");
            }
            right = right.child(field);
        }
        div()
            .id("conversation-status")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(theme.space_2()))
            .flex_shrink_0()
            .px(px(theme.space_3()))
            .py(px(theme.space_1()))
            .border_t_1()
            .border_color(theme.border_subtle())
            .text_size(px(theme.font_ui_secondary()))
            .text_color(theme.token_text_dim())
            .whitespace_nowrap()
            .overflow_hidden()
            .child(
                div()
                    .id("conversation-status-location")
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_color(theme.token_text_muted())
                    .child(location),
            )
            .child(right)
    }

    fn render_lens_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current();
        // The three lenses are one segmented switch: the kit's tab bar owns
        // the selected pill, and the index is the lens the book holds.
        let lens_index = match self.current_lens() {
            Lens::Chat => 0,
            Lens::Terminal => 1,
            Lens::Changes => 2,
        };
        let focused_pane_id = self.focused_pane.clone();
        let focused_pane_id_zoom = focused_pane_id.clone();
        div()
            .id("lens-toggle")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .h(px(40.0))
            .px_3()
            // The row is as wide as the column: a capped width left the rest
            // of the line unpainted, which showed the window's own background
            // as a black strip beside the buttons.
            .w_full()
            .bg(theme.surface_panel)
            .child(
                TabBar::new("lens-switch")
                    .segmented()
                    .selected_index(lens_index)
                    .child(
                        Tab::new()
                            .debug_selector(|| "lens-chat".into())
                            .label("Chat")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_lens(Lens::Chat, cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        Tab::new()
                            .debug_selector(|| "lens-terminal".into())
                            .label("Terminal")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_lens(Lens::Terminal, cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        Tab::new()
                            .debug_selector(|| "lens-changes".into())
                            .label("Changes")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_lens(Lens::Changes, cx);
                                cx.notify();
                            })),
                    ),
            )
            .child(
                Button::new("pane-zoom")
                    .label("Zoom")
                    .secondary()
                    .small()
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
                Button::new("pane-close")
                    .label("Close")
                    .secondary()
                    .small()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(pane_id) = focused_pane_id.clone() {
                            this.call_route("/api/pane/close", &json!({ "pane_id": pane_id }));
                        }
                        this.workspace_sidebar
                            .update(cx, |view, _cx| view.refresh());
                        cx.notify();
                    })),
            )
    }

    /// The focused workspace's tabs, above the lens content (TabStrip
    /// behavior): clicking a tab focuses its first pane.
    fn render_tab_strip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current();
        let (tabs, ws_id, ws_agent) = {
            let workspaces = &self.cached_workspaces;
            let focused = workspaces
                .iter()
                .find(|w| w.focused)
                .or_else(|| workspaces.first());
            match focused {
                Some(ws) => {
                    let tabs = if self.cached_tabs_workspace_id.as_deref() == Some(ws.id.as_str()) {
                        self.cached_tabs.clone()
                    } else {
                        self.workspace_sidebar
                            .update(cx, |view, _cx| view.tabs(&ws.id))
                    };
                    (tabs, ws.id.clone(), ws.agent.clone())
                }
                None => (Vec::new(), String::new(), String::new()),
            }
        };
        let scale = theme.density_scale();
        let ws_id_for_new = ws_id.clone();
        let ws_agent_for_new = ws_agent.clone();
        let next_tab_number = tabs.iter().map(|tab| tab.number).max().unwrap_or(0) + 1;
        let new_tab_label = format!("Tab {next_tab_number}");
        // The kit's tab bar owns the strip: each tab carries its own rename,
        // split and close buttons as its suffix, and the bar's suffix is the
        // new-tab button. A tab with no focused pane selects nothing, so the
        // index is the sentinel the bar reads as "no selection".
        let selected_index = tabs
            .iter()
            .position(|tab| tab.focused)
            .unwrap_or(usize::MAX);
        TabBar::new("tab-strip")
            .small()
            .bg(theme.surface_panel)
            .px_3()
            .py_1()
            .selected_index(selected_index)
            .children(tabs.iter().map(|tab| {
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
                let label = if editing {
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
                let kit_tab = Tab::new().label(label).text_size(px(12.0 * scale)).suffix(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .child(
                            Button::new(format!("tab-edit-{tab_id}"))
                                .label("✎")
                                .text()
                                .xsmall()
                                .px_1()
                                .text_size(px(12.0 * scale))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.editing_tab =
                                        Some((tab_id_for_edit.clone(), tab_label_for_edit.clone()));
                                    this.focus_handle.focus(window, cx);
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new(format!("tab-split-{tab_id}"))
                                .label("◫")
                                .text()
                                .xsmall()
                                .px_1()
                                .text_size(px(12.0 * scale))
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
                            Button::new(format!("tab-close-{tab_id}"))
                                .label("×")
                                .text()
                                .xsmall()
                                .px_1()
                                .text_size(px(12.0 * scale))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.call_route("/api/tab/close", &json!({ "tab_id": tab_id }));
                                    this.workspace_sidebar
                                        .update(cx, |view, _cx| view.refresh());
                                    cx.notify();
                                })),
                        ),
                );
                if let Some(pane_id) = pane_id {
                    kit_tab.on_click(cx.listener(move |this, _, _, cx| {
                        let sidebar = this.workspace_sidebar.clone();
                        sidebar.update(cx, |view, _cx| view.focus_pane(&pane_id));
                        cx.notify();
                    }))
                } else {
                    kit_tab
                }
            }))
            .suffix(
                Button::new("tab-new")
                    .label("+")
                    .secondary()
                    .small()
                    .text_size(px(12.0 * scale))
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

    /// Recompute the cached shell state from the sidebar snapshot. Called on
    /// session events and at startup, never from `render`, so the frame path
    /// never clones the whole session JSON or rebuilds these collections.
    fn refresh_shell_cache(&mut self, cx: &mut App) {
        self.cached_sidebar_fingerprint = self
            .workspace_sidebar
            .read(cx)
            .snapshot_fingerprint()
            .map(str::to_string);
        let (focused, live, agent_label, has_workspaces, split_panes, workspaces, tabs, tabs_ws_id) =
            self.workspace_sidebar.update(cx, |view, _cx| {
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
                let agent_label = focused
                    .as_deref()
                    .and_then(|pane_id| {
                        snapshot.as_ref().and_then(|snapshot| {
                            snapshot
                                .get("panes")
                                .and_then(Value::as_array)
                                .and_then(|panes| {
                                    panes.iter().find(|pane| {
                                        pane.get("pane_id").and_then(Value::as_str) == Some(pane_id)
                                    })
                                })
                                .and_then(|pane| pane.get("display_agent").and_then(Value::as_str))
                                .map(str::to_string)
                        })
                    })
                    .unwrap_or_else(|| "the agent".to_string());
                let workspaces = view.workspaces();
                let has_workspaces = !workspaces.is_empty();
                let focused_ws = workspaces
                    .iter()
                    .find(|w| w.focused)
                    .or_else(|| workspaces.first())
                    .cloned();
                let (split_panes, tabs, tabs_ws_id) = match focused_ws {
                    Some(ws) => {
                        let tabs = view.tabs(&ws.id);
                        let focused_tab = tabs.iter().find(|t| t.focused).or_else(|| tabs.first());
                        let split_panes = match focused_tab {
                            Some(tab) if tab.layout != "single" && tab.pane_ids.len() >= 2 => {
                                tab.pane_ids.clone()
                            }
                            _ => Vec::new(),
                        };
                        (split_panes, tabs, Some(ws.id))
                    }
                    None => (Vec::new(), Vec::new(), None),
                };
                (
                    focused,
                    live,
                    agent_label,
                    has_workspaces,
                    split_panes,
                    workspaces,
                    tabs,
                    tabs_ws_id,
                )
            });
        self.focused_pane = focused;
        self.cached_live_panes = live;
        self.cached_agent_label = agent_label;
        self.cached_has_workspaces = has_workspaces;
        self.cached_split_panes = split_panes;
        self.cached_workspaces = workspaces;
        self.cached_tabs = tabs;
        self.cached_tabs_workspace_id = tabs_ws_id;
    }
}

impl Render for OrionApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let window_w = f32::from(window.bounds().size.width);
        let layout = ShellLayout::for_width(window_w);
        let sidebar_w = layout.sidebar_width_px;
        self.workspace_sidebar
            .update(cx, |view, _cx| view.set_rail(layout.rail));

        // Shell state is cached from session events (see `refresh_shell_cache`);
        // the frame path only reads it. Panes that left the Session do not keep
        // a lens choice alive.
        self.lens_book.retain_panes(&self.cached_live_panes);
        if let Some(pane_id) = self.focused_pane.clone() {
            self.attach_pane(cx, &pane_id);
            // The terminal's placeholder names the agent: "Starting Claude
            // Code… (2.4s)" says far more than a blank grid.
            let agent_label = self.cached_agent_label.clone();
            self.terminal_pane
                .update(cx, |view, _cx| view.set_agent_label(&agent_label));
        }

        // The Changes lens' 2s polls run only while it is active; views are
        // preserved between lens switches (entities stay mounted above).
        let prefs = self.settings.update(cx, |view, _cx| view.prefs().clone());
        if self.last_render_prefs.as_ref() != Some(&prefs) {
            Theme::set_current(Theme::for_mode(prefs.theme, prefs.density));
            crate::theme::sync_gpui_kit(cx);
            self.last_render_prefs = Some(prefs.clone());
        }
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
        let has_workspaces = self.cached_has_workspaces;
        let split_panes = &self.cached_split_panes;
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

        // The reference layout needs the window width and the split state, so
        // the lens visibility is decided here rather than before the split is
        // resolved. The review column reads the same Changes view as the
        // classic lens, so that view polls in either mode.
        let reference = reference_layout(window_w);
        let review_visible = review_column_visible(
            self.review_open,
            reference.collapse_review,
            has_workspaces,
            split_second.is_some(),
        );
        let changes_active = self.current_lens() == Lens::Changes;
        self.file_tree
            .update(cx, |view, cx| view.set_visible(cx, changes_active));
        self.changes_view.update(cx, |view, _cx| {
            view.set_visible(changes_active || review_visible)
        });
        // The Chat lens reads the agent's whole transcript on its poll, so it
        // only reads it while it is the lens on screen. In the three-panel
        // layout the Changes lens keeps the conversation in the middle column,
        // so the chat is on screen there too and must keep polling.
        let chat_active = self.current_lens() == Lens::Chat;
        let chat_on_screen = chat_active || (review_visible && changes_active);
        self.chat_view
            .update(cx, |view, cx| view.set_visible(cx, chat_on_screen));

        let lens_body = if review_visible {
            // The review column owns the diff here; the middle column keeps
            // the conversation, or the live terminal, as its lens.
            match self.current_lens() {
                Lens::Terminal => div()
                    .id("main-content")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .child(self.terminal_pane.clone()),
                Lens::Chat | Lens::Changes => div()
                    .id("main-content")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .child(self.chat_view.clone()),
            }
        } else if !has_workspaces {
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
                    Button::new("empty-new-workspace")
                        .label("New Workspace")
                        .primary()
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
                        .min_w(px(0.0))
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
                                    .min_w(px(0.0))
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
                            .min_w(px(0.0))
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
                                        .min_w(px(0.0))
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
        let theme = Theme::current();
        let mut panels = div()
            .id("app-panels")
            .flex()
            .flex_row()
            .flex_1()
            .min_h(px(0.0))
            .p(px(theme.space_2()))
            .gap(px(theme.panel_gap()));

        if review_visible {
            // Three persistent columns: sessions, conversation, review. The
            // middle column keeps the same header and the same live lens views;
            // only the switcher and the status bar are new.
            panels = panels
                .children(sidebar_visible(self.sidebar_open).then(|| {
                    div()
                        .id("sidebar")
                        .flex()
                        .flex_col()
                        .w(px(reference.sidebar_w))
                        .min_w(px(0.0))
                        .flex_shrink_0()
                        .bg(theme.panel_bg())
                        .rounded(px(theme.radius_panel()))
                        .child(self.workspace_sidebar.clone())
                        .into_any_element()
                }))
                .child(
                    div()
                        .id("conversation-column")
                        .flex()
                        .flex_col()
                        .w(px(reference.conversation_w))
                        .flex_shrink_0()
                        .min_w(px(0.0))
                        .bg(theme.panel_bg())
                        .rounded(px(theme.radius_panel()))
                        .child(self.render_middle_switcher(cx))
                        .children(notice.map(|message| {
                            div()
                                .id("command-notice")
                                .px_3()
                                .py_1()
                                .bg(Theme::current().surface_panel)
                                .text_size(px(11.0))
                                .text_color(Theme::current().indicator_blocked)
                                .child(message)
                                .into_any_element()
                        }))
                        .child(lens_body)
                        .child(self.render_conversation_status(cx)),
                )
                .child(
                    div()
                        .id("review-column")
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w(px(0.0))
                        .bg(theme.panel_bg())
                        .rounded(px(theme.radius_panel()))
                        .child(self.review_pane.clone()),
                );
        } else {
            // The classic two-column shell, unchanged: narrow windows and
            // split panes keep the full three-lens canvas.
            panels = panels
                .children(sidebar_visible(self.sidebar_open).then(|| {
                    div()
                        .id("sidebar")
                        .flex()
                        .flex_col()
                        .w(px(sidebar_w))
                        .min_w(px(0.0))
                        .bg(theme.panel_bg())
                        .rounded(px(theme.radius_panel()))
                        .child(self.workspace_sidebar.clone())
                        .into_any_element()
                }))
                .child(
                    div()
                        .id("main-column")
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w(px(0.0))
                        .bg(theme.panel_bg())
                        .rounded(px(theme.radius_panel()))
                        // The tab strip and lens switcher act on a selected pane;
                        // with no workspace they would be dead chrome.
                        .children(
                            has_workspaces.then(|| self.render_lens_toggle(cx).into_any_element()),
                        )
                        .children(
                            has_workspaces.then(|| self.render_tab_strip(cx).into_any_element()),
                        )
                        .children(notice.map(|message| {
                            div()
                                .id("command-notice")
                                .px_3()
                                .py_1()
                                // The notice sits on the column's own background:
                                // without one it shows the window's black.
                                .bg(Theme::current().surface_panel)
                                .text_size(px(11.0))
                                .text_color(Theme::current().indicator_blocked)
                                .child(message)
                                .into_any_element()
                        }))
                        .child(lens_body),
                );
        }

        let root = div()
            .id("app")
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.window_bg())
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, ev: &gpui_kit::KeyDownEvent, _, cx| {
                this.handle_editing_key(ev, cx)
            }))
            .child(
                div()
                    .id("app-titlebar")
                    .flex()
                    .flex_row()
                    .items_center()
                    .w_full()
                    .h(px(40.0))
                    .flex_shrink_0()
                    .bg(theme.window_bg())
                    .child(self.render_header(window, cx)),
            )
            .child(panels)
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
            );
        root
    }
}

pub fn run() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            gpui_kit::open_window(
                WindowOptions {
                    titlebar: Some(TitlebarOptions {
                        title: Some("Orion".into()),
                        appears_transparent: true,
                        ..Default::default()
                    }),
                    window_bounds: Some(WindowBounds::centered(
                        Size::new(px(1600.0), px(900.0)),
                        cx,
                    )),
                    ..Default::default()
                },
                cx,
                |_window, cx| cx.new(OrionApp::new),
            )
            .expect("Failed to open window");
        });
    // The startup timeline, when the reader asked for one.
    orion_core::trace::print_report();
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

    crate::orion_test!(
        fn the_pump_feeds_chunks_and_notifies_only_on_revision_change(
            cx: &mut gpui_kit::TestAppContext,
        ) {
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
    );

    crate::orion_test!(
        fn a_flood_of_output_asks_for_one_frame_per_frame(cx: &mut gpui_kit::TestAppContext) {
            // The pump waits out the frame budget, so it has a task outstanding:
            // parking is allowed rather than a panic.
            cx.background_executor.allow_parking();
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

            // A flood: chunks arrive over time, so the pump sees them one after
            // another rather than as one batch.
            let started = Instant::now();
            for i in 0..40 {
                tx.send(format!("chunk {i}\r\n"))
                    .expect("the pump is listening");
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            drop(tx);
            // The pump's last tick lands within a frame of the channel closing.
            let deadline = Instant::now() + std::time::Duration::from_secs(5);
            while Instant::now() < deadline {
                cx.run_until_parked();
                let revision = cx.read(|app| view.read(app).revision());
                if revision >= 40 {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let elapsed = started.elapsed();
            let notifies = notifies.load(Ordering::SeqCst);

            assert!(
                cx.read(|app| view.read(app).revision()) >= 40,
                "every chunk reached the grid"
            );
            // One repaint per frame: the renderer is never asked for more frames
            // than the frame rate allows.
            let budget = (elapsed.as_millis() as usize / FRAME_BUDGET.as_millis() as usize) + 2;
            assert!(
                notifies <= budget,
                "the pump asked for {notifies} frames in {elapsed:?}, the budget is {budget}"
            );
        }
    );

    #[::core::prelude::v1::test]
    fn the_pty_channel_blocks_the_producer_at_capacity() {
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

    crate::orion_test!(
        fn a_stale_pump_stops_feeding_when_the_generation_bumps(cx: &mut gpui_kit::TestAppContext) {
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
    );

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

    // The split watches the pane: it takes the output, never the size.
    crate::orion_test!(
        fn the_split_watches_the_pane_without_taking_its_size(cx: &mut gpui_kit::TestAppContext) {
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
    );

    // The split lets go of the pane: its attachment goes and its pump stops.
    crate::orion_test!(
        fn detaching_the_split_stops_its_pump(cx: &mut gpui_kit::TestAppContext) {
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
    );

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

    // The agent's PTY keeps the size it had when the split opens.
    crate::orion_test!(
        fn the_split_attach_leaves_the_pane_the_size_it_had(cx: &mut gpui_kit::TestAppContext) {
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
    );

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

    // The split leaves the runtime's connection interactive.
    //
    // The role belongs to the connection, not to the pane: a split that sent
    // `observe` would turn the main pane read-only on the daemon path.
    crate::orion_test!(
        fn the_split_keeps_the_runtime_connection_interactive(cx: &mut gpui_kit::TestAppContext) {
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
    );
}
