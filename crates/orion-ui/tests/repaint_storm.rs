//! The repaint-storm workload: a recorded agent TUI repaint storm replayed
//! through the real grid, plus a ≥ 2 MiB log replayed the same way and
//! through a live ConPTY.
//!
//! Every later perf verification step reuses this fixture. The numbers the
//! baseline doc records come from running this file with `--nocapture`:
//!
//! ```text
//! cargo test -p orion-ui --test repaint_storm -- --nocapture
//! ```
//!
//! Frame time is what `TerminalPane::render` records (element construction);
//! `window.draw` in the gpui test harness additionally runs layout and paint
//! against the test renderer. Neither is the GPU present time of the real
//! app — see `docs/perf/terminal-lag-baseline.md`.

use std::time::{Duration, Instant};

use orion_core::screen::ScreenState;

const FIXTURE: &[u8] = include_bytes!("fixtures/repaint_storm.ansi");

fn fixture_text() -> String {
    String::from_utf8(FIXTURE.to_vec()).expect("the fixture is valid UTF-8")
}

/// Split at char boundaries so no chunk ever tears a multi-byte sequence.
fn chunks(text: &str, size: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + size).min(text.len());
        while end < text.len() && !text.is_char_boundary(end) {
            end += 1;
        }
        out.push(&text[start..end]);
        start = end;
    }
    out
}

/// A deterministic ≥ 2 MiB styled log, the same shape an agent floods the
/// pane with when it `cat`s a big file.
fn big_log() -> String {
    let mut log = String::with_capacity(3 * 1024 * 1024);
    let mut i: u32 = 0;
    while log.len() < 5 * 1024 * 1024 / 2 {
        let r = (i.wrapping_mul(37).wrapping_add(11)) % 256;
        let g = (i.wrapping_mul(91).wrapping_add(7)) % 256;
        let b = (i.wrapping_mul(53).wrapping_add(29)) % 256;
        let level = ["INFO", "DEBUG", "WARN", "ERROR", "OK"][(i % 5) as usize];
        let color = ["38;5;39", "38;5;245", "38;5;214", "38;5;203", "38;5;78"][(i % 5) as usize];
        log.push_str(&format!(
            "\x1b[38;2;{r};{g};{b}m12:{:02}:{:02}.{:03}\x1b[0m \x1b[{color}m{level:<5}\x1b[0m worker-{} processed chunk {i:05} ({} bytes, rev {})\r\n",
            (i / 60) % 60,
            i % 60,
            (i.wrapping_mul(7)) % 1000,
            i % 8,
            (i.wrapping_mul(13)) % 997,
            i.wrapping_mul(3),
        ));
        i += 1;
    }
    log
}

#[test]
fn the_fixture_replays_through_the_grid_with_monotonic_revisions() {
    let text = fixture_text();
    let mut screen = ScreenState::new(32, 120);
    let mut last_revision = screen.revision();
    let mut feeds = 0u64;
    for chunk in chunks(&text, 16 * 1024) {
        let revision = screen.process(chunk);
        assert!(
            revision > last_revision,
            "revision must advance on every feed: {last_revision} -> {revision}"
        );
        last_revision = revision;
        feeds += 1;
    }
    // The fixture lives in the alternate screen and leaves it at the end,
    // which restores the (blank) main screen — so the full replay shows an
    // empty grid by design. What must hold is that every feed advanced the
    // revision, and that the grid had content before the leave sequence.
    assert_eq!(screen.revision(), feeds);
    let leave = text
        .rfind("\x1b[?1049l")
        .expect("the fixture leaves the alternate screen");
    let mut live = ScreenState::new(32, 120);
    live.process(&text[..leave]);
    assert!(
        !live.visible_text().trim().is_empty(),
        "the replay left content on the grid before the leave sequence"
    );
}

#[test]
fn the_fixture_is_byte_stable() {
    // Same input, same grid: two replays must produce identical cells.
    let text = fixture_text();
    let mut first = ScreenState::new(32, 120);
    let mut second = ScreenState::new(32, 120);
    for chunk in chunks(&text, 16 * 1024) {
        first.process(chunk);
        second.process(chunk);
    }
    assert_eq!(first.revision(), second.revision());
    assert_eq!(first.visible_text(), second.visible_text());
    assert_eq!(first.viewport_cells(0), second.viewport_cells(0));
}

#[test]
fn truncated_and_corrupt_sequences_are_absorbed() {
    let text = fixture_text();
    // Cut the fixture at points that land inside escape sequences and inside
    // multi-byte glyphs; the grid must absorb them without panicking.
    let truncate = |cut: usize| -> &str {
        let mut end = cut.min(text.len());
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        &text[..end]
    };
    for cut in [
        1,
        7,
        64,
        1024,
        text.len() / 3,
        text.len() / 2,
        text.len() - 3,
    ] {
        let mut screen = ScreenState::new(32, 120);
        screen.process(truncate(cut));
        assert!(
            screen.revision() >= 1,
            "a truncated feed still bumps revision"
        );
    }
    // Corrupt sequences: unterminated SGR, a private-mode sequence with no
    // final byte, and raw control bytes mid-stream.
    let mut screen = ScreenState::new(32, 120);
    screen.process("\x1b[38;2;255;100");
    screen.process("\x1b[?1049");
    screen.process("\x1b[");
    screen.process("\x07\x00\x1b]8;;\x07");
    screen.process(&text);
    assert!(screen.revision() >= 5);
}

#[test]
fn the_big_log_replays_through_the_grid() {
    let log = big_log();
    assert!(
        log.len() >= 2 * 1024 * 1024,
        "the big log is at least 2 MiB: {} bytes",
        log.len()
    );
    let mut screen = ScreenState::new(32, 120);
    let started = Instant::now();
    let mut feeds = 0u64;
    for chunk in chunks(&log, 16 * 1024) {
        screen.process(chunk);
        feeds += 1;
    }
    let wall = started.elapsed();
    println!(
        "big log replay: {} bytes in {feeds} feeds, wall {wall:?}, scrollback {}",
        log.len(),
        screen.scrollback_len()
    );
    assert!(screen.scrollback_len() > 1000);
}

/// The live-cat workload: a real ConPTY running `type`/`cat` on a ≥ 2 MiB
/// file, drained and fed into the grid as the bytes arrive.
#[test]
fn live_cat_through_a_pty() {
    use std::io::Read;

    let log = big_log();
    let dir = std::env::temp_dir().join(format!("orion-repaint-storm-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let path = dir.join("big.log");
    std::fs::write(&path, &log).expect("the big log is written");

    let pty_system = portable_pty::native_pty_system();
    let pair = pty_system
        .openpty(portable_pty::PtySize {
            rows: 32,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("a PTY opens");
    let mut command = if cfg!(windows) {
        let mut command = portable_pty::CommandBuilder::new("cmd");
        command.arg("/c");
        command.arg("type");
        command.arg(&path);
        command
    } else {
        let mut command = portable_pty::CommandBuilder::new("cat");
        command.arg(&path);
        command
    };
    command.env("TERM", "xterm-256color");
    let mut child = pair.slave.spawn_command(command).expect("the cat starts");
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().expect("the reader clones");
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 16 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(count) => {
                    if tx.send(buf[..count].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let mut screen = ScreenState::new(32, 120);
    let mut decoder = orion_core::pty::Utf8Decoder::default();
    let started = Instant::now();
    let mut total = 0usize;
    let mut feeds = 0u64;
    loop {
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(bytes) => {
                total += bytes.len();
                let text = decoder.push(&bytes);
                if !text.is_empty() {
                    screen.process(&text);
                    feeds += 1;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // ConPTY does not signal EOF on the output pipe after the
                // child exits; the tail of the stream stays in its buffer.
                // If the child is done and we already have ≥ 2 MiB, the
                // measurement is complete.
                match child.try_wait() {
                    Ok(Some(status)) => {
                        println!(
                            "cat exited with {status}; ConPTY held back the tail ({total} bytes received)"
                        );
                        break;
                    }
                    Ok(None) => panic!("cat still running but silent for 10s after {total} bytes"),
                    Err(error) => panic!("try_wait failed: {error}"),
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let tail = decoder.finish();
    if !tail.is_empty() {
        screen.process(&tail);
        feeds += 1;
    }
    let wall = started.elapsed();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);

    println!("live cat: {total} bytes in {feeds} feeds, wall {wall:?}");
    assert!(
        total >= 2 * 1024 * 1024,
        "expected at least 2 MiB through the PTY, got {total} bytes"
    );
}

#[cfg(debug_assertions)]
mod frame_times {
    use super::*;
    use gpui::{AppContext, Context, Entity, IntoElement, Render, Window};

    struct TerminalRoot {
        view: Entity<orion_ui::terminal::TerminalPane>,
    }

    impl Render for TerminalRoot {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            self.view.clone()
        }
    }

    fn replay_with_draws(
        cx: &mut gpui::TestAppContext,
        text: &str,
        chunk_size: usize,
    ) -> (Duration, orion_ui::perf::PerfStats) {
        let (root, cx) = cx.add_window_view(|_, cx| TerminalRoot {
            view: cx.new(orion_ui::terminal::TerminalPane::new),
        });
        orion_ui::perf::reset();
        let started = Instant::now();
        for chunk in chunks(text, chunk_size) {
            cx.update(|_window, app| {
                let view = root.read(app).view.clone();
                view.update(app, |view, cx| {
                    view.feed(chunk);
                    cx.notify();
                });
            });
            cx.update(|window, cx| window.draw(cx).clear());
        }
        let wall = started.elapsed();
        (wall, orion_ui::perf::snapshot())
    }

    fn report(label: &str, wall: Duration, stats: &orion_ui::perf::PerfStats) {
        let feed = orion_ui::perf::summarize(&stats.feeds);
        let frame = orion_ui::perf::summarize(&stats.frames);
        match (feed, frame) {
            (Some((feed_p50, feed_p95, feed_total)), Some((frame_p50, frame_p95, frame_total))) => {
                println!(
                    "{label}: wall {wall:?} | feeds n={} p50 {feed_p50:?} p95 {feed_p95:?} total {feed_total:?} | frames n={} p50 {frame_p50:?} p95 {frame_p95:?} total {frame_total:?}",
                    stats.feeds.len(),
                    stats.frames.len(),
                );
            }
            _ => println!("{label}: no samples recorded"),
        }
    }

    #[gpui::test]
    fn fixture_replay_frame_times(cx: &mut gpui::TestAppContext) {
        let text = fixture_text();
        let (wall, stats) = replay_with_draws(cx, &text, 4 * 1024);
        report("fixture replay", wall, &stats);
        assert!(!stats.frames.is_empty(), "the replay drew frames");
        assert!(!stats.feeds.is_empty(), "the replay fed the grid");
    }

    #[gpui::test]
    fn big_log_replay_frame_times(cx: &mut gpui::TestAppContext) {
        let log = big_log();
        let (wall, stats) = replay_with_draws(cx, &log, 16 * 1024);
        report("big log replay", wall, &stats);
        assert!(!stats.frames.is_empty(), "the replay drew frames");
        assert!(!stats.feeds.is_empty(), "the replay fed the grid");
    }
}
