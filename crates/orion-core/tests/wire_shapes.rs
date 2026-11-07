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

    let (status, _) = engine.http("POST", "/api/pane/close", &json!({ "pane_id": pane_id }));
    assert_eq!(status, 200);
}
