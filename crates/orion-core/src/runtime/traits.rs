//! The two surfaces the shell talks to.

use std::path::Path;
use std::sync::Arc;

use serde_json::Value;

use crate::engine::pane::{FrameSink, Role};
use crate::error::Result;
use crate::prompt::DetectedPrompt;
use crate::protocol::AgentStatus;

/// The engine surface the shell talks to.
///
/// The shell can hold an engine in its own process or reach one over a socket;
/// both answer the same questions, so the shell has one code path either way.
pub trait Runtime: Send + Sync {
    /// The identity this runtime reports: its version and its protocol version.
    fn identity(&self) -> Value;

    /// Runs one REST call. Returns the status code and the body.
    fn http(&self, method: &str, path: &str, body: &Value) -> (u16, Value);

    /// The live pane handle for a pane id, if it is open.
    fn pane_by_id(&self, pane_id: &str) -> Option<Arc<dyn PaneHandle>>;

    /// Registers a client and returns the connection id its frames go to.
    fn connect(&self, sink: FrameSink) -> String;

    /// Sends one frame to a client.
    fn send_to(&self, connection: &str, frame: Value);

    /// Drops a client and everything it was watching.
    fn disconnect(&self, connection: &str);

    /// Dispatches one client frame. `None` means the answer already went to the
    /// connection's sink and there is nothing for the transport to send.
    fn dispatch_from(
        &self,
        connection: Option<&str>,
        request: &Value,
    ) -> Result<Option<Value>, String>;

    /// The whole session as one value: workspaces, tabs and panes.
    fn snapshot_value(&self) -> Value;

    /// Brings the saved session back.
    fn restore(&self);
}

/// The terminal surface the shell talks to.
///
/// A pane running in this process and a pane running in another one answer the
/// same questions, so the terminal lens has one code path either way.
pub trait PaneHandle: Send + Sync {
    /// The pane's id.
    fn id(&self) -> &str;

    /// The directory the agent runs in.
    fn cwd(&self) -> &Path;

    /// Writes literal text to the agent's stdin.
    fn write(&self, data: &str) -> Result<()>;

    /// Sends logical key chords (`ctrl+c`, `up`, `enter`) as terminal bytes.
    fn keys(&self, keys: &[String]) -> Result<()>;

    /// The terminal's own interrupt: Ctrl+C.
    fn interrupt(&self) -> Result<()>;

    /// Resizes the terminal. Zero means "leave it alone".
    fn resize(&self, cols: u16, rows: u16) -> Result<()>;

    /// Blanks the grid and drops its history. The agent keeps running.
    fn clear(&self);

    /// The agent's status.
    fn status(&self) -> AgentStatus;

    /// The menu the agent is showing right now, if it is showing one.
    fn prompt(&self) -> Option<DetectedPrompt>;

    /// Attaches a connection: replays the screen, then streams every new byte to it.
    fn attach(
        &self,
        connection: &str,
        sink: FrameSink,
        role: Role,
        cols: u16,
        rows: u16,
        keep_size: bool,
    ) -> Result<()>;

    /// Drops one connection's attachment.
    fn detach(&self, connection: &str);

    /// Drops every attachment a closed connection held.
    fn detach_all(&self, connection: &str);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Mirrors [`crate::engine::pane::Pane`]: a `cwd` field and an inherent
    /// accessor, so the trait's borrowed `&Path` has to coexist with both.
    struct Sample {
        cwd: PathBuf,
    }

    impl Sample {
        fn cwd(&self) -> &PathBuf {
            &self.cwd
        }
    }

    impl PaneHandle for Sample {
        fn id(&self) -> &str {
            "sample"
        }

        fn cwd(&self) -> &Path {
            // The field, not the inherent method: `self.cwd` is a place, so the
            // borrow coerces to `&Path` without calling anything.
            &self.cwd
        }

        fn write(&self, _data: &str) -> Result<()> {
            Ok(())
        }

        fn keys(&self, _keys: &[String]) -> Result<()> {
            Ok(())
        }

        fn interrupt(&self) -> Result<()> {
            Ok(())
        }

        fn resize(&self, _cols: u16, _rows: u16) -> Result<()> {
            Ok(())
        }

        fn clear(&self) {}

        fn status(&self) -> AgentStatus {
            AgentStatus::Idle
        }

        fn prompt(&self) -> Option<DetectedPrompt> {
            None
        }

        fn attach(
            &self,
            _connection: &str,
            _sink: FrameSink,
            _role: Role,
            _cols: u16,
            _rows: u16,
            _keep_size: bool,
        ) -> Result<()> {
            Ok(())
        }

        fn detach(&self, _connection: &str) {}

        fn detach_all(&self, _connection: &str) {}
    }

    #[test]
    fn the_runtime_and_the_pane_handle_are_usable_as_trait_objects() {
        // Naming `dyn` at all is the object-safety check: a generic method or an
        // associated type would keep these from being types.
        let _: fn(&dyn Runtime) = |_runtime| {};
        let _: fn(&dyn PaneHandle) = |_pane| {};

        let sample = Arc::new(Sample {
            cwd: PathBuf::from("work"),
        });
        assert_eq!(sample.cwd().as_path(), Path::new("work"));

        // The shell only ever sees the trait object, exactly as it will at run time.
        let pane: Arc<dyn PaneHandle> = sample;
        assert_eq!(pane.id(), "sample");
        assert_eq!(pane.cwd(), Path::new("work"));
        assert_eq!(pane.status(), AgentStatus::Idle);
        assert!(pane.prompt().is_none());
    }
}
