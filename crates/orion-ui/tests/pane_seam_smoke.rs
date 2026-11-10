//! The pane seam against a real agent: the write path works on a real PTY.
//!
//! The unit tests drive a recording double, which proves the lens's rules but
//! not that the seam reaches an agent. This one starts a real agent CLI and
//! interrupts it through the seam: the write goes down the same PTY the
//! reader's messages go down, and the agent must survive it. A machine
//! without the agent skips rather than fails.

use std::sync::Arc;
use std::time::{Duration, Instant};

use orion_core::agents::{find_in_path, LaunchSpec};
use orion_core::engine::pane::{Pane, PaneConfig, PaneEvent};
use orion_core::protocol::{AgentKind, TerminalSize};
use orion_ui::pane_seam::PaneSeam;

fn claude_spec() -> Option<LaunchSpec> {
    find_in_path("claude")?;
    LaunchSpec::for_agent(AgentKind::Claude, &[], None, false).ok()
}

/// A process that echoes what it is sent, so the wire can be checked.
fn echo_spec() -> Option<LaunchSpec> {
    let command = if cfg!(windows) { "more.com" } else { "cat" };
    LaunchSpec::for_command(command, &[]).ok()
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("orion-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

#[test]
fn the_seam_reaches_a_real_agent() {
    let Some(spec) = claude_spec() else {
        eprintln!("skipped: claude is not on PATH");
        return;
    };
    let cwd = scratch("seam-smoke");
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

    // The grid comes up: the PTY carries the agent's output.
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && pane.visible_text().trim().is_empty() {
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        !pane.visible_text().trim().is_empty(),
        "the agent drew nothing: the PTY is not working"
    );

    // The seam writes to that same PTY: an interrupt goes through it.
    let pane = Arc::new(pane);
    let seam: Arc<dyn PaneSeam> = pane.clone();
    seam.stop().expect("the interrupt goes through the seam");
    std::thread::sleep(Duration::from_millis(500));

    assert!(
        pane.exited().is_none(),
        "the interrupt ended the agent: the write did not go down the PTY"
    );

    drop(pane);
}

/// The bytes the seam writes come back off the PTY.
///
/// The agent stands in for the echo here: the point is the wire, not the
/// agent. What the lens sends has to reach the process on the other end of
/// the PTY, byte for byte.
#[test]
fn the_seam_writes_the_readers_words_to_the_process() {
    let Some(spec) = echo_spec() else {
        eprintln!("skipped: no echo command on PATH");
        return;
    };
    let cwd = scratch("seam-echo");
    let events: Arc<dyn Fn(PaneEvent) + Send + Sync> = Arc::new(|_| {});
    let pane = Pane::spawn(
        PaneConfig {
            id: "p1".into(),
            workspace_id: "w1".into(),
            tab_id: "t1".into(),
            agent: AgentKind::Claude,
            label: "echo".into(),
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
    .expect("the process starts in a pane");

    // The reader's words go down the wire.
    let pane = Arc::new(pane);
    let seam: Arc<dyn PaneSeam> = pane.clone();
    seam.submit("orion-seam-marker")
        .expect("the words go through the seam");

    // And they come back: the process echoed them.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut seen = false;
    while Instant::now() < deadline {
        if pane.visible_text().contains("orion-seam-marker") {
            seen = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        seen,
        "the words never reached the process: the seam did not write to the PTY"
    );

    drop(pane);
}
