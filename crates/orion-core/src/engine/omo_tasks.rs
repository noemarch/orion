//! OmO's background tasks and workflows, read from its own state directory.
//!
//! OmO keeps one record per background `task` child in `<state dir>/tasks/st_*.json` and one
//! checkpoint per workflow in `<state dir>/dag/runs/dag_*.json`; both name the session that
//! started them. The state directory is the populated legacy `<cwd>/.omo/senpi-task` when there
//! is one, else `<agent dir>/projects/<name>-<sha256(cwd)[..12]>/senpi-task`.
//!
//! The list the status line opens is refreshed every few seconds, so records are cached by
//! modification time and size, only what the list shows is kept, and one call parses a bounded
//! number of bytes. A running task whose host process is gone reads as `lost`, as OmO marks it.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::engine::introspect::omo;
use crate::protocol::AgentKind;

/// Task states OmO still runs.
const RUNNING: [&str; 2] = ["pending", "running"];
/// Task states that ended.
const ENDED: [&str; 4] = ["completed", "failed", "cancelled", "lost"];
/// Workflow states that have not ended.
const ACTIVE_RUN: [&str; 3] = ["pending", "running", "paused"];
/// Node states a workflow checkpoint can name.
const NODE_STATES: [&str; 8] = [
    "pending",
    "scheduled",
    "running",
    "blocked",
    "completed",
    "failed",
    "skipped",
    "cancelled",
];
/// Workflow states a checkpoint can name.
const RUN_STATES: [&str; 6] = [
    "pending",
    "running",
    "paused",
    "completed",
    "failed",
    "cancelled",
];
const DAY_MS: i64 = 24 * 60 * 60 * 1000;
const RECENT_ENDED_TASKS: usize = 10;
const RECENT_ENDED_RUNS: usize = 5;
const MAX_TASK_FILES: usize = 50_000;
const MAX_RECORD_BYTES: u64 = 1024 * 1024;
const TASK_PARSE_BUDGET: u64 = 8 * 1024 * 1024;
const MAX_RUN_FILES: usize = 10_000;
const MAX_RUN_BYTES: u64 = 8 * 1024 * 1024;
const RUN_PARSE_BUDGET: u64 = 16 * 1024 * 1024;

/// What one task record says, before the host process is checked.
#[derive(Debug, Clone)]
struct KeptTask {
    parent: Option<String>,
    host_pid: Option<u32>,
    task: Option<Value>,
}

/// A cached task record, keyed by the file's modification time and size.
type TaskCache = HashMap<PathBuf, (u64, u64, KeptTask)>;

fn task_cache() -> &'static Mutex<TaskCache> {
    static CACHE: OnceLock<Mutex<TaskCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// A cached workflow checkpoint: the parent session it names, and its run when it is ours.
type RunCache = HashMap<PathBuf, (u64, u64, Option<String>, Option<Value>)>;

fn run_cache() -> &'static Mutex<RunCache> {
    static CACHE: OnceLock<Mutex<RunCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The state directory the pane's session writes, from the server's environment.
pub fn state_dir(kind: AgentKind, cwd: &Path) -> PathBuf {
    let agent_dir = omo::agent_dir(kind).unwrap_or_else(|| PathBuf::from("."));
    state_dir_in(cwd, &agent_dir)
}

/// The state directory under an explicit agent dir, which is how tests reach a seeded store.
pub fn state_dir_in(cwd: &Path, agent_dir: &Path) -> PathBuf {
    let legacy = cwd.join(".omo").join("senpi-task");
    if legacy.is_dir() && holds_records(&legacy) {
        return legacy;
    }
    agent_dir
        .join("projects")
        .join(project_state_key(cwd))
        .join("senpi-task")
}

/// OmO's project key: the canonical cwd's basename, then the first 12 hex of its SHA-256.
pub fn project_state_key(cwd: &Path) -> String {
    let absolute = canonical(cwd);
    let mut hasher = Sha256::new();
    hasher.update(absolute.to_string_lossy().as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    let base = absolute
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut name = if base.is_empty() {
        "root".to_string()
    } else {
        base.chars()
            .map(|character| {
                if character.is_alphanumeric() || matches!(character, '.' | '_' | '-') {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>()
    };
    name.push('-');
    name.push_str(&digest[..12]);
    name
}

/// OmO's rule: anything it cannot read as an empty tree holds records.
fn holds_records(path: &Path) -> bool {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return true;
    };
    if !meta.is_dir() {
        return true;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return true;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if entry.path().is_dir() && !name.contains('.') {
            if holds_records(&entry.path()) {
                return true;
            }
        } else {
            return true;
        }
    }
    false
}

fn canonical(path: &Path) -> PathBuf {
    let path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    path
}

/// The background tasks one OmO session started: running first (oldest first), then up to ten
/// that ended in the last day (newest first).
pub fn tasks(
    state_dir: &Path,
    session_id: &str,
    alive: impl Fn(u32) -> bool,
    now: i64,
) -> Vec<Value> {
    let dir = state_dir.join("tasks");
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            (name.starts_with("st_") && name.ends_with(".json")).then_some(name)
        })
        .collect::<Vec<_>>();
    names.sort();
    names.reverse();
    names.truncate(MAX_TASK_FILES);

    let mut cache = task_cache()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut running: Vec<Value> = Vec::new();
    let mut ended: Vec<Value> = Vec::new();
    let mut budget = TASK_PARSE_BUDGET;
    for name in names {
        let path = dir.join(name);
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.is_file() || meta.len() > MAX_RECORD_BYTES {
            continue;
        }
        let key = (modified_ms(&meta), meta.len());
        let kept = match cache.get(&path) {
            Some((at, size, kept)) if (*at, *size) == key => kept.clone(),
            _ => {
                if budget < meta.len() {
                    continue;
                }
                budget -= meta.len();
                let Ok(text) = fs::read_to_string(&path) else {
                    continue;
                };
                let Ok(record) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                let kept = keep_task(&record);
                cache.insert(path.clone(), (key.0, key.1, kept.clone()));
                kept
            }
        };
        if kept.parent.as_deref() != Some(session_id) {
            continue;
        }
        let Some(task) = kept.task else {
            continue;
        };
        // its host died with it running: lost, as OmO itself marks it, and ended when last heard
        // of; a task still running has not ended, whatever its last update says
        let task = if task.get("status").and_then(Value::as_str) == Some("running") {
            let mut task = task;
            if kept.host_pid.is_some_and(|pid| !alive(pid)) {
                task["status"] = json!("lost");
            } else {
                task["ended_at"] = Value::Null;
            }
            task
        } else {
            task
        };
        // an ended task with no end time counts from its start; with neither, not known to be recent
        let at = task
            .get("ended_at")
            .and_then(time_ms)
            .or_else(|| task.get("started_at").and_then(time_ms))
            .unwrap_or(0);
        if task.get("status").and_then(Value::as_str) == Some("running") {
            running.push(task);
        } else if at != 0 && now - at <= DAY_MS {
            ended.push(task);
        }
    }
    if cache.len() > MAX_TASK_FILES {
        cache.clear();
    }
    running.sort_by_key(|task| task.get("started_at").and_then(time_ms).unwrap_or(0));
    ended
        .sort_by_key(|task| std::cmp::Reverse(task.get("ended_at").and_then(time_ms).unwrap_or(0)));
    ended.truncate(RECENT_ENDED_TASKS);
    running.extend(ended);
    running
}

fn keep_task(record: &Value) -> KeptTask {
    KeptTask {
        parent: record.get("parent_session_id").and_then(label),
        host_pid: record
            .get("host_pid")
            .and_then(Value::as_u64)
            .map(|pid| pid as u32),
        task: one_task(record),
    }
}

fn one_task(record: &Value) -> Option<Value> {
    let id = record.get("task_id").and_then(label)?;
    let raw = record.get("status").and_then(Value::as_str)?;
    let status = if RUNNING.contains(&raw) {
        "running"
    } else if raw == "error" {
        "failed"
    } else if raw == "interrupted" {
        "lost"
    } else if ENDED.contains(&raw) {
        raw
    } else {
        return None;
    };
    let stats = record.get("run_stats").cloned().unwrap_or(Value::Null);
    let ended_at = if status == "running" {
        record.get("updated_at").and_then(label)
    } else {
        record
            .get("terminal_at")
            .and_then(label)
            .or_else(|| record.get("updated_at").and_then(label))
    };
    Some(json!({
        "id": id,
        "title": record.get("task_summary").and_then(label)
            .or_else(|| record.get("description").and_then(label))
            .or_else(|| record.get("name").and_then(label))
            .unwrap_or_else(|| record.get("task_id").and_then(label).unwrap_or_default()),
        "category": record.get("category").and_then(label)
            .or_else(|| record.get("agent_type").and_then(label)),
        "model": record.get("resolved_model").and_then(|model| model.get("display")).and_then(label)
            .or_else(|| record.get("model").and_then(label)),
        "status": status,
        "started_at": record.get("started_at").and_then(label)
            .or_else(|| record.get("created_at").and_then(label)),
        "ended_at": ended_at,
        "turns": stats.get("turns").and_then(number),
        "tool_calls": stats.get("tool_calls").and_then(number),
        "tokens": stats.get("total_tokens").and_then(number),
    }))
}

/// The workflows one OmO session started: still going (oldest first), then up to five that ended
/// in the last day (newest first).
pub fn runs(state_dir: &Path, session_id: &str, now: i64) -> Vec<Value> {
    let dir = state_dir.join("dag").join("runs");
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut recent: Vec<(PathBuf, (u64, u64), u64, u64)> = Vec::new();
    for entry in entries.flatten().take(MAX_RUN_FILES) {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("dag_") || !name.ends_with(".json") {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.is_file() || meta.len() > MAX_RUN_BYTES {
            continue;
        }
        let modified = modified_ms(&meta);
        if now - modified as i64 > DAY_MS {
            continue;
        }
        recent.push((path, (modified, meta.len()), meta.len(), modified));
    }
    recent.sort_by_key(|(_, _, _, modified)| std::cmp::Reverse(*modified));

    let mut cache = run_cache()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut active: Vec<Value> = Vec::new();
    let mut ended: Vec<Value> = Vec::new();
    let mut budget = RUN_PARSE_BUDGET;
    for (path, key, size, _) in recent {
        let (parent, run) = match cache.get(&path) {
            Some((at, cached_size, parent, run)) if (*at, *cached_size) == key => {
                (parent.clone(), run.clone())
            }
            _ => {
                if budget < size {
                    continue;
                }
                budget -= size;
                let Ok(text) = fs::read_to_string(&path) else {
                    continue;
                };
                let parent = parent_session_id(&text);
                let run = if parent.as_deref() == Some(session_id) {
                    serde_json::from_str::<Value>(&text)
                        .ok()
                        .and_then(|record| parse_run(&record))
                } else {
                    None
                };
                cache.insert(path.clone(), (key.0, key.1, parent.clone(), run.clone()));
                (parent, run)
            }
        };
        if parent.as_deref() != Some(session_id) {
            continue;
        }
        let Some(run) = run else {
            continue;
        };
        if run
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|status| ACTIVE_RUN.contains(&status))
        {
            active.push(run);
        } else {
            ended.push(run);
        }
    }
    if cache.len() > 2048 {
        cache.clear();
    }
    active.sort_by_key(|run| run.get("started_at").and_then(time_ms).unwrap_or(0));
    ended.sort_by_key(|run| std::cmp::Reverse(run.get("ended_at").and_then(time_ms).unwrap_or(0)));
    ended.truncate(RECENT_ENDED_RUNS);
    active.extend(ended);
    active
}

fn parse_run(record: &Value) -> Option<Value> {
    let id = record.get("runId").and_then(label)?;
    let status = record.get("status").and_then(Value::as_str)?;
    if !RUN_STATES.contains(&status) {
        return None;
    }
    let mut nodes: Vec<Value> = Vec::new();
    for node in record.get("nodes").and_then(Value::as_array)? {
        let Some(node_id) = node.get("id").and_then(label) else {
            continue;
        };
        let Some(state) = node.get("state").and_then(Value::as_str) else {
            continue;
        };
        if !NODE_STATES.contains(&state) {
            continue;
        }
        nodes.push(json!({
            "id": node_id,
            "label": node.get("label").and_then(label).unwrap_or_else(|| node_id.clone()),
            "state": state,
            "error": if state == "failed" {
                node.get("error").and_then(|error| error.get("message")).and_then(label)
            } else {
                None
            },
        }));
    }
    let mut waves: Vec<Value> = Vec::new();
    let mut placed: Vec<String> = Vec::new();
    for wave in record
        .get("waves")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let ids = wave
            .get("nodeIds")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut placed_wave: Vec<Value> = Vec::new();
        for id in ids {
            let Some(id) = id.as_str() else {
                continue;
            };
            if placed.iter().any(|seen| seen == id) {
                continue;
            }
            if let Some(node) = nodes
                .iter()
                .find(|node| node.get("id").and_then(Value::as_str) == Some(id))
            {
                placed.push(id.to_string());
                placed_wave.push(node.clone());
            }
        }
        if !placed_wave.is_empty() {
            waves.push(json!(placed_wave));
        }
    }
    let rest = nodes
        .into_iter()
        .filter(|node| {
            let id = node.get("id").and_then(Value::as_str).unwrap_or_default();
            !placed.iter().any(|seen| seen == id)
        })
        .collect::<Vec<_>>();
    if !rest.is_empty() {
        waves.push(json!(rest));
    }
    let ended_at = if ACTIVE_RUN.contains(&status) {
        None
    } else {
        record
            .get("completedAt")
            .and_then(label)
            .or_else(|| record.get("updatedAt").and_then(label))
    };
    Some(json!({
        "id": id,
        "name": record.get("name").and_then(label)
            .or_else(|| record.get("runKey").and_then(label))
            .unwrap_or_else(|| id.clone()),
        "status": status,
        "started_at": record.get("startedAt").and_then(label)
            .or_else(|| record.get("createdAt").and_then(label)),
        "ended_at": ended_at,
        "waves": waves,
    }))
}

/// The `parentSessionId` near a checkpoint's start, without parsing its megabyte of nodes.
fn parent_session_id(content: &str) -> Option<String> {
    let at = content.find("\"parentSessionId\"")?;
    let rest = content[at + "\"parentSessionId\"".len()..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// GET /api/pane/omo-tasks' body.
pub fn activity(kind: AgentKind, cwd: &Path, session_id: &str) -> Value {
    let dir = state_dir(kind, cwd);
    let now = now_ms();
    json!({
        "tasks": tasks(&dir, session_id, process_alive, now),
        "runs": runs(&dir, session_id, now),
        "server_time": chrono::Utc::now().to_rfc3339(),
    })
}

fn label(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| text.chars().take(300).collect())
}

fn number(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        value
            .as_f64()
            .filter(|value| value.is_finite() && *value >= 0.0)
            .map(|value| value.round() as u64)
    })
}

fn time_ms(value: &Value) -> Option<i64> {
    let text = value.as_str()?;
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|time| time.timestamp_millis())
}

fn modified_ms(meta: &fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|time| time.as_millis() as u64)
        .unwrap_or(0)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| time.as_millis() as i64)
        .unwrap_or(0)
}

/// Whether the process that started a task is still alive.
///
/// Windows asks the OS; Linux uses procfs. Elsewhere the answer is unknown, and an unknown host
/// is treated as alive so a task is never falsely reported lost.
#[cfg(windows)]
pub fn process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code = 0u32;
        let read = GetExitCodeProcess(handle, &mut code) != 0;
        CloseHandle(handle);
        read && code as i32 == STILL_ACTIVE
    }
}

#[cfg(target_os = "linux")]
pub fn process_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn process_alive(_pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests;
