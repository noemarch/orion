//! One workspace per folder.
//!
//! Asking again for a folder that is already open hands back the pane that is
//! there, so the reader lands on the running agent instead of a second one
//! started on the same files.

use std::path::{Path, PathBuf};

use orion_core::engine::store::Store;
use orion_core::engine::Engine;
use serde_json::{json, Value};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orion-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

fn create(engine: &Engine, cwd: &Path) -> Value {
    let (status, body) = engine.http(
        "POST",
        "/api/workspace/create",
        &json!({ "cwd": cwd.to_string_lossy(), "agent": { "kind": "claude" } }),
    );
    assert_eq!(status, 200, "create failed: {body}");
    body
}

#[test]
fn asking_again_for_the_same_folder_finds_the_workspace_that_is_there() {
    let dir = scratch("one-workspace");
    let cwd = dir.join("project");
    std::fs::create_dir_all(&cwd).expect("a project directory");
    let engine = Engine::with_store(Store::at(dir.join("data")));

    let first = create(&engine, &cwd);
    let first_workspace = first["workspace_id"].as_str().expect("workspace id");
    let first_pane = first["pane_id"].as_str().expect("pane id");

    let second = create(&engine, &cwd);
    assert_eq!(
        second["workspace_id"].as_str(),
        Some(first_workspace),
        "the second request lands on the workspace that is already open: {second}"
    );
    assert_eq!(
        second["pane_id"].as_str(),
        Some(first_pane),
        "and on the agent that is already running: {second}"
    );

    let session = get(&engine, "/api/session");
    let workspaces = session["snapshot"]["workspaces"]
        .as_array()
        .expect("workspaces");
    assert_eq!(
        workspaces.len(),
        1,
        "one folder, one workspace: {workspaces:?}"
    );
    assert_eq!(
        workspaces[0]["workspace_id"].as_str(),
        Some(first_workspace),
        "and it is the workspace that was asked for"
    );
    assert_eq!(
        workspaces[0]["label"].as_str(),
        Some("project"),
        "named after the folder it was opened on"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_different_folder_gets_its_own_workspace() {
    let dir = scratch("two-workspaces");
    let one = dir.join("one");
    let two = dir.join("two");
    std::fs::create_dir_all(&one).expect("one project");
    std::fs::create_dir_all(&two).expect("two projects");
    let engine = Engine::with_store(Store::at(dir.join("data")));

    let first = create(&engine, &one);
    let second = create(&engine, &two);
    assert_ne!(
        first["workspace_id"].as_str(),
        second["workspace_id"].as_str(),
        "two folders, two workspaces"
    );

    let session = get(&engine, "/api/session");
    let workspaces = session["snapshot"]["workspaces"]
        .as_array()
        .expect("workspaces");
    assert_eq!(workspaces.len(), 2, "both folders are open");

    let _ = std::fs::remove_dir_all(&dir);
}

fn get(engine: &Engine, path: &str) -> Value {
    let (status, body) = engine.http("GET", path, &Value::Null);
    assert_eq!(status, 200, "GET {path} failed: {body}");
    body
}
