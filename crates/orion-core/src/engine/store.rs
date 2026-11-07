//! Where Orion keeps what must survive a restart.
//!
//! Three documents, one directory: `workspaces.json` (what was open and which agent ran where),
//! `preferences.json` (the settings every window shares) and `devices.json` (paired phones, M3).
//! Every write goes to a sibling temp file and is renamed over the old one, so a crash mid-write
//! leaves the previous document intact instead of a half-written file the next start cannot read.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agents::LaunchSpec;
use crate::error::{OrionError, Result};
use crate::protocol::{AgentKind, TerminalSize};

/// The saved shape of one pane. Only what is needed to start the agent again: the terminal grid
/// is rebuilt from the agent's own output and transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedPane {
    pub id: String,
    pub agent: String,
    pub label: String,
    pub cwd: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Claude's session id, when it was learned: a restart resumes that conversation.
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedTab {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub number: u32,
    pub pane_ids: Vec<String>,
    pub active_pane_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedWorkspace {
    pub id: String,
    pub label: String,
    pub cwd: String,
    pub active_tab_id: String,
    pub tabs: Vec<SavedTab>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedSession {
    pub version: u32,
    #[serde(default)]
    pub workspaces: Vec<SavedWorkspace>,
    #[serde(default)]
    pub panes: Vec<SavedPane>,
    #[serde(default)]
    pub focused_pane_id: Option<String>,
}

impl Default for SavedSession {
    fn default() -> Self {
        Self {
            version: 1,
            workspaces: Vec::new(),
            panes: Vec::new(),
            focused_pane_id: None,
        }
    }
}

/// The directory Orion writes to: `%APPDATA%\Orion` on Windows, XDG data elsewhere.
pub fn default_data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("ORION_DATA_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        return PathBuf::from(appdata).join("Orion");
    }
    if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(data).join("orion");
    }
    home().unwrap_or_else(|| PathBuf::from(".")).join(".orion")
}

fn home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
}

/// One directory's worth of documents.
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

impl Default for Store {
    fn default() -> Self {
        Self::at(default_data_dir())
    }
}

impl Store {
    pub fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    pub fn read(&self, name: &str) -> Option<Value> {
        let bytes = fs::read(self.path(name)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Writes one document atomically: temp file, flush, rename.
    pub fn write(&self, name: &str, value: &Value) -> Result<()> {
        fs::create_dir_all(&self.dir).map_err(|error| {
            OrionError::Store(format!("failed to create {}: {error}", self.dir.display()))
        })?;
        let text = serde_json::to_vec_pretty(value)
            .map_err(|error| OrionError::Store(format!("failed to encode {name}: {error}")))?;
        let temp = self.path(&format!("{name}.tmp"));
        fs::write(&temp, &text).map_err(|error| {
            OrionError::Store(format!("failed to write {}: {error}", temp.display()))
        })?;
        fs::rename(&temp, self.path(name))
            .map_err(|error| OrionError::Store(format!("failed to replace {name}: {error}")))?;
        Ok(())
    }

    pub fn load_session(&self) -> SavedSession {
        self.read("workspaces.json")
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default()
    }

    pub fn save_session(&self, session: &SavedSession) -> Result<()> {
        let value = serde_json::to_value(session)
            .map_err(|error| OrionError::Store(format!("failed to encode the session: {error}")))?;
        self.write("workspaces.json", &value)
    }

    pub fn load_preferences(&self) -> Value {
        self.read("preferences.json").unwrap_or(Value::Null)
    }

    pub fn save_preferences(&self, value: &Value) -> Result<()> {
        self.write("preferences.json", value)
    }
}

/// The saved pane as a runnable launch: a saved session id wins, else the directory's latest
/// conversation (`--continue`), matching what a user would type after a restart.
pub fn spec_for_saved_pane(pane: &SavedPane) -> Result<LaunchSpec> {
    let kind = AgentKind::from_wire(&pane.agent).ok_or_else(|| {
        OrionError::AgentNotFound(format!("'{}' is not an agent Orion starts", pane.agent))
    })?;
    LaunchSpec::for_agent(
        kind,
        &pane.args,
        pane.session_id.as_deref(),
        pane.session_id.is_none(),
    )
}

pub const DEFAULT_SIZE: TerminalSize = TerminalSize {
    rows: 32,
    cols: 120,
    pixel_width: 0,
    pixel_height: 0,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_survives_a_write_and_read() {
        let dir = std::env::temp_dir().join(format!("orion-store-{}", uuid::Uuid::new_v4()));
        let store = Store::at(dir.clone());
        let session = SavedSession {
            version: 1,
            workspaces: vec![SavedWorkspace {
                id: "w1".into(),
                label: "orion".into(),
                cwd: "C:\\rust-projects\\orion".into(),
                active_tab_id: "t1".into(),
                tabs: vec![SavedTab {
                    id: "t1".into(),
                    label: "orion".into(),
                    number: 1,
                    pane_ids: vec!["p1".into()],
                    active_pane_id: "p1".into(),
                }],
            }],
            panes: vec![SavedPane {
                id: "p1".into(),
                agent: "claude".into(),
                label: "claude".into(),
                cwd: "C:\\rust-projects\\orion".into(),
                args: vec!["--permission-mode".into(), "bypassPermissions".into()],
                session_id: Some("abc".into()),
            }],
            focused_pane_id: Some("p1".into()),
        };
        store.save_session(&session).expect("write");
        let read = store.load_session();
        assert_eq!(read.workspaces.len(), 1);
        assert_eq!(read.panes[0].session_id.as_deref(), Some("abc"));
        // the temp file is gone: the rename consumed it
        assert!(!dir.join("workspaces.json.tmp").exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn an_unknown_agent_in_the_saved_session_is_refused_not_guessed() {
        let pane = SavedPane {
            id: "p1".into(),
            agent: "not-an-agent".into(),
            label: "x".into(),
            cwd: ".".into(),
            args: vec![],
            session_id: None,
        };
        assert!(spec_for_saved_pane(&pane).is_err());
    }
}
