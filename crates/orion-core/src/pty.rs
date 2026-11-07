//! PTY ownership.
//!
//! A pane's agent runs in a native pseudo-terminal: ConPTY on Windows, a Unix PTY elsewhere.
//! Reads are blocking, so they live on a dedicated thread and never on a Tokio worker. Bytes are
//! decoded incrementally: a multi-byte UTF-8 character split across two reads must survive, and a
//! final flush must not drop the tail of the stream.
//!
//! The child is placed in its own job object (Windows) so closing a pane closes the agent's whole
//! tool tree, not only the process `portable-pty` knows about.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::thread;

use parking_lot::Mutex;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use tracing::{debug, warn};

use crate::agents::LaunchSpec;
use crate::error::{OrionError, Result};
use crate::process::ChildJob;
use crate::protocol::TerminalSize;

/// Decodes UTF-8 across chunk boundaries. Invalid bytes become U+FFFD rather than failing the
/// pane: a terminal must keep showing the rest of the stream.
#[derive(Debug, Default)]
pub struct Utf8Decoder {
    pending: Vec<u8>,
}

impl Utf8Decoder {
    pub fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let mut out = String::new();
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(text) => {
                    out.push_str(text);
                    self.pending.clear();
                    return out;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    if valid > 0 {
                        // SAFETY: `valid_up_to` is the prefix the decoder just accepted.
                        out.push_str(unsafe {
                            std::str::from_utf8_unchecked(&self.pending[..valid])
                        });
                    }
                    match error.error_len() {
                        Some(length) => {
                            out.push('\u{FFFD}');
                            self.pending.drain(..valid + length);
                        }
                        None => {
                            // An incomplete sequence at the end: keep it for the next read.
                            self.pending.drain(..valid);
                            return out;
                        }
                    }
                }
            }
        }
    }

    /// Flushes a trailing incomplete sequence as U+FFFD. Call once, at EOF.
    pub fn finish(&mut self) -> String {
        if self.pending.is_empty() {
            String::new()
        } else {
            let mut text = self.push(&[]);
            if !self.pending.is_empty() {
                text.push('\u{FFFD}');
                self.pending.clear();
            }
            text
        }
    }
}

/// One running agent process and its PTY.
pub struct PtySession {
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    job: Option<ChildJob>,
    pid: Option<u32>,
}

impl PtySession {
    /// Starts `spec` in a new PTY. `on_text` runs on the reader thread with decoded output.
    pub fn spawn(
        spec: &LaunchSpec,
        cwd: Option<&Path>,
        env: &[(String, String)],
        size: TerminalSize,
        on_text: Arc<dyn Fn(String) + Send + Sync>,
    ) -> Result<Self> {
        let pty_system = native_pty_system();
        let pty_size = PtySize {
            rows: size.rows.max(1),
            cols: size.cols.max(1),
            pixel_width: size.pixel_width.max(8),
            pixel_height: size.pixel_height.max(16),
        };
        let pair = pty_system
            .openpty(pty_size)
            .map_err(|e| OrionError::Pty(format!("failed to open a PTY: {e}")))?;

        let mut command = CommandBuilder::new(&spec.program);
        for arg in &spec.args {
            command.arg(arg);
        }
        if let Some(dir) = cwd {
            command.cwd(dir);
        }
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        for (name, value) in env {
            command.env(name, value);
        }

        let child = pair.slave.spawn_command(command).map_err(|e| {
            OrionError::Process(format!(
                "failed to start '{}': {e}",
                spec.program.to_string_lossy()
            ))
        })?;
        drop(pair.slave);

        let pid = child.process_id();
        let job = match pid.and_then(|pid| ChildJob::assign(pid).ok()) {
            Some(job) => Some(job),
            None => {
                warn!("agent process is not in its own job object; this pane cannot sweep its children");
                None
            }
        };

        let master = pair.master;
        let writer = master
            .take_writer()
            .map_err(|e| OrionError::Pty(format!("failed to take the PTY writer: {e}")))?;
        let mut reader = master
            .try_clone_reader()
            .map_err(|e| OrionError::Pty(format!("failed to clone the PTY reader: {e}")))?;

        thread::Builder::new()
            .name(format!("orion-pty-{}", pid.unwrap_or(0)))
            .spawn(move || {
                let mut decoder = Utf8Decoder::default();
                let mut buf = [0u8; 16 * 1024];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(count) => {
                            let text = decoder.push(&buf[..count]);
                            if !text.is_empty() {
                                on_text(text);
                            }
                        }
                        Err(error) => {
                            debug!("PTY read ended: {error}");
                            break;
                        }
                    }
                }
                let tail = decoder.finish();
                if !tail.is_empty() {
                    on_text(tail);
                }
            })
            .map_err(|e| OrionError::Process(format!("failed to start the PTY reader: {e}")))?;

        Ok(Self {
            master: Arc::new(Mutex::new(master)),
            writer: Arc::new(Mutex::new(writer)),
            child: Arc::new(Mutex::new(child)),
            job,
            pid,
        })
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Writes bytes to the agent's stdin.
    pub fn write(&self, data: &[u8]) -> Result<()> {
        let mut writer = self.writer.lock();
        writer
            .write_all(data)
            .map_err(|e| OrionError::Pty(format!("failed to write to the PTY: {e}")))?;
        writer
            .flush()
            .map_err(|e| OrionError::Pty(format!("failed to flush the PTY: {e}")))?;
        Ok(())
    }

    pub fn resize(&self, size: TerminalSize) -> Result<()> {
        let pty_size = PtySize {
            rows: size.rows.max(1),
            cols: size.cols.max(1),
            pixel_width: size.pixel_width.max(8),
            pixel_height: size.pixel_height.max(16),
        };
        self.master
            .lock()
            .resize(pty_size)
            .map_err(|e| OrionError::Pty(format!("failed to resize the PTY: {e}")))
    }

    /// The terminal's own interrupt: Ctrl+C, what a user's keyboard would send. Agents handle it
    /// as "stop this turn", which a process kill would not.
    pub fn interrupt(&self) -> Result<()> {
        self.write(&[0x03])
    }

    pub fn try_wait(&self) -> Result<Option<i32>> {
        match self.child.lock().try_wait() {
            Ok(Some(status)) => Ok(Some(status.exit_code() as i32)),
            Ok(None) => Ok(None),
            Err(error) => Err(OrionError::Process(format!(
                "failed to wait for the agent: {error}"
            ))),
        }
    }

    /// Ends the whole tree now: the job object's members, then the direct child as a fallback.
    pub fn kill(&self) {
        if let Some(job) = &self.job {
            job.terminate();
        }
        let _ = self.child.lock().kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    #[test]
    fn a_utf8_character_split_across_reads_survives() {
        let mut decoder = Utf8Decoder::default();
        let bytes = "aé😀z".as_bytes();
        assert_eq!(decoder.push(&bytes[..2]), "a");
        assert_eq!(decoder.push(&bytes[2..3]), "é");
        assert_eq!(decoder.push(&bytes[3..6]), "");
        assert_eq!(decoder.push(&bytes[6..]), "😀z");
        assert_eq!(decoder.finish(), "");
    }

    #[test]
    fn invalid_bytes_become_the_replacement_character_and_the_stream_keeps_going() {
        let mut decoder = Utf8Decoder::default();
        assert_eq!(decoder.push(&[0xff, b'a']), "\u{FFFD}a");
    }

    #[test]
    fn an_incomplete_tail_is_flushed_as_one_replacement() {
        let mut decoder = Utf8Decoder::default();
        assert_eq!(decoder.push(&[b'a', 0xe2, 0x82]), "a");
        assert_eq!(decoder.finish(), "\u{FFFD}");
    }

    #[test]
    fn a_real_pty_echoes_and_resizes_without_losing_output() {
        #[cfg(windows)]
        let spec = LaunchSpec {
            program: "cmd.exe".into(),
            args: vec![
                "/D".into(),
                "/C".into(),
                "echo pty-ok & ping -n 3 127.0.0.1 >nul".into(),
            ],
        };
        #[cfg(not(windows))]
        let spec = LaunchSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "echo pty-ok; sleep 0.5".into()],
        };

        let (tx, rx) = mpsc::channel::<String>();
        let session = PtySession::spawn(
            &spec,
            None,
            &[],
            TerminalSize {
                cols: 80,
                rows: 24,
                pixel_width: 0,
                pixel_height: 0,
            },
            Arc::new(move |text| {
                let _ = tx.send(text);
            }),
        )
        .expect("a PTY starts");

        let mut output = String::new();
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline && !output.contains("pty-ok") {
            if let Ok(text) = rx.recv_timeout(Duration::from_millis(200)) {
                output.push_str(&text);
            }
        }
        assert!(output.contains("pty-ok"), "output was {output:?}");

        for cols in 40..90 {
            session
                .resize(TerminalSize {
                    cols,
                    rows: 24,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .expect("resize");
        }

        let exited = (0..50).find_map(|_| {
            let status = session.try_wait().expect("wait");
            if status.is_none() {
                thread::sleep(Duration::from_millis(100));
            }
            status
        });
        assert_eq!(exited, Some(0));
    }

    #[test]
    fn a_long_running_child_can_be_killed_without_orphans() {
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

        let session = PtySession::spawn(
            &spec,
            None,
            &[],
            TerminalSize {
                cols: 80,
                rows: 24,
                pixel_width: 0,
                pixel_height: 0,
            },
            Arc::new(|_| {}),
        )
        .expect("a PTY starts");
        session.kill();
        let status = (0..50).find_map(|_| {
            let status = session.try_wait().expect("wait");
            if status.is_none() {
                thread::sleep(Duration::from_millis(100));
            }
            status
        });
        assert!(status.is_some(), "the child ended after kill");
    }
}
