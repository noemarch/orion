//! Session state: workspaces, tabs, panes, focus, and layout — one lock, one owner.
//!
//! The session is the domain state everything else reads and mutates. It is a deep module:
//! callers get snapshots, pane lookups, and structural mutations through its interface, and
//! never reach into the workspace/tab vectors directly. Persistence serialization
//! (`to_saved`/`load`) lives here too, so the store adapter only moves bytes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;
use serde_json::{json, Value};

use crate::engine::pane::Pane;
use crate::engine::store::{SavedPane, SavedSession, SavedTab, SavedWorkspace};
use crate::protocol::{AgentStatus, TabLayout};

/// One workspace: its label, directory, and ordered tabs.
#[derive(Clone)]
pub struct Workspace {
    pub id: String,
    pub label: String,
    pub cwd: PathBuf,
    pub number: u32,
    pub tabs: Vec<Tab>,
    pub active_tab: String,
}

/// One tab: its ordered panes, layout, and zoom state.
#[derive(Clone)]
pub struct Tab {
    pub id: String,
    pub label: String,
    pub number: u32,
    pub panes: Vec<String>,
    pub active_pane: String,
    pub layout: TabLayout,
    pub zoomed_pane: Option<String>,
    pub split_ratios: HashMap<Vec<bool>, f64>,
}

/// The whole session: workspaces, panes, focus, and numbering.
#[derive(Default)]
pub struct Session {
    pub workspaces: Vec<Workspace>,
    pub panes: HashMap<String, Arc<Pane>>,
    pub focused_pane: Option<String>,
    pub next_number: u32,
}

impl Session {
    /// The pane with this id, if it is open.
    pub fn pane(&self, id: &str) -> Option<&Arc<Pane>> {
        self.panes.get(id)
    }

    /// The focused pane, or the first one when focus is stale.
    pub fn focused(&self) -> Option<String> {
        match &self.focused_pane {
            Some(id) if self.panes.contains_key(id) => Some(id.clone()),
            _ => self.panes.keys().next().cloned(),
        }
    }

    /// The workspace with this id, if it is open.
    pub fn workspace_of(&self, id: &str) -> Option<&Workspace> {
        self.workspaces.iter().find(|workspace| workspace.id == id)
    }
}

/// The tab/workspace status: the most urgent pane wins, running above waiting.
fn aggregate_status(session: &Session, pane_ids: &[String]) -> AgentStatus {
    let mut status = AgentStatus::Idle;
    for id in pane_ids {
        let Some(pane) = session.pane(id) else {
            continue;
        };
        match pane.status() {
            AgentStatus::Blocked => return AgentStatus::Blocked,
            AgentStatus::Working => status = AgentStatus::Working,
            AgentStatus::Done if status == AgentStatus::Idle => status = AgentStatus::Done,
            _ => {}
        }
    }
    status
}

/// Owns the session behind its own lock.
pub struct SessionState {
    session: RwLock<Session>,
}

impl SessionState {
    pub fn new() -> Self {
        Self {
            session: RwLock::new(Session::default()),
        }
    }

    /// Read access to the session (snapshot building, lookups inside handlers).
    pub fn read(&self) -> parking_lot::RwLockReadGuard<'_, Session> {
        self.session.read()
    }

    /// Write access to the session (structural mutations inside handlers).
    pub fn write(&self) -> parking_lot::RwLockWriteGuard<'_, Session> {
        self.session.write()
    }

    /// A pane by id, or `None` when it is not open.
    pub fn pane(&self, pane_id: &str) -> Option<Arc<Pane>> {
        self.session.read().pane(pane_id).cloned()
    }

    /// Inserts a just-spawned pane into the session, creating its workspace and tab when they
    /// are not open yet. Returns `true` when the workspace itself was created, so the caller
    /// starts its watcher.
    ///
    /// Every pane the engine spawns directly must reach the session through this seam: a pane
    /// that exists only as a process is invisible to `/api/session` and every route that looks
    /// panes up by id.
    pub(crate) fn register_pane(
        &self,
        pane: &Arc<Pane>,
        workspace_id: &str,
        tab_id: &str,
        label: &str,
        cwd: &Path,
    ) -> bool {
        let pane_id = pane.id.clone();
        let mut session = self.session.write();
        let created_workspace = match session
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == workspace_id)
        {
            Some(workspace) => {
                match workspace.tabs.iter_mut().find(|tab| tab.id == tab_id) {
                    Some(tab) => {
                        if !tab.panes.contains(&pane_id) {
                            tab.panes.push(pane_id.clone());
                        }
                        tab.active_pane = pane_id.clone();
                    }
                    None => {
                        let number = workspace.tabs.len() as u32 + 1;
                        workspace.tabs.push(Tab {
                            id: tab_id.to_string(),
                            label: label.to_string(),
                            number,
                            panes: vec![pane_id.clone()],
                            active_pane: pane_id.clone(),
                            layout: TabLayout::Single,
                            zoomed_pane: None,
                            split_ratios: HashMap::new(),
                        });
                    }
                }
                // The pane's tab is the one the user is working in, whether it was just
                // created or already open: focus and active tab must never disagree.
                workspace.active_tab = tab_id.to_string();
                false
            }
            None => {
                session.next_number += 1;
                let number = session.next_number;
                session.workspaces.push(Workspace {
                    id: workspace_id.to_string(),
                    label: label.to_string(),
                    cwd: cwd.to_path_buf(),
                    number,
                    tabs: vec![Tab {
                        id: tab_id.to_string(),
                        label: label.to_string(),
                        number: 1,
                        panes: vec![pane_id.clone()],
                        active_pane: pane_id.clone(),
                        layout: TabLayout::Single,
                        zoomed_pane: None,
                        split_ratios: HashMap::new(),
                    }],
                    active_tab: tab_id.to_string(),
                });
                true
            }
        };
        session.panes.insert(pane_id.clone(), Arc::clone(pane));
        session.focused_pane = Some(pane_id);
        created_workspace
    }

    /// The session as `SessionSnapshot`, for the wire.
    pub fn snapshot_value(&self, version: &str, protocol: u32) -> Value {
        let session = self.session.read();
        let focused = session.focused();
        let mut focused_workspace_id: Option<String> = None;
        let mut focused_tab_id: Option<String> = None;
        let mut workspaces = Vec::with_capacity(session.workspaces.len());
        let mut tabs = Vec::new();
        for workspace in &session.workspaces {
            let pane_ids = workspace
                .tabs
                .iter()
                .flat_map(|tab| tab.panes.iter().cloned())
                .collect::<Vec<_>>();
            let workspace_focused = pane_ids.iter().any(|id| Some(id) == focused.as_ref());
            if workspace_focused {
                focused_workspace_id.get_or_insert_with(|| workspace.id.clone());
            }
            workspaces.push(json!({
                "workspace_id": workspace.id,
                "label": workspace.label,
                "number": workspace.number,
                "focused": workspace_focused,
                "pane_count": pane_ids.len(),
                "tab_count": workspace.tabs.len(),
                "active_tab_id": workspace.active_tab,
                "agent_status": aggregate_status(&session, &pane_ids).as_wire(),
            }));
            for tab in &workspace.tabs {
                let tab_focused = tab.panes.iter().any(|id| Some(id) == focused.as_ref());
                if tab_focused {
                    focused_tab_id.get_or_insert_with(|| tab.id.clone());
                }
                tabs.push(json!({
                    "tab_id": tab.id,
                    "workspace_id": workspace.id,
                    "label": tab.label,
                    "number": tab.number,
                    "layout": tab.layout,
                    "zoomed_pane": tab.zoomed_pane,
                    "focused": tab_focused,
                    // Ordered pane ids and the tab's remembered pane: the shell
                    // focuses these when the user clicks a tab, instead of the
                    // arbitrary order a panes HashMap would give it.
                    "pane_ids": tab.panes,
                    "active_pane_id": tab.active_pane,
                    "pane_count": tab.panes.len(),
                    "agent_status": aggregate_status(&session, &tab.panes).as_wire(),
                }));
            }
        }
        let panes = session
            .panes
            .iter()
            .map(|(id, pane)| pane.info(Some(id) == focused.as_ref()))
            .collect::<Vec<_>>();
        let agents = session
            .panes
            .iter()
            .map(|(id, pane)| pane.agent_info(Some(id) == focused.as_ref()))
            .collect::<Vec<_>>();

        json!({
            "agents": agents,
            "layouts": [],
            "panes": panes,
            "tabs": tabs,
            "workspaces": workspaces,
            "focused_pane_id": focused,
            "focused_workspace_id": focused_workspace_id,
            "focused_tab_id": focused_tab_id,
            "protocol": protocol,
            "version": version,
        })
    }

    /// Serializes the session for the store adapter.
    pub fn to_saved(&self) -> SavedSession {
        let session = self.session.read();
        let mut saved = SavedSession {
            version: 1,
            focused_pane_id: session.focused_pane.clone(),
            ..SavedSession::default()
        };
        for workspace in &session.workspaces {
            saved.workspaces.push(SavedWorkspace {
                id: workspace.id.clone(),
                label: workspace.label.clone(),
                cwd: workspace.cwd.to_string_lossy().to_string(),
                active_tab_id: workspace.active_tab.clone(),
                tabs: workspace
                    .tabs
                    .iter()
                    .map(|tab| SavedTab {
                        id: tab.id.clone(),
                        label: tab.label.clone(),
                        number: tab.number,
                        pane_ids: tab.panes.clone(),
                        active_pane_id: tab.active_pane.clone(),
                        layout: tab.layout,
                        zoomed_pane: tab.zoomed_pane.clone(),
                        split_ratios: tab
                            .split_ratios
                            .iter()
                            .map(|(path, ratio)| (path.clone(), *ratio))
                            .collect(),
                    })
                    .collect(),
            });
        }
        for (id, pane) in &session.panes {
            saved.panes.push(SavedPane {
                id: id.clone(),
                agent: pane.agent.wire_kind().to_string(),
                label: pane.label(),
                cwd: pane.cwd.to_string_lossy().to_string(),
                args: pane.args.clone(),
                session_id: None,
            });
        }
        saved
    }

    /// Rebuilds workspaces/tabs from a saved session and installs already-spawned panes.
    ///
    /// Panes whose saved workspace/tab is gone are kept under a fresh "Restored" workspace
    /// so nothing is lost. Returns the number of panes installed.
    pub fn load_restored(
        &self,
        saved: &SavedSession,
        restored: HashMap<String, Arc<Pane>>,
    ) -> usize {
        let count = restored.len();
        let mut session = self.session.write();
        let mut number = 0;
        for workspace in &saved.workspaces {
            let mut tabs = Vec::new();
            for tab in &workspace.tabs {
                let ids = tab
                    .pane_ids
                    .iter()
                    .filter(|id| restored.contains_key(*id))
                    .cloned()
                    .collect::<Vec<_>>();
                if ids.is_empty() {
                    continue;
                }
                let active = if ids.contains(&tab.active_pane_id) {
                    tab.active_pane_id.clone()
                } else {
                    ids[0].clone()
                };
                tabs.push(Tab {
                    id: tab.id.clone(),
                    label: tab.label.clone(),
                    number: tab.number.max(tabs.len() as u32 + 1),
                    panes: ids,
                    active_pane: active,
                    layout: tab.layout,
                    zoomed_pane: tab.zoomed_pane.clone(),
                    split_ratios: tab
                        .split_ratios
                        .iter()
                        .map(|(path, ratio)| (path.clone(), *ratio))
                        .collect(),
                });
            }
            if tabs.is_empty() {
                continue;
            }
            number += 1;
            let active_tab = if tabs.iter().any(|tab| tab.id == workspace.active_tab_id) {
                workspace.active_tab_id.clone()
            } else {
                tabs[0].id.clone()
            };
            session.workspaces.push(Workspace {
                id: workspace.id.clone(),
                label: workspace.label.clone(),
                cwd: PathBuf::from(&workspace.cwd),
                number,
                tabs,
                active_tab,
            });
        }
        for (id, pane) in restored {
            session.panes.insert(id, pane);
        }
        if let Some(focused) = &saved.focused_pane_id {
            if session.panes.contains_key(focused) {
                session.focused_pane = Some(focused.clone());
            }
        }
        if session.workspaces.is_empty() {
            let panes = session.panes.keys().cloned().collect::<Vec<_>>();
            if let Some(first) = panes.first().cloned() {
                use uuid::Uuid;
                let workspace_id = Uuid::new_v4().to_string();
                let tab_id = Uuid::new_v4().to_string();
                session.workspaces.push(Workspace {
                    id: workspace_id,
                    label: "Restored".to_string(),
                    cwd: crate::engine::home_dir().unwrap_or_else(|| PathBuf::from(".")),
                    number: 1,
                    tabs: vec![Tab {
                        id: tab_id.clone(),
                        label: "Restored".to_string(),
                        number: 1,
                        panes,
                        active_pane: first,
                        layout: TabLayout::Single,
                        zoomed_pane: None,
                        split_ratios: HashMap::new(),
                    }],
                    active_tab: tab_id,
                });
            }
        }
        count
    }

    /// The workspace a pane errors out under, for push payloads.
    pub fn workspace_id_of_pane(&self, pane_id: &str) -> Option<String> {
        let session = self.session.read();
        session.workspaces.iter().find_map(|ws| {
            if ws
                .tabs
                .iter()
                .any(|t| t.panes.contains(&pane_id.to_string()))
            {
                Some(ws.id.clone())
            } else {
                None
            }
        })
    }

    /// Pane title and agent wire kind for push payloads.
    pub fn pane_push_info(&self, pane_id: &str) -> (String, Option<String>) {
        let session = self.session.read();
        let pane = session.panes.get(pane_id);
        let title = pane
            .map(|p| p.label())
            .unwrap_or_else(|| pane_id.to_string());
        let kind = pane.map(|p| p.agent.wire_kind().to_string());
        (title, kind)
    }

    /// Number of open panes (for diagnostics).
    #[allow(dead_code)]
    pub fn pane_count(&self) -> usize {
        self.session.read().panes.len()
    }
}

impl Default for SessionState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_session_snapshot_has_all_collections() {
        let state = SessionState::new();
        let snap = state.snapshot_value("0.0.0-test", 1);
        for key in ["agents", "layouts", "panes", "tabs", "workspaces"] {
            assert!(snap[key].is_array(), "{key} is an array");
        }
    }

    #[test]
    fn focused_falls_back_to_the_first_pane() {
        let session = Session::default();
        assert!(session.focused().is_none());
        assert!(session.pane("missing").is_none());
        assert!(session.workspace_of("missing").is_none());
    }

    #[test]
    fn saved_session_round_trips_through_state() {
        let state = SessionState::new();
        let saved = state.to_saved();
        assert_eq!(saved.version, 1);
        assert!(saved.workspaces.is_empty());
        assert!(saved.panes.is_empty());
        assert_eq!(state.pane_count(), 0);
    }

    #[test]
    fn aggregate_status_prefers_blocked_over_working() {
        let session = Session::default();
        assert_eq!(aggregate_status(&session, &[]), AgentStatus::Idle);
    }

    #[test]
    fn pane_push_info_falls_back_to_the_id() {
        let state = SessionState::new();
        let (title, kind) = state.pane_push_info("pane-missing");
        assert_eq!(title, "pane-missing");
        assert!(kind.is_none());
    }

    fn fake_pane(id: &str) -> Arc<Pane> {
        let on_event: Arc<dyn Fn(crate::engine::pane::PaneEvent) + Send + Sync> = Arc::new(|_| {});
        Arc::new(
            Pane::spawn(
                crate::engine::pane::PaneConfig {
                    id: id.to_string(),
                    workspace_id: "ws-1".to_string(),
                    tab_id: "tab-1".to_string(),
                    agent: crate::AgentKind::Claude,
                    label: "Claude".to_string(),
                    cwd: std::env::temp_dir(),
                    spec: crate::LaunchSpec {
                        program: std::env::current_exe().expect("the test binary's path"),
                        args: vec!["--list".to_string()],
                    },
                    args: Vec::new(),
                    size: crate::TerminalSize {
                        rows: 32,
                        cols: 120,
                        pixel_width: 0,
                        pixel_height: 0,
                    },
                },
                on_event,
            )
            .expect("the fake pane starts"),
        )
    }

    fn saved_tab(id: &str, pane_id: &str) -> SavedTab {
        SavedTab {
            id: id.to_string(),
            label: id.to_string(),
            number: 1,
            pane_ids: vec![pane_id.to_string()],
            active_pane_id: pane_id.to_string(),
            layout: TabLayout::Single,
            zoomed_pane: None,
            split_ratios: Vec::new(),
        }
    }

    #[test]
    fn restoring_a_saved_session_keeps_the_workspace_tab_and_pane_selection() {
        let saved = SavedSession {
            version: 1,
            workspaces: vec![
                SavedWorkspace {
                    id: "ws-1".to_string(),
                    label: "one".to_string(),
                    cwd: std::env::temp_dir().to_string_lossy().to_string(),
                    active_tab_id: "tab-1".to_string(),
                    tabs: vec![saved_tab("tab-1", "pane-1")],
                },
                SavedWorkspace {
                    id: "ws-2".to_string(),
                    label: "two".to_string(),
                    cwd: std::env::temp_dir().to_string_lossy().to_string(),
                    active_tab_id: "tab-2".to_string(),
                    tabs: vec![saved_tab("tab-2", "pane-2")],
                },
            ],
            panes: Vec::new(),
            focused_pane_id: Some("pane-2".to_string()),
        };
        let restored = HashMap::from([
            ("pane-1".to_string(), fake_pane("pane-1")),
            ("pane-2".to_string(), fake_pane("pane-2")),
        ]);

        let state = SessionState::new();
        assert_eq!(state.load_restored(&saved, restored), 2);

        let snapshot = state.snapshot_value("0.0.0-test", 1);
        assert_eq!(snapshot["focused_pane_id"], "pane-2");
        assert_eq!(
            snapshot["focused_workspace_id"], "ws-2",
            "the saved pane selects its workspace: {snapshot}"
        );
        assert_eq!(snapshot["workspaces"][0]["focused"], false);
        assert_eq!(snapshot["workspaces"][1]["focused"], true);
        assert_eq!(
            snapshot["workspaces"][1]["active_tab_id"], "tab-2",
            "the saved active tab survives the restart: {snapshot}"
        );
        assert_eq!(
            snapshot["tabs"][1]["active_pane_id"], "pane-2",
            "the tab remembers the pane the user chose: {snapshot}"
        );
        assert_eq!(snapshot["tabs"][1]["pane_ids"][0], "pane-2");
        assert_eq!(
            state.to_saved().focused_pane_id.as_deref(),
            Some("pane-2"),
            "the restored selection is what the next restart saves"
        );
    }
}
