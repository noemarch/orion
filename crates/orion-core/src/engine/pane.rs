//! One running agent pane.
//!
//! A pane owns three things a client can see: the PTY (the agent), the grid parsed from its
//! output, and the set of connections attached to it. Output has exactly one path — the PTY
//! reader thread hands decoded text to [`Pane`]'s callback, which feeds the grid, recomputes the
//! agent's status, and forwards the bytes to every attached client. The chat lens and the
//! approval card read the same grid, so they can never disagree with the terminal.
//!
//! A pane is always an agent: [`Pane::spawn`] takes a [`LaunchSpec`] resolved from one of Orion's
//! agent kinds. There is no shell variant.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::agents::LaunchSpec;
use crate::error::{OrionError, Result};
use crate::prompt::{self, DetectedPrompt};
use crate::protocol::{AgentKind, AgentStatus, TerminalSize};
use crate::pty::PtySession;
use crate::runtime::PaneHandle;
use crate::screen::ScreenState;

/// A frame sink: one connected client's delivery function. Cheap to clone and safe to call from
/// the PTY reader thread, which is where most frames are produced.
pub type FrameSink = Arc<dyn Fn(Value) + Send + Sync>;

/// What a connection may do with a shared pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Types, resizes and answers prompts.
    Interact,
    /// Only watches: the grid is the interact client's, and this connection adopts it.
    Observe,
}

impl Role {
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("observe") => Self::Observe,
            _ => Self::Interact,
        }
    }
}

/// What a pane tells its owner outside the per-connection stream.
#[derive(Debug, Clone)]
pub enum PaneEvent {
    /// The agent's state changed; `completion` is true when a turn just finished.
    Status {
        pane_id: String,
        status: AgentStatus,
        completion: bool,
    },
    /// The agent process ended.
    Exited { pane_id: String, code: Option<i32> },
}

/// Everything needed to start a pane.
#[derive(Debug, Clone)]
pub struct PaneConfig {
    pub id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub agent: AgentKind,
    pub label: String,
    pub cwd: PathBuf,
    /// The resolved launch. Production callers resolve it through [`LaunchSpec::for_agent`],
    /// which guarantees an identified agent CLI; tests may supply a stand-in.
    pub spec: LaunchSpec,
    /// The agent's own arguments, without the resume flags the spec adds: what a restart saves.
    pub args: Vec<String>,
    pub size: TerminalSize,
}

struct Attached {
    sink: FrameSink,
}

struct PaneState {
    screen: ScreenState,
    attached: HashMap<String, Attached>,
    status: AgentStatus,
    completion_seq: u64,
    state_change_seq: u64,
    /// How many output chunks the grid has consumed. A debounced detection snapshots it and
    /// discards its result if newer output arrived while the screen was being scanned.
    grid_seq: u64,
    prompt: Option<DetectedPrompt>,
    exit: Option<i32>,
}

/// A live agent pane.
pub struct Pane {
    pub id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub agent: AgentKind,
    label: Mutex<String>,
    pub cwd: PathBuf,
    pub args: Vec<String>,
    pub started_at: DateTime<Utc>,
    session: Arc<PtySession>,
    state: Arc<Mutex<PaneState>>,
}

impl Pane {
    /// Starts the agent and wires its output to the grid and its attached clients.
    pub fn spawn(
        config: PaneConfig,
        on_event: Arc<dyn Fn(PaneEvent) + Send + Sync>,
    ) -> Result<Self> {
        let pane_id = config.id.clone();
        let agent = config.agent;
        let state = Arc::new(Mutex::new(PaneState {
            screen: ScreenState::new(config.size.rows.max(1), config.size.cols.max(1)),
            attached: HashMap::new(),
            status: AgentStatus::Idle,
            completion_seq: 0,
            state_change_seq: 0,
            grid_seq: 0,
            prompt: None,
            exit: None,
        }));

        // Prompt detection scans the whole visible screen, so it must not run per PTY chunk under
        // the pane lock. The reader only signals this channel; the detector waits for output to go
        // quiet, then scans a snapshot outside the lock.
        let (detect_wake, detect_rx) = mpsc::sync_channel::<()>(1);

        let output_state = Arc::clone(&state);
        let output_id = pane_id.clone();
        let output_signal = detect_wake.clone();
        let on_text: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |text: String| {
            let sinks: Vec<FrameSink> = {
                let mut state = output_state.lock();
                state.screen.process(&text);
                state.grid_seq = state.grid_seq.wrapping_add(1);
                state
                    .attached
                    .values()
                    .map(|attached| Arc::clone(&attached.sink))
                    .collect()
            };
            // A full channel already has a wake pending: one signal per quiet window is enough.
            let _ = output_signal.try_send(());
            let data = json!({
                "type": "pty-data",
                "pane_id": output_id,
                "data": text,
            });
            for sink in sinks {
                (sink)(data.clone());
            }
        });

        let session =
            PtySession::spawn(&config.spec, Some(&config.cwd), &[], config.size, on_text)?;

        if agent_uses_grid_status(agent) {
            if let Err(error) = spawn_detector(
                pane_id.clone(),
                agent,
                Arc::clone(&state),
                detect_rx,
                Arc::clone(&on_event),
            ) {
                session.kill();
                return Err(error);
            }
        }

        let pane = Self {
            id: pane_id.clone(),
            workspace_id: config.workspace_id,
            tab_id: config.tab_id,
            agent,
            label: Mutex::new(config.label),
            cwd: config.cwd,
            args: config.args,
            started_at: Utc::now(),
            session: Arc::new(session),
            state,
        };

        // An exit watcher: `portable-pty` needs someone to reap the child, and every client must
        // hear that the pane ended even when nobody is attached.
        let watch_state = Arc::clone(&pane.state);
        let watch_session = Arc::clone(&pane.session);
        let watch_id = pane_id;
        let watch_events = Arc::clone(&on_event);
        // Back off from 25ms to a 400ms ceiling: an idle agent wakes this thread ~2.5x/s instead
        // of 4x/s, while a process that exits after the ceiling is reached is still reaped within
        // one 400ms interval. The schedule is 25, 50, 100, 200, 400, 400, ... ms.
        const MAX_EXIT_POLL: std::time::Duration = std::time::Duration::from_millis(400);
        std::thread::Builder::new()
            .name(format!("orion-pane-exit-{watch_id}"))
            .spawn(move || {
                let mut backoff = std::time::Duration::from_millis(25);
                loop {
                    match watch_session.try_wait() {
                        Ok(Some(code)) => {
                            // Flush the last screen before the exit state lands: a prompt drawn by
                            // the final chunk must still be reported.
                            detect_and_apply(&watch_state, &watch_id, agent, &watch_events);
                            let status = status_after_exit(code);
                            {
                                let mut state = watch_state.lock();
                                state.exit = Some(code);
                                state.status = status;
                                state.state_change_seq += 1;
                                if status == AgentStatus::Done {
                                    state.completion_seq += 1;
                                }
                            }
                            watch_events(PaneEvent::Status {
                                pane_id: watch_id.clone(),
                                status,
                                completion: status == AgentStatus::Done,
                            });
                            watch_events(PaneEvent::Exited {
                                pane_id: watch_id.clone(),
                                code: Some(code),
                            });
                            return;
                        }
                        Ok(None) => {
                            std::thread::sleep(backoff);
                            backoff = (backoff * 2).min(MAX_EXIT_POLL);
                        }
                        Err(_) => return,
                    }
                }
            })
            .map_err(|error| OrionError::Process(format!("failed to watch the agent: {error}")))?;

        Ok(pane)
    }

    pub fn pid(&self) -> Option<u32> {
        self.session.pid()
    }

    /// Returns a reference to the underlying PTY session.
    pub fn session(&self) -> &Arc<PtySession> {
        &self.session
    }

    /// Returns the pane's current working directory.
    pub fn cwd(&self) -> &PathBuf {
        &self.cwd
    }

    /// The label the sidebar and the chat's header show.
    pub fn label(&self) -> String {
        self.label.lock().clone()
    }

    /// When the agent started. A transcript older than this belongs to another pane.
    pub fn started(&self) -> std::time::SystemTime {
        self.started_at.into()
    }

    /// Sets the pane's label (the agent keeps its own name in its own process).
    pub fn rename(&self, label: &str) {
        *self.label.lock() = label.to_string();
    }

    /// Clears the terminal grid, keeping size and history policy. Every attached client repaints
    /// the now-blank screen instead of keeping what it drew.
    pub fn clear(&self) {
        let mut state = self.state.lock();
        state.screen.clear();
        let replay = state.screen.replay();
        for attached in state.attached.values() {
            (attached.sink)(json!({
                "type": "pty-data",
                "pane_id": self.id,
                "data": replay,
            }));
        }
    }

    pub fn revision(&self) -> u64 {
        self.state.lock().screen.revision()
    }

    pub fn status(&self) -> AgentStatus {
        self.state.lock().status
    }

    pub fn prompt(&self) -> Option<DetectedPrompt> {
        self.state.lock().prompt.clone()
    }

    pub fn size(&self) -> (u16, u16) {
        self.state.lock().screen.size()
    }

    pub fn exited(&self) -> Option<i32> {
        self.state.lock().exit
    }

    /// Attaches a connection: replays the screen, then streams every new byte to it.
    ///
    /// Replay and registration happen under one lock, so no output can slip between the two and
    /// be seen twice or lost. An interact attach owns the pane's grid unless it asked to keep
    /// the size it already had; an observe attach never resizes — it adopts what is there.
    pub fn attach(
        &self,
        connection: &str,
        sink: FrameSink,
        role: Role,
        cols: u16,
        rows: u16,
        keep_size: bool,
    ) -> Result<()> {
        let mut state = self.state.lock();
        if state.exit.is_some() {
            return Err(OrionError::Pane("the agent has exited".into()));
        }
        let (live_cols, live_rows) = state.screen.size();
        let mut grid = (live_cols, live_rows);
        if role == Role::Interact && !keep_size && cols > 0 && rows > 0 && (cols, rows) != grid {
            self.session.resize(TerminalSize {
                cols,
                rows,
                pixel_width: 0,
                pixel_height: 0,
            })?;
            state.screen.resize(rows, cols);
            grid = (cols, rows);
            let geometry = json!({
                "type": "pane-geometry",
                "pane_id": self.id,
                "cols": cols,
                "rows": rows,
            });
            for other in state.attached.values() {
                (other.sink)(geometry.clone());
            }
        }

        let replay = state.screen.replay();
        (sink)(json!({
            "type": "pty-data",
            "pane_id": self.id,
            "data": replay,
        }));
        (sink)(json!({
            "type": "pane-geometry",
            "pane_id": self.id,
            "cols": grid.0,
            "rows": grid.1,
        }));
        state.attached.insert(
            connection.to_string(),
            Attached {
                sink: Arc::clone(&sink),
            },
        );
        Ok(())
    }

    pub fn detach(&self, connection: &str) {
        self.state.lock().attached.remove(connection);
    }

    /// Drops every attachment a closed connection held.
    pub fn detach_all(&self, connection: &str) {
        self.detach(connection);
    }

    /// Writes client input to the agent.
    pub fn write(&self, data: &str) -> Result<()> {
        self.session.write(data.as_bytes())
    }

    /// Sends logical key chords (`ctrl+c`, `up`, `enter`, a literal character) as terminal bytes.
    pub fn keys(&self, keys: &[String]) -> Result<()> {
        let mut bytes = String::new();
        for key in keys {
            bytes.push_str(&encode_key(key));
        }
        if !bytes.is_empty() {
            self.session.write(bytes.as_bytes())?;
        }
        Ok(())
    }

    pub fn interrupt(&self) -> Result<()> {
        self.session.interrupt()
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        if cols == 0 || rows == 0 {
            return Ok(());
        }
        let mut state = self.state.lock();
        let (live_cols, live_rows) = state.screen.size();
        if (cols, rows) == (live_cols, live_rows) {
            return Ok(());
        }
        self.session.resize(TerminalSize {
            cols,
            rows,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        state.screen.resize(rows, cols);
        let geometry = json!({
            "type": "pane-geometry",
            "pane_id": self.id,
            "cols": cols,
            "rows": rows,
        });
        for attached in state.attached.values() {
            (attached.sink)(geometry.clone());
        }
        Ok(())
    }

    /// Answers the menu the agent is showing, by moving its cursor and pressing Enter.
    ///
    /// The answer is validated against a fresh read: a card that names a menu the pane no longer
    /// shows is refused as `prompt_changed` rather than typing a choice into whatever came next.
    pub fn answer_prompt(
        &self,
        prompt_id: &str,
        option_index: usize,
    ) -> std::result::Result<(), String> {
        self.answer_keys(prompt_id, None, option_index)
    }

    /// Answers with a typed line: move onto the custom row, type it, and submit.
    pub fn answer_prompt_custom(
        &self,
        prompt_id: &str,
        text: &str,
    ) -> std::result::Result<(), String> {
        self.answer_keys(prompt_id, Some(text), 0)
    }

    fn answer_keys(
        &self,
        prompt_id: &str,
        custom: Option<&str>,
        option_index: usize,
    ) -> std::result::Result<(), String> {
        let state = self.state.lock();
        let current = state
            .prompt
            .clone()
            .ok_or_else(|| "prompt_changed".to_string())?;
        if current.id != prompt_id {
            return Err("prompt_changed".to_string());
        }
        let keys = match custom {
            Some(text) => current
                .answer_custom_keys(text)
                .ok_or_else(|| "invalid_option".to_string())?,
            None => current
                .answer_keys(option_index)
                .ok_or_else(|| "invalid_option".to_string())?,
        };
        drop(state);
        self.session
            .write(keys.as_bytes())
            .map_err(|error| error.to_string())?;
        let mut state = self.state.lock();
        state.prompt = None;
        state.status = AgentStatus::Working;
        state.state_change_seq += 1;
        Ok(())
    }

    /// Marks a turn as started: the composer submitted a message.
    pub fn note_submit(&self) {
        let mut state = self.state.lock();
        state.status = AgentStatus::Working;
        state.state_change_seq += 1;
        state.prompt = None;
    }

    /// Applies the turn state the agent's own transcript reports, returning the transition to
    /// report as a pane event. omo and omp have no other status source: their grid never tells a
    /// turn from a redraw, and the transcript is read while a chat is open.
    pub fn note_transcript_status(&self, status: AgentStatus) -> Option<(AgentStatus, bool)> {
        apply_transcript_status(&mut self.state.lock(), status)
    }

    /// The visible screen, plain text.
    pub fn visible_text(&self) -> String {
        self.state.lock().screen.visible_text()
    }

    pub fn history(&self) -> Vec<String> {
        self.state.lock().screen.history()
    }

    /// A chat read: the recent rows, with soft-wrapped rows optionally rejoined.
    pub fn read(&self, lines: usize, unwrapped: bool) -> (String, bool, u64) {
        let mut state = self.state.lock();
        let (text, truncated) = state.screen.read_recent(lines, unwrapped);
        (text, truncated, state.screen.revision())
    }

    pub fn scroll_info(&self) -> Value {
        self.state.lock().screen.scroll_info()
    }

    /// Moves the shared viewport and tells attached clients to repaint.
    pub fn scroll_to(&self, offset_from_bottom: usize) -> Value {
        let mut state = self.state.lock();
        let replay = state.screen.scroll_to(offset_from_bottom);
        for attached in state.attached.values() {
            (attached.sink)(json!({
                "type": "pty-data",
                "pane_id": self.id,
                "data": replay,
            }));
        }
        state.screen.scroll_info()
    }

    pub fn find(&self, query: &str) -> Vec<Value> {
        self.state.lock().screen.find(query, 1024)
    }

    pub fn selection(
        &self,
        anchor_row: usize,
        anchor_col: usize,
        cursor_row: usize,
        cursor_col: usize,
    ) -> String {
        self.state
            .lock()
            .screen
            .selection(anchor_row, anchor_col, cursor_row, cursor_col)
    }

    /// The pane's entry in `SessionSnapshot.panes`.
    pub fn info(&self, focused: bool) -> Value {
        let label = self.label();
        let state = self.state.lock();
        json!({
            "pane_id": self.id,
            "workspace_id": self.workspace_id,
            "tab_id": self.tab_id,
            "agent": self.agent.default_command(),
            "display_agent": self.agent.display_name(),
            "label": label,
            "title": label,
            "cwd": self.cwd.to_string_lossy(),
            "foreground_cwd": self.cwd.to_string_lossy(),
            "agent_status": state.status.as_wire(),
            "focused": focused,
            "revision": state.screen.revision(),
            "terminal_id": self.id,
            "scroll": state.screen.scroll_info(),
        })
    }

    /// The pane's entry in `SessionSnapshot.agents`.
    pub fn agent_info(&self, focused: bool) -> Value {
        let label = self.label();
        let state = self.state.lock();
        json!({
            "pane_id": self.id,
            "workspace_id": self.workspace_id,
            "tab_id": self.tab_id,
            "agent": self.agent.default_command(),
            "display_agent": self.agent.display_name(),
            "name": label,
            "title": label,
            "cwd": self.cwd.to_string_lossy(),
            "agent_status": state.status.as_wire(),
            "focused": focused,
            "revision": state.screen.revision(),
            "terminal_id": self.id,
            "completion_seq": state.completion_seq,
            "state_change_seq": state.state_change_seq,
            "interactive_ready": true,
        })
    }

    pub fn wait_exit(&self) -> Option<i32> {
        self.session.try_wait().ok().flatten()
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        // Closing a pane closes the agent's whole tree: tooling a turn started must not outlive it.
        self.session.kill();
    }
}

/// Applies a turn state the agent's own transcript reported. The transcript says whether a turn
/// is running, not whether the pane rests: its `Working` is the pane's, and its `Idle` ends a run
/// as `Done` once, the way a grid-driven turn ends. Returns the transition to report.
fn apply_transcript_status(
    state: &mut PaneState,
    status: AgentStatus,
) -> Option<(AgentStatus, bool)> {
    let next = match status {
        AgentStatus::Working => AgentStatus::Working,
        AgentStatus::Idle if state.status == AgentStatus::Working => AgentStatus::Done,
        _ => return None,
    };
    if next == state.status {
        return None;
    }
    state.status = next;
    state.state_change_seq += 1;
    let completion = next == AgentStatus::Done;
    if completion {
        state.completion_seq += 1;
    }
    Some((next, completion))
}

/// How long a pane's output must stay quiet before the grid is scanned. Coalesces a burst of PTY
/// chunks into one scan, and lands within a frame or two of the agent going idle.
const DETECT_QUIET: Duration = Duration::from_millis(120);

/// Whether this agent's turn state can be read from its grid.
fn agent_uses_grid_status(agent: AgentKind) -> bool {
    !matches!(
        agent,
        AgentKind::Omo
            | AgentKind::Omp
            | AgentKind::Pi
            | AgentKind::Hermes
            | AgentKind::Grok
            | AgentKind::Cursor
    )
}

/// Follows one pane's output and scans the grid once it goes quiet, off the PTY reader thread.
fn spawn_detector(
    pane_id: String,
    agent: AgentKind,
    state: Arc<Mutex<PaneState>>,
    wake: mpsc::Receiver<()>,
    on_event: Arc<dyn Fn(PaneEvent) + Send + Sync>,
) -> Result<()> {
    std::thread::Builder::new()
        .name(format!("orion-pane-detect-{pane_id}"))
        .spawn(move || loop {
            if wake.recv().is_err() {
                return;
            }
            loop {
                match wake.recv_timeout(DETECT_QUIET) {
                    Ok(()) => continue,
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        // The pane is shutting down: scan the last screen once, then stop.
                        detect_and_apply(&state, &pane_id, agent, &on_event);
                        return;
                    }
                }
            }
            detect_and_apply(&state, &pane_id, agent, &on_event);
        })
        .map(|_| ())
        .map_err(|error| OrionError::Process(format!("failed to watch for prompts: {error}")))
}

/// One detection pass: snapshot the visible screen under the lock, scan it outside, then apply
/// only if the grid did not change while the scan ran. The probe describes one screen, so a newer
/// chunk is the only thing that invalidates it; the current status is read again under the lock.
fn detect_and_apply(
    state: &Mutex<PaneState>,
    pane_id: &str,
    agent: AgentKind,
    on_event: &Arc<dyn Fn(PaneEvent) + Send + Sync>,
) {
    let (visible, grid_seq) = {
        let state = state.lock();
        (state.screen.visible_text(), state.grid_seq)
    };
    let Some(probe) = probe_status(&visible, agent) else {
        return;
    };
    let mut state = state.lock();
    if state.grid_seq != grid_seq {
        return;
    }
    apply_probe(&mut state, probe, pane_id, on_event);
}

/// What one screen scan found, before it is reconciled with the pane's current state.
struct StatusProbe {
    prompt: Option<DetectedPrompt>,
    blocked: bool,
    working: bool,
}

/// Reads the grid for a prompt or a turn in progress. `None` means this agent reports its turn
/// through its transcript, not its screen.
fn probe_status(visible: &str, agent: AgentKind) -> Option<StatusProbe> {
    if !agent_uses_grid_status(agent) {
        return None;
    }
    let prompt = match agent {
        AgentKind::Claude => prompt::detect_claude(visible),
        AgentKind::Codex => prompt::detect_codex(visible),
        _ => None,
    };
    let blocked =
        prompt.is_some() || (agent == AgentKind::Codex && prompt::codex_queued_count(visible) > 0);
    let working = !blocked && agent == AgentKind::Claude && prompt::claude_is_working(visible);
    Some(StatusProbe {
        prompt,
        blocked,
        working,
    })
}

/// Reconciles one scan with the pane's state and reports a transition once.
fn apply_probe(
    state: &mut PaneState,
    probe: StatusProbe,
    pane_id: &str,
    on_event: &Arc<dyn Fn(PaneEvent) + Send + Sync>,
) {
    let previous = state.status;
    let next = if probe.blocked {
        AgentStatus::Blocked
    } else if probe.working {
        AgentStatus::Working
    } else if previous == AgentStatus::Working {
        AgentStatus::Done
    } else {
        previous
    };
    state.prompt = probe.prompt;
    if next == previous {
        return;
    }
    state.status = next;
    state.state_change_seq += 1;
    let completion = next == AgentStatus::Done;
    if completion {
        state.completion_seq += 1;
    }
    on_event(PaneEvent::Status {
        pane_id: pane_id.to_string(),
        status: next,
        completion,
    });
}

#[cfg(test)]
fn update_status(
    state: &mut PaneState,
    pane_id: &str,
    agent: AgentKind,
    on_event: &Arc<dyn Fn(PaneEvent) + Send + Sync>,
) {
    let visible = state.screen.visible_text();
    if let Some(probe) = probe_status(&visible, agent) {
        apply_probe(state, probe, pane_id, on_event);
    }
}

/// A pane whose process ended is done when it exited cleanly and in error when
/// it did not — the only state a PTY can report after its agent is gone.
fn status_after_exit(code: i32) -> AgentStatus {
    if code == 0 {
        AgentStatus::Done
    } else {
        AgentStatus::Error
    }
}

/// Encodes one logical key chord into the bytes a terminal receives.
///
/// The encoding lives with the emulator, so the shell and the pane agree on
/// what a chord means.
fn encode_key(key: &str) -> String {
    crate::terminal_engine::encode_chord(key).unwrap_or_default()
}

/// The path a pane was started in, for a caller that kept only the pane id.
pub fn cwd_of(pane: &Pane) -> &Path {
    &pane.cwd
}

/// The pane answers the shell's [`PaneHandle`] questions by doing exactly what its inherent
/// methods do: a pane in another process can answer them the same way later, so the terminal
/// lens keeps one code path.
impl PaneHandle for Pane {
    fn id(&self) -> &str {
        &self.id
    }

    fn cwd(&self) -> &Path {
        &self.cwd
    }

    fn write(&self, data: &str) -> Result<()> {
        Pane::write(self, data)
    }

    fn keys(&self, keys: &[String]) -> Result<()> {
        Pane::keys(self, keys)
    }

    fn interrupt(&self) -> Result<()> {
        Pane::interrupt(self)
    }

    fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        Pane::resize(self, cols, rows)
    }

    fn clear(&self) {
        Pane::clear(self)
    }

    fn status(&self) -> AgentStatus {
        Pane::status(self)
    }

    fn prompt(&self) -> Option<DetectedPrompt> {
        Pane::prompt(self)
    }

    fn attach(
        &self,
        connection: &str,
        sink: FrameSink,
        role: Role,
        cols: u16,
        rows: u16,
        keep_size: bool,
    ) -> Result<()> {
        Pane::attach(self, connection, sink, role, cols, rows, keep_size)
    }

    fn detach(&self, connection: &str) {
        Pane::detach(self, connection)
    }

    fn detach_all(&self, connection: &str) {
        Pane::detach_all(self, connection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(status: AgentStatus) -> PaneState {
        PaneState {
            screen: ScreenState::new(24, 80),
            attached: HashMap::new(),
            status,
            completion_seq: 0,
            state_change_seq: 0,
            grid_seq: 0,
            prompt: None,
            exit: None,
        }
    }

    #[test]
    fn keys_encode_like_a_terminal() {
        assert_eq!(encode_key("enter"), "\r");
        assert_eq!(encode_key("ctrl+c"), "\x03");
        assert_eq!(encode_key("ctrl+d"), "\x04");
        assert_eq!(encode_key("up"), "\x1b[A");
        assert_eq!(encode_key("shift+tab"), "\x1b[Z");
        assert_eq!(encode_key("alt+x"), "\x1bx");
        assert_eq!(encode_key("F5"), "\x1b[15~");
        assert_eq!(encode_key("a"), "a");
        assert_eq!(encode_key("+"), "+");
        assert_eq!(encode_key("plus"), "+");
        assert_eq!(encode_key("space"), " ");
    }

    #[test]
    fn an_exit_reports_done_only_after_a_clean_run() {
        assert_eq!(status_after_exit(0), AgentStatus::Done);
        assert_eq!(status_after_exit(1), AgentStatus::Error);
        assert_eq!(status_after_exit(-1), AgentStatus::Error);
    }

    #[test]
    fn a_transcript_turn_ends_as_done_once() {
        let mut idle = state(AgentStatus::Idle);
        // an already-finished session read at open is not news
        assert_eq!(apply_transcript_status(&mut idle, AgentStatus::Idle), None);
        assert_eq!(idle.status, AgentStatus::Idle);

        let started = apply_transcript_status(&mut idle, AgentStatus::Working);
        assert_eq!(started, Some((AgentStatus::Working, false)));
        assert_eq!(idle.status, AgentStatus::Working);
        assert_eq!(idle.state_change_seq, 1);
        // the same turn polled again says nothing new
        assert_eq!(
            apply_transcript_status(&mut idle, AgentStatus::Working),
            None
        );

        let ended = apply_transcript_status(&mut idle, AgentStatus::Idle);
        assert_eq!(ended, Some((AgentStatus::Done, true)));
        assert_eq!(idle.status, AgentStatus::Done);
        assert_eq!(idle.completion_seq, 1);
        assert_eq!(apply_transcript_status(&mut idle, AgentStatus::Idle), None);
    }

    #[test]
    fn the_grid_does_not_guess_a_transcript_driven_turn() {
        let mut state = state(AgentStatus::Working);
        let events: Arc<dyn Fn(PaneEvent) + Send + Sync> = Arc::new(|_| {});
        update_status(&mut state, "p1", AgentKind::Omo, &events);
        assert_eq!(state.status, AgentStatus::Working);
        update_status(&mut state, "p1", AgentKind::Omp, &events);
        assert_eq!(state.status, AgentStatus::Working);
        update_status(&mut state, "p1", AgentKind::Pi, &events);
        assert_eq!(state.status, AgentStatus::Working);
        update_status(&mut state, "p1", AgentKind::Hermes, &events);
        assert_eq!(state.status, AgentStatus::Working);
        update_status(&mut state, "p1", AgentKind::Grok, &events);
        assert_eq!(state.status, AgentStatus::Working);
        update_status(&mut state, "p1", AgentKind::Cursor, &events);
        assert_eq!(state.status, AgentStatus::Working);
    }
}
