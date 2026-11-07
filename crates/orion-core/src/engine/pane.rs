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
use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::agents::LaunchSpec;
use crate::error::{OrionError, Result};
use crate::prompt::{self, DetectedPrompt};
use crate::protocol::{AgentKind, AgentStatus, TerminalSize};
use crate::pty::PtySession;
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
        let state = Arc::new(Mutex::new(PaneState {
            screen: ScreenState::new(config.size.rows.max(1), config.size.cols.max(1)),
            attached: HashMap::new(),
            status: AgentStatus::Idle,
            completion_seq: 0,
            state_change_seq: 0,
            prompt: None,
            exit: None,
        }));

        let output_state = Arc::clone(&state);
        let output_id = pane_id.clone();
        let output_agent = config.agent;
        let output_events = Arc::clone(&on_event);
        let on_text: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |text: String| {
            let mut state = output_state.lock();
            state.screen.process(&text);
            update_status(&mut state, &output_id, output_agent, &output_events);
            let data = json!({
                "type": "pty-data",
                "pane_id": output_id,
                "data": text,
            });
            for attached in state.attached.values() {
                (attached.sink)(data.clone());
            }
        });

        let session =
            PtySession::spawn(&config.spec, Some(&config.cwd), &[], config.size, on_text)?;

        let pane = Self {
            id: pane_id.clone(),
            workspace_id: config.workspace_id,
            tab_id: config.tab_id,
            agent: config.agent,
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
        std::thread::Builder::new()
            .name(format!("orion-pane-exit-{watch_id}"))
            .spawn(move || loop {
                match watch_session.try_wait() {
                    Ok(Some(code)) => {
                        {
                            let mut state = watch_state.lock();
                            state.exit = Some(code);
                        }
                        watch_events(PaneEvent::Exited {
                            pane_id: watch_id.clone(),
                            code: Some(code),
                        });
                        return;
                    }
                    Ok(None) => std::thread::sleep(std::time::Duration::from_millis(250)),
                    Err(_) => return,
                }
            })
            .map_err(|error| OrionError::Process(format!("failed to watch the agent: {error}")))?;

        Ok(pane)
    }

    pub fn pid(&self) -> Option<u32> {
        self.session.pid()
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

    /// The visible screen, plain text.
    pub fn visible_text(&self) -> String {
        self.state.lock().screen.visible_text()
    }

    pub fn history(&self) -> Vec<String> {
        self.state.lock().screen.history()
    }

    /// A chat read: the recent rows, with soft-wrapped rows optionally rejoined.
    pub fn read(&self, lines: usize, unwrapped: bool) -> (String, bool, u64) {
        let state = self.state.lock();
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

/// Recomputes the agent status after a grid change, and reports a transition once.
fn update_status(
    state: &mut PaneState,
    pane_id: &str,
    agent: AgentKind,
    on_event: &Arc<dyn Fn(PaneEvent) + Send + Sync>,
) {
    let visible = state.screen.visible_text();
    let menu = match agent {
        AgentKind::Claude => prompt::detect_claude(&visible),
        AgentKind::Codex => prompt::detect_codex(&visible),
        _ => None,
    };
    // A collapsed Codex queue shows only a count: it still blocks, even though only the
    // rollout names the question.
    let blocked =
        menu.is_some() || (agent == AgentKind::Codex && prompt::codex_queued_count(&visible) > 0);
    let working = !blocked
        && match agent {
            AgentKind::Claude => prompt::claude_is_working(&visible),
            _ => false,
        };
    let previous = state.status;
    let next = if blocked {
        AgentStatus::Blocked
    } else if working {
        AgentStatus::Working
    } else if previous == AgentStatus::Working {
        AgentStatus::Done
    } else {
        previous
    };
    state.prompt = menu;
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

/// Encodes one logical key chord into the bytes a terminal receives.
fn encode_key(key: &str) -> String {
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut name = key;
    loop {
        let lowered = name.to_ascii_lowercase();
        if let Some(rest) = lowered.strip_prefix("ctrl+") {
            ctrl = true;
            name = &name[name.len() - rest.len()..];
        } else if let Some(rest) = lowered.strip_prefix("alt+") {
            alt = true;
            name = &name[name.len() - rest.len()..];
        } else if let Some(rest) = lowered.strip_prefix("shift+") {
            shift = true;
            name = &name[name.len() - rest.len()..];
        } else {
            break;
        }
    }

    let modifier = 1 + i32::from(shift) + 2 * i32::from(alt) + 4 * i32::from(ctrl);
    let lowered = name.to_ascii_lowercase();
    // Shift+Tab is its own sequence (CSI Z), not a modified Tab.
    if lowered == "tab" && shift && !ctrl && !alt {
        return "\x1b[Z".to_string();
    }
    let special = match lowered.as_str() {
        "up" => Some("\x1b[A".to_string()),
        "down" => Some("\x1b[B".to_string()),
        "right" => Some("\x1b[C".to_string()),
        "left" => Some("\x1b[D".to_string()),
        "home" => Some("\x1b[H".to_string()),
        "end" => Some("\x1b[F".to_string()),
        "pageup" => Some("\x1b[5~".to_string()),
        "pagedown" => Some("\x1b[6~".to_string()),
        "insert" => Some("\x1b[2~".to_string()),
        "delete" => Some("\x1b[3~".to_string()),
        "backtab" => Some("\x1b[Z".to_string()),
        "backspace" => Some("\x7f".to_string()),
        "enter" | "return" => Some("\r".to_string()),
        "tab" => Some("\t".to_string()),
        "esc" | "escape" => Some("\x1b".to_string()),
        "space" => Some(" ".to_string()),
        "plus" => Some("+".to_string()),
        _ => {
            if let Some(digit) = lowered.strip_prefix('f') {
                if let Ok(number) = digit.parse::<u8>() {
                    if (1..=12).contains(&number) {
                        let code = match number {
                            1..=4 => format!("\x1b[{}~", 10 + number),
                            5 => "\x1b[15~".to_string(),
                            6..=10 => format!("\x1b[{}~", 16 + number),
                            _ => format!("\x1b[{}~", 22 + number),
                        };
                        Some(code)
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        }
    };

    if let Some(sequence) = special {
        if modifier == 1 {
            return sequence;
        }
        if sequence.starts_with("\x1b[") && sequence.ends_with('~') {
            let inner = &sequence[2..sequence.len() - 1];
            return format!("\x1b[{inner};{modifier}~");
        }
        if sequence.len() == 3 && sequence.starts_with("\x1b[") {
            let final_byte = &sequence[2..];
            return format!("\x1b[1;{modifier}{final_byte}");
        }
        if alt && sequence.len() == 1 {
            return format!("\x1b{sequence}");
        }
        return sequence;
    }

    // A literal character.
    if name.chars().count() == 1 {
        let ch = name.chars().next().unwrap_or(' ');
        if ctrl {
            let upper = ch.to_ascii_uppercase();
            if upper.is_ascii_alphabetic() || "@[\\]^_".contains(upper) {
                let code = (upper as u8) & 0x1f;
                let text = char::from(code).to_string();
                return if alt { format!("\x1b{text}") } else { text };
            }
        }
        return if alt {
            format!("\x1b{name}")
        } else {
            name.to_string()
        };
    }
    String::new()
}

/// The path a pane was started in, for a caller that kept only the pane id.
pub fn cwd_of(pane: &Pane) -> &Path {
    &pane.cwd
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
