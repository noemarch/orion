use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::*;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orion-omo-tasks-{tag}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn write_json(path: &Path, value: &Value) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("record dir");
    }
    fs::write(path, value.to_string()).expect("record");
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis() as i64
}

#[test]
fn a_populated_legacy_store_wins() {
    let cwd = temp_dir("legacy-cwd");
    let agent_dir = temp_dir("legacy-agent");
    write_json(
        &cwd.join(".omo/senpi-task/tasks/st_1.json"),
        &json!({ "task_id": "st_1" }),
    );
    assert_eq!(
        state_dir_in(&cwd, &agent_dir),
        cwd.join(".omo").join("senpi-task")
    );
}

#[test]
fn an_empty_legacy_store_does_not_hide_the_project_store() {
    let cwd = temp_dir("empty-cwd");
    let agent_dir = temp_dir("empty-agent");
    fs::create_dir_all(cwd.join(".omo/senpi-task")).expect("empty legacy");
    let dir = state_dir_in(&cwd, &agent_dir);
    assert!(dir.starts_with(agent_dir.join("projects")), "{dir:?}");
    let name = dir
        .parent()
        .and_then(Path::file_name)
        .expect("project key")
        .to_string_lossy()
        .to_string();
    assert!(name.starts_with(&format!("{}-", cwd.file_name().unwrap().to_string_lossy())));
    let hash = name.rsplit('-').next().expect("hash");
    assert_eq!(hash.len(), 12, "{name}");
    assert!(hash.chars().all(|character| character.is_ascii_hexdigit()));
}

#[test]
fn tasks_list_running_first_and_recent_ended() {
    let state = temp_dir("tasks");
    let tasks_dir = state.join("tasks");
    // a fixed clock a minute after the records, so "recent" is a property of the fixture
    let now = chrono::DateTime::parse_from_rfc3339("2026-10-16T21:05:00.000Z")
        .expect("clock")
        .timestamp_millis()
        + 60_000;
    write_json(
        &tasks_dir.join("st_1.json"),
        &json!({
            "task_id": "st_1",
            "parent_session_id": "ses_1",
            "status": "running",
            "host_pid": 111,
            "description": "dead host",
            "started_at": "2026-10-16T21:00:00.000Z",
            "updated_at": "2026-10-16T21:01:00.000Z",
        }),
    );
    write_json(
        &tasks_dir.join("st_2.json"),
        &json!({
            "task_id": "st_2",
            "parent_session_id": "ses_1",
            "status": "completed",
            "task_summary": "finished",
            "terminal_at": "2026-10-16T21:02:00.000Z",
            "started_at": "2026-10-16T21:00:00.000Z",
            "run_stats": { "turns": 2, "tool_calls": 3, "total_tokens": 50 },
        }),
    );
    write_json(
        &tasks_dir.join("st_3.json"),
        &json!({
            "task_id": "st_3",
            "parent_session_id": "someone_else",
            "status": "completed",
            "terminal_at": "2026-10-16T21:02:00.000Z",
        }),
    );
    write_json(
        &tasks_dir.join("st_4.json"),
        &json!({
            "task_id": "st_4",
            "parent_session_id": "ses_1",
            "status": "running",
            "host_pid": 222,
            "task_summary": "alive",
            "started_at": "2026-10-16T21:01:30.000Z",
        }),
    );
    let found = tasks(&state, "ses_1", |pid| pid == 222, now);
    let ids = found
        .iter()
        .map(|task| task["id"].as_str().unwrap_or_default())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["st_4", "st_2", "st_1"], "{found:?}");
    assert_eq!(found[0]["status"], "running");
    assert_eq!(found[0]["ended_at"], Value::Null);
    assert_eq!(found[1]["status"], "completed");
    assert_eq!(found[1]["title"], "finished");
    assert_eq!(found[1]["turns"], 2);
    assert_eq!(found[1]["tool_calls"], 3);
    assert_eq!(found[1]["tokens"], 50);
    assert_eq!(found[2]["status"], "lost");
    assert_eq!(found[2]["ended_at"], "2026-10-16T21:01:00.000Z");
}

#[test]
fn a_task_older_than_a_day_is_left_out() {
    let state = temp_dir("old-task");
    write_json(
        &state.join("tasks/st_1.json"),
        &json!({
            "task_id": "st_1",
            "parent_session_id": "ses_1",
            "status": "completed",
            "terminal_at": "2026-10-14T21:00:00.000Z",
        }),
    );
    assert!(tasks(&state, "ses_1", |_| true, now_ms()).is_empty());
}

#[test]
fn runs_list_active_first_and_keep_their_waves() {
    let state = temp_dir("runs");
    let runs_dir = state.join("dag").join("runs");
    write_json(
        &runs_dir.join("dag_1.json"),
        &json!({
            "runId": "dag_1",
            "parentSessionId": "ses_1",
            "name": "probe",
            "status": "running",
            "startedAt": "2026-10-16T21:00:00.000Z",
            "nodes": [
                { "id": "a", "label": "fetch", "state": "completed" },
                { "id": "b", "label": "write", "state": "running" }
            ],
            "waves": [{ "nodeIds": ["a"] }, { "nodeIds": ["b"] }],
        }),
    );
    write_json(
        &runs_dir.join("dag_2.json"),
        &json!({
            "runId": "dag_2",
            "parentSessionId": "ses_1",
            "name": "old",
            "status": "completed",
            "startedAt": "2026-10-16T20:00:00.000Z",
            "completedAt": "2026-10-16T20:30:00.000Z",
            "nodes": [{ "id": "c", "label": "done", "state": "completed", "error": { "message": "ignored" } }],
            "waves": [{ "nodeIds": ["c"] }],
        }),
    );
    write_json(
        &runs_dir.join("dag_3.json"),
        &json!({
            "runId": "dag_3",
            "parentSessionId": "someone_else",
            "status": "running",
            "nodes": [],
        }),
    );
    let found = runs(&state, "ses_1", now_ms());
    let ids = found
        .iter()
        .map(|run| run["id"].as_str().unwrap_or_default())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["dag_1", "dag_2"], "{found:?}");
    assert_eq!(found[0]["status"], "running");
    assert_eq!(found[0]["waves"][0][0]["label"], "fetch");
    assert_eq!(found[0]["waves"][1][0]["state"], "running");
    assert!(found[1]["ended_at"].is_string());
}

#[test]
fn a_failed_node_carries_its_error() {
    let state = temp_dir("run-error");
    write_json(
        &state.join("dag/runs/dag_1.json"),
        &json!({
            "runId": "dag_1",
            "parentSessionId": "ses_1",
            "status": "failed",
            "nodes": [{ "id": "a", "label": "fetch", "state": "failed", "error": { "message": "network down" } }],
            "waves": [{ "nodeIds": ["a"] }],
        }),
    );
    let found = runs(&state, "ses_1", now_ms());
    assert_eq!(found[0]["waves"][0][0]["error"], "network down");
}
