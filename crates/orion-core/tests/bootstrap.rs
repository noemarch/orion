//! Bootstrapping the native shell must go through the Engine Session.
//!
//! The GPUI shell starts its first agent with `Engine::spawn_pane`. A pane that exists only as a
//! process is invisible to `/api/session` and `/api/pane/conversation`, so the sidebar is empty
//! and the chat gets a 404. This test holds the seam to the promise: one spawn registers the
//! Workspace, Tab, and Pane, focuses them, and every route can find them.

use orion_core::engine::store::Store;
use orion_core::{AgentKind, Engine, LaunchSpec, PaneConfig, TerminalSize};
use serde_json::{json, Value};

/// A deterministic "agent": the test binary lists its own tests and exits. It is a real process
/// in a real PTY, but it needs no external CLI and leaves no transcript behind, so the
/// conversation route exercises its documented scrollback fallback.
fn fake_agent() -> LaunchSpec {
    LaunchSpec {
        program: std::env::current_exe().expect("the test binary's path"),
        args: vec!["--list".to_string()],
    }
}

fn engine_in_project(name: &str) -> (Engine, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("orion-bootstrap-{name}-{}", uuid::Uuid::new_v4()));
    let cwd = dir.join("project");
    std::fs::create_dir_all(&cwd).expect("project directory");
    (Engine::with_store(Store::at(dir.join("state"))), cwd)
}

fn pane_config(
    pane_id: &str,
    workspace_id: &str,
    tab_id: &str,
    cwd: &std::path::Path,
) -> PaneConfig {
    PaneConfig {
        id: pane_id.to_string(),
        workspace_id: workspace_id.to_string(),
        tab_id: tab_id.to_string(),
        agent: AgentKind::Claude,
        label: "Claude".to_string(),
        cwd: cwd.to_path_buf(),
        spec: fake_agent(),
        args: Vec::new(),
        size: TerminalSize {
            rows: 32,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        },
    }
}

#[test]
fn spawning_the_first_pane_registers_and_focuses_the_whole_session() {
    let (engine, cwd) = engine_in_project("first");

    let pane = engine
        .spawn_pane(pane_config(
            "pane-bootstrap",
            "workspace-bootstrap",
            "tab-bootstrap",
            &cwd,
        ))
        .expect("the pane starts");
    assert_eq!(pane.id, "pane-bootstrap");

    let (status, session) = engine.http("GET", "/api/session", &Value::Null);
    assert_eq!(status, 200, "session failed: {session}");
    let snapshot = &session["snapshot"];

    assert_eq!(
        snapshot["workspaces"].as_array().map(Vec::len),
        Some(1),
        "one workspace: {session}"
    );
    assert_eq!(
        snapshot["tabs"].as_array().map(Vec::len),
        Some(1),
        "one tab: {session}"
    );
    assert_eq!(
        snapshot["panes"].as_array().map(Vec::len),
        Some(1),
        "one pane: {session}"
    );
    assert_eq!(snapshot["focused_pane_id"], "pane-bootstrap");
    assert_eq!(
        snapshot["workspaces"][0]["label"], "project",
        "the workspace is named after its folder: {session}"
    );
    assert_eq!(snapshot["panes"][0]["label"], "Claude");
    assert_eq!(
        snapshot["focused_workspace_id"], "workspace-bootstrap",
        "the snapshot names the focused workspace: {session}"
    );
    assert_eq!(
        snapshot["focused_tab_id"], "tab-bootstrap",
        "the snapshot names the focused tab: {session}"
    );
    assert_eq!(snapshot["workspaces"][0]["focused"], true);
    assert_eq!(snapshot["tabs"][0]["focused"], true);
    assert_eq!(snapshot["panes"][0]["focused"], true);

    let (status, conversation) = engine.http(
        "GET",
        "/api/pane/conversation?pane_id=pane-bootstrap",
        &Value::Null,
    );
    assert_eq!(
        status, 200,
        "the conversation route finds the spawned pane: {conversation}"
    );
    assert!(
        conversation["turns"].as_array().is_some(),
        "the conversation answers a shape the Chat Lens reads: {conversation}"
    );
}

#[test]
fn focusing_a_pane_persists_the_selection_for_restart() {
    let dir = std::env::temp_dir().join(format!("orion-bootstrap-focus-{}", uuid::Uuid::new_v4()));
    let cwd = dir.join("project");
    std::fs::create_dir_all(&cwd).expect("project directory");
    let state = dir.join("state");
    let engine = Engine::with_store(Store::at(state.clone()));
    engine
        .spawn_pane(pane_config("pane-1", "ws-1", "tab-1", &cwd))
        .expect("the first pane starts");
    engine
        .spawn_pane(pane_config("pane-2", "ws-1", "tab-2", &cwd))
        .expect("the second pane starts");

    let (status, answer) = engine.http("POST", "/api/pane/focus", &json!({ "pane_id": "pane-1" }));
    assert_eq!(status, 200, "focus failed: {answer}");

    let saved = Store::at(state).load_session();
    assert_eq!(
        saved.focused_pane_id.as_deref(),
        Some("pane-1"),
        "the last selected pane is saved for restart"
    );
    assert_eq!(
        saved.workspaces[0].active_tab_id, "tab-1",
        "the workspace's active tab follows the selected pane"
    );
    assert_eq!(
        saved.workspaces[0].tabs[0].active_pane_id, "pane-1",
        "the tab remembers the pane the user selected"
    );
}

#[test]
fn closing_the_focused_pane_moves_focus_to_a_live_pane_and_drops_its_tab() {
    let (engine, cwd) = engine_in_project("close-pane");
    engine
        .spawn_pane(pane_config("pane-1", "ws-1", "tab-1", &cwd))
        .expect("the first pane starts");
    engine
        .spawn_pane(pane_config("pane-2", "ws-1", "tab-2", &cwd))
        .expect("the second pane starts");

    let (status, answer) = engine.http("POST", "/api/pane/close", &json!({ "pane_id": "pane-2" }));
    assert_eq!(status, 200, "close failed: {answer}");

    let (_, session) = engine.http("GET", "/api/session", &Value::Null);
    let snapshot = &session["snapshot"];
    assert_eq!(
        snapshot["focused_pane_id"], "pane-1",
        "the shell follows the pane that is still open: {session}"
    );
    assert_eq!(
        snapshot["tabs"].as_array().map(Vec::len),
        Some(1),
        "the tab whose last pane closed is gone: {session}"
    );
}

#[test]
fn closing_the_last_workspace_leaves_an_empty_session() {
    let (engine, cwd) = engine_in_project("close-workspace");
    engine
        .spawn_pane(pane_config("pane-1", "ws-1", "tab-1", &cwd))
        .expect("the pane starts");

    let (status, answer) = engine.http(
        "POST",
        "/api/workspace/close",
        &json!({ "workspace_id": "ws-1" }),
    );
    assert_eq!(status, 200, "close failed: {answer}");

    let (_, session) = engine.http("GET", "/api/session", &Value::Null);
    let snapshot = &session["snapshot"];
    assert_eq!(
        snapshot["workspaces"].as_array().map(Vec::len),
        Some(0),
        "no workspace is left to render: {session}"
    );
    assert_eq!(snapshot["panes"].as_array().map(Vec::len), Some(0));
    assert!(
        snapshot["focused_pane_id"].is_null(),
        "nothing is focused after the last workspace closes: {session}"
    );
}

#[test]
fn focusing_a_pane_in_another_workspace_switches_the_whole_selection() {
    let (engine, cwd) = engine_in_project("switch-workspace");
    engine
        .spawn_pane(pane_config("pane-1", "ws-1", "tab-1", &cwd))
        .expect("the first pane starts");
    engine
        .spawn_pane(pane_config("pane-2", "ws-2", "tab-2", &cwd))
        .expect("the second pane starts");

    let (status, answer) = engine.http("POST", "/api/pane/focus", &json!({ "pane_id": "pane-2" }));
    assert_eq!(status, 200, "focus failed: {answer}");

    let (_, session) = engine.http("GET", "/api/session", &Value::Null);
    let snapshot = &session["snapshot"];
    assert_eq!(snapshot["focused_pane_id"], "pane-2");
    assert_eq!(
        snapshot["focused_workspace_id"], "ws-2",
        "the focused workspace follows the pane: {session}"
    );
    assert_eq!(
        snapshot["workspaces"][0]["focused"], false,
        "the old workspace gives up focus: {session}"
    );
    assert_eq!(snapshot["workspaces"][1]["focused"], true);
    assert_eq!(
        snapshot["workspaces"][1]["active_tab_id"], "tab-2",
        "the new workspace's active tab is its tab: {session}"
    );
}

#[test]
fn a_pane_in_a_new_tab_moves_the_active_tab_with_focus() {
    let (engine, cwd) = engine_in_project("tabs");
    let _first = engine
        .spawn_pane(pane_config("pane-1", "ws-1", "tab-1", &cwd))
        .expect("the first pane starts");
    let _second = engine
        .spawn_pane(pane_config("pane-2", "ws-1", "tab-2", &cwd))
        .expect("the second pane starts");

    let (status, session) = engine.http("GET", "/api/session", &Value::Null);
    assert_eq!(status, 200, "session failed: {session}");
    let snapshot = &session["snapshot"];
    assert_eq!(
        snapshot["tabs"].as_array().map(Vec::len),
        Some(2),
        "the second tab was appended: {session}"
    );
    assert_eq!(snapshot["focused_pane_id"], "pane-2");
    assert_eq!(
        snapshot["focused_tab_id"], "tab-2",
        "focus names the new tab: {session}"
    );
    assert_eq!(
        snapshot["workspaces"][0]["active_tab_id"], "tab-2",
        "the workspace's active tab follows focus: {session}"
    );
}
