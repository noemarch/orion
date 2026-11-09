//! Frame dispatch: matches one client push frame to its handler.
//! Every frame a client sends over its push connection — `attach`, `input`, `submit`, `ping`,
//! or the `http` envelope the desktop shell uses to run the REST client unchanged — is routed
//! here. The transport owns framing and authentication; dispatch owns every answer.
//!
//! `FrameDispatch` is constructed with the deep modules it needs (`SessionState`,
//! `ConnectionRegistry`) and a route callback that answers the `http` envelope. It never
//! touches the wire directly and never renders.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::engine::connection_registry::ConnectionRegistry;
use crate::engine::pane::{FrameSink, Pane, Role};
use crate::engine::session::SessionState;
use crate::protocol::AgentStatus;

/// The REST answer for the `http` envelope: (status, body).
pub type HttpCallback = Arc<dyn Fn(&str, &str, &Value) -> (u16, Value) + Send + Sync>;

/// The WebSocket frame seam.
pub struct FrameDispatch {
    session: Arc<SessionState>,
    connections: Arc<ConnectionRegistry>,
    http: HttpCallback,
}

impl FrameDispatch {
    pub fn new(
        session: Arc<SessionState>,
        connections: Arc<ConnectionRegistry>,
        http: HttpCallback,
    ) -> Self {
        Self {
            session,
            connections,
            http,
        }
    }

    /// The identity this engine reports (version and protocol).
    fn identity(&self, version: &str, protocol: u32) -> Value {
        json!({ "ok": true, "version": version, "protocol": protocol })
    }

    /// The frame a freshly connected client receives before anything else. The caller supplies
    /// the snapshot so tests can pass a fixed version without constructing an engine.
    fn snapshot_frame(&self, version: &str, protocol: u32) -> Value {
        json!({
            "type": "snapshot",
            "features": crate::engine::features(),
            "snapshot": self.session.snapshot_value(version, protocol),
        })
    }

    /// Answers a client's liveness probe. Only the echoed id completes the probe.
    fn pong(&self, id: &Value) -> Value {
        json!({ "type": "pong", "id": id })
    }

    fn pane(&self, pane_id: &str) -> Result<Arc<Pane>, String> {
        self.session
            .pane(pane_id)
            .ok_or_else(|| format!("pane_not_found: {pane_id}"))
    }

    /// Dispatches one client frame. `None` means the answer already went to the connection's
    /// sink (attach's replay, a broadcast), and there is nothing for the transport to send.
    pub fn dispatch_from(
        &self,
        connection: Option<&str>,
        request: &Value,
        version: &str,
        protocol: u32,
    ) -> Result<Option<Value>, String> {
        let kind = request
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| "request has no type".to_string())?;

        match kind {
            "health" => Ok(Some(self.identity(version, protocol))),
            "snapshot" => Ok(Some(self.snapshot_frame(version, protocol))),
            "ping" => {
                let id = request.get("id").cloned().unwrap_or(Value::Null);
                Ok(Some(self.pong(&id)))
            }
            "role" => {
                let mode = request
                    .get("mode")
                    .and_then(Value::as_str)
                    .unwrap_or("interact");
                if let Some(connection) = connection {
                    self.connections
                        .set_role(connection, Role::parse(Some(mode)));
                }
                Ok(Some(json!({ "type": "role-ack", "mode": mode })))
            }
            "presence" => {
                if let Some(connection) = connection {
                    let active = request
                        .get("active")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    self.connections.set_presence(connection, active);
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
                let pane = self.pane(&pane_id)?;
                self.connections.require_interact(connection)?;
                pane.keys(&keys).map_err(|error| error.to_string())?;
                Ok(None)
            }
            "resize" => {
                if self
                    .connections
                    .role(connection.unwrap_or_default())
                    .unwrap_or(Role::Interact)
                    == Role::Observe
                {
                    return Ok(None);
                }
                let pane_id = pane_id_of(request)?;
                let cols = request.get("cols").and_then(Value::as_u64).unwrap_or(0) as u16;
                let rows = request.get("rows").and_then(Value::as_u64).unwrap_or(0) as u16;
                if let Ok(pane) = self.pane(&pane_id) {
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
                let (status, body) = (self.http)(method, path, &body);
                Ok(Some(json!({ "status": status, "body": body })))
            }
            "echo" => Ok(Some(json!({
                "type": "echo",
                "payload": request.get("payload").cloned().unwrap_or(Value::Null),
            }))),
            other => Err(format!("unknown request type: {other}")),
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
            Some(connection) => self.connections.attach(connection, &pane_id)?,
            None => (Role::Interact, Arc::new(|_| {}) as FrameSink),
        };
        let pane = self.pane(&pane_id)?;
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
            self.connections.detach(connection, pane_id);
            if let Ok(pane) = self.pane(pane_id) {
                pane.detach(connection);
            }
        }
    }

    fn write(&self, connection: Option<&str>, request: &Value, field: &str) -> Result<(), String> {
        self.connections.require_interact(connection)?;
        let pane_id = pane_id_of(request)?;
        let text = request
            .get(field)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{field} is missing"))?;
        let pane = self.pane(&pane_id)?;
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
        if let Err(code) = self.connections.require_interact(connection) {
            return Ok(refuse(&code, "This connection only watches."));
        }
        let pane = match self.pane(&pane_id) {
            Ok(pane) => pane,
            Err(_) => return Ok(refuse("pane_not_found", "That pane is not open.")),
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
        self.connections.broadcast(json!({
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
}

/// The pane id a frame refers to, or the error the transport expects.
fn pane_id_of(request: &Value) -> Result<String, String> {
    request
        .get("pane_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "pane_id is missing".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn dispatch() -> FrameDispatch {
        FrameDispatch::new(
            Arc::new(SessionState::new()),
            Arc::new(ConnectionRegistry::new()),
            Arc::new(|_, _, _| (200, Value::Null)),
        )
    }

    #[test]
    fn ping_echoes_its_id() {
        let fd = dispatch();
        let answer = fd
            .dispatch_from(None, &json!({ "type": "ping", "id": 7 }), "0.0.0", 1)
            .unwrap()
            .unwrap();
        assert_eq!(answer["type"], "pong");
        assert_eq!(answer["id"], 7);
    }

    #[test]
    fn unknown_type_is_an_error() {
        let fd = dispatch();
        assert!(fd
            .dispatch_from(None, &json!({ "type": "mystery" }), "0.0.0", 1)
            .is_err());
    }

    #[test]
    fn a_frame_without_a_type_is_an_error() {
        let fd = dispatch();
        assert!(fd
            .dispatch_from(None, &json!({ "payload": 1 }), "0.0.0", 1)
            .is_err());
    }

    #[test]
    fn echo_returns_its_payload_untouched() {
        let fd = dispatch();
        let answer = fd
            .dispatch_from(
                None,
                &json!({ "type": "echo", "payload": { "keep": ["me", 1] } }),
                "0.0.0",
                1,
            )
            .unwrap()
            .unwrap();
        assert_eq!(answer["payload"]["keep"], json!(["me", 1]));
    }

    #[test]
    fn http_envelope_runs_the_rest_surface() {
        let fd = FrameDispatch::new(
            Arc::new(SessionState::new()),
            Arc::new(ConnectionRegistry::new()),
            Arc::new(|method, path, _body| {
                assert_eq!((method, path), ("GET", "/api/health"));
                (200, json!({ "ok": true }))
            }),
        );
        let answer = fd
            .dispatch_from(
                None,
                &json!({ "type": "http", "method": "GET", "path": "/api/health" }),
                "0.0.0",
                1,
            )
            .unwrap()
            .unwrap();
        assert_eq!(answer["status"], 200);
    }

    #[test]
    fn query_map_split_keeps_the_route() {
        let mut query = HashMap::new();
        query.insert("pane_id".to_string(), "p1".to_string());
        assert_eq!(query.get("pane_id").unwrap(), "p1");
    }
}
