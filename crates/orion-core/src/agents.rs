use std::env;
use std::path::{Path, PathBuf};

use crate::error::{OrionError, Result};
use crate::process::silent_command;
use crate::protocol::{AgentKind, DiscoveredAgent, ShellKind};

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

    /// Resolves a user-invoked shell. The shell's own default arguments come first, then any
    /// extra arguments; no agent resume flags are ever added.
    pub fn for_shell(kind: ShellKind, extra_args: &[String]) -> Result<Self> {
        let command = kind.default_command();
        let path = find_in_path(command).ok_or_else(|| {
            OrionError::AgentNotFound(format!("'{command}' is not on this PC's PATH"))
        })?;
        let mut args: Vec<String> = kind
            .default_args()
            .iter()
            .map(|arg| (*arg).to_string())
            .collect();
        args.extend(extra_args.iter().cloned());
        Self::from_path(&path, &args)
    }

    /// Resolves a user-supplied shell executable at an absolute path. The path must name a file;
    /// Windows shims are resolved by [`LaunchSpec::from_path`].
    pub fn for_shell_path(path: &Path, args: &[String]) -> Result<Self> {
        if !path.is_file() {
            return Err(OrionError::AgentNotFound(format!(
                "'{}' is not a file on this PC",
                path.display()
            )));
        }
        Self::from_path(path, args)
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

    /// Wraps an agent launch in a shell so the shell stays alive after the agent exits.
    pub fn wrap_agent_in_shell(
        agent: &Self,
        shell: &ShellKind,
        shell_path: Option<&Path>,
    ) -> Result<(Self, String)> {
        wrap_agent_in_shell(agent, shell, shell_path)
    }

    /// Builds a shell-rooted launch around an already-composed inner command.
    ///
    /// A restart has the command a pane was launched with, not the `LaunchSpec` it came from;
    /// this keeps the shell flags in one place so a fresh spawn and a restart agree.
    pub fn for_shell_command(
        inner: &str,
        shell: &ShellKind,
        shell_path: Option<&Path>,
    ) -> Result<Self> {
        reject_control_chars(inner)?;
        let program = match shell_path {
            Some(path) => {
                if !path.is_file() {
                    return Err(OrionError::AgentNotFound(format!(
                        "'{}' is not a file on this PC",
                        path.display()
                    )));
                }
                path.to_path_buf()
            }
            None => find_in_path(shell.default_command()).ok_or_else(|| {
                OrionError::AgentNotFound(format!(
                    "'{}' is not on this PC's PATH",
                    shell.default_command()
                ))
            })?,
        };
        let args = match shell {
            ShellKind::PowerShell | ShellKind::Pwsh => vec![
                "-NoLogo".to_string(),
                "-NoExit".to_string(),
                "-Command".to_string(),
                inner.to_string(),
            ],
            ShellKind::Cmd => vec!["/D".to_string(), "/K".to_string(), inner.to_string()],
            ShellKind::Bash | ShellKind::Zsh => vec![
                "-c".to_string(),
                format!("{}; exec {}", inner, shell.default_command()),
            ],
        };
        Ok(Self { program, args })
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

impl From<ShellKind> for AgentKind {
    fn from(kind: ShellKind) -> Self {
        match kind {
            ShellKind::PowerShell => Self::PowerShell,
            ShellKind::Pwsh => Self::Pwsh,
            ShellKind::Bash => Self::Bash,
            ShellKind::Cmd => Self::Cmd,
            ShellKind::Zsh => Self::Zsh,
        }
    }
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

    /// Discover the shells Orion can open as terminal panes. Kept separate from
    /// [`AgentRegistry::discover_all`] so `/api/agents` stays agent-only.
    pub fn discover_shells() -> Vec<DiscoveredAgent> {
        ShellKind::ALL
            .iter()
            .map(|kind| Self::inspect_shell(*kind))
            .collect()
    }

    fn inspect_shell(kind: ShellKind) -> DiscoveredAgent {
        let command = kind.default_command();
        let path = find_in_path(command);
        let installed = path.is_some();
        DiscoveredAgent {
            kind: kind.into(),
            name: kind.display_name().to_string(),
            command: command.to_string(),
            path: path.map(|p| p.to_string_lossy().to_string()),
            installed,
            version: None,
        }
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
        let output = silent_command(command).arg("--version").output().ok()?;
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

/// Wraps an agent launch in a shell so the shell stays alive after the agent exits.
///
/// Returns the shell launch spec (the PTY root) and the composed inner agent command string.
pub fn wrap_agent_in_shell(
    agent: &LaunchSpec,
    shell: &ShellKind,
    shell_path: Option<&Path>,
) -> Result<(LaunchSpec, String)> {
    let inner = compose_inner_command(agent, shell)?;
    let spec = LaunchSpec::for_shell_command(&inner, shell, shell_path)?;
    Ok((spec, inner))
}

fn compose_inner_command(agent: &LaunchSpec, shell: &ShellKind) -> Result<String> {
    let program = agent.program.to_string_lossy();
    reject_control_chars(&program)?;
    for arg in &agent.args {
        reject_control_chars(arg)?;
    }
    match shell {
        ShellKind::PowerShell | ShellKind::Pwsh => {
            let mut out = String::from("& ");
            out.push_str(&quote_powershell(&program));
            for arg in &agent.args {
                out.push(' ');
                out.push_str(&quote_powershell(arg));
            }
            Ok(out)
        }
        ShellKind::Cmd => {
            let mut parts = vec![quote_cmd(&program)];
            for arg in &agent.args {
                parts.push(quote_cmd(arg));
            }
            Ok(parts.join(" "))
        }
        ShellKind::Bash | ShellKind::Zsh => {
            let mut parts = vec![quote_bash(&program)];
            for arg in &agent.args {
                parts.push(quote_bash(arg));
            }
            Ok(parts.join(" "))
        }
    }
}

fn reject_control_chars(value: &str) -> Result<()> {
    if value.chars().any(char::is_control) {
        return Err(OrionError::Process(
            "control characters are not allowed in a shell command".to_string(),
        ));
    }
    Ok(())
}

fn quote_powershell(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn quote_cmd(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn quote_bash(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn for_shell_resolves_a_shell_and_keeps_default_args() {
        #[cfg(windows)]
        let kind = ShellKind::PowerShell;
        #[cfg(not(windows))]
        let kind = ShellKind::Bash;

        let spec = LaunchSpec::for_shell(kind, &[]).expect("shell on PATH");
        assert!(spec
            .program
            .to_string_lossy()
            .to_ascii_lowercase()
            .contains(kind.default_command()));
        for arg in kind.default_args() {
            assert!(spec.args.iter().any(|value| value == arg));
        }
    }

    #[test]
    fn for_shell_appends_extra_args_after_defaults() {
        #[cfg(windows)]
        let kind = ShellKind::PowerShell;
        #[cfg(not(windows))]
        let kind = ShellKind::Bash;

        let extra = vec!["-Extra".to_string()];
        let spec = LaunchSpec::for_shell(kind, &extra).expect("shell on PATH");
        let defaults = kind.default_args();
        assert_eq!(&spec.args[..defaults.len()], defaults);
        assert_eq!(&spec.args[defaults.len()..], &extra);
    }

    #[test]
    fn discover_shells_returns_five_entries() {
        let shells = AgentRegistry::discover_shells();
        assert_eq!(shells.len(), 5);
        assert_eq!(shells[0].kind, AgentKind::PowerShell);
        assert_eq!(shells[1].kind, AgentKind::Pwsh);
        assert_eq!(shells[2].kind, AgentKind::Bash);
        assert_eq!(shells[3].kind, AgentKind::Cmd);
        assert_eq!(shells[4].kind, AgentKind::Zsh);
        assert!(shells.iter().all(|shell| shell.version.is_none()));
    }

    #[test]
    fn for_shell_path_uses_the_supplied_executable() {
        let exe = std::env::current_exe().expect("the test binary's path");
        let spec = LaunchSpec::for_shell_path(&exe, &[]).expect("current exe is a file");
        assert_eq!(spec.program, exe);
    }

    #[test]
    fn for_shell_path_rejects_a_missing_file() {
        let missing = std::env::temp_dir().join(format!("orion-no-shell-{}", uuid::Uuid::new_v4()));
        assert!(LaunchSpec::for_shell_path(&missing, &[]).is_err());
    }

    #[test]
    fn wrapping_puts_the_shell_first_and_keeps_it_alive() {
        let exe = std::env::current_exe().expect("the test binary's path");
        let agent = LaunchSpec {
            program: PathBuf::from("C:\\agents\\orion-fake-agent.exe"),
            args: vec!["--resume".to_string(), "session-42".to_string()],
        };
        let (spec, inner) = wrap_agent_in_shell(&agent, &ShellKind::Cmd, Some(&exe))
            .expect("the agent wraps in the shell");
        assert_eq!(
            spec.program, exe,
            "the PTY root is the shell, not the agent"
        );
        assert_eq!(spec.args[0], "/D");
        assert_eq!(spec.args[1], "/K", "the shell stays after the agent exits");
        assert_eq!(spec.args[2], inner);
        assert!(
            inner.contains("orion-fake-agent"),
            "the inner command runs the agent: {inner}"
        );
        assert!(
            inner.contains("--resume") && inner.contains("session-42"),
            "the inner command keeps the resume flags: {inner}"
        );
    }

    #[test]
    fn powershell_wrapping_doubles_single_quotes() {
        let exe = std::env::current_exe().expect("the test binary's path");
        let agent = LaunchSpec {
            program: PathBuf::from("C:\\agents\\claude.exe"),
            args: vec!["it's fine".to_string()],
        };
        let (spec, inner) = wrap_agent_in_shell(&agent, &ShellKind::PowerShell, Some(&exe))
            .expect("the agent wraps in PowerShell");
        assert!(
            inner.starts_with("& '"),
            "the call operator is present: {inner}"
        );
        assert!(
            inner.contains("it''s fine"),
            "single quotes are doubled: {inner}"
        );
        assert!(spec.args.iter().any(|arg| arg == "-NoExit"));
        assert_eq!(spec.args.last(), Some(&inner));
    }

    #[test]
    fn cmd_wrapping_doubles_double_quotes() {
        let exe = std::env::current_exe().expect("the test binary's path");
        let agent = LaunchSpec {
            program: PathBuf::from("C:\\agents\\claude.cmd"),
            args: vec!["say \"hi\"".to_string()],
        };
        let (_, inner) = wrap_agent_in_shell(&agent, &ShellKind::Cmd, Some(&exe))
            .expect("the agent wraps in cmd");
        assert!(
            inner.contains("say \"\"hi\"\""),
            "double quotes are doubled: {inner}"
        );
    }

    #[test]
    fn a_command_with_control_characters_is_refused() {
        let exe = std::env::current_exe().expect("the test binary's path");
        let agent = LaunchSpec {
            program: PathBuf::from("claude\n.exe"),
            args: Vec::new(),
        };
        assert!(wrap_agent_in_shell(&agent, &ShellKind::Cmd, Some(&exe)).is_err());
    }

    #[test]
    fn a_stored_command_rebuilds_the_same_shell_spec() {
        let exe = std::env::current_exe().expect("the test binary's path");
        let agent = LaunchSpec {
            program: PathBuf::from("claude"),
            args: vec!["--continue".to_string()],
        };
        let (original, inner) = wrap_agent_in_shell(&agent, &ShellKind::Cmd, Some(&exe))
            .expect("the agent wraps in cmd");
        let rebuilt = LaunchSpec::for_shell_command(&inner, &ShellKind::Cmd, Some(&exe))
            .expect("the stored command rebuilds");
        assert_eq!(rebuilt, original, "a restart matches the original launch");
    }
}
