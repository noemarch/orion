use thiserror::Error;

#[derive(Error, Debug)]
pub enum OrionError {
    #[error("PTY error: {0}")]
    Pty(String),

    #[error("Process error: {0}")]
    Process(String),

    #[error("Agent not found: {0}")]
    AgentNotFound(String),

    #[error("Workspace not found: {0}")]
    WorkspaceNotFound(String),

    #[error("Pane not found: {0}")]
    PaneNotFound(String),

    #[error("Pane error: {0}")]
    Pane(String),

    #[error("Store error: {0}")]
    Store(String),

    #[error("Session transcript error: {0}")]
    Transcript(String),

    #[error("Authentication error: {0}")]
    Auth(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Internal error: {0}")]
    Internal(String),
}

pub type Result<T, E = OrionError> = std::result::Result<T, E>;
