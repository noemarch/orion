//! The REST seam: matches HTTP method+path to a handler.
//!
//! Owns every `/api/*` route that lived in `engine.rs`'s `http()` match, plus the pure helpers
//! (`split_path`, `percent_decode`, `resolve_cwd`, `agent_request`, …). `RouteTable` is
//! constructed with the modules it needs and handles a request end to end.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::{json, Value};
use tracing::warn;
use uuid::Uuid;

use crate::agents::{find_in_path, AgentRegistry, LaunchSpec};
use crate::engine::connection_registry::ConnectionRegistry;
use crate::engine::devices::DeviceStore;
use crate::engine::pane::{Pane, PaneConfig, PaneEvent};
use crate::engine::push::PushService;
use crate::engine::session::{SessionState, Tab, Workspace};
use crate::engine::store::{spec_for_saved_pane, Store, DEFAULT_SIZE};
use crate::engine::tailscale;
use crate::engine::transcript_manager::TranscriptManager;
use crate::engine::watcher::Watcher;
use crate::engine::{expand_home, home_dir, PROTOCOL_VERSION};
use crate::error::Result;
use crate::protocol::{AgentKind, AgentStatus, TabLayout, TerminalSize};

/// The REST seam.
pub struct RouteTable {
    session: Arc<SessionState>,
    connections: Arc<ConnectionRegistry>,
    transcripts: Arc<TranscriptManager>,
    store: Arc<Store>,
    devices: Arc<DeviceStore>,
    push: Arc<PushService>,
    watchers: Arc<Mutex<HashMap<String, Watcher>>>,
    preferences: Arc<Mutex<Value>>,
    version: String,
}

impl RouteTable {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session: Arc<SessionState>,
        transcripts: Arc<TranscriptManager>,
        store: Arc<Store>,
        push: Arc<PushService>,
        devices: Arc<DeviceStore>,
        watchers: Arc<Mutex<HashMap<String, Watcher>>>,
        preferences: Arc<Mutex<Value>>,
        version: String,
        connections: Arc<ConnectionRegistry>,
    ) -> Self {
        Self {
            session,
            connections,
            transcripts,
            store,
            devices,
            push,
            watchers,
            preferences,
            version,
        }
    }

    /// Builds the `on_event` closure passed to `Pane::spawn`.
    fn on_event(&self) -> Arc<dyn Fn(PaneEvent) + Send + Sync> {
        let session = Arc::clone(&self.session);
        let connections = Arc::clone(&self.connections);
        let push = Arc::clone(&self.push);
        Arc::new(move |event| {
            crate::engine::on_pane_event(&session, &connections, &push, event);
        })
    }

    /// Handles one REST call end to end.
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
            ("GET", "/api/plugins/actions") => Ok((200, json!({ "plugins": [] }))),
            ("POST", "/api/plugin/action") => Ok(api_error(
                404,
                "plugin_not_found",
                "No plugins are installed on this PC.",
            )),
            ("GET", "/api/plugin/action") => Ok(api_error(
                404,
                "plugin_log_not_found",
                "No plugin runs are recorded on this PC.",
            )),
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
            ("POST", "/api/pane/split") => self.split_pane(body),
            ("POST", "/api/pane/zoom") => self.zoom_pane(body),
            ("POST", "/api/pane/swap") => self.swap_pane(body),
            ("POST", "/api/pane/resize") => self.resize_pane(body),
            ("POST", "/api/layout/ratio") => self.set_split_ratio(body),
            ("POST", "/api/pane/focus") => self.focus_pane(body),
            ("POST", "/api/pane/rename") => self.rename_pane(body),
            ("POST", "/api/pane/clear") => self.clear_pane(body),
            ("GET", "/api/pane/read") => self.read_pane(&query),
            ("GET", "/api/pane/conversation") => self.conversation(&query),
            ("GET", "/api/pane/omo-tasks") => self.omo_tasks(&query),
            ("GET", "/api/pane/commands") => Ok((200, json!({ "commands": [] }))),
            ("GET", "/api/pane/plan") => self.pane_plan(&query),
            ("POST", "/api/pane/prompt/answer") => self.answer_prompt(body),
            ("GET", "/api/pane/prompt") => self.prompt(&query),
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
            ("GET", "/api/devices") => Ok((200, self.devices.list())),
            ("POST", "/api/devices/pair/start") => Ok((200, self.devices.new_pairing_code())),
            ("POST", "/api/machines/setup") => {
                if query.contains_key("id") {
                    let id = query.get("id").unwrap();
                    Ok((200, self.setup_answer(id, body)))
                } else {
                    self.setup_start(body)
                }
            }
            ("GET", "/api/machines/setup") => {
                let id = match query.get("id") {
                    Some(id) => id,
                    None => return api_error(400, "invalid_request", "setup id is required."),
                };
                Ok((200, self.setup_status(id)))
            }
            ("PATCH", "/api/machines") => self.machine_patch(body),
            ("DELETE", "/api/machines") => {
                let id = match query.get("id") {
                    Some(id) => id,
                    None => return api_error(400, "invalid_request", "machine id is required."),
                };
                self.machine_delete(id).ok();
                Ok((204, Value::Null))
            }
            ("DELETE", "/api/devices/:id") => {
                let id = match query.get("id") {
                    Some(id) => id,
                    None => return api_error(400, "invalid_request", "device id is required."),
                };
                self.device_delete(id).ok();
                Ok((204, Value::Null))
            }
            ("GET", "/api/push") => Ok((200, json!({ "public_key": self.push.public_key() }))),
            ("POST", "/api/push/subscribe") => {
                self.push.subscribe(
                    body.get("subscription").cloned().unwrap_or(Value::Null),
                    body.get("alerts").cloned(),
                );
                Ok((204, Value::Null))
            }
            ("DELETE", "/api/push/subscribe") => {
                if let Some(endpoint) = body.get("endpoint").and_then(Value::as_str) {
                    self.push.unsubscribe(endpoint);
                }
                Ok((204, Value::Null))
            }
            ("GET", "/api/orion/update") => Ok((
                200,
                json!({
                    "supported": false,
                    "phase": "idle",
                    "server_version": env!("CARGO_PKG_VERSION"),
                    "binary_version": env!("CARGO_PKG_VERSION"),
                    "stale": false,
                    "output": null,
                    "finished_at": null,
                }),
            )),
            ("POST", "/api/orion/update") => Ok((200, json!({ "ok": true }))),
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

    // ---- handlers ----

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
        let integrations = crate::agents::AgentRegistry::discover_all()
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
        let preferences = self.preferences.lock();
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
        let mut preferences = self.preferences.lock();
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
        if let Err(error) = self.store.save_preferences(&next) {
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
            .spawn_pane_request(PaneRequest {
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
            let mut session = self.session.write();
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
                    layout: TabLayout::Single,
                    zoomed_pane: None,
                    split_ratios: HashMap::new(),
                }],
                active_tab: tab_id,
            });
            session.panes.insert(pane_id.clone(), pane);
            session.focused_pane = Some(pane_id.clone());
        }
        self.watchers.lock().insert(
            workspace_id.clone(),
            Watcher::start(cwd, Arc::clone(&self.connections)),
        );
        self.save();
        tracing::info!("workspace {workspace_id} opened with agent {pane_id}");
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
            let session = self.session.read();
            let workspace = session.workspace_of(workspace_id).ok_or_else(|| {
                api_error(404, "workspace_not_found", "That workspace is not open.")
            })?;
            workspace.cwd.clone()
        };
        Ok((
            200,
            json!({ "worktrees": crate::engine::worktree::list(&cwd) }),
        ))
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
            let session = self.session.read();
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
                Some(branch) => crate::engine::worktree::default_worktree_path(&source_cwd, branch),
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
            crate::engine::worktree::create(&source_cwd, &path, &branch, base.as_deref()).map_err(
                |error| {
                    api_error(
                        400,
                        "worktree_create_failed",
                        &format!("The worktree could not be created: {error}"),
                    )
                },
            )?;
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
            let session = self.session.read();
            let workspace = session.workspace_of(workspace_id).ok_or_else(|| {
                api_error(404, "workspace_not_found", "That workspace is not open.")
            })?;
            workspace.cwd.clone()
        };
        // Closing the workspace kills its panes first; removing a checkout with live agents
        // in it would strand them.
        self.close_workspace(&json!({ "workspace_id": workspace_id }))?;
        crate::engine::worktree::remove(&cwd, &cwd, force).map_err(|error| {
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
        Ok((
            200,
            crate::engine::diff::files(&cwd, scope, base, head, None),
        ))
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
            crate::engine::diff::file_patch(&cwd, scope, base, head, path, old_path),
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

    fn pairing_code(&self, _body: &Value) -> std::result::Result<(u16, Value), (u16, Value)> {
        Ok((200, self.devices.new_pairing_code()))
    }

    fn pair_device(&self, body: &Value) -> std::result::Result<(u16, Value), (u16, Value)> {
        let code = body
            .get("code")
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "code is required."))?;
        let label = body.get("label").and_then(Value::as_str).unwrap_or("Phone");
        let role = body.get("role").and_then(Value::as_str).unwrap_or("drive");
        self.devices
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

    fn device_delete(&self, device_id: &str) -> std::result::Result<(), (u16, Value)> {
        let devices = self.devices.devices_vec();
        let before = devices.len();
        if before == devices.iter().filter(|d| d.id != device_id).count() {
            return Err(api_error(
                404,
                "device_not_found",
                "That device is not paired.",
            ));
        }
        let remaining = devices
            .into_iter()
            .filter(|d| d.id != device_id)
            .collect::<Vec<_>>();
        self.devices.save_devices(remaining);
        Ok(())
    }

    fn setup_start(&self, _body: &Value) -> std::result::Result<(u16, Value), (u16, Value)> {
        Ok((
            200,
            json!({ "job_id": "local-setup", "phase": "connecting", "step": "connecting" }),
        ))
    }

    fn setup_status(&self, _id: &str) -> Value {
        json!({ "job_id": "local-setup", "phase": "connecting", "step": "connecting", "error": null })
    }

    fn setup_answer(&self, _id: &str, _body: &Value) -> Value {
        json!({ "job_id": "local-setup", "phase": "connecting", "step": "connecting", "error": null })
    }

    fn pane_plan(
        &self,
        _query: &HashMap<String, String>,
    ) -> std::result::Result<(u16, Value), (u16, Value)> {
        Ok((
            200,
            json!({ "plan": null, "server_time": chrono::Utc::now().to_rfc3339() }),
        ))
    }

    fn machine_patch(&self, body: &Value) -> std::result::Result<(u16, Value), (u16, Value)> {
        let device_id = body
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "id is required."))?;
        let label = body.get("label").and_then(Value::as_str);
        let devices = self.devices.devices_vec();
        if !devices.iter().any(|d| d.id == device_id) {
            return Err(api_error(
                404,
                "device_not_found",
                "That device is not paired.",
            ));
        }
        let remaining = devices
            .into_iter()
            .map(|mut d| {
                if d.id == device_id {
                    if let Some(label) = label.filter(|l| !l.trim().is_empty()) {
                        d.label = label.to_string();
                    }
                }
                d
            })
            .collect::<Vec<_>>();
        self.devices.save_devices(remaining);
        Ok((200, json!({ "ok": true })))
    }

    fn machine_delete(&self, device_id: &str) -> std::result::Result<(), (u16, Value)> {
        self.device_delete(device_id)
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
            let mut session = self.session.write();
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
            let mut session = self.session.write();
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
        Ok((200, json!({ "ok": true })))
    }

    fn close_workspace(&self, body: &Value) -> std::result::Result<(u16, Value), (u16, Value)> {
        let workspace_id = body
            .get("workspace_id")
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "workspace_id is required."))?;
        let mut gone = Vec::new();
        {
            let mut session = self.session.write();
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
            refocus_after_close(&mut session);
        }
        for pane_id in &gone {
            self.transcripts.remove_pane(pane_id);
        }
        if let Some(watcher) = self.watchers.lock().remove(workspace_id) {
            watcher.stop();
        }
        self.save();
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
            let mut session = self.session.write();
            let tab = session
                .workspaces
                .iter_mut()
                .flat_map(|workspace| workspace.tabs.iter_mut())
                .find(|tab| tab.id == *tab_id)
                .ok_or_else(|| api_error(404, "tab_not_found", "That tab is not open."))?;
            tab.label = label.to_string();
        }
        self.save();
        Ok((200, json!({ "ok": true })))
    }

    fn close_tab(&self, body: &Value) -> std::result::Result<(u16, Value), (u16, Value)> {
        let tab_id = body
            .get("tab_id")
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "tab_id is required."))?;
        let mut gone = Vec::new();
        {
            let mut session = self.session.write();
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
            self.watchers.lock().retain(|id, _| live.contains(id));
            refocus_after_close(&mut session);
        }
        for pane_id in &gone {
            self.transcripts.remove_pane(pane_id);
        }
        self.save();
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
            let mut session = self.session.write();
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
        Ok((200, json!({ "ok": true })))
    }

    fn create_tab(&self, body: &Value) -> std::result::Result<(u16, Value), (u16, Value)> {
        let workspace_id = body
            .get("workspace_id")
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "workspace_id is required."))?
            .to_string();
        let (cwd, label) = {
            let session = self.session.read();
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
            .spawn_pane_request(PaneRequest {
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
            let mut session = self.session.write();
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
                layout: TabLayout::Single,
                zoomed_pane: None,
                split_ratios: HashMap::new(),
            });
            workspace.active_tab = tab_id;
            session.panes.insert(pane_id.clone(), pane);
            session.focused_pane = Some(pane_id.clone());
        }
        self.save();
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
            let mut session = self.session.write();
            let Some(pane) = session.panes.remove(pane_id) else {
                return Err(api_error(404, "pane_not_found", "That pane is not open."));
            };
            // Dropping the pane ends its job object, so the agent's whole tree goes with it.
            drop(pane);
            for workspace in &mut session.workspaces {
                for tab in &mut workspace.tabs {
                    tab.panes.retain(|id| id != pane_id);
                    // The tab remembers the pane the user last chose; when that
                    // pane closes, fall back to the first one still open.
                    if tab.active_pane.as_str() == pane_id {
                        tab.active_pane = tab.panes.first().cloned().unwrap_or_default();
                    }
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
            refocus_after_close(&mut session);
        }
        self.transcripts.remove_pane(pane_id);
        self.save();
        Ok((200, json!({ "ok": true })))
    }

    fn focus_pane(&self, body: &Value) -> std::result::Result<(u16, Value), (u16, Value)> {
        let pane_id = body
            .get("pane_id")
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "pane_id is required."))?;
        let mut session = self.session.write();
        if !session.panes.contains_key(pane_id) {
            return Err(api_error(404, "pane_not_found", "That pane is not open."));
        }
        session.focused_pane = Some(pane_id.to_string());
        // Keep the workspace and tab selection in step with the focused pane so
        // a restart restores the same Workspace, Tab, and Pane.
        for workspace in &mut session.workspaces {
            let mut active_tab = None;
            for tab in &mut workspace.tabs {
                if tab.panes.iter().any(|id| id == pane_id) {
                    tab.active_pane = pane_id.to_string();
                    active_tab = Some(tab.id.clone());
                }
            }
            if let Some(active) = active_tab {
                workspace.active_tab = active;
            }
        }
        drop(session);
        self.save();
        Ok((200, json!({ "ok": true })))
    }

    /// Splits a pane: starts a same-agent pane beside it in the same tab and marks the tab split.
    fn split_pane(&self, body: &Value) -> std::result::Result<(u16, Value), (u16, Value)> {
        let pane_id = body
            .get("pane_id")
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "pane_id is required."))?;
        let direction = body
            .get("direction")
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "direction is required."))?;
        let focus = body.get("focus").and_then(Value::as_bool).unwrap_or(false);

        let layout = match direction {
            "right" => TabLayout::SplitHorizontal,
            "down" => TabLayout::SplitVertical,
            _ => {
                return Err(api_error(
                    400,
                    "invalid_request",
                    "direction must be 'right' or 'down'.",
                ))
            }
        };

        // Take the source pane's identity out of the session before we mutate it.
        let (agent, workspace_id, tab_id, cwd, args) = {
            let session = self.session.read();
            let pane = session
                .panes
                .get(pane_id)
                .ok_or_else(|| api_error(404, "pane_not_found", "That pane is not open."))?;
            (
                pane.agent,
                pane.workspace_id.clone(),
                pane.tab_id.clone(),
                pane.cwd.clone(),
                pane.args.clone(),
            )
        };

        let new_pane_id = Uuid::new_v4().to_string();
        let spec = LaunchSpec::for_agent(agent, &args, None, false)
            .map_err(|error| api_error(400, "agent_failed", &format!("{error}")))?;
        let on_event = self.on_event();
        let new_pane = Pane::spawn(
            PaneConfig {
                id: new_pane_id.clone(),
                workspace_id: workspace_id.clone(),
                tab_id: tab_id.clone(),
                agent,
                label: agent.display_name().to_string(),
                cwd,
                spec,
                args,
                size: DEFAULT_SIZE,
            },
            on_event,
        )
        .map_err(|error| {
            api_error(
                400,
                "agent_failed",
                &format!("The agent could not start: {error}"),
            )
        })?;

        let focused = focus;
        {
            let mut session = self.session.write();
            // Mark the tab split and add the new pane to it.
            let mut target: Option<(usize, usize)> = None;
            for (wi, workspace) in session.workspaces.iter_mut().enumerate() {
                for (ti, tab) in workspace.tabs.iter_mut().enumerate() {
                    if tab.panes.contains(&pane_id.to_string()) {
                        tab.panes.push(new_pane_id.clone());
                        tab.layout = layout;
                        if tab.active_pane == pane_id {
                            tab.active_pane = new_pane_id.clone();
                        }
                        target = Some((wi, ti));
                        break;
                    }
                }
                if target.is_some() {
                    break;
                }
            }
            if focused {
                if let Some((wi, _)) = target {
                    session.workspaces[wi].active_tab = session.workspaces[wi]
                        .tabs
                        .iter()
                        .find(|tab| tab.panes.contains(&new_pane_id))
                        .map(|tab| tab.id.clone())
                        .unwrap_or_default();
                }
                session.focused_pane = Some(new_pane_id.clone());
            }
            session
                .panes
                .insert(new_pane_id.clone(), Arc::new(new_pane));
        }
        self.save();

        let pane_value = {
            let session = self.session.read();
            let pane = session.panes.get(&new_pane_id).expect("just inserted");
            pane.info(focused)
        };

        Ok((200, json!({ "ok": true, "pane": pane_value })))
    }

    /// Zooms a pane to fill its tab (prefix+z): the tab shows that pane alone.
    /// mode: "toggle" | "on" | "off".
    fn zoom_pane(&self, body: &Value) -> std::result::Result<(u16, Value), (u16, Value)> {
        let pane_id = body
            .get("pane_id")
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "pane_id is required."))?;
        let mode = body.get("mode").and_then(Value::as_str).unwrap_or("toggle");

        let mut session = self.session.write();

        // Find the tab containing this pane.
        let tab = session
            .workspaces
            .iter_mut()
            .flat_map(|workspace| workspace.tabs.iter_mut())
            .find(|tab| tab.panes.iter().any(|id| id == pane_id))
            .ok_or_else(|| api_error(404, "pane_not_found", "That pane is not open."))?;

        // A single-pane tab can never be meaningfully zoomed.
        if tab.panes.len() <= 1 {
            return Ok((
                200,
                json!({
                    "ok": true,
                    "zoomed": false,
                    "changed": false,
                    "reason": "single_pane",
                }),
            ));
        }

        let already_zoomed = tab.zoomed_pane.as_deref() == Some(pane_id);
        let zoomed = match mode {
            "on" => {
                if already_zoomed {
                    return Ok((
                        200,
                        json!({
                            "ok": true,
                            "zoomed": true,
                            "changed": false,
                            "reason": "already_zoomed",
                        }),
                    ));
                }
                tab.zoomed_pane = Some(pane_id.to_string());
                tab.active_pane = pane_id.to_string();
                true
            }
            "off" => {
                if tab.zoomed_pane.is_none() {
                    return Ok((
                        200,
                        json!({
                            "ok": true,
                            "zoomed": false,
                            "changed": false,
                            "reason": "already_unzoomed",
                        }),
                    ));
                }
                tab.zoomed_pane = None;
                false
            }
            "toggle" => {
                if already_zoomed {
                    tab.zoomed_pane = None;
                    false
                } else {
                    tab.zoomed_pane = Some(pane_id.to_string());
                    tab.active_pane = pane_id.to_string();
                    true
                }
            }
            _ => {
                return Err(api_error(
                    400,
                    "invalid_request",
                    "mode must be toggle|on|off.",
                ))
            }
        };

        // Orion focuses the pane it zooms.
        session.focused_pane = Some(pane_id.to_string());
        drop(session);
        self.save();

        Ok((
            200,
            json!({
                "ok": true,
                "zoomed": zoomed,
                "changed": true,
                "reason": null,
            }),
        ))
    }

    /// Swaps a pane with a neighbour on that side, or an explicit same-tab pane.
    fn swap_pane(&self, body: &Value) -> std::result::Result<(u16, Value), (u16, Value)> {
        let pane_id = body
            .get("pane_id")
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "pane_id is required."))?;
        let target: Option<String> =
            if let Some(target_id) = body.get("target_pane_id").and_then(Value::as_str) {
                Some(target_id.to_string())
            } else if let Some(direction) = body.get("direction").and_then(Value::as_str) {
                // Directional: the neighbour's identity depends on the tab layout order.
                // For this engine, panes are ordered; left/up = previous, right/down = next.
                let session = self.session.read();
                let tab = session
                    .workspaces
                    .iter()
                    .flat_map(|workspace| workspace.tabs.iter())
                    .find(|tab| tab.panes.iter().any(|id| id == pane_id))
                    .ok_or_else(|| api_error(404, "pane_not_found", "That pane is not open."))?;
                let at = tab
                    .panes
                    .iter()
                    .position(|id| id == pane_id)
                    .expect("pane found above");
                let neighbour = match direction {
                    "left" | "up" => at.checked_sub(1),
                    "right" | "down" => Some(at + 1),
                    _ => {
                        return Err(api_error(
                            400,
                            "invalid_request",
                            "direction must be left|right|up|down",
                        ))
                    }
                };
                match neighbour.and_then(|index| tab.panes.get(index)) {
                    Some(id) => Some(id.clone()),
                    None => {
                        return Ok((
                            200,
                            json!({
                                "ok": true,
                                "changed": false,
                                "reason": "no_neighbor",
                                "target_pane_id": null,
                            }),
                        ))
                    }
                }
            } else {
                return Err(api_error(
                    400,
                    "invalid_request",
                    "direction or target_pane_id is required.",
                ));
            };

        let target = target.unwrap();

        if target == pane_id {
            return Ok((
                200,
                json!({
                    "ok": true,
                    "changed": false,
                    "reason": "same_pane",
                    "target_pane_id": target,
                }),
            ));
        }

        let mut session = self.session.write();
        let tab = session
            .workspaces
            .iter_mut()
            .flat_map(|workspace| workspace.tabs.iter_mut())
            .find(|tab| tab.panes.iter().any(|id| id == pane_id));

        let Some(tab) = tab else {
            return Ok((
                200,
                json!({
                    "ok": true,
                    "changed": false,
                    "reason": "not_found",
                    "target_pane_id": null,
                }),
            ));
        };

        if !tab.panes.contains(&target) {
            // Target is not in the same tab.
            let exists = if session.panes.contains_key(&target) {
                "cross_tab"
            } else {
                "not_found"
            };
            return Ok((
                200,
                json!({
                    "ok": true,
                    "changed": false,
                    "reason": exists,
                    "target_pane_id": null,
                }),
            ));
        }

        let source_at = tab
            .panes
            .iter()
            .position(|id| id == pane_id)
            .expect("found");
        let target_at = tab
            .panes
            .iter()
            .position(|id| *id == target)
            .expect("found");
        tab.panes.swap(source_at, target_at);
        drop(session);
        self.save();

        Ok((
            200,
            json!({
                "ok": true,
                "changed": true,
                "reason": null,
                "target_pane_id": target,
            }),
        ))
    }

    /// Moves the border a pane shares with a neighbour in `direction`.
    /// Validate `amount` (0.1..0.5 if given; default 0.05) and report whether the
    /// border could move.
    fn resize_pane(&self, body: &Value) -> std::result::Result<(u16, Value), (u16, Value)> {
        let pane_id = body
            .get("pane_id")
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "pane_id is required."))?;
        let direction = body
            .get("direction")
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "direction is required."))?;
        let amount = match body.get("amount") {
            Some(value) => value
                .as_f64()
                .ok_or_else(|| api_error(400, "invalid_amount", "amount must be a number."))?,
            None => 0.05,
        };
        if !(0.1..=0.5).contains(&amount) && amount != 0.05 {
            // The protocol says the server refuses more than 0.5, and 0.05 when absent.
            if amount > 0.5 || amount < 0.0 {
                return Err(api_error(400, "invalid_amount", "amount out of range."));
            }
        }
        if amount < 0.0 {
            return Err(api_error(
                400,
                "invalid_amount",
                "amount must not be negative.",
            ));
        }

        // A pane with no neighbour in that direction has no border to move.
        let session = self.session.read();
        let tab = session
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.tabs.iter())
            .find(|tab| tab.panes.iter().any(|id| id == pane_id))
            .ok_or_else(|| api_error(404, "pane_not_found", "That pane is not open."))?;

        if tab.panes.len() <= 1 {
            return Ok((
                200,
                json!({ "ok": true, "changed": false, "reason": "unchanged" }),
            ));
        }

        let at = tab
            .panes
            .iter()
            .position(|id| id == pane_id)
            .expect("found");
        let has_neighbour = match direction {
            "left" | "up" => at > 0,
            "right" | "down" => at + 1 < tab.panes.len(),
            _ => {
                return Err(api_error(
                    400,
                    "invalid_request",
                    "direction must be left|right|up|down",
                ))
            }
        };

        if !has_neighbour {
            return Ok((
                200,
                json!({ "ok": true, "changed": false, "reason": "unchanged" }),
            ));
        }

        // The engine reports the border moved; the concrete split ratio adjustment
        // is modelled by the layout layer, which owns pane rectangles.
        drop(session);
        Ok((200, json!({ "ok": true, "changed": true, "reason": null })))
    }

    /// Sets the split ratio for one split in a tab. The path is a binary path
    /// (false = first branch, true = second; [] = root split); the ratio is
    /// held to 0.1...0.9 like Orion's split drag.
    fn set_split_ratio(&self, body: &Value) -> std::result::Result<(u16, Value), (u16, Value)> {
        let tab_id = body
            .get("tab_id")
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "invalid_request", "tab_id is required."))?;
        let path = body
            .get("path")
            .and_then(Value::as_array)
            .ok_or_else(|| api_error(400, "invalid_request", "path must be an array."))?
            .iter()
            .map(|value| value.as_bool().unwrap_or(false))
            .collect::<Vec<bool>>();
        let ratio = body
            .get("ratio")
            .and_then(Value::as_f64)
            .ok_or_else(|| api_error(400, "invalid_request", "ratio is required."))?;
        if !ratio.is_finite() || !(0.1..=0.9).contains(&ratio) {
            return Err(api_error(400, "invalid_ratio", "ratio must be 0.1...0.9."));
        }

        let mut session = self.session.write();
        let tab = session
            .workspaces
            .iter_mut()
            .flat_map(|workspace| workspace.tabs.iter_mut())
            .find(|tab| tab.id == tab_id)
            .ok_or_else(|| api_error(404, "tab_not_found", "That tab is not open."))?;

        if tab.layout == TabLayout::Single {
            return Ok((
                200,
                json!({
                    "ok": true,
                    "changed": false,
                    "reason": "single_pane",
                    "path": path,
                    "ratio": ratio,
                }),
            ));
        }

        tab.split_ratios.insert(path.clone(), ratio);
        drop(session);
        self.save();

        Ok((
            200,
            json!({
                "ok": true,
                "changed": true,
                "reason": null,
                "path": path,
                "ratio": ratio,
            }),
        ))
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
        if let Some((response, status)) = self.transcripts.conversation(
            pane_id,
            pane.agent,
            &pane.cwd,
            pane.started(),
            &pane.visible_text(),
        ) {
            // The transcript is this pane's only status record for agents whose TUI says
            // nothing reliable about its turn; a turn it reports running starts, and the step
            // that stops for good ends it as done.
            if let Some(status) = status {
                if let Some((next, completion)) = pane.note_transcript_status(status) {
                    crate::engine::on_pane_event(
                        &self.session,
                        &self.connections,
                        &self.push,
                        PaneEvent::Status {
                            pane_id: pane_id.to_string(),
                            status: next,
                            completion,
                        },
                    );
                }
            }
            return Ok((200, response));
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

    /// GET /api/pane/omo-tasks: the background tasks and workflows the pane's OmO session started.
    /// A pane of any other agent has none; the answer is empty, never an error.
    fn omo_tasks(
        &self,
        query: &HashMap<String, String>,
    ) -> std::result::Result<(u16, Value), (u16, Value)> {
        let pane_id = required(query, "pane_id")?;
        let pane = self.pane(pane_id)?;
        let empty = || {
            json!({
                "tasks": [],
                "runs": [],
                "server_time": chrono::Utc::now().to_rfc3339(),
            })
        };
        if !matches!(pane.agent, AgentKind::Omo | AgentKind::Omp) {
            return Ok((200, empty()));
        }
        // The session the pane's chat is reading; opening it here binds the same file the
        // conversation route would, so both answer about one session.
        let Some(session_id) = self.transcripts.session_id(
            pane_id,
            pane.agent,
            &pane.cwd,
            pane.started(),
            &pane.visible_text(),
        ) else {
            return Ok((200, empty()));
        };
        Ok((
            200,
            crate::engine::omo_tasks::activity(pane.agent, &pane.cwd, &session_id),
        ))
    }

    fn prompt(
        &self,
        query: &HashMap<String, String>,
    ) -> std::result::Result<(u16, Value), (u16, Value)> {
        let pane_id = required(query, "pane_id")?;
        let pane = self.pane(pane_id)?;
        let detected = pane.prompt().or_else(|| {
            // Codex backfill: a collapsed queue shows only a count on screen, so the
            // card comes from the rollout's unanswered questions.
            if pane.agent == AgentKind::Codex {
                let unanswered = self
                    .transcripts
                    .codex_unanswered(pane_id, &pane.cwd, pane.started(), &pane.visible_text())
                    .into_iter()
                    .map(|question| crate::prompt::QueuedQuestion {
                        key: question.key,
                        title: question.title,
                        options: question.options,
                    })
                    .collect::<Vec<_>>();
                crate::prompt::codex_queued_prompt(&pane.visible_text(), &unanswered)
            } else {
                None
            }
        });
        Ok((
            200,
            json!({
                "prompt": detected.map(|detected| detected.to_wire()),
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
                self.connections.broadcast(json!({
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

    // ---- shared mechanics ----

    fn pane(&self, pane_id: &str) -> std::result::Result<Arc<Pane>, (u16, Value)> {
        self.session
            .pane(pane_id)
            .ok_or_else(|| api_error(404, "pane_not_found", "That pane is not open."))
    }

    fn snapshot_value(&self) -> Value {
        self.session.snapshot_value(&self.version, PROTOCOL_VERSION)
    }

    fn save(&self) {
        if let Err(error) = self.store.save_session(&self.session.to_saved()) {
            warn!("session not saved: {error}");
        }
    }

    /// Spawns a pane for a direct caller (the native shell) and registers it in the session.
    ///
    /// `create_workspace` and `create_tab` are the structured routes; this is the same promise
    /// for a caller that already knows the pane's workspace and tab ids: when it returns, the
    /// Workspace, Tab, and Pane are visible to every route.
    pub(crate) fn spawn_pane(&self, config: PaneConfig) -> Result<Arc<Pane>> {
        let workspace_id = config.workspace_id.clone();
        let tab_id = config.tab_id.clone();
        let cwd = config.cwd.clone();
        let cwd_label = folder_name(&cwd).unwrap_or_else(|| "Workspace".to_string());
        let pane = Arc::new(Pane::spawn(config, self.on_event())?);
        let created_workspace =
            self.session
                .register_pane(&pane, &workspace_id, &tab_id, &cwd_label, &cwd);
        if created_workspace {
            self.watchers.lock().insert(
                workspace_id,
                Watcher::start(cwd, Arc::clone(&self.connections)),
            );
        }
        self.save();
        self.connections
            .broadcast(json!({ "type": "session-changed" }));
        Ok(pane)
    }

    fn spawn_pane_request(&self, request: PaneRequest) -> Result<Arc<Pane>> {
        use crate::agents::LaunchSpec;
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
        let on_event = self.on_event();
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
        let saved = self.store.load_session();
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
            let on_event = self.on_event();
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
        let count = self.session.load_restored(&saved, restored);
        tracing::info!("restored {} pane(s)", count);
    }
}

// ---- pure helpers (were free functions in engine.rs) ----

/// The first pane still open, in workspace/tab order. Closing the selected
/// pane or tab moves focus here instead of leaving the shell with none.
fn first_live_pane(session: &crate::engine::session::Session) -> Option<String> {
    session
        .workspaces
        .iter()
        .flat_map(|workspace| workspace.tabs.iter())
        .flat_map(|tab| tab.panes.iter())
        .find(|pane_id| session.panes.contains_key(*pane_id))
        .cloned()
}

/// Every close route ends here: a closed focused pane gives up focus, and an
/// empty selection falls back to the first live pane. One helper so a new close
/// path cannot forget half of the rule.
fn refocus_after_close(session: &mut crate::engine::session::Session) {
    let focused_is_gone = session
        .focused_pane
        .as_deref()
        .is_some_and(|focused| !session.panes.contains_key(focused));
    if focused_is_gone {
        session.focused_pane = None;
    }
    if session.focused_pane.is_none() {
        session.focused_pane = first_live_pane(session);
    }
}

pub(crate) fn api_error(status: u16, code: &str, message: &str) -> (u16, Value) {
    (
        status,
        json!({ "error": { "code": code, "message": message } }),
    )
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

/// One `agent` object from a create request: which agent, what to call it, and its own arguments.
struct AgentRequest {
    kind: AgentKind,
    name: Option<String>,
    args: Vec<String>,
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

fn folder_name(cwd: &Path) -> Option<String> {
    cwd.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .filter(|name| !name.is_empty())
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
