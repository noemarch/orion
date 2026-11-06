use std::env;
use std::path::PathBuf;
use std::process::Command;

use crate::protocol::{AgentKind, DiscoveredAgent};

pub struct AgentRegistry;

impl AgentRegistry {
    /// Discover all supported agent CLIs installed on the host system's PATH.
    pub fn discover_all() -> Vec<DiscoveredAgent> {
        let kinds = [
            AgentKind::Claude,
            AgentKind::Codex,
            AgentKind::Antigravity,
            AgentKind::OpenCode,
            AgentKind::Cursor,
            AgentKind::Grok,
            AgentKind::Omp,
            AgentKind::Omo,
            AgentKind::Pi,
            AgentKind::Hermes,
            AgentKind::Devin,
        ];

        kinds.iter().map(|kind| Self::inspect_agent(*kind)).collect()
    }

    /// Inspect a specific agent kind on PATH.
    pub fn inspect_agent(kind: AgentKind) -> DiscoveredAgent {
        let cmd_name = kind.default_command();
        let path = find_in_path(cmd_name);
        let installed = path.is_some();
        let version = if installed {
            Self::query_version(cmd_name)
        } else {
            None
        };

        DiscoveredAgent {
            kind,
            name: kind.display_name().to_string(),
            command: cmd_name.to_string(),
            path: path.map(|p| p.to_string_lossy().to_string()),
            installed,
            version,
        }
    }

    fn query_version(command: &str) -> Option<String> {
        let output = Command::new(command).arg("--version").output().ok()?;
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !stdout.is_empty() {
                return Some(stdout);
            }
        }
        None
    }
}

/// Locate an executable in the system PATH.
pub fn find_in_path(binary: &str) -> Option<PathBuf> {
    let path_var = env::var_os("PATH")?;
    for dir in env::split_paths(&path_var) {
        let full_path = dir.join(binary);
        if full_path.is_file() {
            return Some(full_path);
        }

        #[cfg(windows)]
        {
            let exe_path = dir.join(format!("{binary}.exe"));
            if exe_path.is_file() {
                return Some(exe_path);
            }
            let cmd_path = dir.join(format!("{binary}.cmd"));
            if cmd_path.is_file() {
                return Some(cmd_path);
            }
            let bat_path = dir.join(format!("{binary}.bat"));
            if bat_path.is_file() {
                return Some(bat_path);
            }
        }
    }
    None
}
