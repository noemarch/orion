//! The terminal startup timeline, measured.
//!
//! One run per child program, through the same path the shell uses: the
//! engine's pane, a real ConPTY, the `gpu-terminal` attachment, the PTY pump
//! and the terminal view's own render. Two children are measured:
//!
//! - a **control shell**, whose own startup is a few milliseconds, and
//! - an **agent CLI**, whose startup is what the reader waits for.
//!
//! The difference between the two runs is the agent CLI's own cost. What is
//! left in the control run is Orion's.
//!
//! Run it with:
//!
//! ```text
//! cargo test -j 2 -p orion-ui --test terminal_startup -- --nocapture
//! ```

use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::test::TestWindowExt;

use gpui_kit::{AppContext, Context, Entity, IntoElement, Render, Window};
use orion_core::agents::{find_in_path, LaunchSpec};
use orion_core::engine::pane::PaneConfig;
use orion_core::protocol::{AgentKind, TerminalSize};
use orion_core::trace::{self, Event};
use orion_core::{Engine, Role};

use orion_ui::app::{pty_sink, spawn_pty_pump};
use orion_ui::terminal::TerminalPane;

/// The root the harness mounts: the terminal view, nothing else.
struct TerminalRoot {
    view: Entity<TerminalPane>,
}

impl Render for TerminalRoot {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        self.view.clone()
    }
}

/// What one run measured.
struct Measurement {
    label: String,
    /// The whole timeline, in order.
    timeline: trace::Timeline,
    /// The time from the workspace open to the child's first byte.
    child_startup_ms: f64,
    /// The time from the PTY request to the child existing.
    pty_open_ms: f64,
    /// The time from the reader selecting the Terminal tab to the first frame
    /// with content on it.
    switch_ms: f64,
    /// The time from the workspace open to the first frame with content.
    to_first_frame_ms: f64,
}

/// The time between two events on the timeline.
fn gap(timeline: &trace::Timeline, from: Event, to: Event) -> f64 {
    let at = |event: Event| {
        timeline
            .records()
            .iter()
            .find(|record| record.event == event)
            .map(|record| record.at)
    };
    match (at(from), at(to)) {
        (Some(from), Some(to)) => to - from,
        _ => f64::NAN,
    }
}

/// Runs one child program through the shell's terminal path.
fn measure(
    cx: &mut gpui_kit::TestAppContext,
    engine: &Engine,
    label: &str,
    spec: LaunchSpec,
    agent: AgentKind,
    wait_for: Duration,
) -> Measurement {
    // The engine is the runtime: in the app it is a separate, already-running
    // process, so its setup is not part of the workspace-open path and is not
    // on the clock.
    let engine = engine.clone();

    // The reader asked for a workspace: the clock starts here.
    trace::begin();
    trace::record_with(
        Event::WorkspaceOpenClicked,
        Some(format!("{label} ({})", spec.program.to_string_lossy())),
    );

    // The shell reached its runtime.
    trace::record(Event::BackendConnected);

    // The pane is created: the PTY opens and the child starts.
    let cwd = std::env::current_dir().unwrap_or_default();
    let pane = engine
        .spawn_pane(PaneConfig {
            id: format!("pane-{label}"),
            workspace_id: "workspace-startup".to_string(),
            tab_id: "tab-startup".to_string(),
            agent,
            label: label.to_string(),
            cwd,
            spec,
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

    // The terminal view exists (the shell builds it when the app starts).
    let view = cx.update(|app| app.new(TerminalPane::new));

    // The shell attaches the pane and starts the pump.
    let (pty_rx, sink) = pty_sink();
    pane.attach("gpu-terminal", sink, Role::Interact, 120, 32, false)
        .expect("the pane attaches");
    cx.update(|app| {
        view.update(app, |view, cx| {
            view.set_pane(pane.clone());
            cx.notify();
        });
        spawn_pty_pump(view.clone(), pty_rx, Arc::new(AtomicU64::new(0)), app);
    });

    // Wait for the child's output to reach the grid. The terminal is not on
    // screen yet — the reader is still on the chat lens — so nothing is
    // painted here. Bytes are not enough: the attachment replays the screen
    // as escape sequences, so the wait is for readable content.
    let deadline = Instant::now() + wait_for;
    let mut fed = false;
    while Instant::now() < deadline {
        cx.run_until_parked();
        if cx.read(|app| view.read(app).has_content()) {
            fed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(fed, "{label}: no content reached the grid in {wait_for:?}");

    // The reader selects the Terminal tab. Only now does the view mount and
    // paint, which is what the shell does: the terminal element is not in the
    // tree while another lens is on screen.
    trace::record(Event::TerminalTabSelected);
    let (_root, vcx) = cx.add_window_view(move |_, _cx| TerminalRoot { view: view.clone() });
    vcx.update(|window, cx| window.render_frame(cx));

    let timeline = trace::snapshot();
    pane.session().kill();
    Measurement {
        label: label.to_string(),
        child_startup_ms: gap(&timeline, Event::PtySpawnReturned, Event::ChildFirstByte),
        pty_open_ms: gap(&timeline, Event::PtySpawnRequested, Event::PtySpawnReturned),
        switch_ms: gap(
            &timeline,
            Event::TerminalTabSelected,
            Event::FirstNonemptyFramePainted,
        ),
        to_first_frame_ms: gap(
            &timeline,
            Event::WorkspaceOpenClicked,
            Event::FirstNonemptyFramePainted,
        ),
        timeline,
    }
}

/// Prints one run's numbers.
fn report(measurement: &Measurement) {
    println!("\n=== {} ===", measurement.label);
    println!("{}", measurement.timeline.report());
    println!(
        "- PTY open + child start: {:.1} ms",
        measurement.pty_open_ms
    );
    println!(
        "- the child's own startup (spawn returned -> first byte): {:.1} ms",
        measurement.child_startup_ms
    );
    println!(
        "- terminal tab selected -> first frame with content: {:.1} ms",
        measurement.switch_ms
    );
    println!(
        "- workspace open -> first frame with content: {:.1} ms",
        measurement.to_first_frame_ms
    );
}

/// The control child: a plain shell. Its own startup is the floor.
fn control_spec() -> Option<(LaunchSpec, &'static str)> {
    if cfg!(windows) {
        Some((
            LaunchSpec::for_command("cmd.exe", &[]).ok()?,
            "control: cmd.exe",
        ))
    } else {
        Some((LaunchSpec::for_command("sh", &[]).ok()?, "control: sh"))
    }
}

/// The agent child: the CLI the reader waits for.
fn agent_spec() -> Option<(LaunchSpec, &'static str, AgentKind)> {
    let kind = AgentKind::Claude;
    match LaunchSpec::for_agent(kind, &[], None, false) {
        Ok(spec) => Some((spec, "agent: claude", kind)),
        Err(_) => None,
    }
}

orion_ui::orion_test!(
    fn the_terminal_startup_timeline_separates_the_agent_from_the_app(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        if find_in_path("claude").is_none() {
            eprintln!("skipped: claude is not on PATH");
            return;
        }
        // The timeline is on whether or not the reader set the variable.
        trace::force_enabled();
        // The pump waits on the channel, so the executor always has a task
        // outstanding: parking is allowed rather than a panic.
        cx.background_executor.allow_parking();

        let mut measurements = Vec::new();

        // A throwaway store: the measurement must not disturb the reader's own
        // saved session.
        let data_dir =
            std::env::temp_dir().join(format!("orion-startup-timeline-{}", std::process::id()));
        std::fs::create_dir_all(&data_dir).expect("a temp data dir");
        let engine = Engine::with_store(orion_core::engine::store::Store::at(data_dir.clone()));

        if let Some((spec, label)) = control_spec() {
            measurements.push(measure(
                cx,
                &engine,
                label,
                spec,
                AgentKind::Claude,
                Duration::from_secs(20),
            ));
        }
        if let Some((spec, label, kind)) = agent_spec() {
            measurements.push(measure(
                cx,
                &engine,
                label,
                spec,
                kind,
                // An agent CLI is slow to say anything, and slower still on a
                // loaded machine. The bound is generous because the measurement
                // is about the agent's own startup, not about a deadline.
                Duration::from_secs(120),
            ));
        } else {
            println!("\n=== agent: claude ===\nnot on this machine's PATH; skipped");
        }

        let _ = std::fs::remove_dir_all(&data_dir);

        assert!(
            !measurements.is_empty(),
            "at least the control shell must run"
        );
        for measurement in &measurements {
            report(measurement);
        }

        // The control run is the app's own cost: the PTY opens, the child starts
        // and the grid fills. It must be fast enough for the shell to feel alive.
        let control = measurements
            .iter()
            .find(|measurement| measurement.label.starts_with("control"))
            .expect("the control run happened");
        assert!(
            control.switch_ms < 200.0,
            "the terminal tab must show content within 200 ms, took {:.1} ms",
            control.switch_ms
        );
    }
);

/// One blocking call the shell makes, and how long it took.
struct Step {
    name: &'static str,
    millis: f64,
}

orion_ui::orion_test!(
    fn the_startup_path_costs_the_ui_thread(cx: &mut gpui_kit::TestAppContext) {
        if find_in_path("claude").is_none() {
            eprintln!("skipped: claude is not on PATH");
            return;
        }
        trace::force_enabled();
        cx.background_executor.allow_parking();

        // A throwaway store: the measurement must not depend on, or disturb, the
        // reader's own saved session.
        let data_dir = std::env::temp_dir().join(format!("orion-startup-{}", std::process::id()));
        std::fs::create_dir_all(&data_dir).expect("a temp data dir");
        let engine = Engine::with_store(orion_core::engine::store::Store::at(data_dir.clone()));

        // A throwaway folder: no workspace is open for it, so the create route
        // really starts an agent.
        let cwd = std::env::temp_dir().join(format!("orion-startup-cwd-{}", std::process::id()));
        std::fs::create_dir_all(&cwd).expect("a temp working dir");
        let mut steps: Vec<Step> = Vec::new();

        let mut timed = |name: &'static str, body: &mut dyn FnMut()| {
            let started = Instant::now();
            body();
            steps.push(Step {
                name,
                millis: started.elapsed().as_secs_f64() * 1000.0,
            });
        };

        timed("restore the saved session", &mut || engine.restore());
        timed("read the session snapshot", &mut || {
            let _ = engine.snapshot_value();
        });

        let mut pane_id = String::new();
        timed("create the workspace (spawn the agent)", &mut || {
            let (status, body) = engine.http(
                "POST",
                "/api/workspace/create",
                &serde_json::json!({
                    "cwd": cwd.to_string_lossy(),
                    "agent": { "kind": "claude" },
                }),
            );
            assert_eq!(status, 200, "the workspace opens: {body}");
            pane_id = body["pane_id"].as_str().unwrap_or_default().to_string();
        });
        assert!(!pane_id.is_empty(), "the workspace returned a pane");

        timed("look the pane up", &mut || {
            let _ = engine.pane_by_id(&pane_id);
        });
        timed("attach the terminal", &mut || {
            let (rx, sink) = pty_sink();
            let pane = engine.pane_by_id(&pane_id).expect("the pane is open");
            pane.attach("gpu-terminal", sink, Role::Interact, 120, 32, false)
                .expect("the pane attaches");
            drop(rx);
        });
        timed("read the conversation (the chat lens)", &mut || {
            let (status, _) = engine.http(
                "GET",
                &format!("/api/pane/conversation?pane_id={pane_id}"),
                &serde_json::Value::Null,
            );
            assert_eq!(status, 200);
        });
        timed("list the directories (the changes lens)", &mut || {
            let (status, _) = engine.http(
                "GET",
                &format!(
                    "/api/workspace/directories?path={}&files=1",
                    cwd.to_string_lossy()
                ),
                &serde_json::Value::Null,
            );
            assert_eq!(status, 200);
        });

        println!("\n=== the startup path, on the UI thread ===");
        let mut total = 0.0;
        for step in &steps {
            println!("- {:<44} {:>8.1} ms", step.name, step.millis);
            total += step.millis;
        }
        println!("- {:<44} {:>8.1} ms", "total", total);

        // The changed-file list is a git call. It used to run while the reader
        // was on the chat lens; it runs when the changes lens comes on screen
        // now, so it is measured apart.
        let started = Instant::now();
        let (status, _) = engine.http(
            "GET",
            &format!("/api/workspace/diff/files?pane_id={pane_id}"),
            &serde_json::Value::Null,
        );
        assert_eq!(status, 200);
        println!(
            "\n- {:<44} {:>8.1} ms",
            "list the changed files (changes lens only)",
            started.elapsed().as_secs_f64() * 1000.0
        );

        // The agent the workspace started must not outlive the measurement.
        if let Some(pane) = engine.pane_by_id(&pane_id) {
            pane.session().kill();
        }
        let _ = std::fs::remove_dir_all(&data_dir);
        let _ = std::fs::remove_dir_all(&cwd);
    }
);
