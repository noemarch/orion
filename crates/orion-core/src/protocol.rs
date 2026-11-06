use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Supported AI Agent kinds in Orion.
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
    Shell,
}

impl AgentKind {
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::Antigravity => "Antigravity",
            Self::OpenCode => "OpenCode",
            Self::Cursor => "Cursor CLI",
            Self::Grok => "Grok Build",
            Self::Omp => "omp",
            Self::Omo => "Oh My OpenAgent",
            Self::Pi => "pi",
            Self::Hermes => "Hermes",
            Self::Devin => "Devin CLI",
            Self::Shell => "Terminal Shell",
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
            Self::Shell => {
                #[cfg(windows)]
                {
                    "powershell.exe"
                }
                #[cfg(not(windows))]
                {
                    "bash"
                }
            }
        }
    }
}

/// Operational state of an agent session or pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Idle,
    Running,
    NeedsInput,
    Done,
    Error,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TabLayout {
    Single,
    SplitHorizontal,
    SplitVertical,
    Grid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneInfo {
    pub id: String,
    pub tab_id: String,
    pub title: String,
    pub agent_kind: AgentKind,
    pub status: AgentStatus,
    pub cwd: String,
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
    CommandApproval { command: String },
    FileEditApproval { file_path: String, diff: Option<String> },
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
