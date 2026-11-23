//! The public seam for shell-first agent panes and in-place restart.
//!
//! The unit tests cover the wrapper and the route helpers. These two tests drive the same
//! behavior through `Engine`'s public API, the way the native shell does.

use std::time::{Duration, Instant};

use orion_core::agents::{find_in_path, LaunchSpec};
use orion_core::engine::store::Store;
use orion_core::engine::Engine;
use orion_core::protocol::{AgentKind, ShellKind, TerminalSize};
use orion_core::PaneConfig;
use serde_json::{json, Value};

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("orion-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

fn shell_kind() -> ShellKind {
    #[cfg(windows)]
    {
        ShellKind::Cmd
    }
    #[cfg(not(windows))]
    {
        ShellKind::Bash
    }
}

fn shell_program() -> std::path::PathBuf {
    find_in_path(shell_kind().default_command()).expect("a shell on PATH")
}

fn wait_for_exit(pane: &orion_core::engine::pane::Pane) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if pane.exited().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("the pane did not exit");
}

fn wait_for(label: &str, predicate: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if predicate() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for {label}");
}

#[test]
fn a_wrapped_agent_pane_keeps_the_shell_root_and_the_inner_command() {
    let dir = scratch("shell-first");
    let engine = Engine::with_store(Store::at(dir.join("state")));
    let shell = shell_program();
    let agent = LaunchSpec {
        program: std::path::PathBuf::from("C:\\agents\\claude.exe"),
        args: vec!["--resume".to_string(), "session-42".to_string()],
    };
    let (spec, inner) = LaunchSpec::wrap_agent_in_shell(&agent, &shell_kind(), Some(&shell))
        .expect("the agent wraps in the shell");
    assert_eq!(spec.program, shell, "the PTY root is the shell");

    let pane = engine
        .spawn_pane(PaneConfig {
            id: "p1".into(),
            workspace_id: "w1".into(),
            tab_id: "t1".into(),
            agent: AgentKind::Claude,
            label: "Claude".into(),
            cwd: dir.clone(),
            spec,
            args: Vec::new(),
            shell_path: Some(shell.to_string_lossy().to_string()),
            launch_command: Some(inner.clone()),
            size: TerminalSize {
                rows: 32,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            },
        })
        .expect("the pane starts");

    assert_eq!(pane.launch_command.as_deref(), Some(inner.as_str()));
    assert!(
        pane.launch_command
            .as_deref()
            .is_some_and(|command| command.contains("claude")),
        "the saved command runs the agent: {inner}"
    );
    assert_eq!(
        pane.shell_path.as_deref(),
        Some(shell.to_string_lossy().as_ref()),
        "the resolved shell path is kept on the pane"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn restart_returns_an_exited_pane_under_the_same_id() {
    let dir = scratch("restart-route");
    let engine = Engine::with_store(Store::at(dir.join("state")));
    let shell = shell_program();
    let (status, _) = engine.http(
        "PUT",
        "/api/preferences",
        &json!({
            "settings": {
                "defaultShell": shell_kind().wire_kind(),
                "shellPath": shell.to_string_lossy().to_string()
            }
        }),
    );
    assert_eq!(status, 200, "the shell preference is stored");

    #[cfg(windows)]
    let exit_args = vec!["/D".to_string(), "/C".to_string(), "exit 0".to_string()];
    #[cfg(not(windows))]
    let exit_args = vec!["-c".to_string(), "exit 0".to_string()];
    let pane = engine
        .spawn_pane(PaneConfig {
            id: "p-restart".into(),
            workspace_id: "w1".into(),
            tab_id: "t1".into(),
            agent: AgentKind::Claude,
            label: "Claude".into(),
            cwd: dir.clone(),
            spec: LaunchSpec {
                program: shell.clone(),
                args: exit_args,
            },
            args: Vec::new(),
            shell_path: None,
            launch_command: Some("fake-claude --resume abc".into()),
            size: TerminalSize {
                rows: 32,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            },
        })
        .expect("the pane starts");
    wait_for_exit(&pane);

    let (status, body) = engine.http(
        "POST",
        "/api/pane/restart",
        &json!({ "pane_id": "p-restart" }),
    );
    assert_eq!(status, 200, "restart failed: {body}");
    assert_eq!(body["pane_id"], "p-restart");
    assert_eq!(body["restarted"], true);

    let restarted = engine.pane_by_id("p-restart").expect("the pane is open");
    assert_eq!(
        restarted.launch_command.as_deref(),
        Some("fake-claude --resume abc")
    );
    assert_eq!(restarted.workspace_id, "w1");
    assert_eq!(restarted.tab_id, "t1");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn restart_refuses_a_pane_that_is_still_running() {
    let dir = scratch("restart-alive");
    let engine = Engine::with_store(Store::at(dir.join("state")));
    let pane = engine
        .spawn_pane(PaneConfig {
            id: "p-live".into(),
            workspace_id: "w1".into(),
            tab_id: "t1".into(),
            agent: AgentKind::from(shell_kind()),
            label: "shell".into(),
            cwd: dir.clone(),
            spec: LaunchSpec {
                program: shell_program(),
                args: Vec::new(),
            },
            args: Vec::new(),
            shell_path: None,
            launch_command: None,
            size: TerminalSize {
                rows: 32,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            },
        })
        .expect("the pane starts");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && pane.visible_text().trim().is_empty() {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !pane.visible_text().trim().is_empty(),
        "the shell drew a prompt"
    );

    let (status, body): (u16, Value) =
        engine.http("POST", "/api/pane/restart", &json!({ "pane_id": "p-live" }));
    assert_eq!(status, 409, "a live pane cannot be restarted: {body}");
    assert_eq!(body["error"]["code"], "pane_alive");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_inner_command_exit_returns_to_the_shell_prompt_in_the_same_pane() {
    let dir = scratch("prompt-returns");
    let engine = Engine::with_store(Store::at(dir.join("state")));
    let shell = shell_program();
    let run_id = uuid::Uuid::new_v4().simple().to_string();
    let exit_marker = format!("ORION_T6_EXIT_{run_id}");
    let prompt_marker = format!("ORION_T6_PROMPT_{run_id}");

    #[cfg(windows)]
    let inner = format!("echo {exit_marker} && set PROMPT={prompt_marker}$G");
    #[cfg(not(windows))]
    let inner = format!("export PS1='{prompt_marker}$ '; echo {exit_marker}");

    let spec = LaunchSpec::for_shell_command(&inner, &shell_kind(), Some(&shell))
        .expect("the inner command wraps in the shell");

    let pane = engine
        .spawn_pane(PaneConfig {
            id: "p-prompt".into(),
            workspace_id: "w1".into(),
            tab_id: "t1".into(),
            agent: AgentKind::Claude,
            label: "Claude".into(),
            cwd: dir.clone(),
            spec,
            args: Vec::new(),
            shell_path: Some(shell.to_string_lossy().to_string()),
            launch_command: Some(inner.clone()),
            size: TerminalSize {
                rows: 32,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            },
        })
        .expect("the pane starts");

    wait_for("the inner command's exit marker", || {
        pane.visible_text().contains(&exit_marker)
    });
    wait_for("the shell prompt after the inner command exited", || {
        pane.visible_text().contains(&prompt_marker)
    });

    let alive = engine
        .pane_by_id("p-prompt")
        .expect("the pane is still open under the same id");
    assert!(alive.exited().is_none(), "the shell is still running");

    let text = alive.visible_text();
    let exit_at = text
        .find(&exit_marker)
        .expect("the exit marker is on screen");
    let prompt_at = text
        .find(&prompt_marker)
        .expect("the prompt marker is on screen");
    assert!(
        prompt_at > exit_at,
        "the prompt is drawn after the inner command exited: {text}"
    );
    let _ = std::fs::remove_dir_all(dir);
}
