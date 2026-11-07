//! File watching for the Changes view.
//!
//! The engine watches each workspace's folder with `notify` and broadcasts `session-changed`
//! (debounced) when a file changes, so the diff list refreshes without polling. The watcher
//! stops when the workspace closes or the app exits.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::engine::Inner;

pub struct Watcher {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Watcher {
    pub(crate) fn start(cwd: PathBuf, inner: Arc<Inner>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name(format!("orion-watch-{}", cwd.display()))
            .spawn(move || watch_loop(&cwd, &inner, &flag))
            .ok();
        Self { stop, thread }
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn watch_loop(cwd: &Path, inner: &Inner, stop: &Arc<AtomicBool>) {
    use notify::{RecursiveMode, Watcher as _};
    let (tx, rx) = std::sync::mpsc::channel();
    let Ok(mut watcher) = notify::recommended_watcher(tx) else {
        return;
    };
    if watcher.watch(cwd, RecursiveMode::Recursive).is_err() {
        return;
    }
    let mut last = std::time::Instant::now() - std::time::Duration::from_secs(1);
    while !stop.load(Ordering::Relaxed) {
        match rx.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(_) => {
                // Debounce burst changes into one broadcast.
                if last.elapsed() >= std::time::Duration::from_millis(500) {
                    last = std::time::Instant::now();
                    inner.broadcast(serde_json::json!({ "type": "session-changed" }));
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}
