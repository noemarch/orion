//! A real agent CLI on a real PTY: the M1 gate.
//!
//! These tests start Claude Code, not a stand-in. They prove the things a fake child cannot:
//! the Windows shim resolves to a process the pane owns, ConPTY gives the TUI a grid the engine
//! can read, resize does not lose the screen, Ctrl+C does not kill the TUI, and closing the pane
//! ends the agent's whole process tree. A machine without the agent skips rather than fails.

use std::sync::Arc;
use std::time::{Duration, Instant};

use orion_core::agents::{find_in_path, LaunchSpec};
use orion_core::engine::pane::{Pane, PaneConfig, PaneEvent};
use orion_core::engine::store::Store;
use orion_core::engine::Engine;
use orion_core::protocol::{AgentKind, TerminalSize};
use serde_json::{json, Value};

fn claude_spec() -> Option<LaunchSpec> {
    find_in_path("claude")?;
    LaunchSpec::for_agent(AgentKind::Claude, &[], None, false).ok()
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("orion-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    let output = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output();
    match output {
        Ok(output) => String::from_utf8_lossy(&output.stdout).contains(&pid.to_string()),
        Err(_) => false,
    }
}

#[cfg(not(windows))]
fn process_alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

fn wait_for_screen(pane: &Pane, seconds: u64) -> String {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut text = String::new();
    while Instant::now() < deadline {
        text = pane.visible_text();
        if !text.trim().is_empty() {
            return text;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    text
}

#[test]
fn claude_draws_a_grid_and_its_tree_dies_with_the_pane() {
    let Some(spec) = claude_spec() else {
        eprintln!("skipped: claude is not on PATH");
        return;
    };
    let cwd = scratch("agent-smoke");
    let events: Arc<dyn Fn(PaneEvent) + Send + Sync> = Arc::new(|_| {});
    let pane = Pane::spawn(
        PaneConfig {
            id: "p1".into(),
            workspace_id: "w1".into(),
            tab_id: "t1".into(),
            agent: AgentKind::Claude,
            label: "claude".into(),
            cwd: cwd.clone(),
            spec,
            args: Vec::new(),
            size: TerminalSize {
                cols: 120,
                rows: 32,
                pixel_width: 0,
                pixel_height: 0,
            },
        },
        events,
    )
    .expect("the agent starts in a pane");
    let pid = pane.pid().expect("the agent has a pid");

    let text = wait_for_screen(&pane, 30);
    assert!(
        !text.trim().is_empty(),
        "Claude drew nothing: the PTY or the grid is not working"
    );

    // A resize storm must leave one coherent grid, not lose the TUI.
    for (cols, rows) in [(100u16, 30u16), (90, 28), (110, 34), (80, 24), (120, 32)] {
        pane.resize(cols, rows).expect("resize");
    }
    assert_eq!(pane.size(), (32, 120));
    let after = pane.visible_text();
    assert!(
        !after.trim().is_empty(),
        "the screen went blank after resizing"
    );

    // Ctrl+C interrupts a turn; it must not end the TUI.
    pane.keys(&["ctrl+c".to_string()]).expect("ctrl+c");
    std::thread::sleep(Duration::from_millis(500));
    assert!(pane.exited().is_none(), "Ctrl+C ended the agent");

    drop(pane);
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && process_alive(pid) {
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = std::fs::remove_dir_all(&cwd);
    assert!(!process_alive(pid), "agent {pid} outlived its pane");
}

#[test]
fn the_http_route_starts_an_agent_and_reads_its_screen() {
    if claude_spec().is_none() {
        eprintln!("skipped: claude is not on PATH");
        return;
    }
    let dir = scratch("agent-route");
    let engine = Engine::with_store(Store::at(dir.join("data")));
    let cwd = dir.join("project");
    std::fs::create_dir_all(&cwd).expect("project directory");

    let (status, body) = engine.http(
        "POST",
        "/api/workspace/create",
        &json!({
            "cwd": cwd.to_string_lossy(),
            "label": "smoke",
            "agent": { "kind": "claude" },
        }),
    );
    assert_eq!(status, 200, "create failed: {body}");
    assert_eq!(body["agent_started"], true);
    let pane_id = body["pane_id"].as_str().expect("a pane id").to_string();

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut drawn = String::new();
    while Instant::now() < deadline {
        let (status, read) = engine.http(
            "GET",
            &format!("/api/pane/read?pane_id={pane_id}&source=visible"),
            &Value::Null,
        );
        assert_eq!(status, 200, "read failed: {read}");
        drawn = read["read"]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if !drawn.trim().is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    assert!(
        !drawn.trim().is_empty(),
        "the route started a pane but its grid stayed blank"
    );

    // The snapshot names the agent the pane runs, not a generic shell.
    let (_, session) = engine.http("GET", "/api/session", &Value::Null);
    let panes = session["snapshot"]["panes"].as_array().expect("panes");
    assert_eq!(panes.len(), 1);
    assert_eq!(panes[0]["pane_id"], pane_id);
    assert_eq!(panes[0]["agent"], "claude");

    let (status, _) = engine.http("POST", "/api/pane/close", &json!({ "pane_id": pane_id }));
    assert_eq!(status, 200);
    let (_, session) = engine.http("GET", "/api/session", &Value::Null);
    assert_eq!(
        session["snapshot"]["panes"]
            .as_array()
            .expect("panes")
            .len(),
        0
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn opencode_spec() -> Option<LaunchSpec> {
    find_in_path("opencode")?;
    LaunchSpec::for_agent(AgentKind::OpenCode, &[], None, false).ok()
}

#[test]
fn opencode_draws_a_grid_and_its_tree_dies_with_the_pane() {
    let Some(spec) = opencode_spec() else {
        eprintln!("skipped: opencode is not on PATH");
        return;
    };
    let cwd = scratch("opencode-smoke");
    let events: Arc<dyn Fn(PaneEvent) + Send + Sync> = Arc::new(|_| {});
    let pane = Pane::spawn(
        PaneConfig {
            id: "p1".into(),
            workspace_id: "w1".into(),
            tab_id: "t1".into(),
            agent: AgentKind::OpenCode,
            label: "opencode".into(),
            cwd: cwd.clone(),
            spec,
            args: Vec::new(),
            size: TerminalSize {
                cols: 120,
                rows: 32,
                pixel_width: 0,
                pixel_height: 0,
            },
        },
        events,
    )
    .expect("the agent starts in a pane");
    let pid = pane.pid().expect("the agent has a pid");

    let text = wait_for_screen(&pane, 45);
    assert!(
        !text.trim().is_empty(),
        "OpenCode drew nothing: the PTY or the grid is not working"
    );

    // A resize storm must leave one coherent grid, not lose the TUI.
    for (cols, rows) in [(100u16, 30u16), (90, 28), (110, 34), (80, 24), (120, 32)] {
        pane.resize(cols, rows).expect("resize");
    }
    assert_eq!(pane.size(), (32, 120));
    let after = pane.visible_text();
    assert!(
        !after.trim().is_empty(),
        "the screen went blank after resizing"
    );

    drop(pane);
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && process_alive(pid) {
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = std::fs::remove_dir_all(&cwd);
    assert!(!process_alive(pid), "agent {pid} outlived its pane");
}

fn omp_spec() -> Option<LaunchSpec> {
    find_in_path("omp")?;
    LaunchSpec::for_agent(AgentKind::Omp, &[], None, false).ok()
}

#[test]
fn omp_draws_a_grid_and_its_tree_dies_with_the_pane() {
    let Some(spec) = omp_spec() else {
        eprintln!("skipped: omp is not on PATH");
        return;
    };
    let cwd = scratch("omp-smoke");
    let events: Arc<dyn Fn(PaneEvent) + Send + Sync> = Arc::new(|_| {});
    let pane = Pane::spawn(
        PaneConfig {
            id: "p1".into(),
            workspace_id: "w1".into(),
            tab_id: "t1".into(),
            agent: AgentKind::Omp,
            label: "omp".into(),
            cwd: cwd.clone(),
            spec,
            args: Vec::new(),
            size: TerminalSize {
                cols: 120,
                rows: 32,
                pixel_width: 0,
                pixel_height: 0,
            },
        },
        events,
    )
    .expect("the agent starts in a pane");
    let pid = pane.pid().expect("the agent has a pid");

    let text = wait_for_screen(&pane, 45);
    assert!(
        !text.trim().is_empty(),
        "OMP drew nothing: the PTY or the grid is not working"
    );

    // A resize storm must leave one coherent grid, not lose the TUI.
    for (cols, rows) in [(100u16, 30u16), (90, 28), (110, 34), (80, 24), (120, 32)] {
        pane.resize(cols, rows).expect("resize");
    }
    assert_eq!(pane.size(), (32, 120));
    let after = pane.visible_text();
    assert!(
        !after.trim().is_empty(),
        "the screen went blank after resizing"
    );

    drop(pane);
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && process_alive(pid) {
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = std::fs::remove_dir_all(&cwd);
    assert!(!process_alive(pid), "agent {pid} outlived its pane");
}
