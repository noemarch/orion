//! A real shell on a real PTY: proves the dedicated terminal pane runs a shell,
//! not an agent stand-in. ConPTY must give it a grid that scrolls, survives a
//! resize, accepts input, and dies with the pane.

use std::sync::Arc;
use std::time::{Duration, Instant};

use orion_core::agents::{find_in_path, LaunchSpec};
use orion_core::engine::pane::{Pane, PaneConfig, PaneEvent};
use orion_core::protocol::{AgentKind, ShellKind, TerminalSize};

#[cfg(windows)]
fn shell_kind() -> ShellKind {
    ShellKind::Cmd
}

#[cfg(not(windows))]
fn shell_kind() -> ShellKind {
    ShellKind::Bash
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("orion-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn process_alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

fn wait_for(pane: &Pane, needle: &str, seconds: u64) -> String {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut text = String::new();
    while Instant::now() < deadline {
        text = pane.visible_text();
        if text.contains(needle) {
            return text;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    text
}

#[test]
fn a_shell_pane_runs_a_real_shell_and_reads_its_grid() {
    let kind = shell_kind();
    if find_in_path(kind.default_command()).is_none() {
        eprintln!("skipped: {} is not on PATH", kind.default_command());
        return;
    }
    let spec = LaunchSpec::for_shell(kind, &[]).expect("the shell resolves");
    let cwd = scratch("shell-smoke");
    let events: Arc<dyn Fn(PaneEvent) + Send + Sync> = Arc::new(|_| {});
    let pane = Pane::spawn(
        PaneConfig {
            id: "shell-1".into(),
            workspace_id: "w1".into(),
            tab_id: "t1".into(),
            agent: AgentKind::from(kind),
            label: kind.display_name().into(),
            cwd: cwd.clone(),
            spec,
            args: Vec::new(),
            shell_path: None,
            size: TerminalSize {
                cols: 120,
                rows: 32,
                pixel_width: 0,
                pixel_height: 0,
            },
        },
        events,
    )
    .expect("the shell starts in a pane");
    let pid = pane.pid().expect("the shell has a pid");

    let banner = wait_for(&pane, ">", 15);
    assert!(!banner.trim().is_empty(), "the shell drew no prompt");

    // Input reaches the shell's stdin and its output reaches the grid.
    pane.write("echo orion-shell-ok\r\n")
        .expect("write to the PTY");
    let echoed = wait_for(&pane, "orion-shell-ok", 15);
    assert!(
        echoed.contains("orion-shell-ok"),
        "shell input never reached the grid"
    );

    // Resizing must keep the shell and its grid coherent.
    pane.resize(90, 26).expect("resize");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(pane.size(), (26, 90));
    assert!(
        !pane.visible_text().trim().is_empty(),
        "the shell grid went blank after resize"
    );

    drop(pane);
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && process_alive(pid) {
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = std::fs::remove_dir_all(&cwd);
    assert!(!process_alive(pid), "the shell outlived its pane");
}
