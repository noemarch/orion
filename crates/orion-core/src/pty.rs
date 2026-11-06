use std::io::{Read, Write};
use std::sync::Arc;
use std::thread;

use parking_lot::Mutex;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::error::{OrionError, Result};
use crate::protocol::TerminalSize;

/// A handle to an active PTY session.
pub struct PtySession {
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
}

impl PtySession {
    /// Spawn a new process attached to a native PTY.
    pub fn spawn(
        command: &str,
        args: &[String],
        cwd: Option<&str>,
        env: &[(String, String)],
        size: TerminalSize,
        output_tx: mpsc::Sender<Vec<u8>>,
    ) -> Result<Self> {
        let pty_system = native_pty_system();

        let pty_size = PtySize {
            rows: size.rows,
            cols: size.cols,
            pixel_width: size.pixel_width,
            pixel_height: size.pixel_height,
        };

        let pair = pty_system
            .openpty(pty_size)
            .map_err(|e| OrionError::Pty(format!("Failed to open PTY: {e}")))?;

        let mut cmd = CommandBuilder::new(command);
        for arg in args {
            cmd.arg(arg);
        }
        if let Some(dir) = cwd {
            cmd.cwd(dir);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| OrionError::Process(format!("Failed to spawn command '{command}': {e}")))?;

        drop(pair.slave); // Ensure slave is closed in parent

        let master = pair.master;
        let writer = master
            .take_writer()
            .map_err(|e| OrionError::Pty(format!("Failed to take PTY writer: {e}")))?;

        let mut reader = master
            .try_clone_reader()
            .map_err(|e| OrionError::Pty(format!("Failed to clone PTY reader: {e}")))?;

        // Dedicated thread for blocking PTY reads to protect Tokio async workers
        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        debug!("PTY reader reached EOF");
                        break;
                    }
                    Ok(n) => {
                        let data = buf[..n].to_vec();
                        if output_tx.blocking_send(data).is_err() {
                            debug!("PTY output channel closed, stopping reader thread");
                            break;
                        }
                    }
                    Err(e) => {
                        debug!("PTY read finished or encountered error: {e}");
                        break;
                    }
                }
            }
        });

        Ok(Self {
            master: Arc::new(Mutex::new(master)),
            writer: Arc::new(Mutex::new(writer)),
            child: Arc::new(Mutex::new(child)),
        })
    }

    /// Write raw bytes to the PTY stdin.
    pub fn write_all(&self, data: &[u8]) -> Result<()> {
        let mut writer = self.writer.lock();
        writer
            .write_all(data)
            .map_err(|e| OrionError::Pty(format!("Failed to write to PTY: {e}")))?;
        writer
            .flush()
            .map_err(|e| OrionError::Pty(format!("Failed to flush PTY: {e}")))?;
        Ok(())
    }

    /// Resize the pseudo-terminal.
    pub fn resize(&self, size: TerminalSize) -> Result<()> {
        let pty_size = PtySize {
            rows: size.rows,
            cols: size.cols,
            pixel_width: size.pixel_width,
            pixel_height: size.pixel_height,
        };
        let master = self.master.lock();
        master
            .resize(pty_size)
            .map_err(|e| OrionError::Pty(format!("Failed to resize PTY: {e}")))?;
        Ok(())
    }

    /// Terminate the child process safely.
    pub fn kill(&self) -> Result<()> {
        let mut child = self.child.lock();
        match child.kill() {
            Ok(_) => Ok(()),
            Err(e) => {
                warn!("Process termination warning: {e}");
                Ok(())
            }
        }
    }

    /// Check if child process has exited.
    pub fn try_wait(&self) -> Result<Option<u32>> {
        let mut child = self.child.lock();
        match child.try_wait() {
            Ok(Some(status)) => Ok(Some(status.exit_code())),
            Ok(None) => Ok(None),
            Err(e) => Err(OrionError::Process(format!("Failed to wait for process: {e}"))),
        }
    }
}
