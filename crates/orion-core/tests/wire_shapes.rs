//! Wire shapes must not drift: every response the conformance suite exercises is checked for the
//! field names the client reads, so a renamed key fails here instead of in the UI.

use orion_core::engine::store::Store;
use orion_core::engine::Engine;
use serde_json::{json, Value};

fn assert_fields(value: &Value, fields: &[&str], what: &str) {
    let object = value
        .as_object()
        .unwrap_or_else(|| panic!("{what} is not an object: {value}"));
    for field in fields {
        assert!(
            object.contains_key(*field),
            "{what} is missing {field}: {value}"
        );
    }
}

fn get(engine: &Engine, path: &str) -> Value {
    let (status, body) = engine.http("GET", path, &Value::Null);
    assert_eq!(status, 200, "GET {path} failed: {body}");
    body
}

#[test]
fn rest_responses_keep_the_field_names_the_client_reads() {
    let engine = Engine::with_store(Store::at(
        std::env::temp_dir().join(format!("orion-wire-{}", uuid::Uuid::new_v4())),
    ));

    let health = get(&engine, "/api/health?scope=bridge");
    assert_fields(&health, &["ok", "auth", "bridge_protocol"], "bridge health");
    assert_fields(
        &health["auth"],
        &["required", "authenticated"],
        "bridge auth",
    );

    let machines = get(&engine, "/api/machines");
    let machine = &machines["machines"][0];
    assert_fields(
        machine,
        &[
            "id", "name", "kind", "enabled", "state", "snapshot", "orion",
        ],
        "machine",
    );
    assert_fields(
        &machine["orion"],
        &["version", "protocol", "terminal_attach"],
        "machine orion",
    );

    let session = get(&engine, "/api/session");
    assert_fields(
        &session["snapshot"],
        &[
            "workspaces",
            "tabs",
            "panes",
            "agents",
            "focused_pane_id",
            "focused_workspace_id",
            "focused_tab_id",
            "protocol",
            "version",
        ],
        "snapshot",
    );

    let agents = get(&engine, "/api/agents");
    let agent = &agents["agents"][0];
    assert_fields(agent, &["kind", "label", "installed"], "agent");

    let integrations = get(&engine, "/api/integrations");
    let integration = &integrations["integrations"][0];
    assert_fields(
        integration,
        &["target", "label", "command", "available", "state"],
        "integration",
    );

    let (status, preferences) = engine.http(
        "PUT",
        "/api/preferences",
        &json!({ "settings": { "theme": "dark" } }),
    );
    assert_eq!(status, 200, "preferences put failed: {preferences}");
    assert_fields(&preferences, &["revision", "settings"], "preferences");
    let fetched = get(&engine, "/api/preferences");
    assert_eq!(fetched["settings"]["theme"], "dark");

    let access = get(&engine, "/api/access");
    assert_fields(
        &access,
        &["tailscale_active", "tailscale_ip", "tailnet_url", "port"],
        "access",
    );
}

#[test]
fn pane_read_and_prompt_keep_their_client_fields() {
    let dir = std::env::temp_dir().join(format!("orion-wire-pane-{}", uuid::Uuid::new_v4()));
    let engine = Engine::with_store(Store::at(dir.join("data")));
    let cwd = dir.join("project");
    std::fs::create_dir_all(&cwd).expect("project directory");

    let (status, created) = engine.http(
        "POST",
        "/api/workspace/create",
        &json!({ "cwd": cwd.to_string_lossy(), "label": "wire", "agent": { "kind": "claude" } }),
    );
    assert_eq!(status, 200, "create failed: {created}");
    assert_fields(
        &created,
        &["pane_id", "workspace_id", "agent_started"],
        "workspace create",
    );
    let pane_id = created["pane_id"].as_str().expect("pane id").to_string();

    let read = get(
        &engine,
        &format!("/api/pane/read?pane_id={pane_id}&source=visible&format=text"),
    );
    assert_fields(
        &read["read"],
        &[
            "pane_id",
            "workspace_id",
            "tab_id",
            "source",
            "format",
            "text",
            "truncated",
            "revision",
        ],
        "pane read",
    );

    let prompt = get(&engine, &format!("/api/pane/prompt?pane_id={pane_id}"));
    assert_fields(
        &prompt,
        &["prompt", "suggestion", "restored"],
        "pane prompt",
    );

    let conversation = get(
        &engine,
        &format!("/api/pane/conversation?pane_id={pane_id}"),
    );
    assert_fields(&conversation, &["source", "turns"], "pane conversation");

    let scroll = get(&engine, &format!("/api/pane/scroll?pane_id={pane_id}"));
    assert_fields(&scroll, &["scroll"], "pane scroll");
    assert_fields(
        &scroll["scroll"],
        &[
            "offset_from_bottom",
            "max_offset_from_bottom",
            "viewport_rows",
        ],
        "pane scroll info",
    );

    let diff = get(
        &engine,
        &format!("/api/workspace/diff/files?pane_id={pane_id}&scope=workspace"),
    );
    assert_fields(
        &diff,
        &["repository", "scope", "files", "total", "limits"],
        "repo diff files",
    );
    assert_fields(
        &diff["repository"],
        &["root", "branch", "head", "detached", "unborn"],
        "repo diff repository",
    );

    let patch = get(
        &engine,
        &format!("/api/workspace/diff/file?pane_id={pane_id}&scope=workspace&path=README.md"),
    );
    assert_fields(
        &patch,
        &[
            "repository",
            "scope",
            "base",
            "head",
            "file",
            "patch",
            "unavailable",
            "truncated",
            "limits",
        ],
        "repo diff file",
    );
    assert_fields(
        &patch["file"],
        &[
            "path",
            "old_path",
            "status",
            "additions",
            "deletions",
            "binary",
        ],
        "repo diff file entry",
    );

    let (status, _) = engine.http("POST", "/api/pane/close", &json!({ "pane_id": pane_id }));
    assert_eq!(status, 200);
}

#[test]
fn the_files_routes_answer_what_the_tree_and_reader_need() {
    let dir = std::env::temp_dir().join(format!("orion-wire-files-{}", uuid::Uuid::new_v4()));
    let engine = Engine::with_store(Store::at(dir.join("data")));
    let project = dir.join("project");
    std::fs::create_dir_all(project.join("src")).expect("project directory");
    std::fs::create_dir_all(project.join("docs")).expect("docs directory");
    std::fs::write(project.join("src").join("main.rs"), "fn main() {}\n").expect("main.rs");
    std::fs::write(project.join("notes.txt"), "notes\n").expect("notes.txt");
    // A folder only this project has: if the tree route ever resolved a relative path against
    // the pane's cwd, this name would come back in the listing below.
    let marker = format!("only-in-project-{}", uuid::Uuid::new_v4());
    std::fs::create_dir_all(project.join(&marker)).expect("marker directory");
    let project_path = project.to_string_lossy().to_string();

    let (status, created) = engine.http(
        "POST",
        "/api/workspace/create",
        &json!({ "cwd": &project_path, "label": "files", "agent": { "kind": "claude" } }),
    );
    assert_eq!(status, 200, "create failed: {created}");
    assert_fields(
        &created,
        &["pane_id", "workspace_id", "agent_started"],
        "workspace create",
    );
    let pane_id = created["pane_id"].as_str().expect("pane id").to_string();
    let workspace_id = created["workspace_id"]
        .as_str()
        .expect("workspace id")
        .to_string();

    // The tree lists one directory at a time, naming the folder it read and offering the files
    // beside the folders so a row can show a file without a second request.
    let listing = get(
        &engine,
        &format!("/api/workspace/directories?path={project_path}&files=1"),
    );
    assert_fields(
        &listing,
        &[
            "path",
            "parent",
            "home",
            "directories",
            "truncated",
            "files",
        ],
        "directory listing",
    );
    assert_eq!(
        listing["path"], project_path,
        "the listing names the directory it read: {listing}"
    );
    let directories = listing["directories"].as_array().expect("directories");
    assert!(
        directories.iter().any(|entry| entry == "docs"),
        "directories must contain docs: {listing}"
    );
    let files = listing["files"].as_array().expect("files");
    assert!(
        files.iter().any(|entry| entry["name"] == "notes.txt"),
        "files must contain the root file: {listing}"
    );
    for entry in files {
        assert_fields(entry, &["name", "size"], "directory file entry");
    }

    // A chat hands the reader a path relative to its workspace cwd; the reader must be able to
    // stat and open it without the UI joining anything itself.
    let stat = get(
        &engine,
        &format!("/api/fs/stat?pane_id={pane_id}&path=src/main.rs"),
    );
    assert_fields(
        &stat,
        &["path", "name", "size", "modified", "mime", "kind"],
        "file stat",
    );
    assert_eq!(stat["name"], "main.rs", "stat names the file: {stat}");
    assert_eq!(stat["kind"], "file", "stat says it is a file: {stat}");

    let file = get(
        &engine,
        &format!("/api/fs/file?pane_id={pane_id}&path=src/main.rs"),
    );
    assert_eq!(
        file["__raw_text__"], "fn main() {}\n",
        "relative read must return the file text: {file}"
    );

    // An absolute path must answer the same way: a chat may hand either form.
    let absolute = project
        .join("src")
        .join("main.rs")
        .to_string_lossy()
        .to_string();
    let absolute_stat = get(
        &engine,
        &format!("/api/fs/stat?pane_id={pane_id}&path={absolute}"),
    );
    assert_eq!(
        absolute_stat["path"], absolute,
        "absolute stat names the file it read: {absolute_stat}"
    );
    assert_eq!(
        absolute_stat["name"], "main.rs",
        "absolute stat names the file: {absolute_stat}"
    );
    let absolute_file = get(
        &engine,
        &format!("/api/fs/file?pane_id={pane_id}&path={absolute}"),
    );
    assert_eq!(
        absolute_file["__raw_text__"], "fn main() {}\n",
        "absolute read must return the file text: {absolute_file}"
    );

    // The reader's error path is a status, not an empty body: "no readable file at that path".
    let (missing_status, missing) = engine.http(
        "GET",
        &format!("/api/fs/file?pane_id={pane_id}&path=does/not/exist.rs"),
        &Value::Null,
    );
    assert_eq!(
        missing_status, 404,
        "a missing file must answer 404: {missing}"
    );

    // WHY: the tree always sends an absolute path. This route takes no pane_id at all, so it
    // takes a relative path as given (relative to the engine process's own cwd) instead of
    // joining it onto a pane's cwd — only `~` is expanded. A relative path therefore never
    // reaches the pane's project.
    let relative = get(&engine, "/api/workspace/directories?path=.");
    assert_ne!(
        relative["path"].as_str(),
        Some(project_path.as_str()),
        "a relative path must not resolve to the pane's cwd: {relative}"
    );
    let relative_directories = relative["directories"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !relative_directories.iter().any(|entry| entry == &marker),
        "a relative path must not list the pane's project: {relative}"
    );

    // Close before removing: the workspace owns a watcher on the project directory, so the
    // directory cannot be deleted while it is open.
    let _ = engine.http(
        "POST",
        "/api/workspace/close",
        &json!({ "workspace_id": workspace_id }),
    );
    let _ = std::fs::remove_dir_all(&dir);
}
