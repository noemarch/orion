//! One running program and the grid it draws on.
//!
//! This is what the daemon owns and what the registry holds: the process, the grid parsed from its
//! output, and the status the program reported about itself. A client may attach at any moment, so
//! the grid is fed as output arrives rather than read at attach time — a reader sees the screen as
//! it is, not as it was.
//!
//! Status is never inferred from the screen. A program that has not reported one has none.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use parking_lot::Mutex;

use crate::agents::LaunchSpec;
use crate::error::{OrionError, Result};
use crate::protocol::{ReportedStatus, TerminalSize};
use crate::pty::PtySession;
use crate::screen::ScreenState;

/// How many recent output chunks a late reader can catch up on. Bounded so a terminal that runs
/// for days cannot grow a second copy of its own scrollback.
const RECENT_CHUNKS: usize = 200;

/// One consumer of a terminal's output. Cheap to clone and safe to call from the PTY reader
/// thread, which is where every chunk is produced.
pub type OutputSink = Arc<dyn Fn(String) + Send + Sync>;

struct TerminalState {
    screen: ScreenState,
    reported: Option<ReportedStatus>,
    exited: Option<i32>,
    /// The most recent chunks, oldest first: what a reader that attaches late missed.
    output: VecDeque<String>,
    sinks: Vec<OutputSink>,
}

/// One running program and the grid it draws on.
///
/// The terminal owns the process and the grid: the grid is fed from the
/// process's output as it arrives, so a reader that attaches later sees the
/// screen as it is, not as it was.
pub struct Terminal {
    id: String,
    name: Option<String>,
    parent_id: Option<String>,
    session: Arc<PtySession>,
    state: Arc<Mutex<TerminalState>>,
}

impl Terminal {
    /// Starts `spec` in a new terminal and returns it.
    pub fn spawn(
        id: String,
        spec: &LaunchSpec,
        cwd: Option<&Path>,
        env: &[(String, String)],
        size: TerminalSize,
    ) -> Result<Self> {
        let state = Arc::new(Mutex::new(TerminalState {
            screen: ScreenState::new(size.rows.max(1), size.cols.max(1)),
            reported: None,
            exited: None,
            output: VecDeque::with_capacity(RECENT_CHUNKS),
            sinks: Vec::new(),
        }));

        // Runs on the PTY reader thread, so it takes the lock exactly once and keeps nothing but a
        // grid feed and a ring push inside it. The sinks are copied out before the lock is
        // released and called afterwards: a sink that reads the terminal back (a reader that
        // snapshots on its first chunk) would otherwise deadlock against the held lock, and a
        // slow sink would stall the program's own output.
        let output_state = Arc::clone(&state);
        let on_text: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |text: String| {
            let sinks = {
                let mut state = output_state.lock();
                state.screen.process(&text);
                if state.output.len() >= RECENT_CHUNKS {
                    state.output.pop_front();
                }
                state.output.push_back(text.clone());
                state.sinks.clone()
            };
            for sink in sinks {
                sink(text.clone());
            }
        });

        let session = Arc::new(PtySession::spawn(spec, cwd, env, size, on_text)?);

        // `portable-pty` needs someone to reap the child, and a registry that asks for the exit
        // code must not have to poll the PTY itself. A weak handle keeps a dropped terminal from
        // being kept alive by this thread; the session it still holds is what ends.
        let watch_state = Arc::downgrade(&state);
        let watch_session = Arc::clone(&session);
        let watch_id = id.clone();
        thread::Builder::new()
            .name(format!("orion-terminal-exit-{watch_id}"))
            .spawn(move || loop {
                match watch_session.try_wait() {
                    Ok(Some(code)) => {
                        if let Some(state) = watch_state.upgrade() {
                            state.lock().exited = Some(code);
                        }
                        return;
                    }
                    Ok(None) => thread::sleep(Duration::from_millis(100)),
                    Err(_) => return,
                }
            })
            .map_err(|error| {
                OrionError::Process(format!("failed to watch the terminal: {error}"))
            })?;

        Ok(Self {
            id,
            name: None,
            parent_id: None,
            session,
            state,
        })
    }

    /// The terminal's id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The human name, if it has one.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// The id of the terminal that started this one, if any.
    pub fn parent_id(&self) -> Option<&str> {
        self.parent_id.as_deref()
    }

    /// Gives the terminal a human name.
    pub fn set_name(&mut self, name: &str) {
        self.name = Some(name.to_string());
    }

    /// Records which terminal started this one.
    pub fn set_parent(&mut self, parent_id: &str) {
        self.parent_id = Some(parent_id.to_string());
    }

    /// Writes bytes to the program's stdin.
    pub fn write_input(&self, data: &str) -> Result<()> {
        self.session.write(data.as_bytes())
    }

    /// Sends logical key chords as terminal bytes.
    pub fn keys(&self, keys: &[String]) -> Result<()> {
        let mut bytes = String::new();
        for key in keys {
            bytes.push_str(&encode_key(key));
        }
        if bytes.is_empty() {
            return Ok(());
        }
        self.session.write(bytes.as_bytes())
    }

    /// The program's own interrupt: Ctrl+C.
    pub fn interrupt(&self) -> Result<()> {
        self.session.interrupt()
    }

    /// Resizes the terminal and the grid with it.
    ///
    /// Zero means "leave it alone": a client that has not measured itself yet must not collapse
    /// the program's screen. The grid moves with the PTY under one lock, so a repaint never sees a
    /// width the program is not drawing at.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        if cols == 0 || rows == 0 {
            return Ok(());
        }
        let mut state = self.state.lock();
        let (live_rows, live_cols) = state.screen.size();
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
        Ok(())
    }

    /// The whole screen as one string of plain text.
    pub fn snapshot(&self) -> String {
        self.state.lock().screen.visible_text()
    }

    /// The screen as the ANSI the reader's terminal can replay.
    pub fn replay(&self) -> String {
        self.state.lock().screen.replay()
    }

    /// The recent rows of output, newest last.
    ///
    /// Rows the PTY soft-wrapped are rejoined: a caller prints them into a terminal of another
    /// width, and the wrap belongs to the program's window, not to the text.
    pub fn read(&self, lines: usize) -> String {
        self.state.lock().screen.read_recent(lines, true).0
    }

    /// The program's own report of what it is doing, if it has reported.
    pub fn reported_status(&self) -> Option<ReportedStatus> {
        self.state.lock().reported
    }

    /// Records what the program reported about itself.
    pub fn set_reported_status(&self, status: ReportedStatus) {
        self.state.lock().reported = Some(status);
    }

    /// The exit code, if the program has ended.
    pub fn exited(&self) -> Option<i32> {
        self.state.lock().exited
    }

    /// Ends the program and its whole tree.
    pub fn kill(&self) {
        self.session.kill();
    }

    /// Hands every new output chunk to `sink` until the terminal is dropped.
    ///
    /// The recent chunks go to the new sink first: a reader that attaches late still sees what it
    /// missed. Registration and the snapshot happen under the same lock the reader thread takes,
    /// so no chunk is lost between the two and none is handed over twice.
    pub fn subscribe_output(&self, sink: OutputSink) {
        let recent: Vec<String> = {
            let mut state = self.state.lock();
            state.sinks.push(Arc::clone(&sink));
            state.output.iter().cloned().collect()
        };
        for chunk in recent {
            sink(chunk);
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // A terminal nobody holds must not leave its program running: the daemon's registry is the
        // only thing that keeps a process alive.
        self.session.kill();
    }
}

/// Encodes one logical key chord into the bytes a terminal receives.
///
/// The grammar is the terminal lens's (`engine::pane`), so a keystroke in the shell and a `send`
/// from the CLI reach the program as the same bytes.
fn encode_key(key: &str) -> String {
    let (ctrl, alt, shift, name) = split_modifiers(key);
    let modifier = 1 + i32::from(shift) + 2 * i32::from(alt) + 4 * i32::from(ctrl);
    let lowered = name.to_ascii_lowercase();

    // Shift+Tab is its own sequence (CSI Z), not a modified Tab.
    if lowered == "tab" && modifier == 2 {
        return "\x1b[Z".to_string();
    }

    if let Some(sequence) = named_key(&lowered) {
        if modifier == 1 {
            return sequence.to_string();
        }
        if let Some(inner) = sequence
            .strip_prefix("\x1b[")
            .and_then(|rest| rest.strip_suffix('~'))
        {
            return format!("\x1b[{inner};{modifier}~");
        }
        if sequence.len() == 3 {
            return format!("\x1b[1;{}{}", modifier, &sequence[2..]);
        }
        if alt && sequence.len() == 1 {
            return format!("\x1b{sequence}");
        }
        return sequence.to_string();
    }

    // A literal character.
    if name.chars().count() == 1 {
        let ch = name.chars().next().unwrap_or(' ');
        if ctrl && ch.is_ascii() {
            let upper = ch.to_ascii_uppercase();
            if upper.is_ascii_alphabetic() || "@[\\]^_".contains(upper) {
                let code = u32::from(upper) & 0x1f;
                let text = char::from_u32(code).unwrap_or(upper).to_string();
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

/// Splits `ctrl+alt+shift+name` into its modifiers and the key they modify.
fn split_modifiers(key: &str) -> (bool, bool, bool, &str) {
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
            return (ctrl, alt, shift, name);
        }
    }
}

/// The byte sequence for a key that has no character of its own.
fn named_key(lowered: &str) -> Option<&'static str> {
    Some(match lowered {
        "up" => "\x1b[A",
        "down" => "\x1b[B",
        "right" => "\x1b[C",
        "left" => "\x1b[D",
        "home" => "\x1b[H",
        "end" => "\x1b[F",
        "pageup" => "\x1b[5~",
        "pagedown" => "\x1b[6~",
        "insert" => "\x1b[2~",
        "delete" => "\x1b[3~",
        "backtab" => "\x1b[Z",
        "backspace" => "\x7f",
        "enter" | "return" => "\r",
        "tab" => "\t",
        "esc" | "escape" => "\x1b",
        "space" => " ",
        "plus" => "+",
        "f1" => "\x1b[11~",
        "f2" => "\x1b[12~",
        "f3" => "\x1b[13~",
        "f4" => "\x1b[14~",
        "f5" => "\x1b[15~",
        "f6" => "\x1b[17~",
        "f7" => "\x1b[18~",
        "f8" => "\x1b[19~",
        "f9" => "\x1b[20~",
        "f10" => "\x1b[21~",
        "f11" => "\x1b[23~",
        "f12" => "\x1b[24~",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Instant;

    const MARKER: &str = "orion-terminal-ok";

    fn size() -> TerminalSize {
        TerminalSize {
            cols: 80,
            rows: 24,
            pixel_width: 0,
            pixel_height: 0,
        }
    }

    /// A program that prints once and ends.
    fn printer() -> LaunchSpec {
        #[cfg(windows)]
        let spec = LaunchSpec {
            program: "cmd.exe".into(),
            args: vec!["/D".into(), "/C".into(), format!("echo {MARKER}")],
        };
        #[cfg(not(windows))]
        let spec = LaunchSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), format!("echo {MARKER}")],
        };
        spec
    }

    /// A program that holds the terminal open long enough to be resized or killed.
    fn sleeper() -> LaunchSpec {
        #[cfg(windows)]
        let spec = LaunchSpec {
            program: "cmd.exe".into(),
            args: vec!["/D".into(), "/C".into(), "ping -n 30 127.0.0.1 >nul".into()],
        };
        #[cfg(not(windows))]
        let spec = LaunchSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "sleep 30".into()],
        };
        spec
    }

    fn wait_until(mut ready: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if ready() {
                return true;
            }
            thread::sleep(Duration::from_millis(100));
        }
        false
    }

    #[test]
    fn a_terminal_shows_what_the_program_printed() {
        let terminal =
            Terminal::spawn("t1".into(), &printer(), None, &[], size()).expect("a terminal starts");
        assert!(
            wait_until(|| terminal.snapshot().contains(MARKER)),
            "screen was {:?}",
            terminal.snapshot()
        );
    }

    #[test]
    fn a_terminal_that_has_not_reported_has_no_status() {
        let terminal =
            Terminal::spawn("t2".into(), &printer(), None, &[], size()).expect("a terminal starts");
        assert_eq!(terminal.reported_status(), None);
        terminal.set_reported_status(ReportedStatus::Done);
        assert_eq!(terminal.reported_status(), Some(ReportedStatus::Done));
    }

    #[test]
    fn a_reader_that_attaches_late_still_sees_the_output() {
        let terminal =
            Terminal::spawn("t3".into(), &printer(), None, &[], size()).expect("a terminal starts");
        // The chunk is in the grid and in the ring together, so a screen that shows it proves the
        // late reader has something to catch up on.
        assert!(
            wait_until(|| terminal.snapshot().contains(MARKER)),
            "the program never printed"
        );

        let (tx, rx) = mpsc::channel::<String>();
        terminal.subscribe_output(Arc::new(move |text: String| {
            let _ = tx.send(text);
        }));

        let mut caught = String::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && !caught.contains(MARKER) {
            if let Ok(text) = rx.recv_timeout(Duration::from_millis(100)) {
                caught.push_str(&text);
            }
        }
        assert!(caught.contains(MARKER), "late reader saw {caught:?}");
    }

    #[test]
    fn a_terminal_can_be_resized() {
        let terminal =
            Terminal::spawn("t4".into(), &sleeper(), None, &[], size()).expect("a terminal starts");
        terminal.resize(100, 40).expect("the terminal resizes");
        // The grid reports `(rows, cols)`.
        assert_eq!(terminal.state.lock().screen.size(), (40, 100));

        // Zero is "leave it alone": a client that has not measured itself must not blank the grid.
        terminal.resize(0, 0).expect("a zero resize is a no-op");
        assert_eq!(terminal.state.lock().screen.size(), (40, 100));
        terminal.kill();
    }

    #[test]
    fn the_terminal_is_safe_to_share_between_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Terminal>();
    }

    #[test]
    fn a_terminal_ends_with_its_program() {
        let terminal =
            Terminal::spawn("t6".into(), &sleeper(), None, &[], size()).expect("a terminal starts");
        terminal.kill();
        assert!(
            wait_until(|| terminal.exited().is_some()),
            "the program outlived its terminal"
        );
    }

    #[test]
    fn keys_encode_like_a_terminal() {
        assert_eq!(encode_key("enter"), "\r");
        assert_eq!(encode_key("ctrl+c"), "\x03");
        assert_eq!(encode_key("up"), "\x1b[A");
        assert_eq!(encode_key("shift+tab"), "\x1b[Z");
        assert_eq!(encode_key("alt+x"), "\x1bx");
        assert_eq!(encode_key("f5"), "\x1b[15~");
        assert_eq!(encode_key("a"), "a");
    }
}
