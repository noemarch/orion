use std::collections::HashMap;
use std::sync::Arc;

use orion_core::agents::AgentRegistry;
use orion_core::protocol::{AgentKind, DiscoveredAgent, TerminalSize, WorkspaceInfo};
use orion_core::pty::PtySession;
use orion_core::workspace::WorkspaceManager;
use parking_lot::RwLock;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::mpsc;
use tracing::{error, info};

pub struct AppState {
    pub workspace_manager: Arc<WorkspaceManager>,
    pub pty_sessions: Arc<RwLock<HashMap<String, Arc<PtySession>>>>,
}

#[tauri::command]
fn discover_agents() -> Vec<DiscoveredAgent> {
    AgentRegistry::discover_all()
}

#[tauri::command]
fn create_workspace(
    state: State<'_, AppState>,
    name: String,
    working_dir: String,
    agent_kind: AgentKind,
) -> WorkspaceInfo {
    state
        .workspace_manager
        .create_workspace(&name, &working_dir, agent_kind)
}

#[tauri::command]
fn list_workspaces(state: State<'_, AppState>) -> Vec<WorkspaceInfo> {
    state.workspace_manager.list_workspaces()
}

#[tauri::command]
async fn spawn_agent_pty(
    app: AppHandle,
    state: State<'_, AppState>,
    pane_id: String,
    command: String,
    args: Vec<String>,
    cwd: Option<String>,
    size: TerminalSize,
) -> Result<(), String> {
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(1024);

    let session = PtySession::spawn(
        &command,
        &args,
        cwd.as_deref(),
        &[],
        size,
        tx,
    )
    .map_err(|e| e.to_string())?;

    let session_arc = Arc::new(session);
    state.pty_sessions.write().insert(pane_id.clone(), session_arc);

    let pane_id_clone = pane_id.clone();
    let app_handle = app.clone();

    // Stream PTY output to frontend via Tauri event
    tokio::spawn(async move {
        let event_name = format!("orion://pty-output/{pane_id_clone}");
        while let Some(bytes) = rx.recv().await {
            let text = String::from_utf8_lossy(&bytes).to_string();
            if let Err(e) = app_handle.emit(&event_name, text) {
                error!("Failed to emit PTY output event: {e}");
                break;
            }
        }
        info!("PTY output loop ended for pane {pane_id_clone}");
    });

    Ok(())
}

#[tauri::command]
fn write_pty(state: State<'_, AppState>, pane_id: String, data: String) -> Result<(), String> {
    let sessions = state.pty_sessions.read();
    let session = sessions
        .get(&pane_id)
        .ok_or_else(|| format!("No active PTY session for pane {pane_id}"))?;

    session.write_all(data.as_bytes()).map_err(|e| e.to_string())
}

#[tauri::command]
fn resize_pty(state: State<'_, AppState>, pane_id: String, size: TerminalSize) -> Result<(), String> {
    let sessions = state.pty_sessions.read();
    let session = sessions
        .get(&pane_id)
        .ok_or_else(|| format!("No active PTY session for pane {pane_id}"))?;

    session.resize(size).map_err(|e| e.to_string())
}

#[tauri::command]
fn kill_pty(state: State<'_, AppState>, pane_id: String) -> Result<(), String> {
    let mut sessions = state.pty_sessions.write();
    if let Some(session) = sessions.remove(&pane_id) {
        session.kill().map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn run() {
    tracing_subscriber::fmt::init();
    info!("Starting Orion desktop application");

    let app_state = AppState {
        workspace_manager: Arc::new(WorkspaceManager::new()),
        pty_sessions: Arc::new(RwLock::new(HashMap::new())),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            discover_agents,
            create_workspace,
            list_workspaces,
            spawn_agent_pty,
            write_pty,
            resize_pty,
            kill_pty,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
