//! What the runtime must remember when it is gone.
//!
//! Two kinds of memory, one directory: `terminals.json`, the records of the terminals the daemon
//! owned, and `screens/<id>.ansi`, the last screen each one showed. The directory sits beside the
//! socket and the lock, is private to the user, and every write goes to a sibling temp file and is
//! renamed over its target, so a crash mid-write leaves the previous document readable.
//!
//! A record is a memory, not a process: reading it back starts nothing. The daemon decides what to
//! do with it, and the app shows the saved screen of a terminal that is no longer running.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{OrionError, Result};
use crate::runtime::registry::TerminalInfo;
use crate::runtime::transport;

/// The file the terminal records live in.
const TERMINALS: &str = "terminals.json";

/// The directory the saved screens live in, under the store's own.
const SCREENS: &str = "screens";

/// The runtime's own state: the terminals it owns and the screens they showed.
///
/// The files live in the runtime's directory, beside the socket and the lock,
/// and only the user who owns them can read them.
#[derive(Debug, Clone)]
pub struct RuntimeStore {
    dir: PathBuf,
}

impl RuntimeStore {
    /// A store for the runtime's state directory.
    pub fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// A store in the runtime's directory beside the socket.
    pub fn in_runtime_dir() -> Self {
        Self::at(transport::runtime_dir())
    }

    /// The directory this store writes to.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Writes the terminal records.
    pub fn save_terminals(&self, terminals: &[TerminalInfo]) -> Result<()> {
        ensure_private_dir(&self.dir)?;
        let text = serde_json::to_vec_pretty(terminals).map_err(|error| {
            OrionError::Store(format!("failed to encode the terminals: {error}"))
        })?;
        write_atomic(&self.dir.join(TERMINALS), &text)
    }

    /// Reads the terminal records back.
    ///
    /// A missing or unreadable document is no records: a restart with nothing to read is a fresh
    /// start, not a failure the daemon has to describe.
    pub fn load_terminals(&self) -> Vec<TerminalInfo> {
        fs::read(self.dir.join(TERMINALS))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Writes the saved screen of one terminal.
    pub fn save_screen(&self, id: &str, screen: &str) -> Result<()> {
        let dir = self.dir.join(SCREENS);
        ensure_private_dir(&dir)?;
        write_atomic(&self.screen_path(id), screen.as_bytes())
    }

    /// Reads the saved screen of one terminal.
    pub fn load_screen(&self, id: &str) -> Option<String> {
        fs::read_to_string(self.screen_path(id)).ok()
    }

    /// Where one terminal's screen lives.
    ///
    /// The id comes from the registry, but a file name is a file name: it is reduced to the
    /// characters a file name may carry, so no id can walk out of the directory.
    fn screen_path(&self, id: &str) -> PathBuf {
        self.dir
            .join(SCREENS)
            .join(format!("{}.ansi", transport::sanitize(id)))
    }
}

/// Creates a directory the current user alone may read.
///
/// Unix needs the mode set explicitly; on Windows the directory inherits the user's profile ACL,
/// which is already user-only.
fn ensure_private_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).map_err(|error| {
        OrionError::Store(format!("failed to create {}: {error}", dir.display()))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|error| {
            OrionError::Store(format!("failed to protect {}: {error}", dir.display()))
        })?;
    }
    Ok(())
}

/// Writes one document atomically: temp file, then rename over the target.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let temp = path.with_extension("tmp");
    fs::write(&temp, bytes).map_err(|error| {
        OrionError::Store(format!("failed to write {}: {error}", temp.display()))
    })?;
    fs::rename(&temp, path).map_err(|error| {
        OrionError::Store(format!("failed to replace {}: {error}", path.display()))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ReportedStatus;
    use uuid::Uuid;

    /// A directory of its own, so one test cannot see another's files.
    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("orion-persist-{tag}-{}", Uuid::new_v4().simple()));
        fs::create_dir_all(&dir).expect("the test directory exists");
        dir
    }

    fn record(id: &str) -> TerminalInfo {
        TerminalInfo {
            id: id.to_string(),
            name: Some("shell".to_string()),
            parent_id: Some("00000001".to_string()),
            status: Some(ReportedStatus::Working),
            exited: None,
            cwd: PathBuf::from("C:\\rust-projects\\orion"),
            program: "claude".to_string(),
        }
    }

    #[test]
    fn the_records_survive_a_round_trip() {
        let dir = temp_dir("records");
        let store = RuntimeStore::at(dir.clone());
        let saved = vec![record("0000000a"), record("0000000b")];

        store.save_terminals(&saved).expect("the records are saved");
        let read = store.load_terminals();

        assert_eq!(read, saved);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_saved_screen_is_read_back() {
        let dir = temp_dir("screen");
        let store = RuntimeStore::at(dir.clone());
        let screen = "\x1b[2J\x1b[Hok\r\n";

        store
            .save_screen("0000000a", screen)
            .expect("the screen is saved");

        assert_eq!(store.load_screen("0000000a").as_deref(), Some(screen));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_terminal_id_cannot_escape_the_screens_directory() {
        let root = temp_dir("escape");
        let dir = root.join("state");
        let store = RuntimeStore::at(dir.clone());

        store
            .save_screen("../../escape", "screen")
            .expect("the screen is saved");

        // The id is a name, not a path: nothing lands above the screens directory.
        assert!(!root.join("escape.ansi").exists());
        assert!(!root.join("escape").exists());
        assert_eq!(store.load_screen("../../escape").as_deref(), Some("screen"));
        let _ = fs::remove_dir_all(root);
    }
}
