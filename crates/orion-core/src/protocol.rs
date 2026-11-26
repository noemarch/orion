use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Shells Orion can open as a user-invoked terminal pane.
///
/// Shells are the one deliberate exception to the agent-only rule: they are opened only through
/// `POST /api/pane/shell` and never run agent status detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShellKind {
    PowerShell,
    Pwsh,
    Bash,
    Cmd,
    Zsh,
}

impl ShellKind {
    pub const ALL: [Self; 5] = [
        Self::PowerShell,
        Self::Pwsh,
        Self::Bash,
        Self::Cmd,
        Self::Zsh,
    ];

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::PowerShell => "Windows PowerShell",
            Self::Pwsh => "PowerShell 7",
            Self::Bash => "Bash",
            Self::Cmd => "Command Prompt",
            Self::Zsh => "Zsh",
        }
    }

    pub fn default_command(&self) -> &'static str {
        match self {
            Self::PowerShell => "powershell",
            Self::Pwsh => "pwsh",
            Self::Bash => "bash",
            Self::Cmd => "cmd",
            Self::Zsh => "zsh",
        }
    }

    pub fn default_args(&self) -> &'static [&'static str] {
        match self {
            Self::PowerShell | Self::Pwsh => &["-NoLogo"],
            Self::Bash | Self::Cmd | Self::Zsh => &[],
        }
    }

    pub fn wire_kind(&self) -> &'static str {
        match self {
            Self::PowerShell => "powershell",
            Self::Pwsh => "pwsh",
            Self::Bash => "bash",
            Self::Cmd => "cmd",
            Self::Zsh => "zsh",
        }
    }

    pub fn from_wire(kind: &str) -> Option<Self> {
        Some(match kind {
            "powershell" | "shell" => Self::PowerShell,
            "pwsh" | "powershell7" => Self::Pwsh,
            "bash" => Self::Bash,
            "cmd" => Self::Cmd,
            "zsh" => Self::Zsh,
            _ => return None,
        })
    }
}

/// Kinds of program a pane may launch.
///
/// A pane may launch an agent CLI or, as the one deliberate exception, a shell. Shells are opened
/// only through `POST /api/pane/shell` and never run agent status detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Claude,
    Codex,
    Antigravity,
    OpenCode,
    Cursor,
    Grok,
    Omp,
    Omo,
    Pi,
    Hermes,
    Devin,
    PowerShell,
    Pwsh,
    Bash,
    Cmd,
    Zsh,
}

impl AgentKind {
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::Antigravity => "Antigravity (agy)",
            Self::OpenCode => "OpenCode",
            Self::Cursor => "Cursor CLI",
            Self::Grok => "Grok Build",
            Self::Omp => "omp",
            Self::Omo => "Oh My OpenAgent",
            Self::Pi => "pi",
            Self::Hermes => "Hermes",
            Self::Devin => "Devin CLI",
            Self::PowerShell => ShellKind::PowerShell.display_name(),
            Self::Pwsh => ShellKind::Pwsh.display_name(),
            Self::Bash => ShellKind::Bash.display_name(),
            Self::Cmd => ShellKind::Cmd.display_name(),
            Self::Zsh => ShellKind::Zsh.display_name(),
        }
    }

    pub fn default_command(&self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Antigravity => "agy",
            Self::OpenCode => "opencode",
            Self::Cursor => "agent",
            Self::Grok => "grok",
            Self::Omp => "omp",
            Self::Omo => "omo",
            Self::Pi => "pi",
            Self::Hermes => "hermes",
            Self::Devin => "devin",
            Self::PowerShell => ShellKind::PowerShell.default_command(),
            Self::Pwsh => ShellKind::Pwsh.default_command(),
            Self::Bash => ShellKind::Bash.default_command(),
            Self::Cmd => ShellKind::Cmd.default_command(),
            Self::Zsh => ShellKind::Zsh.default_command(),
        }
    }

    /// The kind as the client names it (`AgentKind.kind` in `shared/protocol.ts`), which is also
    /// what a saved workspace stores.
    pub fn wire_kind(&self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Antigravity => "agy",
            Self::OpenCode => "opencode",
            Self::Cursor => "cursor",
            Self::Grok => "grok",
            Self::Omp => "omp",
            Self::Omo => "omo",
            Self::Pi => "pi",
            Self::Hermes => "hermes",
            Self::Devin => "devin",
            Self::PowerShell => ShellKind::PowerShell.wire_kind(),
            Self::Pwsh => ShellKind::Pwsh.wire_kind(),
            Self::Bash => ShellKind::Bash.wire_kind(),
            Self::Cmd => ShellKind::Cmd.wire_kind(),
            Self::Zsh => ShellKind::Zsh.wire_kind(),
        }
    }

    pub fn from_wire(kind: &str) -> Option<Self> {
        Some(match kind {
            "claude" => Self::Claude,
            "codex" => Self::Codex,
            "agy" | "antigravity" => Self::Antigravity,
            "opencode" => Self::OpenCode,
            "cursor" | "agent" => Self::Cursor,
            "grok" => Self::Grok,
            "omp" => Self::Omp,
            "omo" => Self::Omo,
            "pi" => Self::Pi,
            "hermes" => Self::Hermes,
            "devin" => Self::Devin,
            "powershell" | "shell" => Self::PowerShell,
            "pwsh" | "powershell7" => Self::Pwsh,
            "bash" => Self::Bash,
            "cmd" => Self::Cmd,
            "zsh" => Self::Zsh,
            _ => return None,
        })
    }

    /// Whether this kind is a user-invoked shell rather than an AI agent CLI.
    pub fn is_shell(&self) -> bool {
        matches!(
            self,
            Self::PowerShell | Self::Pwsh | Self::Bash | Self::Cmd | Self::Zsh
        )
    }

    /// The shell this kind represents, or `None` for an AI agent CLI.
    pub fn as_shell(&self) -> Option<ShellKind> {
        match self {
            Self::PowerShell => Some(ShellKind::PowerShell),
            Self::Pwsh => Some(ShellKind::Pwsh),
            Self::Bash => Some(ShellKind::Bash),
            Self::Cmd => Some(ShellKind::Cmd),
            Self::Zsh => Some(ShellKind::Zsh),
            _ => None,
        }
    }
}

/// Operational state of an agent session or pane.
///
/// The names are the wire's (`AgentStatus` in `shared/engine-types.ts`): the client paints
/// `working` as a running pulse, `blocked` as attention, `done` as a finished turn. `error` is a
/// detail of this engine; a client that does not know it draws the generic state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    Unknown,
    Error,
}

impl AgentStatus {
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Unknown => "unknown",
            Self::Error => "error",
        }
    }

    /// The inverse of [`AgentStatus::as_wire`]. `None` is a status this
    /// engine does not know, so each view decides its own fallback.
    pub fn from_wire(raw: &str) -> Option<Self> {
        match raw {
            "idle" => Some(Self::Idle),
            "working" => Some(Self::Working),
            "blocked" => Some(Self::Blocked),
            "done" => Some(Self::Done),
            "unknown" => Some(Self::Unknown),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

/// What a running program says about itself.
///
/// A program reports its own state; Orion never guesses it from the screen. A
/// terminal that has not reported has no status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReportedStatus {
    /// The program is working.
    Working,
    /// The program finished its work.
    Done,
    /// The program cannot go on without help.
    Blocked,
    /// The program failed.
    Error,
}

impl ReportedStatus {
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::Error => "error",
        }
    }

    /// The inverse of [`ReportedStatus::as_wire`]. `None` is a status this
    /// engine does not know, so the caller decides its own fallback.
    pub fn from_wire(raw: &str) -> Option<Self> {
        match raw {
            "working" => Some(Self::Working),
            "done" => Some(Self::Done),
            "blocked" => Some(Self::Blocked),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

/// Information about an installed and discovered agent CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredAgent {
    pub kind: AgentKind,
    pub name: String,
    pub command: String,
    pub path: Option<String>,
    pub installed: bool,
    pub version: Option<String>,
}

/// Workspace layout definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub id: String,
    pub name: String,
    pub working_dir: String,
    pub tabs: Vec<TabInfo>,
    pub active_tab_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabInfo {
    pub id: String,
    pub title: String,
    pub panes: Vec<PaneInfo>,
    pub active_pane_id: Option<String>,
    pub layout: TabLayout,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TabLayout {
    #[default]
    #[serde(rename = "single")]
    Single,
    #[serde(rename = "horizontal")]
    SplitHorizontal,
    #[serde(rename = "vertical")]
    SplitVertical,
    #[serde(rename = "grid")]
    Grid,
}

/// A pane is strictly a running AI Agent session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneInfo {
    pub id: String,
    pub tab_id: String,
    pub title: String,
    pub agent_kind: AgentKind,
    pub status: AgentStatus,
    pub cwd: String,
    /// RFC 3339 UTC time this pane's process was started.
    pub started_at: String,
    pub pid: Option<u32>,
    pub git_branch: Option<String>,
    pub worktree_path: Option<String>,
    pub has_unread: bool,
    pub plan_summary: Option<PlanSummary>,
}

/// Structured summary of an active agent plan or tasklist.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanSummary {
    pub total_steps: usize,
    pub completed_steps: usize,
    pub active_step_label: Option<String>,
    pub steps: Vec<PlanStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    pub id: String,
    pub label: String,
    pub status: PlanStepStatus,
    pub duration_seconds: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlanStepStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Skipped,
}

/// Interactive prompt or approval request intercepted from an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub id: String,
    pub pane_id: String,
    pub prompt_type: PromptType,
    pub question: String,
    pub description: Option<String>,
    pub options: Vec<PromptOption>,
    pub default_option_index: Option<usize>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptType {
    CommandApproval {
        command: String,
    },
    FileEditApproval {
        file_path: String,
        diff: Option<String>,
    },
    MultipleChoice,
    TextInput,
    ModelSelection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptOption {
    pub index: usize,
    pub label: String,
    pub description: Option<String>,
    pub is_danger: bool,
}

/// Response payload sent by the user to answer a prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalResponse {
    pub request_id: String,
    pub pane_id: String,
    pub selected_option_index: Option<usize>,
    pub text_answer: Option<String>,
    pub confirmed: bool,
}

/// Structured turn in an agent conversation transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationTurn {
    pub id: String,
    pub pane_id: String,
    pub role: TurnRole,
    pub content: String,
    pub tool_calls: Vec<ToolCallItem>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TurnRole {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallItem {
    pub id: String,
    pub tool_name: String,
    pub parameters: serde_json::Value,
    pub output: Option<String>,
    pub status: ToolCallStatus,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolCallStatus {
    Running,
    Success,
    Failed,
    Denied,
}

/// PTY dimensions for window size synchronization.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TerminalSize {
    pub rows: u16,
    pub cols: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

/// Remote pairing & Tailscale status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteAccessStatus {
    pub tailscale_active: bool,
    pub tailscale_ip: Option<String>,
    pub tailnet_url: Option<String>,
    pub port: u16,
    pub qr_code_svg: Option<String>,
    pub pairing_code: Option<String>,
}

/// The JSON payload inside every web push notification (RFC 8291).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PushPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    pub pane_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub title: String,
    pub body: String,
    pub tag: String,
}

#[cfg(test)]
mod tests {
    use super::{AgentKind, AgentStatus, PaneInfo, ReportedStatus, ShellKind};

    #[test]
    fn pane_info_serializes_started_at() {
        let info = PaneInfo {
            id: "p1".to_string(),
            tab_id: "t1".to_string(),
            title: "Build".to_string(),
            agent_kind: AgentKind::Claude,
            status: AgentStatus::Working,
            cwd: "/work/orion".to_string(),
            started_at: "2026-11-25T11:58:00Z".to_string(),
            pid: None,
            git_branch: None,
            worktree_path: None,
            has_unread: false,
            plan_summary: None,
        };
        let value = serde_json::to_value(&info).expect("pane info serializes");
        assert_eq!(
            value.get("started_at").and_then(serde_json::Value::as_str),
            Some("2026-11-25T11:58:00Z")
        );
    }

    #[test]
    fn shell_kinds_wire_names_round_trip() {
        assert_eq!(ShellKind::ALL.len(), 5);
        for kind in ShellKind::ALL {
            assert_eq!(ShellKind::from_wire(kind.wire_kind()), Some(kind));
            let agent: AgentKind = kind.into();
            assert_eq!(AgentKind::from_wire(agent.wire_kind()), Some(agent));
            assert_eq!(agent.as_shell(), Some(kind));
        }
    }

    #[test]
    fn zsh_round_trips_through_wire() {
        assert_eq!(ShellKind::from_wire("zsh"), Some(ShellKind::Zsh));
        assert_eq!(ShellKind::Zsh.wire_kind(), "zsh");
        assert_eq!(ShellKind::Zsh.display_name(), "Zsh");
        assert_eq!(ShellKind::Zsh.default_command(), "zsh");
        assert_eq!(AgentKind::from_wire("zsh"), Some(AgentKind::Zsh));
        assert_eq!(AgentKind::Zsh.wire_kind(), "zsh");
        assert_eq!(AgentKind::Zsh.as_shell(), Some(ShellKind::Zsh));
        assert!(AgentKind::Zsh.is_shell());
    }

    #[test]
    fn shell_kind_from_wire_accepts_aliases() {
        assert_eq!(ShellKind::from_wire("shell"), Some(ShellKind::PowerShell));
        assert_eq!(ShellKind::from_wire("powershell7"), Some(ShellKind::Pwsh));
        assert_eq!(AgentKind::from_wire("shell"), Some(AgentKind::PowerShell));
        assert_eq!(AgentKind::from_wire("powershell7"), Some(AgentKind::Pwsh));
        assert_eq!(ShellKind::from_wire("nope"), None);
    }

    #[test]
    fn shell_kind_default_args() {
        assert_eq!(ShellKind::PowerShell.default_args(), &["-NoLogo"]);
        assert_eq!(ShellKind::Pwsh.default_args(), &["-NoLogo"]);
        assert!(ShellKind::Bash.default_args().is_empty());
        assert!(ShellKind::Cmd.default_args().is_empty());
        assert!(ShellKind::Zsh.default_args().is_empty());
    }

    #[test]
    fn is_shell_is_true_only_for_shell_kinds() {
        for kind in ShellKind::ALL {
            let agent: AgentKind = kind.into();
            assert!(agent.is_shell(), "{agent:?} should be a shell");
        }
        for agent in [
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
        ] {
            assert!(!agent.is_shell(), "{agent:?} should not be a shell");
            assert_eq!(agent.as_shell(), None);
        }
    }

    #[test]
    fn agent_status_wire_names_round_trip() {
        for status in [
            AgentStatus::Idle,
            AgentStatus::Working,
            AgentStatus::Blocked,
            AgentStatus::Done,
            AgentStatus::Unknown,
            AgentStatus::Error,
        ] {
            assert_eq!(AgentStatus::from_wire(status.as_wire()), Some(status));
        }
        assert_eq!(AgentStatus::from_wire("compacting"), None);
    }

    #[test]
    fn reported_status_wire_names_round_trip() {
        for status in [
            ReportedStatus::Working,
            ReportedStatus::Done,
            ReportedStatus::Blocked,
            ReportedStatus::Error,
        ] {
            assert_eq!(ReportedStatus::from_wire(status.as_wire()), Some(status));
        }
        assert_eq!(ReportedStatus::from_wire("compacting"), None);
    }
}
