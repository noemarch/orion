use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use parking_lot::RwLock;
use tracing::{info, warn};
use uuid::Uuid;

use crate::error::{OrionError, Result};
use crate::protocol::{AgentKind, AgentStatus, PaneInfo, TabInfo, TabLayout, WorkspaceInfo};

pub struct WorkspaceManager {
    workspaces: Arc<RwLock<HashMap<String, WorkspaceInfo>>>,
    active_workspace_id: Arc<RwLock<Option<String>>>,
}

impl WorkspaceManager {
    pub fn new() -> Self {
        Self {
            workspaces: Arc::new(RwLock::new(HashMap::new())),
            active_workspace_id: Arc::new(RwLock::new(None)),
        }
    }

    /// Create a new workspace rooted at a working directory, launched with a specific AI Agent.
    pub fn create_workspace(
        &self,
        name: &str,
        working_dir: &str,
        agent_kind: AgentKind,
    ) -> WorkspaceInfo {
        let ws_id = Uuid::new_v4().to_string();
        let tab_id = Uuid::new_v4().to_string();
        let pane_id = Uuid::new_v4().to_string();

        let initial_pane = PaneInfo {
            id: pane_id.clone(),
            tab_id: tab_id.clone(),
            title: agent_kind.display_name().to_string(),
            agent_kind,
            status: AgentStatus::Idle,
            cwd: working_dir.to_string(),
            pid: None,
            git_branch: Self::detect_git_branch(working_dir),
            worktree_path: None,
            has_unread: false,
            plan_summary: None,
        };

        let initial_tab = TabInfo {
            id: tab_id.clone(),
            title: agent_kind.display_name().to_string(),
            panes: vec![initial_pane],
            active_pane_id: Some(pane_id),
            layout: TabLayout::Single,
        };

        let workspace = WorkspaceInfo {
            id: ws_id.clone(),
            name: name.to_string(),
            working_dir: working_dir.to_string(),
            tabs: vec![initial_tab],
            active_tab_id: Some(tab_id),
        };

        self.workspaces.write().insert(ws_id.clone(), workspace.clone());
        *self.active_workspace_id.write() = Some(ws_id);

        workspace
    }

    /// List all open workspaces.
    pub fn list_workspaces(&self) -> Vec<WorkspaceInfo> {
        self.workspaces.read().values().cloned().collect()
    }

    /// Get a specific workspace by ID.
    pub fn get_workspace(&self, id: &str) -> Option<WorkspaceInfo> {
        self.workspaces.read().get(id).cloned()
    }

    /// Split an agent pane inside a tab.
    pub fn split_pane(
        &self,
        workspace_id: &str,
        tab_id: &str,
        agent_kind: AgentKind,
        direction: TabLayout,
    ) -> Result<PaneInfo> {
        let mut wss = self.workspaces.write();
        let ws = wss
            .get_mut(workspace_id)
            .ok_or_else(|| OrionError::WorkspaceNotFound(workspace_id.to_string()))?;

        let tab = ws
            .tabs
            .iter_mut()
            .find(|t| t.id == tab_id)
            .ok_or_else(|| OrionError::Internal("Tab not found".into()))?;

        let new_pane_id = Uuid::new_v4().to_string();
        let new_pane = PaneInfo {
            id: new_pane_id.clone(),
            tab_id: tab_id.to_string(),
            title: agent_kind.display_name().to_string(),
            agent_kind,
            status: AgentStatus::Idle,
            cwd: ws.working_dir.clone(),
            pid: None,
            git_branch: Self::detect_git_branch(&ws.working_dir),
            worktree_path: None,
            has_unread: false,
            plan_summary: None,
        };

        tab.panes.push(new_pane.clone());
        tab.active_pane_id = Some(new_pane_id);
        tab.layout = direction;

        Ok(new_pane)
    }

    /// Create a dedicated Git worktree for an isolated agent task.
    pub fn create_git_worktree(
        &self,
        repo_root: &str,
        branch_name: &str,
    ) -> Result<PathBuf> {
        let worktree_dir = Path::new(repo_root)
            .join(".orion")
            .join("worktrees")
            .join(branch_name.replace('/', "-"));

        if let Some(parent) = worktree_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let output = Command::new("git")
            .current_dir(repo_root)
            .args([
                "worktree",
                "add",
                "-B",
                branch_name,
                worktree_dir.to_str().unwrap(),
            ])
            .output()
            .map_err(|e| OrionError::Process(format!("Failed to execute git worktree: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(OrionError::Process(format!(
                "Git worktree creation failed: {stderr}"
            )));
        }

        info!(
            "Created git worktree for branch '{}' at {:?}",
            branch_name, worktree_dir
        );
        Ok(worktree_dir)
    }

    /// Remove a Git worktree cleanly.
    pub fn remove_git_worktree(&self, repo_root: &str, worktree_path: &Path) -> Result<()> {
        let output = Command::new("git")
            .current_dir(repo_root)
            .args(["worktree", "remove", "--force", worktree_path.to_str().unwrap()])
            .output()
            .map_err(|e| OrionError::Process(format!("Failed to remove git worktree: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("Git worktree removal warning: {stderr}");
        }

        Ok(())
    }

    fn detect_git_branch(cwd: &str) -> Option<String> {
        let output = Command::new("git")
            .current_dir(cwd)
            .args(["branch", "--show-current"])
            .output()
            .ok()?;

        if output.status.success() {
            let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !branch.is_empty() {
                return Some(branch);
            }
        }
        None
    }
}
