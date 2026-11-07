use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{OrionError, Result};
use crate::protocol::{AgentKind, DiscoveredAgent};

/// What to put in a PTY: a program and its arguments, already resolved past any platform shim.
///
/// On Windows an npm-installed CLI is a `.cmd` file, and `CreateProcess` cannot run one. Instead
/// of wrapping every agent in `cmd /c`, the shim is read and its JavaScript entry point is run by
/// `node.exe` directly: the pane's process *is* the agent, as the agent-only rule requires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
}

impl LaunchSpec {
    /// Resolves one of Orion's agent kinds, including a resume of a saved session.
    pub fn for_agent(
        kind: AgentKind,
        extra_args: &[String],
        resume: Option<&str>,
        resume_last: bool,
    ) -> Result<Self> {
        let command = kind.default_command();
        let mut args = Vec::new();
        if let Some(id) = resume {
            args.push("--resume".to_string());
            args.push(id.to_string());
        } else if resume_last {
            args.push("--continue".to_string());
        }
        args.extend(extra_args.iter().cloned());
        Self::for_command(command, &args)
    }

    /// Resolves an arbitrary command name or path. Used by agent kinds and by tests.
    pub fn for_command(command: &str, args: &[String]) -> Result<Self> {
        let path = find_in_path(command).ok_or_else(|| {
            OrionError::AgentNotFound(format!("'{command}' is not on this PC's PATH"))
        })?;
        Self::from_path(&path, args)
    }

    pub fn from_path(path: &Path, args: &[String]) -> Result<Self> {
        #[cfg(windows)]
        if let Some(extension) = path.extension().and_then(|value| value.to_str()) {
            let extension = extension.to_ascii_lowercase();
            if matches!(extension.as_str(), "cmd" | "bat") {
                if let Some(spec) = resolve_batch_shim(path, args) {
                    return Ok(spec);
                }
                // A shim that names no script: the only remaining way to run it is the shell it
                // was written for. This is the documented Windows fallback, never the first path.
                let shell = env::var_os("COMSPEC").unwrap_or_else(|| "cmd.exe".into());
                let mut shell_args = vec!["/D".to_string(), "/C".to_string()];
                shell_args.push(path.to_string_lossy().to_string());
                shell_args.extend(args.iter().cloned());
                return Ok(Self {
                    program: PathBuf::from(shell),
                    args: shell_args,
                });
            }
        }

        Ok(Self {
            program: path.to_path_buf(),
            args: args.to_vec(),
        })
    }
}

/// Reads a batch shim's JavaScript entry point and resolves the node that runs it.
#[cfg(windows)]
fn resolve_batch_shim(path: &Path, args: &[String]) -> Option<LaunchSpec> {
    let text = std::fs::read_to_string(path).ok()?;
    let dir = path.parent()?.to_path_buf();
    let mut entry = None;
    for token in text.split('"') {
        let token = token.trim();
        let lowered = token.to_ascii_lowercase();
        if !(lowered.ends_with(".js") || lowered.ends_with(".mjs") || lowered.ends_with(".cjs")) {
            continue;
        }
        if let Some(rest) = strip_dp0(token) {
            let candidate = dir.join(rest.trim_start_matches(['\\', '/']));
            if candidate.is_file() {
                entry = Some(candidate);
                break;
            }
        } else if token.contains("node_modules") {
            let candidate = dir.join(token.trim_start_matches(['\\', '/']));
            if candidate.is_file() {
                entry = Some(candidate);
                break;
            }
        }
    }
    let entry = entry?;
    let node = dir
        .join("node.exe")
        .is_file()
        .then(|| dir.join("node.exe"))
        .or_else(|| find_in_path("node"))?;
    let mut resolved = vec![entry.to_string_lossy().to_string()];
    resolved.extend(args.iter().cloned());
    Some(LaunchSpec {
        program: node,
        args: resolved,
    })
}

#[cfg(windows)]
fn strip_dp0(token: &str) -> Option<&str> {
    let lowered = token.to_ascii_lowercase();
    for prefix in ["%~dp0%", "%dp0%", "%~dp0"] {
        if lowered.starts_with(prefix) {
            return Some(&token[prefix.len()..]);
        }
    }
    None
}

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

        kinds
            .iter()
            .map(|kind| Self::inspect_agent(*kind))
            .collect()
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
        #[cfg(windows)]
        {
            // A batch file or executable is what Windows can start; an extensionless npm shell
            // script beside them is not. Check the runnable names first.
            for extension in ["exe", "cmd", "bat"] {
                let candidate = dir.join(format!("{binary}.{extension}"));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
            let exact = dir.join(binary);
            if exact.extension().is_some() && exact.is_file() {
                return Some(exact);
            }
            continue;
        }

        #[cfg(not(windows))]
        {
            let full_path = dir.join(binary);
            if full_path.is_file() {
                return Some(full_path);
            }
        }
    }
    None
}
