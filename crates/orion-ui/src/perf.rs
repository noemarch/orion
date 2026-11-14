//! Debug-only timing instrumentation for the terminal hot path.
//!
//! Compiled out of release builds entirely: every item here is gated on
//! `debug_assertions`, and the module itself is only registered under the
//! same cfg in `lib.rs`. Nothing in this file may change behavior — it only
//! observes `Instant` deltas around `feed()` and `render()`.

use std::sync::Mutex;
use std::time::Duration;

/// Per-feed and per-frame deltas, in the order they were recorded.
#[derive(Default)]
pub struct PerfStats {
    pub feeds: Vec<Duration>,
    pub frames: Vec<Duration>,
}

impl PerfStats {
    const fn new() -> Self {
        Self {
            feeds: Vec::new(),
            frames: Vec::new(),
        }
    }
}

static STATS: Mutex<PerfStats> = Mutex::new(PerfStats::new());

/// Record one `feed()` call's wall time.
pub fn record_feed(delta: Duration) {
    if let Ok(mut stats) = STATS.lock() {
        stats.feeds.push(delta);
    }
}

/// Record one `render()` call's wall time (element construction only; GPUI's
/// layout/paint happens after `render` returns).
pub fn record_frame(delta: Duration) {
    if let Ok(mut stats) = STATS.lock() {
        stats.frames.push(delta);
    }
}

/// A copy of everything recorded so far.
pub fn snapshot() -> PerfStats {
    STATS
        .lock()
        .map(|stats| PerfStats {
            feeds: stats.feeds.clone(),
            frames: stats.frames.clone(),
        })
        .unwrap_or_default()
}

/// Drop everything recorded so far.
pub fn reset() {
    if let Ok(mut stats) = STATS.lock() {
        stats.feeds.clear();
        stats.frames.clear();
    }
}

/// p50 and p95 of a sample set, plus the total. `None` when empty.
pub fn summarize(samples: &[Duration]) -> Option<(Duration, Duration, Duration)> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort();
    let total: Duration = sorted.iter().sum();
    let pick = |quantile: f64| -> Duration {
        let index = ((sorted.len() as f64 * quantile).ceil() as usize).saturating_sub(1);
        sorted[index.min(sorted.len() - 1)]
    };
    Some((pick(0.50), pick(0.95), total))
}
