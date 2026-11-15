//! The startup timeline, recorded on one monotonic clock.
//!
//! Off by default. `ORION_TRACE=1` turns it on; every other value (including
//! an unset variable) leaves it off, and an off trace costs one atomic load
//! per call.
//!
//! The timeline answers one question: where did the time go between "the
//! reader asked for a workspace" and "the terminal painted something"? Every
//! event is stamped in milliseconds since the timeline began, so the gaps
//! between two events are the delays that matter.
//!
//! The events are deliberately coarse. They mark the boundaries a reader can
//! see — the click, the connection, the spawn, the first byte, the first
//! paint — and the details carry what was asked for at that moment.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// Whether the trace was turned on by the program rather than the environment.
static FORCED: AtomicBool = AtomicBool::new(false);

/// How many PTY reads are sampled. Enough to see the shape of a burst, small
/// enough that the trace never grows without bound.
const MAX_READ_SAMPLES: usize = 50;

/// The boundaries the startup timeline marks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The reader asked for a workspace.
    WorkspaceOpenClicked,
    /// The shell reached its runtime; the "Connecting…" state ends here.
    BackendConnected,
    /// A PTY was asked for.
    PtySpawnRequested,
    /// The child process exists.
    PtySpawnReturned,
    /// The child wrote its first byte.
    ChildFirstByte,
    /// The terminal view was built.
    TerminalViewCreated,
    /// The Terminal lens was selected.
    TerminalTabSelected,
    /// A size reached both the PTY and the grid.
    FirstResizeApplied,
    /// A frame with real content was painted.
    FirstNonemptyFramePainted,
}

impl Event {
    /// The name the timeline table prints.
    pub fn name(self) -> &'static str {
        match self {
            Event::WorkspaceOpenClicked => "workspace_open_clicked",
            Event::BackendConnected => "backend_connected",
            Event::PtySpawnRequested => "pty_spawn_requested",
            Event::PtySpawnReturned => "pty_spawn_returned",
            Event::ChildFirstByte => "child_first_byte",
            Event::TerminalViewCreated => "terminal_view_created",
            Event::TerminalTabSelected => "terminal_tab_selected",
            Event::FirstResizeApplied => "first_resize_applied",
            Event::FirstNonemptyFramePainted => "first_nonempty_frame_painted",
        }
    }
}

/// One marked moment.
#[derive(Debug, Clone)]
pub struct Record {
    pub event: Event,
    /// Milliseconds since the timeline began.
    pub at: f64,
    /// What was asked for at that moment, when there is something to say.
    pub detail: Option<String>,
}

/// One PTY read: how many bytes, and when.
#[derive(Debug, Clone)]
pub struct ReadSample {
    pub at: f64,
    pub bytes: usize,
}

/// Everything one timeline holds.
#[derive(Debug, Default)]
pub struct Timeline {
    origin: Option<Instant>,
    records: Vec<Record>,
    reads: Vec<ReadSample>,
    repaints: u64,
    /// Bytes read in total, including the reads past the sample cap.
    read_bytes: u64,
}

impl Timeline {
    /// Marks a moment on the timeline.
    pub fn record(&mut self, event: Event, detail: Option<String>) {
        let now = Instant::now();
        let origin = *self.origin.get_or_insert(now);
        self.records.push(Record {
            event,
            at: now.duration_since(origin).as_secs_f64() * 1000.0,
            detail,
        });
    }

    /// Samples one PTY read.
    pub fn record_read(&mut self, bytes: usize) {
        let now = Instant::now();
        let origin = *self.origin.get_or_insert(now);
        self.read_bytes += bytes as u64;
        if self.reads.len() < MAX_READ_SAMPLES {
            self.reads.push(ReadSample {
                at: now.duration_since(origin).as_secs_f64() * 1000.0,
                bytes,
            });
        }
    }

    /// Counts one painted frame.
    pub fn count_repaint(&mut self) {
        self.repaints += 1;
    }

    /// The records, oldest first.
    pub fn records(&self) -> &[Record] {
        &self.records
    }

    /// The sampled reads.
    pub fn reads(&self) -> &[ReadSample] {
        &self.reads
    }

    /// How many frames were painted.
    pub fn repaints(&self) -> u64 {
        self.repaints
    }

    /// How many bytes the PTY read in total.
    pub fn read_bytes(&self) -> u64 {
        self.read_bytes
    }

    /// The time from the first record to the last, in milliseconds.
    pub fn span_ms(&self) -> f64 {
        match (self.records.first(), self.records.last()) {
            (Some(first), Some(last)) => last.at - first.at,
            _ => 0.0,
        }
    }

    /// The table the report prints.
    pub fn table(&self) -> String {
        let mut out = String::new();
        out.push_str("| event | ms | detail |\n");
        out.push_str("|---|---:|---|\n");
        for record in &self.records {
            let detail = record.detail.as_deref().unwrap_or("");
            let _ = writeln!(out, "| {} | {:.1} | {} |", record.event.name(), record.at, detail);
        }
        out
    }

    /// The read and repaint summary the report prints.
    pub fn io_summary(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "- pty reads sampled: {} (cap {}), bytes read in total: {}",
            self.reads.len(),
            MAX_READ_SAMPLES,
            self.read_bytes
        );
        if let (Some(first), Some(last)) = (self.reads.first(), self.reads.last()) {
            let span = last.at - first.at;
            let per_second = if span > 0.0 {
                (self.reads.len() as f64 / span) * 1000.0
            } else {
                0.0
            };
            let average = self.read_bytes as f64 / self.reads.len().max(1) as f64;
            let _ = writeln!(
                out,
                "- read rate over the sampled window: {:.0} reads/s, {:.0} bytes/read",
                per_second, average
            );
        }
        let _ = writeln!(out, "- frames painted: {}", self.repaints);
        out
    }

    /// The whole report: the table, then the read and repaint summary.
    pub fn report(&self) -> String {
        let mut out = String::new();
        out.push_str(&self.table());
        out.push('\n');
        out.push_str(&self.io_summary());
        out
    }
}

fn timeline() -> &'static Mutex<Timeline> {
    static TIMELINE: OnceLock<Mutex<Timeline>> = OnceLock::new();
    TIMELINE.get_or_init(|| Mutex::new(Timeline::default()))
}

/// Turns the trace on for this process, whatever the environment says.
///
/// The measurement harness needs a timeline whether or not the reader set the
/// variable. The shell never calls this: it reads the environment only.
pub fn force_enabled() {
    FORCED.store(true, Ordering::Relaxed);
}

/// Whether the trace is on. Read from the environment once.
pub fn enabled() -> bool {
    if FORCED.load(Ordering::Relaxed) {
        return true;
    }
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| match std::env::var("ORION_TRACE") {
        Ok(value) => !matches!(value.trim(), "" | "0" | "false" | "off" | "no"),
        Err(_) => false,
    })
}

/// Marks a moment on the timeline. Does nothing when the trace is off.
pub fn record(event: Event) {
    record_with(event, None);
}

/// Marks a moment on the timeline, with a detail to print beside it.
pub fn record_with(event: Event, detail: Option<String>) {
    if !enabled() {
        return;
    }
    if let Ok(mut timeline) = timeline().lock() {
        timeline.record(event, detail);
    }
}

/// Samples one PTY read. Does nothing when the trace is off.
pub fn record_read(bytes: usize) {
    if !enabled() {
        return;
    }
    if let Ok(mut timeline) = timeline().lock() {
        timeline.record_read(bytes);
    }
}

/// Counts one painted frame. Does nothing when the trace is off.
pub fn count_repaint() {
    if !enabled() {
        return;
    }
    if let Ok(mut timeline) = timeline().lock() {
        timeline.count_repaint();
    }
}

/// Starts a new timeline: the next record is the origin.
pub fn begin() {
    if !enabled() {
        return;
    }
    if let Ok(mut timeline) = timeline().lock() {
        *timeline = Timeline::default();
    }
}

/// A copy of the timeline.
pub fn snapshot() -> Timeline {
    timeline()
        .lock()
        .map(|timeline| {
            let mut copy = Timeline::default();
            copy.origin = timeline.origin;
            copy.records = timeline.records.clone();
            copy.reads = timeline.reads.clone();
            copy.repaints = timeline.repaints;
            copy.read_bytes = timeline.read_bytes;
            copy
        })
        .unwrap_or_default()
}

/// Prints the report to stderr.
pub fn print_report() {
    if !enabled() {
        return;
    }
    eprintln!("\n--- orion startup timeline ---\n{}", snapshot().report());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_timeline_stamps_events_in_order() {
        let mut timeline = Timeline::default();
        timeline.record(Event::WorkspaceOpenClicked, None);
        timeline.record(Event::BackendConnected, Some("live".to_string()));
        assert_eq!(timeline.records().len(), 2);
        assert_eq!(timeline.records()[0].event, Event::WorkspaceOpenClicked);
        assert_eq!(timeline.records()[1].detail.as_deref(), Some("live"));
        assert!(timeline.records()[1].at >= timeline.records()[0].at);
    }

    #[test]
    fn the_table_names_every_event() {
        let mut timeline = Timeline::default();
        for event in [
            Event::WorkspaceOpenClicked,
            Event::BackendConnected,
            Event::PtySpawnRequested,
            Event::PtySpawnReturned,
            Event::ChildFirstByte,
            Event::TerminalViewCreated,
            Event::TerminalTabSelected,
            Event::FirstResizeApplied,
            Event::FirstNonemptyFramePainted,
        ] {
            timeline.record(event, None);
        }
        let table = timeline.table();
        for event in [
            Event::WorkspaceOpenClicked,
            Event::BackendConnected,
            Event::PtySpawnRequested,
            Event::PtySpawnReturned,
            Event::ChildFirstByte,
            Event::TerminalViewCreated,
            Event::TerminalTabSelected,
            Event::FirstResizeApplied,
            Event::FirstNonemptyFramePainted,
        ] {
            assert!(table.contains(event.name()), "{} missing from the table", event.name());
        }
    }

    #[test]
    fn reads_are_capped_but_still_counted() {
        let mut timeline = Timeline::default();
        for _ in 0..(MAX_READ_SAMPLES + 10) {
            timeline.record_read(100);
        }
        assert_eq!(timeline.reads().len(), MAX_READ_SAMPLES);
        assert_eq!(timeline.read_bytes(), (MAX_READ_SAMPLES as u64 + 10) * 100);
    }

    #[test]
    fn an_unset_trace_is_off() {
        // The environment of the test process decides this, so the assertion is
        // about the shape of the rule, not about a particular value.
        let value = std::env::var("ORION_TRACE");
        let expected = match value {
            Ok(raw) => !matches!(raw.trim(), "" | "0" | "false" | "off" | "no"),
            Err(_) => false,
        };
        assert_eq!(enabled(), expected);
    }
}
