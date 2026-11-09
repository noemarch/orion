//! The engine seam.
//!
//! Every client request — the Tauri IPC bridge on desktop, the HTTP/WS gateway for browsers —
//! funnels through this module. The transport owns framing and authentication; the engine owns
//! every answer. Two shapes reach it:
//!
//! - **Requests** ([`Engine::dispatch_from`]) carry a frame the client sent over its push
//!   connection: `attach`, `input`, `submit`, `ping`, or the `http` envelope the desktop shell
//!   uses to run the REST client unchanged.
//! - **Routes** ([`Engine::http`]) are the REST surface itself: `/api/session`, `/api/pane/read`,
//!   `/api/workspace/create` and the rest of `shared/protocol.ts`.
//!
//! State is one session: workspaces, their tabs, and the agent panes running in them. A pane is
//! always an agent CLI; Orion has no shell panes. Output reaches connected clients as
//! `pty-data`, status changes as `pane-status`, and structure changes as `session-changed`.

pub mod auth;
pub(crate) mod connection_registry;
pub mod devices;
pub mod diff;
pub(crate) mod frame_dispatch;
pub mod introspect;
pub mod omo_tasks;
pub mod pane;
pub mod push;
pub(crate) mod route_table;
pub(crate) mod session;
pub mod store;
pub mod tailscale;
pub(crate) mod transcript_manager;
pub mod watcher;
pub mod worktree;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::{json, Value};

use self::connection_registry::ConnectionRegistry;
use self::devices::DeviceStore;
use self::frame_dispatch::FrameDispatch;
use self::pane::{Pane, PaneEvent};
use self::push::PushService;
use self::route_table::RouteTable;
use self::session::SessionState;
use self::store::Store;
use self::transcript_manager::TranscriptManager;
use self::watcher::Watcher;

pub use self::pane::FrameSink;
pub use self::pane::PaneConfig;
pub use self::pane::Role;
pub use crate::agents::LaunchSpec;

/// The wire protocol revision the engine answers with. Clients compare it with the revision they
/// were built against; a mismatch is a compatibility error, never a silent downgrade.
pub const PROTOCOL_VERSION: u32 = 1;

/// What this engine advertises in its snapshot. A client only uses a verb it sees here, so a
/// feature is listed the day it works, never before.
static FEATURES: [&str; 3] = ["ping", "submit", "pending-input"];

/// What this engine advertises (shared with frame dispatch).
pub fn features() -> &'static [&'static str] {
    &FEATURES
}

pub fn expand_home(value: &str, home: &Path) -> PathBuf {
    if value == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        return home.join(rest);
    }
    PathBuf::from(value)
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
}

/// The process-wide engine. Cheap to clone: state lives behind the handles it owns.
#[derive(Clone)]
pub struct Engine {
    session: Arc<SessionState>,
    connections: Arc<ConnectionRegistry>,
    devices: Arc<DeviceStore>,
    version: String,
    routes: Arc<RouteTable>,
    frames: Arc<FrameDispatch>,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    pub fn new() -> Self {
        Self::with_store(Store::default())
    }

    pub fn with_store(store: Store) -> Self {
        if std::env::var_os("ORION_SKIP_LEGACY_IMPORT").is_none() {
            let _ = store.import_predecessor_preferences();
        }
        let preferences = store.load_preferences();
        let devices = DeviceStore::new(&store);
        let push = PushService::new(&store);
        let version = env!("CARGO_PKG_VERSION").to_string();

        let session = Arc::new(SessionState::new());
        let connections = Arc::new(ConnectionRegistry::new());
        let transcripts = Arc::new(TranscriptManager::new());
        let store = Arc::new(store);
        let devices = Arc::new(devices);
        let push = Arc::new(push);
        let watchers: Arc<Mutex<HashMap<String, Watcher>>> = Arc::new(Mutex::new(HashMap::new()));
        let preferences = Arc::new(Mutex::new(preferences));

        let routes = Arc::new(RouteTable::new(
            Arc::clone(&session),
            Arc::clone(&transcripts),
            Arc::clone(&store),
            Arc::clone(&push),
            Arc::clone(&devices),
            Arc::clone(&watchers),
            Arc::clone(&preferences),
            version.clone(),
            Arc::clone(&connections),
        ));
        let frames = Arc::new(FrameDispatch::new(
            Arc::clone(&session),
            Arc::clone(&connections),
            {
                let routes = Arc::clone(&routes);
                Arc::new(move |method, path, body| routes.http(method, path, body))
            },
        ));

        Self {
            session,
            connections,
            devices,
            version,
            routes,
            frames,
        }
    }

    /// The identity this engine reports (version and protocol) for a client's first frame.
    pub fn identity(&self) -> Value {
        json!({
            "ok": true,
            "version": self.version,
            "protocol": PROTOCOL_VERSION,
        })
    }

    /// Registers a push connection. The caller sends frames by calling the sink; nothing is
    /// buffered, so a connection that never reads cannot grow memory here.
    pub fn connect(&self, sink: FrameSink) -> String {
        self.connections.connect(sink)
    }

    /// Sends one frame to an open connection, if it is still open.
    pub fn send_to(&self, connection: &str, frame: Value) {
        self.connections.send_to(connection, frame);
    }

    /// Forgets a connection and every attachment it held.
    pub fn disconnect(&self, connection: &str) {
        let attached = self.connections.disconnect(connection);
        for pane_id in attached {
            if let Some(pane) = self.session.pane(&pane_id) {
                pane.detach(connection);
            }
        }
    }

    /// Whether a connection's page is active right now (within the last 30 seconds). A present
    /// connection already sees the UI live, so it does not need push duplicates of the same event.
    pub fn is_present(&self, connection_id: &str) -> bool {
        self.connections.is_present(connection_id)
    }

    /// The frame a freshly connected client receives before anything else.
    pub fn snapshot_frame(&self) -> Value {
        json!({
            "type": "snapshot",
            "features": FEATURES,
            "snapshot": self.snapshot_value(),
        })
    }

    /// The session as `SessionSnapshot`.
    pub fn snapshot_value(&self) -> Value {
        self.session.snapshot_value(&self.version, PROTOCOL_VERSION)
    }

    /// Answers a client's liveness probe. Only the echoed id completes the probe, so an id is
    /// required: a pong without one would answer nothing.
    pub fn pong(&self, id: &Value) -> Value {
        json!({ "type": "pong", "id": id })
    }

    /// Spawns a new agent pane and registers it in the session.
    ///
    /// This is the public seam external UIs (like the GPUI shell) use to start agents directly.
    /// The pane's Workspace and Tab are created when they are not open yet, and the new pane
    /// becomes the focused one, so `/api/session` and the chat route can find it immediately.
    ///
    /// `config.spec` is the pane's resolved launch: production callers build it with
    /// [`LaunchSpec::for_agent`], so the process is an identified agent CLI. Tests may pass a
    /// stand-in through this seam to keep the bootstrap deterministic.
    pub fn spawn_pane(&self, config: PaneConfig) -> crate::error::Result<Arc<Pane>> {
        self.routes.spawn_pane(config)
    }

    /// Dispatches one request without a push connection. Used by bounded commands and tests.
    pub fn dispatch(&self, request: &Value) -> crate::error::Result<Value, String> {
        self.dispatch_from(None, request)
            .map(|answer| answer.unwrap_or_else(|| json!({ "type": "ok" })))
    }

    /// Dispatches one client frame. `None` means the answer already went to the connection's
    /// sink (attach's replay, a broadcast), and there is nothing for the transport to send.
    pub fn dispatch_from(
        &self,
        connection: Option<&str>,
        request: &Value,
    ) -> crate::error::Result<Option<Value>, String> {
        self.frames
            .dispatch_from(connection, request, &self.version, PROTOCOL_VERSION)
    }

    /// Runs one REST call. Both transports use it: the desktop shell's `http` envelope and,
    /// from M2, the Axum gateway.
    pub fn http(&self, method: &str, path: &str, body: &Value) -> (u16, Value) {
        let result = self.routes.http(method, path, body);
        // Handlers that changed the session structure need every client to repaint.
        let mutating = [
            "/api/workspace/create",
            "/api/workspace/rename",
            "/api/workspace/move",
            "/api/workspace/close",
            "/api/worktree/create",
            "/api/worktree/open",
            "/api/worktree/remove",
            "/api/tab/create",
            "/api/tab/rename",
            "/api/tab/close",
            "/api/tab/move",
            "/api/pane/close",
            "/api/pane/split",
            "/api/pane/zoom",
            "/api/pane/swap",
            "/api/pane/resize",
            "/api/layout/ratio",
            "/api/pane/rename",
        ];
        if mutating.iter().any(|prefix| path.starts_with(prefix)) {
            self.broadcast(json!({ "type": "session-changed" }));
        }
        result
    }

    fn broadcast(&self, frame: Value) {
        self.connections.broadcast(frame);
    }

    /// Whether `token` belongs to a paired device (checked by the gateway's auth gate).
    pub fn device_authenticated(&self, token: &str) -> bool {
        self.devices.matches(token)
    }

    /// Restores the workspaces saved by the last run.
    pub fn restore(&self) {
        self.routes.restore();
    }
}

/// Fans one pane event out to clients and evaluates push predicates.
pub(crate) fn on_pane_event(
    session: &SessionState,
    connections: &ConnectionRegistry,
    push: &PushService,
    event: PaneEvent,
) {
    use crate::protocol::AgentStatus;
    match event {
        PaneEvent::Status {
            pane_id,
            status,
            completion,
        } => {
            connections.broadcast(json!({
                "type": "pane-status",
                "pane_id": pane_id,
                "agent_status": status.as_wire(),
            }));
            if completion {
                connections.broadcast(json!({ "type": "session-changed" }));
            }

            if status == AgentStatus::Blocked || status == AgentStatus::Done {
                let (title, agent_kind) = session.pane_push_info(&pane_id);
                let workspace_id = session.workspace_id_of_pane(&pane_id);
                let body = match status {
                    AgentStatus::Blocked => "waiting for your input".to_string(),
                    AgentStatus::Done => "work finished".to_string(),
                    _ => String::new(),
                };
                if !body.is_empty() {
                    let payload = crate::protocol::PushPayload {
                        machine_id: Some("local".to_string()),
                        pane_id: Some(pane_id.clone()),
                        workspace_id,
                        title,
                        body,
                        tag: format!("orion-pane-{pane_id}"),
                    };
                    push.broadcast(&payload, status, agent_kind.as_deref());
                }
            }
        }
        PaneEvent::Exited { pane_id, code } => {
            connections.broadcast(json!({
                "type": "pty-exit",
                "pane_id": pane_id,
                "code": code,
            }));
            connections.broadcast(json!({
                "type": "pane-exited",
                "pane_id": pane_id,
            }));
            connections.broadcast(json!({ "type": "session-changed" }));

            let (title, agent_kind) = session.pane_push_info(&pane_id);
            let workspace_id = session.workspace_id_of_pane(&pane_id);
            let payload = crate::protocol::PushPayload {
                machine_id: Some("local".to_string()),
                pane_id: Some(pane_id.clone()),
                workspace_id,
                title,
                body: "terminal ended".to_string(),
                tag: format!("orion-pane-{pane_id}"),
            };
            push.broadcast(&payload, AgentStatus::Done, agent_kind.as_deref());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn engine() -> Engine {
        std::env::set_var("ORION_SKIP_LEGACY_IMPORT", "1");
        let dir = std::env::temp_dir().join(format!("orion-engine-{}", Uuid::new_v4()));
        Engine::with_store(Store::at(dir))
    }

    #[test]
    fn health_reports_the_protocol_and_the_shell_fields() {
        let engine = engine();
        let (status, body) = engine.http("GET", "/api/health", &Value::Null);
        assert_eq!(status, 200);
        assert_eq!(body["ok"], true);
        assert_eq!(body["orion"]["protocol"], PROTOCOL_VERSION);
        let (_, bridge) = engine.http("GET", "/api/health?scope=bridge", &Value::Null);
        assert_eq!(bridge["auth"]["authenticated"], true);
        assert_eq!(bridge["bridge_protocol"], PROTOCOL_VERSION);
    }

    #[test]
    fn snapshot_is_a_valid_empty_session() {
        let engine = engine();
        let (status, body) = engine.http("GET", "/api/session", &Value::Null);
        assert_eq!(status, 200);
        for key in ["agents", "layouts", "panes", "tabs", "workspaces"] {
            assert!(
                body["snapshot"][key].as_array().is_some(),
                "{key} is an array"
            );
        }
    }

    #[test]
    fn the_roster_names_one_local_machine() {
        let engine = engine();
        let (status, body) = engine.http("GET", "/api/machines", &Value::Null);
        assert_eq!(status, 200);
        let machines = body["machines"].as_array().unwrap();
        assert_eq!(machines.len(), 1);
        assert_eq!(machines[0]["id"], "local");
        assert_eq!(machines[0]["kind"], "local");
        assert_eq!(machines[0]["orion"]["terminal_attach"], true);
    }

    #[test]
    fn a_workspace_without_an_agent_is_refused() {
        let engine = engine();
        let (status, body) = engine.http("POST", "/api/workspace/create", &json!({}));
        assert_eq!(status, 400);
        assert_eq!(body["error"]["code"], "invalid_request");
    }

    #[test]
    fn omo_tasks_names_the_pane_it_tells_about() {
        let engine = engine();
        let (status, body) = engine.http("GET", "/api/pane/omo-tasks", &Value::Null);
        assert_eq!(status, 400);
        assert_eq!(body["error"]["code"], "invalid_request");
        let (status, body) = engine.http("GET", "/api/pane/omo-tasks?pane_id=nope", &Value::Null);
        assert_eq!(status, 404);
        assert_eq!(body["error"]["code"], "pane_not_found");
        // read-only: another verb is refused, not answered
        let (status, _) = engine.http("POST", "/api/pane/omo-tasks?pane_id=nope", &Value::Null);
        assert_eq!(status, 404);
    }

    #[test]
    fn plugins_actions_lists_no_plugins() {
        let engine = engine();
        let (status, body) = engine.http("GET", "/api/plugins/actions", &Value::Null);
        assert_eq!(status, 200);
        let plugins = body["plugins"].as_array().unwrap();
        assert_eq!(plugins.len(), 0);
    }

    #[test]
    fn plugin_action_reports_not_installed() {
        let engine = engine();
        let (status, body) = engine.http(
            "POST",
            "/api/plugin/action",
            &json!({ "plugin_id": "example", "action_id": "apply" }),
        );
        assert_eq!(status, 404);
        assert_eq!(body["error"]["code"], "plugin_not_found");
    }

    #[test]
    fn plugin_action_status_reports_no_runs() {
        let engine = engine();
        let (status, body) = engine.http(
            "GET",
            "/api/plugin/action?plugin_id=example&log_id=abc",
            &Value::Null,
        );
        assert_eq!(status, 404);
        assert_eq!(body["error"]["code"], "plugin_log_not_found");
    }

    #[test]
    fn pane_commands_lists_no_commands() {
        let engine = engine();
        let (status, body) = engine.http("GET", "/api/pane/commands?pane_id=nope", &Value::Null);
        assert_eq!(status, 200);
        let commands = body["commands"].as_array().unwrap();
        assert_eq!(commands.len(), 0);
    }
    #[test]
    fn echo_round_trips_a_megabyte_byte_for_byte() {
        // One MiB of four-byte characters exercises every UTF-8 boundary and the JSON string path.
        let payload = "\u{1F600}\u{00E9}\u{4E2D}\u{0000}".repeat(1024 * 64);
        let answer = engine()
            .dispatch(&json!({ "type": "echo", "payload": payload }))
            .unwrap();
        assert_eq!(answer["payload"], json!(payload));
    }

    #[test]
    fn an_unknown_request_is_an_error_not_a_panic() {
        assert!(engine().dispatch(&json!({ "type": "nope" })).is_err());
        assert!(engine().dispatch(&json!({})).is_err());
    }

    #[test]
    fn connect_frames_are_accepted_without_error() {
        let engine = engine();
        let role = engine
            .dispatch(&json!({ "type": "role", "mode": "observe" }))
            .unwrap();
        assert_eq!(role["type"], "role-ack");
        assert_eq!(role["mode"], "observe");

        for kind in ["presence", "detach", "resize", "release-size"] {
            let request = if kind == "presence" {
                json!({ "type": "presence", "active": true })
            } else if kind == "resize" {
                json!({ "type": "resize", "pane_id": "p1", "cols": 100, "rows": 30 })
            } else {
                json!({ "type": kind, "pane_id": "p1" })
            };
            assert!(engine.dispatch(&request).is_ok(), "{kind}");
        }

        // Attaching names a pane that must exist: an unknown id is an error, not a pretend
        // attachment that the next output frame would fan out to.
        assert!(engine
            .dispatch(&json!({ "type": "attach", "pane_id": "p1", "cols": 80, "rows": 24 }))
            .is_err());
    }

    #[test]
    fn an_unknown_pane_is_a_typed_error_not_a_panic() {
        let engine = engine();
        let (status, body) = engine.http(
            "GET",
            "/api/pane/read?pane_id=missing&source=recent",
            &Value::Null,
        );
        assert_eq!(status, 404);
        assert_eq!(body["error"]["code"], "pane_not_found");
    }

    #[test]
    fn preferences_round_trip_their_revision() {
        let engine = engine();
        let (status, _) = engine.http("GET", "/api/preferences", &Value::Null);
        assert_eq!(status, 404);
        let (status, body) = engine.http(
            "PUT",
            "/api/preferences",
            &json!({ "settings": { "theme": "dark" } }),
        );
        assert_eq!(status, 200);
        assert_eq!(body["revision"], 1);
        let (_, again) = engine.http("GET", "/api/preferences", &Value::Null);
        assert_eq!(again["settings"]["theme"], "dark");
    }

    #[test]
    fn a_tiny_frame_sink_receives_broadcasts() {
        let engine = engine();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = Arc::clone(&seen);
        engine.connect(Arc::new(move |frame| sink_seen.lock().push(frame)));
        engine.broadcast(json!({ "type": "session-changed" }));
        assert_eq!(seen.lock().len(), 1);
    }

    #[test]
    fn presence_suppresses_broadcasts() {
        let engine = engine();
        let present_seen = Arc::new(Mutex::new(Vec::new()));
        let absent_seen = Arc::new(Mutex::new(Vec::new()));
        let present_sink = Arc::clone(&present_seen);
        let absent_sink = Arc::clone(&absent_seen);
        let present_id = engine.connect(Arc::new(move |frame| present_sink.lock().push(frame)));
        let absent_id = engine.connect(Arc::new(move |frame| absent_sink.lock().push(frame)));

        // Mark the first connection as active on its page.
        engine
            .dispatch_from(
                Some(&present_id),
                &json!({ "type": "presence", "active": true }),
            )
            .unwrap();

        engine.broadcast(json!({
            "type": "pane-status",
            "pane_id": "p1",
            "agent_status": "working",
        }));

        // The present connection already sees the UI live: nothing pushed to it.
        assert!(
            present_seen.lock().is_empty(),
            "a present connection must not receive pane-status it already sees live: {present_id}"
        );
        // The absent connection still receives the frame.
        assert_eq!(
            absent_seen.lock().len(),
            1,
            "an absent connection must still receive pane-status: {absent_id}"
        );
    }

    #[test]
    fn worktrees_create_list_and_remove() {
        let dir = std::env::temp_dir().join(format!("orion-wt-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("repo dir");
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .expect("git runs");
            assert!(
                output.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        std::fs::write(dir.join("file.txt"), "hello").expect("seed file");
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "seed"]);

        let engine = Engine::with_store(Store::at(dir.join("orion-data")));
        let (status, created) = engine.http(
            "POST",
            "/api/workspace/create",
            &json!({ "cwd": dir.to_string_lossy(), "label": "main", "agent": { "kind": "claude" } }),
        );
        assert_eq!(status, 200, "workspace create failed: {created}");
        let workspace_id = created["workspace_id"]
            .as_str()
            .expect("workspace id")
            .to_string();

        let worktree = dir.join("feature-wt");
        let second = dir.join("second-wt");
        let (status, opened) = engine.http(
            "POST",
            "/api/worktree/create",
            &json!({ "workspace_id": workspace_id, "branch": "feature", "path": worktree.to_string_lossy() }),
        );
        assert_eq!(status, 200, "worktree create failed: {opened}");
        assert_eq!(opened["branch"], "feature");
        assert!(
            worktree.join("file.txt").exists(),
            "the checkout has the seed file"
        );

        // Two agents in parallel worktrees: a second checkout opens while the first is live.
        let (status, second_opened) = engine.http(
            "POST",
            "/api/worktree/create",
            &json!({ "workspace_id": workspace_id, "branch": "second", "path": second.to_string_lossy() }),
        );
        assert_eq!(
            status, 200,
            "second worktree create failed: {second_opened}"
        );

        let (_, listed) = engine.http(
            "GET",
            &format!("/api/worktree/list?workspace_id={workspace_id}"),
            &Value::Null,
        );
        let worktrees = listed["worktrees"].as_array().expect("worktrees");
        assert_eq!(worktrees.len(), 3, "main + two worktrees: {listed}");
        let (_, session) = engine.http("GET", "/api/session", &Value::Null);
        let workspace_ids: Vec<&str> = session["snapshot"]["workspaces"]
            .as_array()
            .expect("workspaces")
            .iter()
            .map(|workspace| workspace["workspace_id"].as_str().expect("id"))
            .collect();
        assert!(
            workspace_ids.contains(&opened["workspace_id"].as_str().expect("opened id"))
                && workspace_ids
                    .contains(&second_opened["workspace_id"].as_str().expect("second id")),
            "both worktree workspaces are open: {workspace_ids:?}"
        );

        let (status, removed) = engine.http(
            "POST",
            "/api/worktree/remove",
            &json!({ "workspace_id": opened["workspace_id"], "force": true }),
        );
        assert_eq!(status, 200, "worktree remove failed: {removed}");
        assert!(!worktree.exists(), "the checkout is gone after remove");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn workspaces_and_tabs_rename_move_and_close() {
        let dir = std::env::temp_dir().join(format!("orion-manage-{}", Uuid::new_v4()));
        let engine = Engine::with_store(Store::at(dir.clone()));
        let cwd = dir.join("project");
        std::fs::create_dir_all(&cwd).expect("project directory");
        let (status, created) = engine.http(
            "POST",
            "/api/workspace/create",
            &json!({ "cwd": cwd.to_string_lossy(), "label": "one", "agent": { "kind": "claude" } }),
        );
        assert_eq!(status, 200, "create failed: {created}");
        let workspace_id = created["workspace_id"]
            .as_str()
            .expect("workspace id")
            .to_string();
        let _pane_id = created["pane_id"].as_str().expect("pane id").to_string();

        let (status, renamed) = engine.http(
            "POST",
            "/api/workspace/rename",
            &json!({ "workspace_id": workspace_id, "label": "renamed" }),
        );
        assert_eq!(status, 200, "rename failed: {renamed}");
        let (_, session) = engine.http("GET", "/api/session", &Value::Null);
        assert_eq!(session["snapshot"]["workspaces"][0]["label"], "renamed");
        let tab_id = session["snapshot"]["tabs"][0]["tab_id"]
            .as_str()
            .expect("tab id")
            .to_string();

        let (status, renamed) = engine.http(
            "POST",
            "/api/tab/rename",
            &json!({ "tab_id": tab_id, "label": "tab-two" }),
        );
        assert_eq!(status, 200, "tab rename failed: {renamed}");

        let (status, moved) = engine.http(
            "POST",
            "/api/tab/move",
            &json!({ "tab_id": tab_id, "insert_index": 0 }),
        );
        assert_eq!(status, 200, "tab move failed: {moved}");

        let (status, closed) = engine.http(
            "POST",
            "/api/workspace/close",
            &json!({ "workspace_id": workspace_id }),
        );
        assert_eq!(status, 200, "workspace close failed: {closed}");
        let (_, session) = engine.http("GET", "/api/session", &Value::Null);
        assert!(session["snapshot"]["workspaces"]
            .as_array()
            .expect("workspaces")
            .is_empty());
        assert!(session["snapshot"]["panes"]
            .as_array()
            .expect("panes")
            .is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn split_pane_changes_tab_layout() {
        let dir = std::env::temp_dir().join(format!("orion-split-{}", Uuid::new_v4()));
        let engine = Engine::with_store(Store::at(dir.clone()));
        let cwd = dir.join("project");
        std::fs::create_dir_all(&cwd).expect("project directory");

        let (status, created) = engine.http(
            "POST",
            "/api/workspace/create",
            &json!({ "cwd": cwd.to_string_lossy(), "label": "split", "agent": { "kind": "claude" } }),
        );
        assert_eq!(status, 200, "create failed: {created}");
        let pane_id = created["pane_id"].as_str().expect("pane id").to_string();

        // Split to the right: tab becomes SplitHorizontal.
        let (status, split) = engine.http(
            "POST",
            "/api/pane/split",
            &json!({ "pane_id": pane_id, "direction": "right" }),
        );
        assert_eq!(status, 200, "split right failed: {split}");
        assert_eq!(split["ok"], true);
        let new_pane_id = split["pane"]["pane_id"]
            .as_str()
            .expect("new pane id")
            .to_string();
        assert_ne!(new_pane_id, pane_id);

        let (_, session) = engine.http("GET", "/api/session", &Value::Null);
        let tabs = session["snapshot"]["tabs"].as_array().expect("tabs");
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0]["layout"], "horizontal");
        assert_eq!(tabs[0]["pane_count"], 2);

        // Split below: tab becomes SplitVertical.
        let (status, split) = engine.http(
            "POST",
            "/api/pane/split",
            &json!({ "pane_id": pane_id, "direction": "down" }),
        );
        assert_eq!(status, 200, "split down failed: {split}");

        let (_, session) = engine.http("GET", "/api/session", &Value::Null);
        let tabs = session["snapshot"]["tabs"].as_array().expect("tabs");
        assert_eq!(tabs[0]["layout"], "vertical");
        assert_eq!(tabs[0]["pane_count"], 3);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn zoom_pane_toggles_tab_zoom() {
        let dir = std::env::temp_dir().join(format!("orion-zoom-{}", Uuid::new_v4()));
        let engine = Engine::with_store(Store::at(dir.clone()));
        let cwd = dir.join("project");
        std::fs::create_dir_all(&cwd).expect("project directory");

        let (status, created) = engine.http(
            "POST",
            "/api/workspace/create",
            &json!({ "cwd": cwd.to_string_lossy(), "label": "zoom", "agent": { "kind": "claude" } }),
        );
        assert_eq!(status, 200, "create failed: {created}");
        let pane_id = created["pane_id"].as_str().expect("pane id").to_string();

        // A single-pane tab cannot be zoomed.
        let (status, zoom) = engine.http("POST", "/api/pane/zoom", &json!({ "pane_id": pane_id }));
        assert_eq!(status, 200, "zoom single failed: {zoom}");
        assert_eq!(zoom["zoomed"], false);
        assert_eq!(zoom["reason"], "single_pane");

        // Split so the tab has two panes.
        let (status, _split) = engine.http(
            "POST",
            "/api/pane/split",
            &json!({ "pane_id": pane_id, "direction": "right" }),
        );
        assert_eq!(status, 200);

        // Zoom: tab shows that pane alone.
        let (status, zoom) = engine.http("POST", "/api/pane/zoom", &json!({ "pane_id": pane_id }));
        assert_eq!(status, 200, "zoom failed: {zoom}");
        assert_eq!(zoom["zoomed"], true);
        assert_eq!(zoom["changed"], true);

        let (_, session) = engine.http("GET", "/api/session", &Value::Null);
        let tabs = session["snapshot"]["tabs"].as_array().expect("tabs");
        assert_eq!(tabs[0]["zoomed_pane"], pane_id);

        // Toggle again un-zooms.
        let (status, zoom) = engine.http("POST", "/api/pane/zoom", &json!({ "pane_id": pane_id }));
        assert_eq!(status, 200, "unzoom failed: {zoom}");
        assert_eq!(zoom["zoomed"], false);

        // Zoom on, then mode=on again reports already_zoomed.
        let (status, _) = engine.http(
            "POST",
            "/api/pane/zoom",
            &json!({ "pane_id": pane_id, "mode": "on" }),
        );
        assert_eq!(status, 200);
        let (status, zoom) = engine.http(
            "POST",
            "/api/pane/zoom",
            &json!({ "pane_id": pane_id, "mode": "on" }),
        );
        assert_eq!(status, 200);
        assert_eq!(zoom["reason"], "already_zoomed");

        // Mode=off un-zooms; repeating reports already_unzoomed.
        let (status, _) = engine.http(
            "POST",
            "/api/pane/zoom",
            &json!({ "pane_id": pane_id, "mode": "off" }),
        );
        assert_eq!(status, 200);
        let (status, zoom) = engine.http(
            "POST",
            "/api/pane/zoom",
            &json!({ "pane_id": pane_id, "mode": "off" }),
        );
        assert_eq!(status, 200);
        assert_eq!(zoom["reason"], "already_unzoomed");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn swap_pane_exchanges_same_tab_positions() {
        let dir = std::env::temp_dir().join(format!("orion-swap-{}", Uuid::new_v4()));
        let engine = Engine::with_store(Store::at(dir.clone()));
        let cwd = dir.join("project");
        std::fs::create_dir_all(&cwd).expect("project directory");

        let (status, created) = engine.http(
            "POST",
            "/api/workspace/create",
            &json!({ "cwd": cwd.to_string_lossy(), "label": "swap", "agent": { "kind": "claude" } }),
        );
        assert_eq!(status, 200);
        let pane_a = created["pane_id"].as_str().expect("pane a").to_string();

        let (status, split) = engine.http(
            "POST",
            "/api/pane/split",
            &json!({ "pane_id": pane_a, "direction": "right" }),
        );
        assert_eq!(status, 200);
        let pane_b = split["pane"]["pane_id"]
            .as_str()
            .expect("pane b")
            .to_string();

        // Directional swap: pane_a moves right (into pane_b's spot).
        let (status, swapped) = engine.http(
            "POST",
            "/api/pane/swap",
            &json!({ "pane_id": pane_a, "direction": "right" }),
        );
        assert_eq!(status, 200, "swap failed: {swapped}");
        assert_eq!(swapped["changed"], true);
        assert_eq!(swapped["target_pane_id"], pane_b);

        // Swapping right again: pane_a is now last, so no neighbour there.
        let (status, swapped) = engine.http(
            "POST",
            "/api/pane/swap",
            &json!({ "pane_id": pane_a, "direction": "right" }),
        );
        assert_eq!(status, 200);
        assert_eq!(swapped["changed"], false);
        assert_eq!(swapped["reason"], "no_neighbor");

        // Explicit target swap back.
        let (status, swapped) = engine.http(
            "POST",
            "/api/pane/swap",
            &json!({ "pane_id": pane_a, "target_pane_id": pane_b }),
        );
        assert_eq!(status, 200, "explicit swap failed: {swapped}");
        assert_eq!(swapped["changed"], true);

        // Same pane swap is a no-op.
        let (status, swapped) = engine.http(
            "POST",
            "/api/pane/swap",
            &json!({ "pane_id": pane_a, "target_pane_id": pane_a }),
        );
        assert_eq!(status, 200);
        assert_eq!(swapped["reason"], "same_pane");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resize_pane_moves_border_between_neighbours() {
        let dir = std::env::temp_dir().join(format!("orion-resize-{}", Uuid::new_v4()));
        let engine = Engine::with_store(Store::at(dir.clone()));
        let cwd = dir.join("project");
        std::fs::create_dir_all(&cwd).expect("project directory");

        let (status, created) = engine.http(
            "POST",
            "/api/workspace/create",
            &json!({ "cwd": cwd.to_string_lossy(), "label": "resize", "agent": { "kind": "claude" } }),
        );
        assert_eq!(status, 200);
        let pane_id = created["pane_id"].as_str().expect("pane id").to_string();

        // Single pane: no border to move.
        let (status, resized) = engine.http(
            "POST",
            "/api/pane/resize",
            &json!({ "pane_id": pane_id, "direction": "right" }),
        );
        assert_eq!(status, 200, "resize single failed: {resized}");
        assert_eq!(resized["changed"], false);

        // Split so there is a border.
        let (status, _) = engine.http(
            "POST",
            "/api/pane/split",
            &json!({ "pane_id": pane_id, "direction": "right" }),
        );
        assert_eq!(status, 200);

        let (status, resized) = engine.http(
            "POST",
            "/api/pane/resize",
            &json!({ "pane_id": pane_id, "direction": "right" }),
        );
        assert_eq!(status, 200, "resize failed: {resized}");
        assert_eq!(resized["changed"], true);

        // An excessive amount is refused.
        let (status, _) = engine.http(
            "POST",
            "/api/pane/resize",
            &json!({ "pane_id": pane_id, "direction": "right", "amount": 0.9 }),
        );
        assert_eq!(status, 400, "amount 0.9 must be refused");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn layout_ratio_sets_split_ratio_within_bounds() {
        let dir = std::env::temp_dir().join(format!("orion-ratio-{}", Uuid::new_v4()));
        let engine = Engine::with_store(Store::at(dir.clone()));
        let cwd = dir.join("project");
        std::fs::create_dir_all(&cwd).expect("project directory");

        let (status, created) = engine.http(
            "POST",
            "/api/workspace/create",
            &json!({ "cwd": cwd.to_string_lossy(), "label": "ratio", "agent": { "kind": "claude" } }),
        );
        assert_eq!(status, 200);
        let pane_id = created["pane_id"].as_str().expect("pane id").to_string();
        let (_, session) = engine.http("GET", "/api/session", &Value::Null);
        let tab_id = session["snapshot"]["tabs"][0]["tab_id"]
            .as_str()
            .expect("tab id")
            .to_string();

        // Single pane: nothing to ratio.
        let (status, ratio) = engine.http(
            "POST",
            "/api/layout/ratio",
            &json!({ "tab_id": tab_id, "path": [], "ratio": 0.5 }),
        );
        assert_eq!(status, 200, "ratio single failed: {ratio}");
        assert_eq!(ratio["changed"], false);
        assert_eq!(ratio["reason"], "single_pane");

        let (status, _) = engine.http(
            "POST",
            "/api/pane/split",
            &json!({ "pane_id": pane_id, "direction": "right" }),
        );
        assert_eq!(status, 200);

        let (status, ratio) = engine.http(
            "POST",
            "/api/layout/ratio",
            &json!({ "tab_id": tab_id, "path": [], "ratio": 0.6 }),
        );
        assert_eq!(status, 200, "ratio failed: {ratio}");
        assert_eq!(ratio["changed"], true);
        assert_eq!(ratio["ratio"], 0.6);

        let (status, _) = engine.http(
            "POST",
            "/api/layout/ratio",
            &json!({ "tab_id": tab_id, "path": [], "ratio": 0.05 }),
        );
        assert_eq!(status, 400, "ratio 0.05 must be refused");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pane_plan_returns_null_when_no_plan() {
        let engine = engine();
        let (status, body) = engine.http("GET", "/api/pane/plan?pane_id=nope", &Value::Null);
        assert_eq!(status, 200);
        assert_eq!(body["plan"], Value::Null);
        assert!(body["server_time"].is_string());
    }

    #[test]
    fn devices_pair_start_returns_code() {
        let engine = engine();
        let (status, body) = engine.http("POST", "/api/devices/pair/start", &Value::Null);
        assert_eq!(status, 200);
        assert!(body["code"].is_string());
        assert!(body["expires_at"].is_string());
    }

    #[test]
    fn machines_setup_start_and_status() {
        let engine = engine();
        let (status, body) = engine.http("POST", "/api/machines/setup", &json!({}));
        assert_eq!(status, 200);
        assert!(body["job_id"].is_string());

        let (status, body) = engine.http("GET", "/api/machines/setup?id=local-setup", &Value::Null);
        assert_eq!(status, 200);
        assert_eq!(body["job_id"], "local-setup");
    }

    #[test]
    fn machine_patch_and_delete_device() {
        let engine = engine();
        let (status, _body) = engine.http("POST", "/api/devices/pair/start", &Value::Null);
        assert_eq!(status, 200);

        let (status, _) = engine.http(
            "PATCH",
            "/api/machines",
            &json!({ "id": "nonexistent", "label": "Phone" }),
        );
        assert_eq!(status, 404);

        let (status, _) = engine.http("DELETE", "/api/machines?id=nonexistent", &Value::Null);
        assert_eq!(status, 204);

        let (status, _) = engine.http("DELETE", "/api/devices/:id?id=nonexistent", &Value::Null);
        assert_eq!(status, 204);
    }
}
