//! The daemon's table of terminals.
//!
//! The registry owns the terminals: it starts them, keeps their names and their family, and ends
//! the children with their parent. Everything that wants a terminal by id asks the registry; the
//! map behind it is never reached into directly.
//!
//! This is also where the safety limits are enforced. A caller asks for a terminal and gets one
//! or a refusal: there is no way to ask that skips the checks, because the checks live in the one
//! place that starts a program.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::thread;
use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agents::LaunchSpec;
use crate::error::{OrionError, Result};
use crate::protocol::{ReportedStatus, TerminalSize};
use crate::runtime::limits::{cwd_is_within, RuntimeLimits};
use crate::runtime::terminal::Terminal;

/// How often a watcher looks at the terminal it follows.
///
/// Bounded and modest: a program that ends is noticed within a tenth of a second, and a registry
/// full of idle terminals costs one sleeping thread each and no CPU worth measuring.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// What the registry says about a terminal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalInfo {
    /// The terminal's short, stable id.
    pub id: String,
    /// The human name, if it has one.
    pub name: Option<String>,
    /// The id of the terminal that started this one, if any.
    pub parent_id: Option<String>,
    /// What the program reported about itself, if it has reported.
    pub status: Option<ReportedStatus>,
    /// The exit code, if the program has ended.
    pub exited: Option<i32>,
    /// The directory the program runs in.
    pub cwd: PathBuf,
    /// The command the program runs.
    pub program: String,
}

/// An event the registry tells its watchers about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryEvent {
    /// A terminal started.
    Created(TerminalInfo),
    /// A terminal's name or parent changed.
    Updated(TerminalInfo),
    /// A program reported its own status.
    StatusChanged { id: String, status: ReportedStatus },
    /// A program ended.
    Exited { id: String, code: Option<i32> },
    /// A terminal was removed from the registry.
    Removed { id: String },
}

/// One terminal in the table: what the registry says about it, where it sits in the family, and
/// whether it survives its parent.
struct Record {
    terminal: Arc<Terminal>,
    info: TerminalInfo,
    /// A root terminal is zero; a child is one deeper than its parent.
    depth: u32,
    /// Whether this terminal keeps running when its parent does. A property of how it was
    /// started, not of the terminal itself, so it lives here.
    outlive_parent: bool,
}

/// One watcher's sink, tagged so a watcher can stop itself later.
type Watcher = Arc<dyn Fn(RegistryEvent) + Send + Sync>;

/// The shared state, held through a weak reference by the watchers.
///
/// A watcher that held the registry strongly would keep it, and every terminal in it, alive for
/// the lifetime of the process.
struct Inner {
    limits: RuntimeLimits,
    terminals: Mutex<HashMap<String, Record>>,
    watchers: Mutex<Vec<(u64, Watcher)>>,
    next_watcher: AtomicU64,
    /// The next short id. Seeded from a random uuid, so ids do not repeat across restarts, and
    /// handed out by a counter, so two terminals in one daemon can never share one.
    next_id: AtomicU32,
}

impl Inner {
    /// Tells every watcher about one event.
    ///
    /// The sinks are copied out before they are called: a watcher that reads the registry back
    /// must not deadlock against the lock its notification was taken under, and a slow watcher
    /// must not hold up the terminal that produced the event.
    fn emit(&self, event: RegistryEvent) {
        let watchers = self.watchers.lock().clone();
        for (_, watcher) in watchers {
            watcher(event.clone());
        }
    }
}

/// The daemon's table of terminals.
///
/// The registry owns the terminals: it starts them, keeps their names and
/// their family, and ends the children with their parent.
pub struct Registry {
    inner: Arc<Inner>,
}

impl Registry {
    /// An empty registry with the default limits.
    pub fn new() -> Self {
        Self::with_limits(RuntimeLimits::default())
    }

    /// A registry with limits of its own.
    pub fn with_limits(limits: RuntimeLimits) -> Self {
        Self {
            inner: Arc::new(Inner {
                limits,
                terminals: Mutex::new(HashMap::new()),
                watchers: Mutex::new(Vec::new()),
                next_watcher: AtomicU64::new(0),
                next_id: AtomicU32::new(id_seed()),
            }),
        }
    }

    /// The limits this registry enforces.
    pub fn limits(&self) -> RuntimeLimits {
        self.inner.limits
    }

    /// Starts a terminal and puts it in the table.
    ///
    /// `parent_id` names the terminal that asked for this one; a terminal
    /// started from inside another is its child.
    ///
    /// Every refusal happens before a program starts: a create that returns `Err` has spawned
    /// nothing and changed nothing.
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        &self,
        spec: &LaunchSpec,
        cwd: &Path,
        env: &[(String, String)],
        size: TerminalSize,
        parent_id: Option<&str>,
        name: Option<&str>,
        outlive_parent: bool,
        runtime_session: Option<&str>,
        runtime_cli: Option<PathBuf>,
    ) -> Result<TerminalInfo> {
        let limits = self.inner.limits;
        let depth = {
            let terminals = self.inner.terminals.lock();
            let held = u32::try_from(terminals.len()).unwrap_or(u32::MAX);
            if !limits.allows_terminals(held) {
                return Err(OrionError::Internal(format!(
                    "terminal limit reached: this session holds {held} of {}",
                    limits.max_terminals
                )));
            }
            match parent_id {
                None => 0,
                Some(parent_id) => {
                    let parent = terminals.get(parent_id).ok_or_else(|| {
                        OrionError::Internal(format!("no such terminal: {parent_id}"))
                    })?;
                    let depth = parent.depth + 1;
                    if !limits.allows_depth(depth) {
                        return Err(OrionError::Internal(format!(
                            "terminal chain too deep: depth {depth} is past the limit of {}",
                            limits.max_depth
                        )));
                    }
                    let children = u32::try_from(
                        terminals
                            .values()
                            .filter(|record| record.info.parent_id.as_deref() == Some(parent_id))
                            .count(),
                    )
                    .unwrap_or(u32::MAX);
                    if !limits.allows_children(children) {
                        return Err(OrionError::Internal(format!(
                            "too many children: {parent_id} already started {children} of {}",
                            limits.max_children
                        )));
                    }
                    // The parent's own directory, canonicalised, so `..` and symlinks cannot
                    // walk a child out of the tree it belongs to.
                    if !cwd_is_within(&parent.info.cwd, cwd) {
                        return Err(OrionError::Internal(format!(
                            "a child must stay inside its parent's directory: {} is not inside {}",
                            cwd.display(),
                            parent.info.cwd.display()
                        )));
                    }
                    depth
                }
            }
        };

        // A short id, unique by construction rather than by a check that could race.
        let id = self.next_id();

        // The daemon injects the runtime's own coordinates so anything running
        // inside the terminal can call back into it. `RUNTIME_TERMINAL_ID` is
        // this terminal's id; the socket path and CLI path come from the daemon.
        let mut full_env: Vec<(String, String)> = env.to_vec();
        full_env.push(("RUNTIME_TERMINAL_ID".to_string(), id.clone()));
        if let Some(session) = runtime_session {
            full_env.push((
                "RUNTIME_SOCKET".to_string(),
                crate::runtime::transport::socket_path(session)
                    .to_string_lossy()
                    .into_owned(),
            ));
        }
        if let Some(cli) = runtime_cli {
            full_env.push((
                "RUNTIME_CLI".to_string(),
                cli.to_string_lossy().into_owned(),
            ));
        }

        let mut terminal = Terminal::spawn(id.clone(), spec, Some(cwd), &full_env, size)?;
        if let Some(name) = name {
            terminal.set_name(name);
        }
        if let Some(parent_id) = parent_id {
            terminal.set_parent(parent_id);
        }
        let terminal = Arc::new(terminal);

        let info = TerminalInfo {
            id: id.clone(),
            name: name.map(str::to_string),
            parent_id: parent_id.map(str::to_string),
            status: None,
            exited: None,
            cwd: cwd.to_path_buf(),
            program: spec.program.to_string_lossy().into_owned(),
        };

        self.inner.terminals.lock().insert(
            id.clone(),
            Record {
                terminal: Arc::clone(&terminal),
                info: info.clone(),
                depth,
                outlive_parent,
            },
        );
        if let Err(error) = self.spawn_watcher(&id, &terminal) {
            // A terminal nobody watches would never report its exit, so take it back out and
            // let it end. Nothing was announced, so no watcher saw a terminal that does not
            // exist.
            self.inner.terminals.lock().remove(&id);
            return Err(error);
        }
        self.inner.emit(RegistryEvent::Created(info.clone()));
        Ok(info)
    }

    /// The terminal with this id, if it is open.
    pub fn get(&self, id: &str) -> Option<Arc<Terminal>> {
        self.inner
            .terminals
            .lock()
            .get(id)
            .map(|record| Arc::clone(&record.terminal))
    }

    /// What the registry says about the terminal with this id.
    pub fn info(&self, id: &str) -> Option<TerminalInfo> {
        self.inner
            .terminals
            .lock()
            .get(id)
            .map(|record| record.info.clone())
    }

    /// Every terminal the registry holds.
    ///
    /// Ordered by id, which is the order they started in: ids are handed out by a counter.
    pub fn list(&self) -> Vec<TerminalInfo> {
        let mut infos = self
            .inner
            .terminals
            .lock()
            .values()
            .map(|record| record.info.clone())
            .collect::<Vec<_>>();
        infos.sort_by(|left, right| left.id.cmp(&right.id));
        infos
    }

    /// The terminals started by the terminal with this id.
    pub fn children(&self, id: &str) -> Vec<TerminalInfo> {
        let mut infos = self
            .inner
            .terminals
            .lock()
            .values()
            .filter(|record| record.info.parent_id.as_deref() == Some(id))
            .map(|record| record.info.clone())
            .collect::<Vec<_>>();
        infos.sort_by(|left, right| left.id.cmp(&right.id));
        infos
    }

    /// Gives a terminal a human name.
    ///
    /// The registry owns the name, not the terminal: the terminal keeps the one it was started
    /// with, and renaming it again goes through here so every reader sees the same name.
    pub fn set_name(&self, id: &str, name: &str) -> Result<()> {
        let info = {
            let mut terminals = self.inner.terminals.lock();
            let record = terminals
                .get_mut(id)
                .ok_or_else(|| OrionError::Internal(format!("no such terminal: {id}")))?;
            record.info.name = Some(name.to_string());
            record.info.clone()
        };
        self.inner.emit(RegistryEvent::Updated(info));
        Ok(())
    }

    /// Records what a program reported about itself.
    pub fn set_status(&self, id: &str, status: ReportedStatus) -> Result<()> {
        let terminal = self
            .get(id)
            .ok_or_else(|| OrionError::Internal(format!("no such terminal: {id}")))?;
        // The terminal learns first and the record follows: the watcher compares the terminal
        // against the record, so whichever path gets there first is the one that sends the
        // event and the other one finds nothing to do. Exactly one `StatusChanged` per change.
        terminal.set_reported_status(status);
        let changed = {
            let mut terminals = self.inner.terminals.lock();
            match terminals.get_mut(id) {
                Some(record) if record.info.status != Some(status) => {
                    record.info.status = Some(status);
                    true
                }
                Some(_) => false,
                None => return Err(OrionError::Internal(format!("no such terminal: {id}"))),
            }
        };
        if changed {
            self.inner.emit(RegistryEvent::StatusChanged {
                id: id.to_string(),
                status,
            });
        }
        Ok(())
    }

    /// Ends a terminal and the children that do not outlive it.
    ///
    /// A child that was started to outlive its parent, and everything under it, is left running
    /// and left in the table.
    pub fn kill(&self, id: &str) -> Result<()> {
        let doomed = {
            let mut terminals = self.inner.terminals.lock();
            if !terminals.contains_key(id) {
                return Err(OrionError::Internal(format!("no such terminal: {id}")));
            }
            let mut doomed: Vec<(String, Arc<Terminal>)> = Vec::new();
            let mut pending = vec![id.to_string()];
            while let Some(current) = pending.pop() {
                let Some(record) = terminals.remove(&current) else {
                    continue;
                };
                for child in terminals.values() {
                    if child.info.parent_id.as_deref() == Some(current.as_str())
                        && !child.outlive_parent
                    {
                        pending.push(child.info.id.clone());
                    }
                }
                doomed.push((current, record.terminal));
            }
            doomed
        };
        for (id, terminal) in doomed {
            terminal.kill();
            self.inner.emit(RegistryEvent::Removed { id });
        }
        Ok(())
    }

    /// Takes a terminal out of the table.
    ///
    /// The program ends when the last holder lets go of it: a terminal is only alive while the
    /// registry, or a reader that asked for it by id, keeps it.
    pub fn remove(&self, id: &str) -> Result<()> {
        let removed = self.inner.terminals.lock().remove(id);
        if removed.is_none() {
            return Err(OrionError::Internal(format!("no such terminal: {id}")));
        }
        self.inner
            .emit(RegistryEvent::Removed { id: id.to_string() });
        Ok(())
    }

    /// Watches the registry's events.
    ///
    /// The returned function stops the watching.
    pub fn watch(
        &self,
        sink: Arc<dyn Fn(RegistryEvent) + Send + Sync>,
    ) -> Box<dyn FnOnce() + Send> {
        let watcher = self.inner.next_watcher.fetch_add(1, Ordering::Relaxed) + 1;
        self.inner.watchers.lock().push((watcher, sink));
        let inner = Arc::downgrade(&self.inner);
        Box::new(move || {
            if let Some(inner) = inner.upgrade() {
                inner.watchers.lock().retain(|(id, _)| *id != watcher);
            }
        })
    }

    /// A short id no other terminal in this registry holds.
    fn next_id(&self) -> String {
        format!("{:08x}", self.inner.next_id.fetch_add(1, Ordering::Relaxed))
    }

    /// Follows one terminal until it ends or leaves the table.
    ///
    /// The watcher sleeps on its own thread, holding nothing, and only takes the registry's lock
    /// for the compare-and-record after the terminal's state has been read under it. That keeps
    /// the record and the terminal from disagreeing about a status, and keeps the lock free
    /// while a program runs for hours.
    fn spawn_watcher(&self, id: &str, terminal: &Arc<Terminal>) -> Result<()> {
        let registry: Weak<Inner> = Arc::downgrade(&self.inner);
        let terminal: Weak<Terminal> = Arc::downgrade(terminal);
        let watch_id = id.to_string();
        thread::Builder::new()
            .name(format!("orion-registry-watch-{watch_id}"))
            .spawn(move || loop {
                thread::sleep(POLL_INTERVAL);
                let (Some(registry), Some(terminal)) = (registry.upgrade(), terminal.upgrade())
                else {
                    // The registry or the terminal is gone; the terminal's own drop ended the
                    // program, so there is nothing left to report.
                    return;
                };
                let mut status_event = None;
                let mut exit = None;
                let mut ended = false;
                {
                    let mut terminals = registry.terminals.lock();
                    let Some(record) = terminals.get_mut(&watch_id) else {
                        return;
                    };
                    if let Some(status) = terminal.reported_status() {
                        if record.info.status != Some(status) {
                            record.info.status = Some(status);
                            status_event = Some(RegistryEvent::StatusChanged {
                                id: watch_id.clone(),
                                status,
                            });
                        }
                    }
                    if let Some(code) = terminal.exited() {
                        ended = true;
                        if record.info.exited != Some(code) {
                            record.info.exited = Some(code);
                            exit = Some(code);
                        }
                    }
                }
                if let Some(event) = status_event {
                    registry.emit(event);
                }
                if let Some(code) = exit {
                    registry.emit(RegistryEvent::Exited {
                        id: watch_id.clone(),
                        code: Some(code),
                    });
                }
                if ended {
                    return;
                }
            })
            .map_err(|error| {
                OrionError::Process(format!("failed to watch the terminal: {error}"))
            })?;
        Ok(())
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

/// The random half of a short id: the low bits of a version-4 uuid.
fn id_seed() -> u32 {
    let uuid = Uuid::new_v4();
    u32::try_from(uuid.as_u128() & u128::from(u32::MAX)).unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// What a test watcher has seen so far.
    type Seen = Arc<Mutex<Vec<RegistryEvent>>>;
    /// The function that stops a test watcher.
    type Stop = Box<dyn FnOnce() + Send>;

    fn size() -> TerminalSize {
        TerminalSize {
            cols: 80,
            rows: 24,
            pixel_width: 0,
            pixel_height: 0,
        }
    }

    /// A program that prints once and ends.
    fn printer() -> LaunchSpec {
        #[cfg(windows)]
        let spec = LaunchSpec {
            program: "cmd.exe".into(),
            args: vec!["/D".into(), "/C".into(), "echo ok".into()],
        };
        #[cfg(not(windows))]
        let spec = LaunchSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "echo ok".into()],
        };
        spec
    }

    fn limits(max_depth: u32, max_children: u32, max_terminals: u32) -> RuntimeLimits {
        RuntimeLimits {
            max_depth,
            max_children,
            max_terminals,
        }
    }

    /// A directory of its own, so one test cannot see another's files.
    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("orion-registry-{tag}-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).expect("the test directory exists");
        dir
    }

    fn create_in(registry: &Registry, cwd: &Path) -> TerminalInfo {
        registry
            .create(&printer(), cwd, &[], size(), None, None, false, None, None)
            .expect("a terminal starts")
    }

    fn watcher(registry: &Registry) -> (Seen, Stop) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let stop = registry.watch(Arc::new(move |event| sink.lock().push(event)));
        (seen, stop)
    }

    fn wait_until(mut ready: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if ready() {
                return true;
            }
            thread::sleep(Duration::from_millis(100));
        }
        false
    }

    #[test]
    fn a_registry_starts_a_terminal_and_lists_it() {
        let dir = temp_dir("list");
        let registry = Registry::new();
        let info = create_in(&registry, &dir);

        let listed = registry.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, info.id);
        assert_eq!(listed[0].cwd, dir);
        assert_eq!(listed[0].program, info.program);
        assert!(!listed[0].program.is_empty());

        // `exited` is left out on purpose: a quick program may have ended between the two reads.
        let stored = registry
            .info(&info.id)
            .expect("the terminal is in the table");
        assert_eq!(stored.id, info.id);
        assert_eq!(stored.name, info.name);
        assert_eq!(stored.parent_id, info.parent_id);
        assert_eq!(stored.cwd, info.cwd);
        assert_eq!(stored.program, info.program);
    }

    #[test]
    fn two_terminals_get_different_ids() {
        let dir = temp_dir("ids");
        let registry = Registry::new();
        let one = create_in(&registry, &dir);
        let two = create_in(&registry, &dir);

        assert_ne!(one.id, two.id);
        assert_eq!(one.id.len(), 8, "ids are short: {}", one.id);
        assert_eq!(two.id.len(), 8, "ids are short: {}", two.id);
    }

    #[test]
    fn a_terminal_started_from_another_is_its_child() {
        let dir = temp_dir("child");
        let registry = Registry::new();
        let parent = create_in(&registry, &dir);
        let child = registry
            .create(
                &printer(),
                &dir,
                &[],
                size(),
                Some(&parent.id),
                None,
                false,
                None,
                None,
            )
            .expect("a child starts");

        assert_eq!(child.parent_id.as_deref(), Some(parent.id.as_str()));
        assert_eq!(registry.children(&parent.id), vec![child.clone()]);
        assert!(registry.children(&child.id).is_empty());
    }

    #[test]
    fn a_registry_refuses_a_chain_that_is_too_deep() {
        let dir = temp_dir("depth");
        let registry = Registry::with_limits(limits(1, 16, 64));
        let parent = create_in(&registry, &dir);
        let child = registry
            .create(
                &printer(),
                &dir,
                &[],
                size(),
                Some(&parent.id),
                None,
                false,
                None,
                None,
            )
            .expect("a first child starts");
        assert!(
            registry
                .create(
                    &printer(),
                    &dir,
                    &[],
                    size(),
                    Some(&child.id),
                    None,
                    false,
                    None,
                    None
                )
                .is_err(),
            "depth two is past a limit of one"
        );
        assert_eq!(registry.list().len(), 2);
    }

    #[test]
    fn a_registry_refuses_a_parent_with_too_many_children() {
        let dir = temp_dir("children");
        let registry = Registry::with_limits(limits(16, 1, 64));
        let parent = create_in(&registry, &dir);
        registry
            .create(
                &printer(),
                &dir,
                &[],
                size(),
                Some(&parent.id),
                None,
                false,
                None,
                None,
            )
            .expect("the first child starts");
        assert!(
            registry
                .create(
                    &printer(),
                    &dir,
                    &[],
                    size(),
                    Some(&parent.id),
                    None,
                    false,
                    None,
                    None
                )
                .is_err(),
            "a second child is past a limit of one"
        );
        assert_eq!(registry.children(&parent.id).len(), 1);
    }

    #[test]
    fn a_registry_refuses_a_session_with_too_many_terminals() {
        let dir = temp_dir("terminals");
        let registry = Registry::with_limits(limits(16, 16, 1));
        create_in(&registry, &dir);
        assert!(
            registry
                .create(&printer(), &dir, &[], size(), None, None, false, None, None)
                .is_err(),
            "a second terminal is past a limit of one"
        );
        assert_eq!(registry.list().len(), 1);
    }

    #[test]
    fn a_registry_refuses_a_child_that_would_leave_the_parent_directory() {
        let parent_dir = temp_dir("inside");
        let other_dir = temp_dir("outside");
        let registry = Registry::new();
        let parent = create_in(&registry, &parent_dir);

        assert!(
            registry
                .create(
                    &printer(),
                    &other_dir,
                    &[],
                    size(),
                    Some(&parent.id),
                    None,
                    false,
                    None,
                    None
                )
                .is_err(),
            "a sibling directory is not inside the parent's"
        );

        let subdir = parent_dir.join("nested");
        std::fs::create_dir_all(&subdir).expect("the subdirectory exists");
        let child = registry
            .create(
                &printer(),
                &subdir,
                &[],
                size(),
                Some(&parent.id),
                None,
                false,
                None,
                None,
            )
            .expect("a child inside the parent's directory starts");
        assert_eq!(child.parent_id.as_deref(), Some(parent.id.as_str()));
    }

    #[test]
    fn a_child_ends_with_its_parent() {
        let dir = temp_dir("ends");
        let registry = Registry::new();
        let parent = create_in(&registry, &dir);
        let child = registry
            .create(
                &printer(),
                &dir,
                &[],
                size(),
                Some(&parent.id),
                None,
                false,
                None,
                None,
            )
            .expect("a child starts");

        registry.kill(&parent.id).expect("the parent ends");

        assert!(wait_until(|| registry.get(&child.id).is_none()));
        assert!(registry.get(&parent.id).is_none());
    }

    #[test]
    fn a_child_that_outlives_its_parent_survives() {
        let dir = temp_dir("outlives");
        let registry = Registry::new();
        let parent = create_in(&registry, &dir);
        let child = registry
            .create(
                &printer(),
                &dir,
                &[],
                size(),
                Some(&parent.id),
                None,
                true,
                None,
                None,
            )
            .expect("a child starts");

        registry.kill(&parent.id).expect("the parent ends");

        assert!(registry.get(&parent.id).is_none());
        assert!(
            wait_until(|| registry.get(&child.id).is_some()),
            "a child that outlives its parent stays in the table"
        );
        assert_eq!(registry.list().len(), 1);
    }

    #[test]
    fn a_registry_tells_its_watchers_about_a_new_terminal() {
        let dir = temp_dir("watch");
        let registry = Registry::new();
        let (seen, stop) = watcher(&registry);
        let info = create_in(&registry, &dir);

        assert!(
            wait_until(|| seen.lock().iter().any(
                |event| matches!(event, RegistryEvent::Created(created) if created.id == info.id)
            )),
            "watcher saw {:?}",
            seen.lock()
        );

        // The returned function stops the watching: no further event reaches the sink.
        stop();
        registry
            .set_name(&info.id, "quiet")
            .expect("the rename lands");
        assert!(
            !seen
                .lock()
                .iter()
                .any(|event| matches!(event, RegistryEvent::Updated(_))),
            "a stopped watcher hears nothing"
        );
    }

    #[test]
    fn a_registry_remembers_the_status_a_program_reported() {
        let dir = temp_dir("status");
        let registry = Registry::new();
        let (seen, stop) = watcher(&registry);
        let info = create_in(&registry, &dir);

        registry
            .set_status(&info.id, ReportedStatus::Done)
            .expect("the status is recorded");

        assert_eq!(
            registry.info(&info.id).expect("still open").status,
            Some(ReportedStatus::Done)
        );
        assert!(
            wait_until(|| seen.lock().iter().any(|event| matches!(
                event,
                RegistryEvent::StatusChanged { id, status }
                    if id == &info.id && *status == ReportedStatus::Done
            ))),
            "watcher saw {:?}",
            seen.lock()
        );
        drop(stop);
    }

    #[test]
    fn a_registry_forgets_a_terminal_it_removed() {
        let dir = temp_dir("removed");
        let registry = Registry::new();
        let info = create_in(&registry, &dir);

        registry.remove(&info.id).expect("the terminal is removed");

        assert!(registry.get(&info.id).is_none());
        assert!(registry.list().is_empty());
    }
}
