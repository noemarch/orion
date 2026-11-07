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
pub mod devices;
pub mod diff;
pub mod introspect;
pub mod pane;
pub mod push;
pub mod store;
pub mod tailscale;
pub mod watcher;
pub mod worktree;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};
use serde_json::{json, Value};
use tracing::{info, warn};
use uuid::Uuid;

use crate::agents::{find_in_path, AgentRegistry, LaunchSpec};
use crate::error::Result;
use crate::prompt;
use crate::protocol::{AgentKind, AgentStatus, TerminalSize};

use self::introspect::claude::ClaudeTranscript;
use self::introspect::codex::CodexTranscript;
use self::introspect::opencode::OpencodeTranscript;
use self::pane::{FrameSink, Pane, PaneConfig, PaneEvent, Role};
use self::store::{
    spec_for_saved_pane, SavedPane, SavedSession, SavedTab, SavedWorkspace, Store, DEFAULT_SIZE,
};

/// The wire protocol revision the engine answers with. Clients compare it with the revision they
/// were built against; a mismatch is a compatibility error, never a silent downgrade.
pub const PROTOCOL_VERSION: u32 = 1;

/// What this engine advertises in its snapshot. A client only uses a verb it sees here, so a
/// feature is listed the day it works, never before.
const FEATURES: [&str; 3] = ["ping", "submit", "pending-input"];

/// The frame sink a connection sends to. Cheap to clone and safe to call from any thread.
type Sink = FrameSink;

/// One client connection: where its frames go, what it may do, and what it is attached to.
struct Connection {
    sink: Sink,
    role: Role,
    attached: HashSet<String>,
}

struct Workspace {
    id: String,
    label: String,
    cwd: PathBuf,
    number: u32,
    tabs: Vec<Tab>,
    active_tab: String,
}

struct Tab {
    id: String,
    label: String,
    number: u32,
    panes: Vec<String>,
    active_pane: String,
}

#[derive(Default)]
struct Session {
    workspaces: Vec<Workspace>,
    panes: HashMap<String, Arc<Pane>>,
    focused_pane: Option<String>,
    next_number: u32,
}

impl Session {
    fn pane(&self, id: &str) -> Option<&Arc<Pane>> {
        self.panes.get(id)
    }

    fn focused(&self) -> Option<String> {
        match &self.focused_pane {
            Some(id) if self.panes.contains_key(id) => Some(id.clone()),
            _ => self.panes.keys().next().cloned(),
        }
    }

    fn workspace_of(&self, id: &str) -> Option<&Workspace> {
        self.workspaces.iter().find(|workspace| workspace.id == id)
    }
}

/// Everything the engine owns. Held behind one `Arc` so a pane's event callback can reach it
/// without keeping the engine itself alive.
pub(crate) struct Inner {
    store: Store,
    session: RwLock<Session>,
    connections: Mutex<HashMap<String, Connection>>,
    next_connection: AtomicU64,
    presence: Mutex<HashMap<String, Instant>>,
    transcripts: Mutex<HashMap<String, ClaudeTranscript>>,
    codex_transcripts: Mutex<HashMap<String, CodexTranscript>>,
    opencode_transcripts: Mutex<HashMap<String, OpencodeTranscript>>,
    devices: devices::DeviceStore,
    push: push::PushService,
    watchers: Mutex<HashMap<String, watcher::Watcher>>,
    preferences: Mutex<Value>,
}

/// The process-wide engine. Cheap to clone: state lives behind the handles it owns.
#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
    version: String,
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
        let preferences = store.load_preferences();
        let devices = devices::DeviceStore::new(&store);
        let push = push::PushService::new(&store);
        Self {
            inner: Arc::new(Inner {
                store,
                session: RwLock::new(Session::default()),
                connections: Mutex::new(HashMap::new()),
                next_connection: AtomicU64::new(0),
                presence: Mutex::new(HashMap::new()),
                transcripts: Mutex::new(HashMap::new()),
                codex_transcripts: Mutex::new(HashMap::new()),
                opencode_transcripts: Mutex::new(HashMap::new()),
                devices,
                push,
                watchers: Mutex::new(HashMap::new()),
                preferences: Mutex::new(preferences),
            }),
            version: env!("CARGO_PKG_VERSION").to_string(),
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
    pub fn connect(&self, sink: Sink) -> String {
        let id = format!(
            "conn-{}",
            self.inner.next_connection.fetch_add(1, Ordering::Relaxed) + 1
        );
        self.inner.connections.lock().insert(
            id.clone(),
            Connection {
                sink,
                role: Role::Interact,
                attached: HashSet::new(),
            },
        );
        id
    }

    /// Sends one frame to an open connection, if it is still open.
    pub fn send_to(&self, connection: &str, frame: Value) {
        if let Some(entry) = self.inner.connections.lock().get(connection) {
            (entry.sink)(frame);
        }
    }

    /// Forgets a connection and every attachment it held.
    pub fn disconnect(&self, connection: &str) {
        let Some(entry) = self.inner.connections.lock().remove(connection) else {
            return;
        };
        let session = self.inner.session.read();
        for pane_id in entry.attached {
            if let Some(pane) = session.pane(&pane_id) {
                pane.detach(connection);
            }
        }
    }

    /// Whether a connection's page is active right now (within the last 30 seconds). A present
    /// connection already sees the UI live, so it does not need push duplicates of the same event.
    pub fn is_present(&self, connection_id: &str) -> bool {
        self.inner.is_present(connection_id)
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
        let session = self.inner.session.read();
        let focused = session.focused();
        let mut workspaces = Vec::with_capacity(session.workspaces.len());
        let mut tabs = Vec::new();
        for workspace in &session.workspaces {
            let pane_ids = workspace
                .tabs
                .iter()
                .flat_map(|tab| tab.panes.iter().cloned())
                .collect::<Vec<_>>();
            workspaces.push(json!({
                "workspace_id": workspace.id,
                "label": workspace.label,
                "number": workspace.number,
                "focused": pane_ids.iter().any(|id| Some(id) == focused.as_ref()),
                "pane_count": pane_ids.len(),
                "tab_count": workspace.tabs.len(),
                "active_tab_id": workspace.active_tab,
                "agent_status": aggregate_status(&session, &pane_ids).as_wire(),
            }));
            for tab in &workspace.tabs {
                tabs.push(json!({
                    "tab_id": tab.id,
                    "workspace_id": workspace.id,
                    "label": tab.label,
                    "number": tab.number,
                    "focused": tab.panes.iter().any(|id| Some(id) == focused.as_ref()),
                    "pane_count": tab.panes.len(),
                    "agent_status": aggregate_status(&session, &tab.panes).as_wire(),
                }));
            }
        }
        let panes = session
            .panes
            .iter()
            .map(|(id, pane)| pane.info(Some(id) == focused.as_ref()))
            .collect::<Vec<_>>();
        let agents = session
            .panes
            .iter()
            .map(|(id, pane)| pane.agent_info(Some(id) == focused.as_ref()))
            .collect::<Vec<_>>();

        json!({
            "agents": agents,
            "layouts": [],
            "panes": panes,
            "tabs": tabs,
            "workspaces": workspaces,
            "focused_pane_id": focused,
            "protocol": PROTOCOL_VERSION,
            "version": self.version,
        })
    }

    /// Answers a client's liveness probe. Only the echoed id completes the probe, so an id is
    /// required: a pong without one would answer nothing.
    pub fn pong(&self, id: &Value) -> Value {
        json!({ "type": "pong", "id": id })
    }

    /// Dispatches one request without a push connection. Used by bounded commands and tests.
    pub fn dispatch(&self, request: &Value) -> Result<Value, String> {
        self.dispatch_from(None, request)
            .map(|answer| answer.unwrap_or_else(|| json!({ "type": "ok" })))
    }

    /// Dispatches one client frame. `None` means the answer already went to the connection's
    /// sink (attach's replay, a broadcast), and there is nothing for the transport to send.
    pub fn dispatch_from(
        &self,
        connection: Option<&str>,
        request: &Value,
    ) -> Result<Option<Value>, String> {
        let kind = request
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| "request has no type".to_string())?;

        match kind {
            "health" => Ok(Some(self.identity())),
            "snapshot" => Ok(Some(self.snapshot_frame())),
            "ping" => {
                let id = request
                    .get("id")
                    .ok_or_else(|| "ping has no id".to_string())?;
                Ok(Some(self.pong(id)))
            }
            "role" => {
                let mode = request
                    .get("mode")
                    .and_then(Value::as_str)
                    .unwrap_or("interact");
                if let Some(connection) = connection {
                    if let Some(entry) = self.inner.connections.lock().get_mut(connection) {
                        entry.role = Role::parse(Some(mode));
                    }
                }
                Ok(Some(json!({ "type": "role-ack", "mode": mode })))
            }
            "presence" => {
                if let Some(connection) = connection {
                    let active = request
                        .get("active")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let mut presence = self.inner.presence.lock();
                    if active {
                        presence.insert(connection.to_string(), Instant::now());
                    } else {
                        presence.remove(connection);
                    }
                }
                Ok(None)
            }
            "attach" => {
                self.attach(connection, request)?;
                Ok(None)
            }
            "detach" => {
                self.detach(connection, request);
                Ok(None)
            }
            "input" => {
                self.write(connection, request, "text")?;
                Ok(None)
            }
            "keys" => {
                let pane_id = pane_id_of(request)?;
                let keys = request
                    .get("keys")
                    .and_then(Value::as_array)
                    .ok_or_else(|| "keys has no list".to_string())?
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                let pane = self.pane(&pane_id).map_err(|(_, body)| body.to_string())?;
                self.require_interact(connection)?;
                pane.keys(&keys).map_err(|error| error.to_string())?;
                Ok(None)
            }
            "resize" => {
                if self.role(connection)? == Role::Observe {
                    return Ok(None);
                }
                let pane_id = pane_id_of(request)?;
                let cols = request.get("cols").and_then(Value::as_u64).unwrap_or(0) as u16;
                let rows = request.get("rows").and_then(Value::as_u64).unwrap_or(0) as u16;
                if let Ok(pane) = self.pane(&pane_id).map_err(|(_, body)| body.to_string()) {
                    let _ = pane.resize(cols, rows);
                }
                Ok(None)
            }
            "release-size" | "pty-ack" => Ok(None),
            "watch" | "unwatch" => Ok(Some(json!({
                "type": "watch-end",
                "pane_id": request.get("pane_id").cloned().unwrap_or(Value::Null),
            }))),
            "conversation-watch" => Ok(None),
            "take-over" | "pending-action" | "secret" => Ok(Some(json!({
                "type": "error",
                "code": "unsupported",
                "message": "this build does not support that verb yet",
            }))),
            "submit" => self.submit(connection, request).map(Some),
            // The desktop's fetch shim: one REST call, answered with a status and a JSON body.
            "http" => {
                let method = request
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or("GET");
                let path = request
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "http has no path".to_string())?;
                let body = request.get("body").cloned().unwrap_or(Value::Null);
                let (status, body) = self.http(method, path, &body);
                Ok(Some(json!({ "status": status, "body": body })))
            }
            // A round-trip probe used by the transport test: the payload comes back untouched,
            // byte for byte, so a lossy decode anywhere in the pipe is a failing test.
            "echo" => Ok(Some(json!({
                "type": "echo",
                "payload": request.get("payload").cloned().unwrap_or(Value::Null),
            }))),
            other => Err(format!("unknown request type: {other}")),
        }
    }

    fn role(&self, connection: Option<&str>) -> Result<Role, String> {
        let Some(connection) = connection else {
            return Ok(Role::Interact);
        };
        Ok(self
            .inner
            .connections
            .lock()
            .get(connection)
            .map(|entry| entry.role)
            .unwrap_or(Role::Interact))
    }

    fn require_interact(&self, connection: Option<&str>) -> Result<(), String> {
        match self.role(connection)? {
            Role::Interact => Ok(()),
            Role::Observe => Err("read_only".to_string()),
        }
    }

    fn attach(&self, connection: Option<&str>, request: &Value) -> Result<(), String> {
        let pane_id = pane_id_of(request)?;
        let cols = request.get("cols").and_then(Value::as_u64).unwrap_or(0) as u16;
        let rows = request.get("rows").and_then(Value::as_u64).unwrap_or(0) as u16;
        let keep_size = request
            .get("keep_size")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let (role, sink) = match connection {
            Some(connection) => {
                let mut connections = self.inner.connections.lock();
                let entry = connections
                    .get_mut(connection)
                    .ok_or_else(|| format!("no such connection: {connection}"))?;
                entry.attached.insert(pane_id.clone());
                (entry.role, Arc::clone(&entry.sink))
            }
            None => (Role::Interact, Arc::new(|_| {}) as Sink),
        };
        let pane = self.pane(&pane_id).map_err(|(_, body)| body.to_string())?;
        pane.attach(
            connection.unwrap_or("anonymous"),
            sink,
            role,
            cols,
            rows,
            keep_size,
        )
        .map_err(|error| error.to_string())
    }

    fn detach(&self, connection: Option<&str>, request: &Value) {
        let Some(pane_id) = request.get("pane_id").and_then(Value::as_str) else {
            return;
        };
        if let Some(connection) = connection {
            if let Some(entry) = self.inner.connections.lock().get_mut(connection) {
                entry.attached.remove(pane_id);
            }
            if let Ok(pane) = self.pane(pane_id).map_err(|(_, body)| body.to_string()) {
                pane.detach(connection);
            }
        }
    }

    fn write(&self, connection: Option<&str>, request: &Value, field: &str) -> Result<(), String> {
        self.require_interact(connection)?;
        let pane_id = pane_id_of(request)?;
        let text = request
            .get(field)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{field} is missing"))?;
        let pane = self.pane(&pane_id).map_err(|(_, body)| body.to_string())?;
        pane.write(text).map_err(|error| error.to_string())
    }

    fn submit(&self, connection: Option<&str>, request: &Value) -> Result<Value, String> {
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let pane_id = request
            .get("pane_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let refuse = |code: &str, message: &str| {
            json!({
                "type": "submit-result",
                "id": id,
                "pane_id": pane_id,
                "ok": false,
                "code": code,
                "message": message,
            })
        };
        if let Err(code) = self.require_interact(connection) {
            return Ok(refuse(&code, "This connection only watches."));
        }
        let pane = match self.pane(&pane_id) {
            Ok(pane) => pane,
            Err((_, body)) => return Ok(body),
        };
        let payload = request
            .get("payload")
            .or_else(|| request.get("text"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if payload.trim().is_empty() {
            return Ok(refuse("invalid_submit_text", "The message is empty."));
        }
        let mut typed = payload.to_string();
        typed.push('\r');
        pane.write(&typed).map_err(|error| error.to_string())?;
        pane.note_submit();
        self.broadcast(json!({
            "type": "pane-status",
            "pane_id": pane_id,
            "agent_status": AgentStatus::Working.as_wire(),
        }));
        Ok(json!({
            "type": "submit-result",
            "id": id,
            "pane_id": pane_id,
            "ok": true,
        }))
    }

    fn pane(&self, pane_id: &str) -> std::result::Result<Arc<Pane>, (u16, Value)> {
        self.inner
            .session
            .read()
            .pane(pane_id)
            .cloned()
            .ok_or_else(|| api_error(404, "pane_not_found", "That pane is not open."))
    }

    fn broadcast(&self, frame: Value) {
        let kind = frame.get("type").and_then(Value::as_str).unwrap_or("");
        let suppress = kind == "pane-status" || kind == "session-changed";
        let connections = self.inner.connections.lock();
        for (connection_id, connection) in connections.iter() {
            // A present connection already sees the UI live: do not push it a duplicate of an
            // event it is already looking at. Absent connections still get the frame.
            if suppress && self.inner.is_present(connection_id) {
                continue;
            }
            (connection.sink)(frame.clone());
        }
    }

    // ---- REST ---------------------------------------------------------------------------

    /// Runs one REST call. Both transports use it: the desktop shell's `http` envelope and,
    /// from M2, the Axum gateway.
    pub fn http(&self, method: &str, path: &str, body: &Value) -> (u16, Value) {
        let (route, query) = split_path(path);
        let method = method.to_ascii_uppercase();
        let result = match (method.as_str(), route.as_str()) {
            ("GET", "/api/health") => match query.get("scope").map(String::as_str) {
                Some("bridge") => Ok((
                    200,
                    json!({
                        "ok": true,
                        "auth": {
                            "required": false,
                            "authenticated": true,
                            "via": "local",
                            "role": "drive",
                        },
                        "bridge_protocol": PROTOCOL_VERSION,
                    }),
                )),
                _ => Ok((
                    200,
                    json!({
                        "ok": true,
                        "orion": {
                            "version": self.version,
                            "protocol": PROTOCOL_VERSION,
                            "terminal_attach": true,
                        },
                        "auth": {
                            "required": false,
                            "authenticated": true,
                            "via": "local",
                            "role": "drive",
                        },
                    }),
                )),
            },
            ("GET", "/api/machines") => Ok((200, self.machines())),
            ("GET", "/api/session") => Ok((200, json!({ "snapshot": self.snapshot_value() }))),
            ("GET", "/api/agents") => Ok((200, self.agents())),
            ("GET", "/api/integrations") => Ok((200, self.integrations())),
            ("GET", "/api/preferences") => self.get_preferences(),
            ("PUT", "/api/preferences") => self.put_preferences(body),
            ("POST", "/api/workspace/create") => self.create_workspace(body),
            ("POST", "/api/workspace/rename") => self.rename_workspace(body),
            ("POST", "/api/workspace/move") => self.move_workspace(body),
            ("POST", "/api/workspace/close") => self.close_workspace(body),
            ("GET", "/api/worktree/list") => self.worktree_list(&query),
            ("POST", "/api/worktree/create") => self.worktree_create(body, true),
            ("POST", "/api/worktree/open") => self.worktree_create(body, false),
            ("POST", "/api/worktree/remove") => self.worktree_remove(body),
            ("POST", "/api/tab/create") => self.create_tab(body),
            ("POST", "/api/tab/rename") => self.rename_tab(body),
            ("POST", "/api/tab/close") => self.close_tab(body),
            ("POST", "/api/tab/move") => self.move_tab(body),
            ("POST", "/api/pane/close") => self.close_pane(body),
            ("POST", "/api/pane/focus") => self.focus_pane(body),
            ("POST", "/api/pane/rename") => self.rename_pane(body),
            ("POST", "/api/pane/clear") => self.clear_pane(body),
            ("GET", "/api/pane/read") => self.read_pane(&query),
            ("GET", "/api/pane/conversation") => self.conversation(&query),
            ("GET", "/api/pane/prompt") => self.prompt(&query),
            ("POST", "/api/pane/prompt/answer") => self.answer_prompt(body),
            ("GET", "/api/pane/scroll") => self.scroll(&query),
            ("POST", "/api/pane/scroll") => self.scroll_to(body),
            ("POST", "/api/pane/find") => self.find(body),
            ("GET", "/api/pane/selection") => self.selection(&query),
            ("GET", "/api/workspace/directories") => self.directories(&query),
            ("GET", "/api/workspace/diff/files") => self.diff_files(&query),
            ("GET", "/api/workspace/diff/file") => self.diff_file(&query),
            ("GET", "/api/fs/stat") => self.fs_stat(&query),
            ("GET", "/api/fs/file") => self.fs_file(&query),
            ("GET", "/api/fs/favicon") => self.fs_favicon(&query),
            ("POST", "/api/devices/pairing-code") => self.pairing_code(body),
            ("POST", "/api/devices/pair") => self.pair_device(body),
            ("GET", "/api/devices") => Ok((200, self.inner.devices.list())),
            ("GET", "/api/push") => {
                Ok((200, json!({ "public_key": self.inner.push.public_key() })))
            }
            ("POST", "/api/push/subscribe") => {
                self.inner.push.subscribe(
                    body.get("subscription").cloned().unwrap_or(Value::Null),
                    body.get("alerts").cloned(),
                );
                Ok((204, Value::Null))
            }
            ("DELETE", "/api/push/subscribe") => {
                if let Some(endpoint) = body.get("endpoint").and_then(Value::as_str) {
                    self.inner.push.unsubscribe(endpoint);
                }
                Ok((204, Value::Null))
            }
            ("GET", "/api/access") => Ok((200, self.access())),
            (_, path) if path.starts_with("/api/") => Err(api_error(
                404,
                "not_found",
                "This build does not serve that route yet.",
            )),
            _ => Err(api_error(404, "not_found", "No such route.")),
        };
        result.unwrap_or_else(|error| error)
    }

    fn access(&self) -> Value {
        let output = tailscale::read_tailscale();
        let tailscale = match &output {
            Some(output) => tailscale::parse_tailscale(output, 7317),
            None => tailscale::parse_tailscale(
                &tailscale::TailscaleOutput {
                    status: None,
                    serve: None,
                },
                7317,
            ),
        };
        json!({
            "tailscale_active": tailscale["state"] == "running",
            "tailscale_ip": output
                .as_ref()
                .and_then(|output| output.status.as_deref())
                .and_then(tailscale::parse_tailscale_ip),
            "tailnet_url": tailscale["serving_url"].as_str().map(str::to_string),
            "port": 7317,
            "tailscale": tailscale,
            "qr_code_svg": null,
            "pairing_code": null,
        })
    }

    fn machines(&self) -> Value {
        json!({
            "machines": [{
                "id": "local",
                "name": "This PC",
                "kind": "local",
                "enabled": true,
                "state": "connected",
                "error": null,
                "snapshot": self.snapshot_value(),
                "orion": {
                    "version": self.version,
                    "protocol": PROTOCOL_VERSION,
                    "terminal_attach": true,
                },
            }],
        })
    }

    fn agents(&self) -> Value {
        let agents = AgentRegistry::discover_all()
            .into_iter()
            .map(|agent| {
                json!({
                    "kind": agent.kind.wire_kind(),
                    "label": agent.name,
                    "installed": agent.installed,
                })
            })
            .collect::<Vec<_>>();
        json!({ "agents": agents })
    }

    fn integrations(&self) -> Value {
        let integrations = AgentRegistry::discover_all()
            .into_iter()
            .map(|agent| {
                json!({
                    "target": agent.kind.wire_kind(),
                    "label": agent.name,
                    "command": agent.command,
                    "available": agent.installed,
                    "state": if agent.installed { "current" } else { "not_installed" },
                })
            })
            .collect::<Vec<_>>();
        json!({ "integrations": integrations })
    }

    fn get_preferences(&self) -> std::result::Result<(u16, Value), (u16, Value)> {
        let preferences = self.inner.preferences.lock();
        if preferences.is_null() {
            return Err(api_error(
                404,
                "not_found",
                "No preference has been written.",
            ));
        }
        Ok((200, preferences.clone()))
    }

    fn put_preferences(&self, body: &Value) -> std::result::Result<(u16, Value), (u16, Value)> {
        let mut preferences = self.inner.preferences.lock();
        let revision = preferences
            .get("revision")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            + 1;
        let mut settings = preferences
            .get("settings")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if let Some(change) = body.get("settings").and_then(Value::as_object) {
            for (key, value) in change {
                settings.insert(key.clone(), value.clone());
            }
        }
        let mut next = json!({ "revision": revision, "settings": settings });
        if let Some(pins) = body.get("pins") {
            next["pins"] = pins.clone();
        }
        if let Err(error) = self.inner.store.save_preferences(&next) {
            warn!("preferences not saved: {error}");
        }
        *preferences = next.clone();
        Ok((200, next))
    }

    fn create_workspace(&self, body: &Value) -> std::result::Result<(u16, Value), (u16, Value)> {
        let cwd = resolve_cwd(body.get("cwd").and_then(Value::as_str))?;
        let label = body
            .get("label")
            .and_then(Value::as_str)
            .filter(|label| !label.trim().is_empty())
            .map(str::to_string)
            .or_else(|| folder_name(&cwd));
        let agent = agent_request(body)?;
        let workspace_id = Uuid::new_v4().to_string();
        let tab_id = Uuid::new_v4().to_string();
        let pane_id = Uuid::new_v4().to_string();
        let label = label.unwrap_or_else(|| "Workspace".to_string());
        let pane_label = agent.name.clone().unwrap_or_else(|| label.clone());
        let pane = self
            .spawn_pane(PaneRequest {
                id: pane_id.clone(),
                workspace_id: workspace_id.clone(),
                tab_id: tab_id.clone(),
                agent: agent.kind,
                label: pane_label,
                cwd: cwd.clone(),
                args: agent.args,
                session: None,
                resume_last: false,
            })
            .map_err(|error| {
                api_error(
                    400,
                    "agent_failed",
                    &format!("The agent could not start: {error}"),
                )
            })?;
        {
            let mut session = self.inner.session.write();
            session.next_number += 1;
            let number = session.next_number;
            session.workspaces.push(Workspace {
                id: workspace_id.clone(),
                label: label.clone(),
                cwd: cwd.clone(),
                number,
                tabs: vec![Tab {
                    id: tab_id.clone(),
                    label: label.clone(),
                    number: 1,
                    panes: vec![pane_id.clone()],
                    active_pane: pane_id.clone(),
                }],
                active_tab: tab_id,
            });
            session.panes.insert(pane_id.clone(), pane);
            session.focused_pane = Some(pane_id.clone());
        }
        self.inner.watchers.lock().insert(
            workspace_id.clone(),
            watcher::Watcher::start(cwd, Arc::clone(&self.inner)),
        );
        self.save();
        self.broadcast(json!({ "type": "session-changed" }));
        info!("workspace {workspace_id} opened with agent {pane_id}");
        Ok((
            200,
            json!({
                "workspace_id": workspace_id,
                "pane_id": pane_id,
                "agent_started": true,
            }),
        ))
    }

    fn worktree_list(
        &self,
        query: &HashMap<String, String>,
    ) -> std::result::Result<(u16, Value), (u16, Value)> {
        let workspace_id = required(query, "workspace_id")?;
        let cwd = {
            let session = self.inner.session.read();
            let workspace = session.workspace_of(workspace_id).ok_or_else(|| {
                api_error(404, "workspace_not_found", "That workspace is not open.")
            })?;
            workspace.cwd.clone()
        };
        Ok((200, json!({ "worktrees": worktree::list(&cwd) })))
    }

    fn worktree_create(
        &self,
        body: &Value,
        creating: bool,
    ) -> std::result::Result<(u16, Value), (u16, Value)> {
        let workspace_id = body
            .get("workspace_id")
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "workspace_id is required."))?;
        let source_cwd = {
            let session = self.inner.session.read();
            let workspace = session.workspace_of(workspace_id).ok_or_else(|| {
                api_error(404, "workspace_not_found", "That workspace is not open.")
            })?;
            workspace.cwd.clone()
        };
        let branch = body
            .get("branch")
            .and_then(Value::as_str)
            .filter(|branch| !branch.trim().is_empty())
            .map(str::to_string);
        let base = body
            .get("base")
            .and_then(Value::as_str)
            .filter(|base| !base.trim().is_empty())
            .map(str::to_string);
        let label = body
            .get("label")
            .and_then(Value::as_str)
            .filter(|label| !label.trim().is_empty())
            .map(str::to_string);
        let path = match body.get("path").and_then(Value::as_str) {
            Some(path) if !path.trim().is_empty() => {
                let path = PathBuf::from(path);
                if !path.is_absolute() {
                    return Err(api_error(400, "invalid_path", "path must be absolute."));
                }
                path
            }
            _ => match &branch {
                Some(branch) => worktree::default_worktree_path(&source_cwd, branch),
                None => {
                    return Err(api_error(
                        400,
                        "invalid_request",
                        "branch or path is required.",
                    ))
                }
            },
        };
        let branch = branch.unwrap_or_else(|| {
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("worktree")
                .to_string()
        });
        if creating {
            worktree::create(&source_cwd, &path, &branch, base.as_deref()).map_err(|error| {
                api_error(
                    400,
                    "worktree_create_failed",
                    &format!("The worktree could not be created: {error}"),
                )
            })?;
        }
        let agent = body.get("agent").cloned();
        let request = match agent {
            Some(agent) if !agent.is_null() => json!({
                "cwd": path.to_string_lossy(),
                "label": label.unwrap_or_else(|| branch.clone()),
                "agent": agent,
            }),
            _ => {
                // No agent named: the checkout still opens as a workspace with its branch agent.
                json!({
                    "cwd": path.to_string_lossy(),
                    "label": label.unwrap_or_else(|| branch.clone()),
                    "agent": { "kind": "claude" },
                })
            }
        };
        let (status, created) = self.create_workspace(&request)?;
        let _ = status;
        Ok((
            200,
            json!({
                "workspace_id": created["workspace_id"],
                "pane_id": created["pane_id"],
                "path": path.to_string_lossy(),
                "branch": branch,
                "already_open": false,
            }),
        ))
    }

    fn worktree_remove(&self, body: &Value) -> std::result::Result<(u16, Value), (u16, Value)> {
        let workspace_id = body
            .get("workspace_id")
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "workspace_id is required."))?;
        let force = body.get("force").and_then(Value::as_bool).unwrap_or(false);
        let cwd = {
            let session = self.inner.session.read();
            let workspace = session.workspace_of(workspace_id).ok_or_else(|| {
                api_error(404, "workspace_not_found", "That workspace is not open.")
            })?;
            workspace.cwd.clone()
        };
        // Closing the workspace kills its panes first; removing a checkout with live agents
        // in it would strand them.
        self.close_workspace(&json!({ "workspace_id": workspace_id }))?;
        worktree::remove(&cwd, &cwd, force).map_err(|error| {
            api_error(
                400,
                "worktree_remove_failed",
                &format!("The worktree could not be removed: {error}"),
            )
        })?;
        Ok((
            200,
            json!({ "ok": true, "path": cwd.to_string_lossy(), "forced": force }),
        ))
    }

    fn diff_files(
        &self,
        query: &HashMap<String, String>,
    ) -> std::result::Result<(u16, Value), (u16, Value)> {
        let cwd = self.diff_cwd(query)?;
        let scope = query
            .get("scope")
            .map(String::as_str)
            .unwrap_or("workspace");
        let base = query.get("base").map(String::as_str);
        let head = query.get("head").map(String::as_str);
        Ok((200, diff::files(&cwd, scope, base, head, None)))
    }

    fn diff_file(
        &self,
        query: &HashMap<String, String>,
    ) -> std::result::Result<(u16, Value), (u16, Value)> {
        let cwd = self.diff_cwd(query)?;
        let scope = query
            .get("scope")
            .map(String::as_str)
            .unwrap_or("workspace");
        let base = query.get("base").map(String::as_str);
        let head = query.get("head").map(String::as_str);
        let path = query
            .get("path")
            .map(String::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "path is required."))?;
        let old_path = query.get("old_path").map(String::as_str);
        Ok((
            200,
            diff::file_patch(&cwd, scope, base, head, path, old_path),
        ))
    }

    fn diff_cwd(
        &self,
        query: &HashMap<String, String>,
    ) -> std::result::Result<PathBuf, (u16, Value)> {
        if let Some(pane_id) = query.get("pane_id") {
            let pane = self.pane(pane_id)?;
            return Ok(pane.cwd.clone());
        }
        let directory = query.get("directory").map(String::as_str).ok_or_else(|| {
            api_error(400, "invalid_request", "pane_id or directory is required.")
        })?;
        resolve_cwd(Some(directory))
    }

    fn fs_path(
        &self,
        query: &HashMap<String, String>,
    ) -> std::result::Result<PathBuf, (u16, Value)> {
        let path = query
            .get("path")
            .map(String::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "path is required."))?;
        let base = match query.get("pane_id") {
            Some(pane_id) => match self.pane(pane_id) {
                Ok(pane) => pane.cwd.clone(),
                Err(_) => PathBuf::from("."),
            },
            None => PathBuf::from("."),
        };
        let expanded = if path == "~" {
            home_dir().unwrap_or(base)
        } else if let Some(rest) = path.strip_prefix("~/") {
            home_dir()
                .map(|home| home.join(rest))
                .unwrap_or(base.join(rest))
        } else {
            let path = PathBuf::from(path);
            if path.is_absolute() {
                path
            } else {
                base.join(path)
            }
        };
        Ok(expanded)
    }

    fn fs_stat(
        &self,
        query: &HashMap<String, String>,
    ) -> std::result::Result<(u16, Value), (u16, Value)> {
        let path = self.fs_path(query)?;
        if !path.exists() {
            return Err(api_error(404, "not_found", "no readable file at that path"));
        }
        let metadata = std::fs::metadata(&path)
            .map_err(|_| api_error(404, "not_found", "no readable file at that path"))?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        Ok((
            200,
            json!({
                "path": path.to_string_lossy(),
                "name": name,
                "size": metadata.len(),
                "modified": metadata
                    .modified()
                    .ok()
                    .and_then(|when| chrono::DateTime::<chrono::Utc>::from(when).into())
                    .map(|when| when.to_rfc3339())
                    .unwrap_or_default(),
                "mime": "text/plain",
                "kind": if metadata.is_dir() { "directory" } else { "file" },
            }),
        ))
    }

    fn fs_file(
        &self,
        query: &HashMap<String, String>,
    ) -> std::result::Result<(u16, Value), (u16, Value)> {
        let path = self.fs_path(query)?;
        if !path.is_file() {
            return Err(api_error(404, "not_found", "no readable file at that path"));
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|_| api_error(400, "unsupported", "not a text file"))?;
        Ok((200, json!({ "__raw_text__": text })))
    }

    fn fs_favicon(
        &self,
        query: &HashMap<String, String>,
    ) -> std::result::Result<(u16, Value), (u16, Value)> {
        let root = query
            .get("root")
            .map(String::as_str)
            .filter(|root| !root.trim().is_empty())
            .ok_or_else(|| api_error(400, "invalid_request", "root is required."))?;
        let root = PathBuf::from(root);
        for candidate in ["favicon.ico", "icon.png", "favicon.png"] {
            let path = root.join(candidate);
            if path.is_file() {
                return Ok((
                    200,
                    json!({
                        "icon": {
                            "path": path.to_string_lossy(),
                            "name": candidate,
                            "size": std::fs::metadata(&path).map(|metadata| metadata.len()).unwrap_or(0),
                            "modified": String::new(),
                            "mime": "image/png",
                            "kind": "file",
                        }
                    }),
                ));
            }
        }
        Ok((200, json!({ "icon": null })))
    }

    /// Whether `token` belongs to a paired device (checked by the gateway's auth gate).
    pub fn device_authenticated(&self, token: &str) -> bool {
        self.inner.devices.matches(token)
    }

    fn pairing_code(&self, _body: &Value) -> std::result::Result<(u16, Value), (u16, Value)> {
        Ok((200, self.inner.devices.new_pairing_code()))
    }

    fn pair_device(&self, body: &Value) -> std::result::Result<(u16, Value), (u16, Value)> {
        let code = body
            .get("code")
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "code is required."))?;
        let label = body.get("label").and_then(Value::as_str).unwrap_or("Phone");
        let role = body.get("role").and_then(Value::as_str).unwrap_or("drive");
        self.inner
            .devices
            .pair(code, label, role)
            .map(|paired| (200, paired))
            .ok_or_else(|| {
                api_error(
                    401,
                    "pairing_required",
                    "that code is wrong or has expired; start a new pairing",
                )
            })
    }

    fn rename_workspace(&self, body: &Value) -> std::result::Result<(u16, Value), (u16, Value)> {
        let workspace_id = body
            .get("workspace_id")
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "workspace_id is required."))?;
        let label = body
            .get("label")
            .and_then(Value::as_str)
            .filter(|label| !label.trim().is_empty())
            .ok_or_else(|| api_error(400, "invalid_request", "label is required."))?;
        {
            let mut session = self.inner.session.write();
            let workspace = session
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.id == *workspace_id)
                .ok_or_else(|| {
                    api_error(404, "workspace_not_found", "That workspace is not open.")
                })?;
            workspace.label = label.to_string();
        }
        self.save();
        self.broadcast(json!({ "type": "session-changed" }));
        Ok((200, json!({ "ok": true })))
    }

    fn move_workspace(&self, body: &Value) -> std::result::Result<(u16, Value), (u16, Value)> {
        let workspace_id = body
            .get("workspace_id")
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "workspace_id is required."))?;
        let index = body
            .get("insert_index")
            .and_then(Value::as_u64)
            .ok_or_else(|| api_error(400, "invalid_request", "insert_index is required."))?;
        {
            let mut session = self.inner.session.write();
            let from = session
                .workspaces
                .iter()
                .position(|workspace| workspace.id == *workspace_id)
                .ok_or_else(|| {
                    api_error(404, "workspace_not_found", "That workspace is not open.")
                })?;
            let workspace = session.workspaces.remove(from);
            let at = (index as usize).min(session.workspaces.len());
            session.workspaces.insert(at, workspace);
        }
        self.save();
        self.broadcast(json!({ "type": "session-changed" }));
        Ok((200, json!({ "ok": true })))
    }

    fn close_workspace(&self, body: &Value) -> std::result::Result<(u16, Value), (u16, Value)> {
        let workspace_id = body
            .get("workspace_id")
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "workspace_id is required."))?;
        let mut gone = Vec::new();
        {
            let mut session = self.inner.session.write();
            let position = session
                .workspaces
                .iter()
                .position(|workspace| workspace.id == *workspace_id)
                .ok_or_else(|| {
                    api_error(404, "workspace_not_found", "That workspace is not open.")
                })?;
            let workspace = session.workspaces.remove(position);
            for tab in &workspace.tabs {
                for pane_id in &tab.panes {
                    if let Some(pane) = session.panes.remove(pane_id) {
                        drop(pane);
                        gone.push(pane_id.clone());
                    }
                }
            }
            if session
                .focused_pane
                .as_deref()
                .is_some_and(|focused| gone.contains(&focused.to_string()))
            {
                session.focused_pane = None;
            }
        }
        for pane_id in gone {
            self.inner.transcripts.lock().remove(&pane_id);
            self.inner.codex_transcripts.lock().remove(&pane_id);
        }
        if let Some(watcher) = self.inner.watchers.lock().remove(workspace_id) {
            watcher.stop();
        }
        self.save();
        self.broadcast(json!({ "type": "session-changed" }));
        Ok((200, json!({ "ok": true })))
    }

    fn rename_tab(&self, body: &Value) -> std::result::Result<(u16, Value), (u16, Value)> {
        let tab_id = body
            .get("tab_id")
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "tab_id is required."))?;
        let label = body
            .get("label")
            .and_then(Value::as_str)
            .filter(|label| !label.trim().is_empty())
            .ok_or_else(|| api_error(400, "invalid_request", "label is required."))?;
        {
            let mut session = self.inner.session.write();
            let tab = session
                .workspaces
                .iter_mut()
                .flat_map(|workspace| workspace.tabs.iter_mut())
                .find(|tab| tab.id == *tab_id)
                .ok_or_else(|| api_error(404, "tab_not_found", "That tab is not open."))?;
            tab.label = label.to_string();
        }
        self.save();
        self.broadcast(json!({ "type": "session-changed" }));
        Ok((200, json!({ "ok": true })))
    }

    fn close_tab(&self, body: &Value) -> std::result::Result<(u16, Value), (u16, Value)> {
        let tab_id = body
            .get("tab_id")
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "tab_id is required."))?;
        let mut gone = Vec::new();
        {
            let mut session = self.inner.session.write();
            let mut closed = false;
            let mut pane_ids = Vec::new();
            for workspace in &mut session.workspaces {
                if let Some(position) = workspace.tabs.iter().position(|tab| tab.id == *tab_id) {
                    let tab = workspace.tabs.remove(position);
                    closed = true;
                    pane_ids.extend(tab.panes.iter().cloned());
                    if workspace.active_tab == *tab_id {
                        if let Some(first) = workspace.tabs.first() {
                            workspace.active_tab = first.id.clone();
                        }
                    }
                    break;
                }
            }
            if !closed {
                return Err(api_error(404, "tab_not_found", "That tab is not open."));
            }
            for pane_id in pane_ids {
                if let Some(pane) = session.panes.remove(&pane_id) {
                    drop(pane);
                    gone.push(pane_id);
                }
            }
            session
                .workspaces
                .retain(|workspace| !workspace.tabs.is_empty());
            let live: std::collections::HashSet<String> = session
                .workspaces
                .iter()
                .map(|workspace| workspace.id.clone())
                .collect();
            let mut watchers = self.inner.watchers.lock();
            watchers.retain(|id, _| live.contains(id));
            if session
                .focused_pane
                .as_deref()
                .is_some_and(|focused| gone.contains(&focused.to_string()))
            {
                session.focused_pane = None;
            }
        }
        for pane_id in gone {
            self.inner.transcripts.lock().remove(&pane_id);
            self.inner.codex_transcripts.lock().remove(&pane_id);
        }
        self.save();
        self.broadcast(json!({ "type": "session-changed" }));
        Ok((200, json!({ "ok": true })))
    }

    fn move_tab(&self, body: &Value) -> std::result::Result<(u16, Value), (u16, Value)> {
        let tab_id = body
            .get("tab_id")
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "tab_id is required."))?;
        let index = body
            .get("insert_index")
            .and_then(Value::as_u64)
            .ok_or_else(|| api_error(400, "invalid_request", "insert_index is required."))?;
        {
            let mut session = self.inner.session.write();
            let workspace = session
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.tabs.iter().any(|tab| tab.id == *tab_id))
                .ok_or_else(|| api_error(404, "tab_not_found", "That tab is not open."))?;
            let from = workspace
                .tabs
                .iter()
                .position(|tab| tab.id == *tab_id)
                .expect("tab found above");
            let tab = workspace.tabs.remove(from);
            let at = (index as usize).min(workspace.tabs.len());
            workspace.tabs.insert(at, tab);
        }
        self.save();
        self.broadcast(json!({ "type": "session-changed" }));
        Ok((200, json!({ "ok": true })))
    }

    fn create_tab(&self, body: &Value) -> std::result::Result<(u16, Value), (u16, Value)> {
        let workspace_id = body
            .get("workspace_id")
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "workspace_id is required."))?
            .to_string();
        let (cwd, label) = {
            let session = self.inner.session.read();
            let workspace = session.workspace_of(&workspace_id).ok_or_else(|| {
                api_error(404, "workspace_not_found", "That workspace is not open.")
            })?;
            (workspace.cwd.clone(), workspace.label.clone())
        };
        let cwd = match body.get("cwd").and_then(Value::as_str) {
            Some(cwd) => resolve_cwd(Some(cwd))?,
            None => cwd,
        };
        let label = body
            .get("label")
            .and_then(Value::as_str)
            .filter(|label| !label.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| label.clone());
        let agent = agent_request(body)?;
        let tab_id = Uuid::new_v4().to_string();
        let pane_id = Uuid::new_v4().to_string();
        let pane_label = agent.name.clone().unwrap_or_else(|| label.clone());
        let pane = self
            .spawn_pane(PaneRequest {
                id: pane_id.clone(),
                workspace_id: workspace_id.clone(),
                tab_id: tab_id.clone(),
                agent: agent.kind,
                label: pane_label,
                cwd,
                args: agent.args,
                session: None,
                resume_last: false,
            })
            .map_err(|error| {
                api_error(
                    400,
                    "agent_failed",
                    &format!("The agent could not start: {error}"),
                )
            })?;
        {
            let mut session = self.inner.session.write();
            let workspace = session
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.id == workspace_id)
                .ok_or_else(|| {
                    api_error(404, "workspace_not_found", "That workspace is not open.")
                })?;
            let number = workspace.tabs.len() as u32 + 1;
            workspace.tabs.push(Tab {
                id: tab_id.clone(),
                label,
                number,
                panes: vec![pane_id.clone()],
                active_pane: pane_id.clone(),
            });
            workspace.active_tab = tab_id;
            session.panes.insert(pane_id.clone(), pane);
            session.focused_pane = Some(pane_id.clone());
        }
        self.save();
        self.broadcast(json!({ "type": "session-changed" }));
        Ok((
            200,
            json!({
                "workspace_id": workspace_id,
                "pane_id": pane_id,
                "agent_started": true,
            }),
        ))
    }

    fn close_pane(&self, body: &Value) -> std::result::Result<(u16, Value), (u16, Value)> {
        let pane_id = body
            .get("pane_id")
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "pane_id is required."))?;
        {
            let mut session = self.inner.session.write();
            let Some(pane) = session.panes.remove(pane_id) else {
                return Err(api_error(404, "pane_not_found", "That pane is not open."));
            };
            // Dropping the pane ends its job object, so the agent's whole tree goes with it.
            drop(pane);
            for workspace in &mut session.workspaces {
                for tab in &mut workspace.tabs {
                    tab.panes.retain(|id| id != pane_id);
                }
                workspace.tabs.retain(|tab| !tab.panes.is_empty());
                if !workspace
                    .tabs
                    .iter()
                    .any(|tab| tab.id == workspace.active_tab)
                {
                    if let Some(first) = workspace.tabs.first() {
                        workspace.active_tab = first.id.clone();
                    }
                }
            }
            session
                .workspaces
                .retain(|workspace| !workspace.tabs.is_empty());
            if session.focused_pane.as_deref() == Some(pane_id) {
                session.focused_pane = None;
            }
        }
        self.inner.transcripts.lock().remove(pane_id);
        self.inner.codex_transcripts.lock().remove(pane_id);
        self.inner.opencode_transcripts.lock().remove(pane_id);
        self.save();
        self.broadcast(json!({ "type": "session-changed" }));
        Ok((200, json!({ "ok": true })))
    }

    fn focus_pane(&self, body: &Value) -> std::result::Result<(u16, Value), (u16, Value)> {
        let pane_id = body
            .get("pane_id")
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "pane_id is required."))?;
        let mut session = self.inner.session.write();
        if !session.panes.contains_key(pane_id) {
            return Err(api_error(404, "pane_not_found", "That pane is not open."));
        }
        session.focused_pane = Some(pane_id.to_string());
        Ok((200, json!({ "ok": true })))
    }

    fn rename_pane(&self, body: &Value) -> std::result::Result<(u16, Value), (u16, Value)> {
        let pane_id = body
            .get("pane_id")
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "pane_id is required."))?;
        let label = body
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let pane = self.pane(pane_id)?;
        pane.rename(&label);
        self.save();
        self.broadcast(json!({ "type": "session-changed" }));
        Ok((200, json!({ "ok": true })))
    }

    fn clear_pane(&self, body: &Value) -> std::result::Result<(u16, Value), (u16, Value)> {
        let pane_id = body
            .get("pane_id")
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "pane_id is required."))?;
        self.pane(pane_id)?.clear();
        Ok((200, json!({ "ok": true })))
    }

    fn read_pane(
        &self,
        query: &HashMap<String, String>,
    ) -> std::result::Result<(u16, Value), (u16, Value)> {
        let pane_id = required(query, "pane_id")?;
        let pane = self.pane(pane_id)?;
        let source = query
            .get("source")
            .cloned()
            .unwrap_or_else(|| "recent".to_string());
        let lines = query
            .get("lines")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(2000)
            .clamp(1, 20_000);
        let (text, truncated, revision) = match source.as_str() {
            "visible" | "detection" => (pane.visible_text(), false, pane.revision()),
            "recent" => pane.read(lines, false),
            "recent_unwrapped" => pane.read(lines, true),
            other => {
                return Err(api_error(
                    400,
                    "invalid_request",
                    &format!("unknown read source '{other}'"),
                ))
            }
        };
        let format = query
            .get("format")
            .cloned()
            .unwrap_or_else(|| "text".to_string());
        Ok((
            200,
            json!({
                "read": {
                    "pane_id": pane.id,
                    "workspace_id": pane.workspace_id,
                    "tab_id": pane.tab_id,
                    "source": source,
                    "format": format,
                    "text": text,
                    "truncated": truncated,
                    "revision": revision,
                }
            }),
        ))
    }

    fn conversation(
        &self,
        query: &HashMap<String, String>,
    ) -> std::result::Result<(u16, Value), (u16, Value)> {
        let pane_id = required(query, "pane_id")?;
        let pane = self.pane(pane_id)?;
        if pane.agent == AgentKind::Claude {
            let mut transcripts = self.inner.transcripts.lock();
            if !transcripts.contains_key(pane_id) {
                if let Some(transcript) =
                    ClaudeTranscript::open(&pane.cwd, None, Some(pane.started()))
                {
                    info!(
                        "claude transcript for {}: {}",
                        pane_id,
                        transcript.path().display()
                    );
                    transcripts.insert(pane_id.to_string(), transcript);
                }
            }
            if let Some(transcript) = transcripts.get_mut(pane_id) {
                transcript.refresh();
                return Ok((200, transcript.response()));
            }
        } else if pane.agent == AgentKind::Codex {
            let mut transcripts = self.inner.codex_transcripts.lock();
            if !transcripts.contains_key(pane_id) {
                if let Some(transcript) = CodexTranscript::open_for_screen(
                    &pane.cwd,
                    &pane.visible_text(),
                    Some(pane.started()),
                ) {
                    info!(
                        "codex transcript for {}: {}",
                        pane_id,
                        transcript.path().display()
                    );
                    transcripts.insert(pane_id.to_string(), transcript);
                }
            }
            if let Some(transcript) = transcripts.get_mut(pane_id) {
                transcript.refresh();
                return Ok((200, transcript.response()));
            }
        } else if pane.agent == AgentKind::OpenCode {
            let mut transcripts = self.inner.opencode_transcripts.lock();
            if !transcripts.contains_key(pane_id) {
                if let Some(transcript) = OpencodeTranscript::open(&pane.cwd, Some(pane.started()))
                {
                    info!(
                        "opencode transcript for {}: {}",
                        pane_id,
                        transcript.path().display()
                    );
                    transcripts.insert(pane_id.to_string(), transcript);
                }
            }
            if let Some(transcript) = transcripts.get_mut(pane_id) {
                transcript.refresh();
                return Ok((200, transcript.response()));
            }
        }
        // The agent has not written a transcript yet, or it keeps none: the chat reads the grid.
        Ok((
            200,
            json!({
                "source": "scrollback",
                "turns": [],
            }),
        ))
    }

    fn prompt(
        &self,
        query: &HashMap<String, String>,
    ) -> std::result::Result<(u16, Value), (u16, Value)> {
        let pane_id = required(query, "pane_id")?;
        let pane = self.pane(pane_id)?;
        let mut detected = pane.prompt();
        // A collapsed Codex queue shows only a count; its first question comes from the rollout.
        if detected.is_none() && pane.agent == AgentKind::Codex {
            let visible = pane.visible_text();
            if prompt::codex_queued_count(&visible) > 0 {
                let mut transcripts = self.inner.codex_transcripts.lock();
                if !transcripts.contains_key(pane_id) {
                    if let Some(transcript) =
                        CodexTranscript::open_for_screen(&pane.cwd, &visible, Some(pane.started()))
                    {
                        transcripts.insert(pane_id.to_string(), transcript);
                    }
                }
                if let Some(transcript) = transcripts.get_mut(pane_id) {
                    transcript.refresh();
                    let unanswered = transcript
                        .unanswered_questions()
                        .into_iter()
                        .map(|question| prompt::QueuedQuestion {
                            key: question.key,
                            title: question.title,
                            options: question.options,
                        })
                        .collect::<Vec<_>>();
                    detected = prompt::codex_queued_prompt(&visible, &unanswered);
                }
            }
        }
        Ok((
            200,
            json!({
                "prompt": detected.map(|prompt| prompt.to_wire()),
                "suggestion": null,
                "restored": null,
            }),
        ))
    }

    fn answer_prompt(&self, body: &Value) -> std::result::Result<(u16, Value), (u16, Value)> {
        let pane_id = body
            .get("pane_id")
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "pane_id is required."))?;
        let prompt_id = body
            .get("prompt_id")
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "prompt_id is required."))?;
        let option = body
            .get("option_index")
            .and_then(Value::as_u64)
            .map(|index| index as usize);
        let custom = body.get("custom_text").and_then(Value::as_str);
        if option.is_none() && custom.is_none() {
            return Err(api_error(
                400,
                "invalid_request",
                "This build answers with option_index or custom_text.",
            ));
        }
        let pane = self.pane(pane_id)?;
        // A collapsed Codex queue answers by opening it first (alt+up), then answering the
        // question that shows.
        let mut answer_id = prompt_id.to_string();
        if pane.agent == AgentKind::Codex
            && pane.prompt().is_none()
            && pane.keys(&["alt+up".to_string()]).is_ok()
        {
            for _ in 0..20 {
                std::thread::sleep(std::time::Duration::from_millis(100));
                if let Some(open) = pane.prompt() {
                    answer_id = open.id;
                    break;
                }
            }
        }
        let result = match (option, custom) {
            (Some(option), _) => pane.answer_prompt(&answer_id, option),
            (None, Some(text)) => pane.answer_prompt_custom(&answer_id, text),
            (None, None) => unreachable!(),
        };
        match result {
            Ok(()) => {
                if let Ok(pane) = self.pane(pane_id) {
                    let _ = pane;
                }
                self.broadcast(json!({
                    "type": "pane-status",
                    "pane_id": pane_id,
                    "agent_status": AgentStatus::Working.as_wire(),
                }));
                Ok((200, json!({ "ok": true })))
            }
            Err(code) if code == "prompt_changed" => Err(api_error(
                409,
                "prompt_changed",
                "The question on screen changed; read it again.",
            )),
            Err(code) => Err(api_error(400, "invalid_request", &code)),
        }
    }

    fn scroll(
        &self,
        query: &HashMap<String, String>,
    ) -> std::result::Result<(u16, Value), (u16, Value)> {
        let pane_id = required(query, "pane_id")?;
        let pane = self.pane(pane_id)?;
        Ok((200, json!({ "scroll": pane.scroll_info() })))
    }

    fn scroll_to(&self, body: &Value) -> std::result::Result<(u16, Value), (u16, Value)> {
        let pane_id = body
            .get("pane_id")
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "pane_id is required."))?;
        let offset = body
            .get("offset_from_bottom")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let pane = self.pane(pane_id)?;
        Ok((200, json!({ "scroll": pane.scroll_to(offset) })))
    }

    fn find(&self, body: &Value) -> std::result::Result<(u16, Value), (u16, Value)> {
        let pane_id = body
            .get("pane_id")
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "pane_id is required."))?;
        let query = body
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let direction = body
            .get("direction")
            .and_then(Value::as_str)
            .unwrap_or("forward");
        let jump = body.get("jump").and_then(Value::as_bool).unwrap_or(true);
        let pane = self.pane(pane_id)?;
        let matches = pane.find(query);
        let revision = pane.revision();
        let total = matches.len();
        let current = if total == 0 {
            None
        } else if direction == "backward" {
            Some(total)
        } else {
            Some(1)
        };
        let hit = current.and_then(|index| matches.get(index - 1).cloned());
        if jump {
            if let Some(hit) = &hit {
                let row = hit["start"]["row"].as_u64().unwrap_or(0) as usize;
                let viewport = pane
                    .scroll_info()
                    .get("viewport_rows")
                    .and_then(Value::as_u64)
                    .unwrap_or(24) as usize;
                let history = pane.history().len();
                let offset = history.saturating_sub(row + viewport / 2);
                pane.scroll_to(offset);
            }
        }
        Ok((
            200,
            json!({
                "total": total,
                "current": current,
                "match": hit,
                "content_revision": revision,
                "matches": matches,
                "scroll": pane.scroll_info(),
            }),
        ))
    }

    fn selection(
        &self,
        query: &HashMap<String, String>,
    ) -> std::result::Result<(u16, Value), (u16, Value)> {
        let pane_id = required(query, "pane_id")?;
        let number = |name: &str| -> usize {
            query
                .get(name)
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0)
        };
        let pane = self.pane(pane_id)?;
        let text = pane.selection(
            number("anchor_row"),
            number("anchor_col"),
            number("cursor_row"),
            number("cursor_col"),
        );
        Ok((200, json!({ "text": text })))
    }

    fn directories(
        &self,
        query: &HashMap<String, String>,
    ) -> std::result::Result<(u16, Value), (u16, Value)> {
        let home = home_dir().unwrap_or_else(|| PathBuf::from("."));
        let requested = query
            .get("path")
            .filter(|path| !path.is_empty())
            .map(|path| expand_home(path, &home))
            .unwrap_or_else(|| home.clone());
        let hidden = query.get("hidden").map(String::as_str) == Some("1");
        let files = query.get("files").map(String::as_str) == Some("1");
        let entries = std::fs::read_dir(&requested).map_err(|_| {
            api_error(
                400,
                "invalid_cwd",
                "That directory is not there or cannot be read.",
            )
        })?;
        let mut directories = Vec::new();
        let mut file_entries = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !hidden && name.starts_with('.') {
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                directories.push(name);
            } else if files && kind.is_file() {
                let size = entry.metadata().map(|metadata| metadata.len()).unwrap_or(0);
                file_entries.push(json!({ "name": name, "size": size }));
            }
        }
        directories.sort_by_key(|name| name.to_lowercase());
        let parent = requested
            .parent()
            .filter(|parent| parent != &requested)
            .map(|parent| parent.to_string_lossy().to_string());
        let mut body = json!({
            "path": requested.to_string_lossy(),
            "parent": parent,
            "home": home.to_string_lossy(),
            "directories": directories,
            "truncated": false,
        });
        if files {
            body["files"] = Value::Array(file_entries);
        }
        Ok((200, body))
    }

    // ---- panes --------------------------------------------------------------------------

    fn spawn_pane(&self, request: PaneRequest) -> Result<Arc<Pane>> {
        let spec = LaunchSpec::for_agent(
            request.agent,
            &request.args,
            request.session.as_deref(),
            request.session.is_none() && request.resume_last,
        )?;
        let size = TerminalSize {
            rows: DEFAULT_SIZE.rows,
            cols: DEFAULT_SIZE.cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let weak = Arc::downgrade(&self.inner);
        let on_event = Arc::new(move |event: PaneEvent| {
            if let Some(inner) = Weak::upgrade(&weak) {
                inner.on_pane_event(event);
            }
        });
        let pane = Pane::spawn(
            PaneConfig {
                id: request.id,
                workspace_id: request.workspace_id,
                tab_id: request.tab_id,
                agent: request.agent,
                label: request.label,
                cwd: request.cwd,
                spec,
                args: request.args,
                size,
            },
            on_event,
        )?;
        Ok(Arc::new(pane))
    }

    /// Restores the workspaces saved by the last run. Each pane resumes its agent (`--continue`
    /// or the saved session id), so a restart puts the user back where they were.
    pub fn restore(&self) {
        let saved = self.inner.store.load_session();
        if saved.panes.is_empty() {
            return;
        }
        let mut restored: HashMap<String, Arc<Pane>> = HashMap::new();
        for pane in &saved.panes {
            let cwd = PathBuf::from(&pane.cwd);
            if !cwd.is_dir() {
                warn!("not restoring pane {}: {} is gone", pane.id, pane.cwd);
                continue;
            }
            let Some(agent) = AgentKind::from_wire(&pane.agent) else {
                continue;
            };
            let orphan_workspace = saved
                .workspaces
                .iter()
                .find(|workspace| {
                    workspace
                        .tabs
                        .iter()
                        .any(|tab| tab.pane_ids.iter().any(|id| id == &pane.id))
                })
                .map(|workspace| workspace.id.clone())
                .unwrap_or_default();
            let tab_id = saved
                .workspaces
                .iter()
                .flat_map(|workspace| workspace.tabs.iter())
                .find(|tab| tab.pane_ids.iter().any(|id| id == &pane.id))
                .map(|tab| tab.id.clone())
                .unwrap_or_default();
            let spec = match spec_for_saved_pane(pane) {
                Ok(spec) => spec,
                Err(error) => {
                    warn!("not restoring pane {}: {error}", pane.id);
                    continue;
                }
            };
            let weak = Arc::downgrade(&self.inner);
            let on_event = Arc::new(move |event: PaneEvent| {
                if let Some(inner) = Weak::upgrade(&weak) {
                    inner.on_pane_event(event);
                }
            });
            match Pane::spawn(
                PaneConfig {
                    id: pane.id.clone(),
                    workspace_id: orphan_workspace,
                    tab_id,
                    agent,
                    label: pane.label.clone(),
                    cwd,
                    spec,
                    args: pane.args.clone(),
                    size: DEFAULT_SIZE,
                },
                on_event,
            ) {
                Ok(pane) => {
                    restored.insert(pane.id.clone(), Arc::new(pane));
                }
                Err(error) => warn!("not restoring pane {}: {error}", pane.id),
            }
        }
        if restored.is_empty() {
            return;
        }
        let mut session = self.inner.session.write();
        let mut number = 0;
        for workspace in &saved.workspaces {
            let mut tabs = Vec::new();
            for tab in &workspace.tabs {
                let ids = tab
                    .pane_ids
                    .iter()
                    .filter(|id| restored.contains_key(*id))
                    .cloned()
                    .collect::<Vec<_>>();
                if ids.is_empty() {
                    continue;
                }
                let active = if ids.contains(&tab.active_pane_id) {
                    tab.active_pane_id.clone()
                } else {
                    ids[0].clone()
                };
                tabs.push(Tab {
                    id: tab.id.clone(),
                    label: tab.label.clone(),
                    number: tab.number.max(tabs.len() as u32 + 1),
                    panes: ids,
                    active_pane: active,
                });
            }
            if tabs.is_empty() {
                continue;
            }
            number += 1;
            let active_tab = if tabs.iter().any(|tab| tab.id == workspace.active_tab_id) {
                workspace.active_tab_id.clone()
            } else {
                tabs[0].id.clone()
            };
            let panes = tabs
                .iter()
                .flat_map(|tab| tab.panes.iter().cloned())
                .collect::<Vec<_>>();
            session.workspaces.push(Workspace {
                id: workspace.id.clone(),
                label: workspace.label.clone(),
                cwd: PathBuf::from(&workspace.cwd),
                number,
                tabs,
                active_tab,
            });
            let _ = panes;
        }
        for (id, pane) in restored {
            session.panes.insert(id, pane);
        }
        if let Some(focused) = saved.focused_pane_id {
            if session.panes.contains_key(&focused) {
                session.focused_pane = Some(focused);
            }
        }
        if session.workspaces.is_empty() {
            // The saved workspaces were gone: keep the panes as one workspace so nothing is lost.
            let panes = session.panes.keys().cloned().collect::<Vec<_>>();
            if let Some(first) = panes.first().cloned() {
                let workspace_id = Uuid::new_v4().to_string();
                let tab_id = Uuid::new_v4().to_string();
                session.workspaces.push(Workspace {
                    id: workspace_id,
                    label: "Restored".to_string(),
                    cwd: home_dir().unwrap_or_else(|| PathBuf::from(".")),
                    number: 1,
                    tabs: vec![Tab {
                        id: tab_id.clone(),
                        label: "Restored".to_string(),
                        number: 1,
                        panes,
                        active_pane: first,
                    }],
                    active_tab: tab_id,
                });
            }
        }
        drop(session);
        info!("restored {} pane(s)", self.inner.session.read().panes.len());
    }

    fn save(&self) {
        let saved = {
            let session = self.inner.session.read();
            let mut saved = SavedSession {
                version: 1,
                focused_pane_id: session.focused_pane.clone(),
                ..SavedSession::default()
            };
            for workspace in &session.workspaces {
                saved.workspaces.push(SavedWorkspace {
                    id: workspace.id.clone(),
                    label: workspace.label.clone(),
                    cwd: workspace.cwd.to_string_lossy().to_string(),
                    active_tab_id: workspace.active_tab.clone(),
                    tabs: workspace
                        .tabs
                        .iter()
                        .map(|tab| SavedTab {
                            id: tab.id.clone(),
                            label: tab.label.clone(),
                            number: tab.number,
                            pane_ids: tab.panes.clone(),
                            active_pane_id: tab.active_pane.clone(),
                        })
                        .collect(),
                });
            }
            for (id, pane) in &session.panes {
                saved.panes.push(SavedPane {
                    id: id.clone(),
                    agent: pane.agent.wire_kind().to_string(),
                    label: pane.label(),
                    cwd: pane.cwd.to_string_lossy().to_string(),
                    args: pane.args.clone(),
                    session_id: None,
                });
            }
            saved
        };
        if let Err(error) = self.inner.store.save_session(&saved) {
            warn!("session not saved: {error}");
        }
    }
}

impl Inner {
    fn on_pane_event(&self, event: PaneEvent) {
        match event {
            PaneEvent::Status {
                pane_id,
                status,
                completion,
            } => {
                self.broadcast(json!({
                    "type": "pane-status",
                    "pane_id": pane_id,
                    "agent_status": status.as_wire(),
                }));
                if completion {
                    self.broadcast(json!({ "type": "session-changed" }));
                }
            }
            PaneEvent::Exited { pane_id, code } => {
                self.broadcast(json!({
                    "type": "pty-exit",
                    "pane_id": pane_id,
                    "code": code,
                }));
                self.broadcast(json!({
                    "type": "pane-exited",
                    "pane_id": pane_id,
                }));
                self.broadcast(json!({ "type": "session-changed" }));
            }
        }
    }

    /// Whether `connection_id` is present (active within the last 30 seconds).
    fn is_present(&self, connection_id: &str) -> bool {
        let presence = self.presence.lock();
        let Some(when) = presence.get(connection_id) else {
            return false;
        };
        when.elapsed() <= Duration::from_secs(30)
    }

    fn broadcast(&self, frame: Value) {
        let kind = frame.get("type").and_then(Value::as_str).unwrap_or("");
        let connections = self.connections.lock();
        for (connection_id, connection) in connections.iter() {
            // A present connection already sees the UI live: do not push it a duplicate of an
            // event it is already looking at. Absent connections still get the frame.
            if (kind == "pane-status" || kind == "session-changed")
                && self.is_present(connection_id)
            {
                continue;
            }
            (connection.sink)(frame.clone());
        }
    }
}

struct PaneRequest {
    id: String,
    workspace_id: String,
    tab_id: String,
    agent: AgentKind,
    label: String,
    cwd: PathBuf,
    args: Vec<String>,
    session: Option<String>,
    resume_last: bool,
}

/// One `agent` object from a create request: which agent, what to call it, and its own arguments.
struct AgentRequest {
    kind: AgentKind,
    name: Option<String>,
    args: Vec<String>,
}

fn api_error(status: u16, code: &str, message: &str) -> (u16, Value) {
    (
        status,
        json!({ "error": { "code": code, "message": message } }),
    )
}

fn pane_id_of(request: &Value) -> std::result::Result<String, String> {
    request
        .get("pane_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "pane_id is missing".to_string())
}

fn required<'a>(
    query: &'a HashMap<String, String>,
    name: &str,
) -> std::result::Result<&'a str, (u16, Value)> {
    query
        .get(name)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| api_error(400, "invalid_request", &format!("{name} is required.")))
}

fn agent_request(body: &Value) -> std::result::Result<AgentRequest, (u16, Value)> {
    let agent = body.get("agent").ok_or_else(|| {
        api_error(
            400,
            "invalid_request",
            "Orion only opens agent panes: name the agent to start.",
        )
    })?;
    let kind = agent
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| api_error(400, "invalid_request", "agent.kind is required."))?;
    let kind = AgentKind::from_wire(kind)
        .ok_or_else(|| api_error(400, "unknown_agent", "Orion does not start that agent."))?;
    let command = kind.default_command();
    if find_in_path(command).is_none() {
        return Err(api_error(
            400,
            "agent_not_found",
            &format!("'{command}' is not on this PC's PATH."),
        ));
    }
    let name = agent
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .map(str::to_string);
    let args = agent
        .get("args")
        .and_then(Value::as_array)
        .map(|args| {
            args.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(AgentRequest { kind, name, args })
}

fn resolve_cwd(cwd: Option<&str>) -> std::result::Result<PathBuf, (u16, Value)> {
    let home = home_dir().unwrap_or_else(|| PathBuf::from("."));
    let path = match cwd {
        Some(cwd) if !cwd.trim().is_empty() => expand_home(cwd.trim(), &home),
        _ => home,
    };
    if !path.is_dir() {
        return Err(api_error(
            400,
            "invalid_cwd",
            "That directory is not there.",
        ));
    }
    Ok(path)
}

fn expand_home(value: &str, home: &Path) -> PathBuf {
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

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
}

fn folder_name(cwd: &Path) -> Option<String> {
    cwd.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .filter(|name| !name.is_empty())
}

/// The tab/workspace status: the most urgent pane wins, a running turn above a waiting one.
fn aggregate_status(session: &Session, pane_ids: &[String]) -> AgentStatus {
    let mut status = AgentStatus::Idle;
    for id in pane_ids {
        let Some(pane) = session.pane(id) else {
            continue;
        };
        match pane.status() {
            AgentStatus::Blocked => return AgentStatus::Blocked,
            AgentStatus::Working => status = AgentStatus::Working,
            AgentStatus::Done if status == AgentStatus::Idle => status = AgentStatus::Done,
            _ => {}
        }
    }
    status
}

/// Splits `/api/pane/read?pane_id=x` into the route and its decoded query.
fn split_path(path: &str) -> (String, HashMap<String, String>) {
    let mut parts = path.splitn(2, '?');
    let route = parts.next().unwrap_or_default().to_string();
    let mut query = HashMap::new();
    if let Some(text) = parts.next() {
        for pair in text.split('&') {
            if pair.is_empty() {
                continue;
            }
            let mut sides = pair.splitn(2, '=');
            let key = percent_decode(sides.next().unwrap_or_default());
            let value = percent_decode(sides.next().unwrap_or_default());
            query.insert(key, value);
        }
    }
    (route, query)
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                let hex = |byte: u8| -> Option<u8> {
                    match byte {
                        b'0'..=b'9' => Some(byte - b'0'),
                        b'a'..=b'f' => Some(byte - b'a' + 10),
                        b'A'..=b'F' => Some(byte - b'A' + 10),
                        _ => None,
                    }
                };
                if let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                    out.push(high * 16 + low);
                    index += 3;
                } else {
                    out.push(bytes[index]);
                    index += 1;
                }
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> Engine {
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
}
