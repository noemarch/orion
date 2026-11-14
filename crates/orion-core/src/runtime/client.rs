//! The client half of the runtime socket protocol.
//!
//! A `RuntimeClient` speaks to a daemon over one connection that carries both
//! request/response frames and server-pushed event frames. The transport
//! (named pipe, unix socket) is someone else's job; this module only needs a
//! [`FrameConnection`] to read and write frames.
//!
//! # Verbs this client sends
//!
//! The daemon agent implements exactly these. Each request is
//! `{"id": "<uuid>", "type": "<verb>", ...}` and the answer carries the same
//! `id`.
//!
//! | verb | request shape | answer shape |
//! |---|---|---|
//! | `health` | `{"type":"health"}` (no id; the handshake) | identity frame `{"ok":true,"version":..,"protocol":u32}` |
//! | `http` | `{"type":"http","method":..,"path":..,"body":..}` | `{"id":..,"status":u16,"body":Value}` |
//! | `pane-open` | `{"type":"pane-open","pane_id":..}` | `{"id":..,"ok":bool,"cwd":..}` |
//! | `input` | `{"type":"input","pane_id":..,"text":..}` | (none; fire and forget) |
//! | `keys` | `{"type":"keys","pane_id":..,"keys":[..]}` | (none) |
//! | `resize` | `{"type":"resize","pane_id":..,"cols":u16,"rows":u16}` | (none) |
//! | `attach` | `{"type":"attach","pane_id":..,"cols":..,"rows":..,"keep_size":bool}` | (none) |
//! | `detach` | `{"type":"detach","pane_id":..}` | (none) |
//! | `role` | `{"type":"role","mode":"interact"\|"observe"}` | `{"type":"role-ack","mode":..}` |
//! | `snapshot` | `{"type":"snapshot"}` | `{"type":"snapshot","features":..,"snapshot":..}` |
//! | `restore` | `{"type":"restore"}` | (none) |
//! | `status` | `{"type":"status","pane_id":..}` | `{"id":..,"agent_status":..}` |
//! | `prompt` | `{"type":"prompt","pane_id":..}` | `{"id":..,"prompt":Value\|null}` |
//! | `clear` | `{"type":"clear","pane_id":..}` | (none) |
//!
//! Pushed frames (no `id`) are fanned to registered sinks: `pty-data`,
//! `pane-geometry`, `pane-status`, `pty-exit`, `session-changed`, etc.

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;

use parking_lot::Mutex;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::engine::pane::Role;
use crate::error::Result;
use crate::prompt::DetectedPrompt;
use crate::protocol::AgentStatus;
use crate::runtime::framing;
use crate::runtime::traits::{PaneHandle, Runtime};

/// A frame the daemon pushed, with no request waiting for it.
pub type FrameSink = std::sync::Arc<dyn Fn(Value) + Send + Sync>;

/// The connection the client speaks over. The transport (named pipe, unix
/// socket) is someone else's job; the client only needs to read and write
/// frames.
///
/// Implementations must be cloneable through [`FrameConnection::clone_box`],
/// sharing the underlying transport (typically via an `Arc`) so that a reader
/// and a writer can operate concurrently.
pub trait FrameConnection: Send {
    /// Reads one frame, or `Ok(None)` when the peer closed.
    fn read_frame(&mut self) -> io::Result<Option<Value>>;
    /// Writes one frame.
    fn write_frame(&mut self, frame: &Value) -> io::Result<()>;
    /// Returns a clone sharing the underlying transport.
    fn clone_box(&self) -> Box<dyn FrameConnection>;
}

impl Clone for Box<dyn FrameConnection> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

struct RuntimeClientInner {
    writer: Mutex<Box<dyn FrameConnection>>,
    in_flight: Mutex<HashMap<String, mpsc::Sender<Value>>>,
    on_frame_sinks: Mutex<HashMap<u64, FrameSink>>,
    next_sink: AtomicU64,
    connections: Mutex<HashMap<String, FrameSink>>,
    identity: Value,
    alive: AtomicBool,
    /// Short-TTL cache for GET responses so repeated calls (polls, render)
    /// return instantly without a socket round-trip.
    http_cache: Mutex<HashMap<String, (u16, Value, std::time::Instant)>>,
}

impl RuntimeClientInner {
    fn fan(&self, frame: Value) {
        let on_frame: Vec<FrameSink> = self.on_frame_sinks.lock().values().cloned().collect();
        let conns: Vec<FrameSink> = self.connections.lock().values().cloned().collect();
        for sink in on_frame.into_iter().chain(conns) {
            sink(frame.clone());
        }
    }
}

/// A client of a runtime that lives somewhere else.
#[derive(Clone)]
pub struct RuntimeClient {
    inner: std::sync::Arc<RuntimeClientInner>,
}

impl RuntimeClient {
    /// Starts a client over `connection` and completes the handshake.
    pub fn new(mut connection: Box<dyn FrameConnection>) -> Result<Self, String> {
        // Handshake: send health, read identity, check protocol.
        let health = json!({ "type": "health" });
        connection
            .write_frame(&health)
            .map_err(|error| error.to_string())?;
        let answer = connection
            .read_frame()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "connection closed during handshake".to_string())?;
        let protocol = answer
            .get("protocol")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX) as u32;
        if protocol != framing::PROTOCOL_VERSION {
            return Err(format!(
                "protocol mismatch: client {} vs daemon {}",
                framing::PROTOCOL_VERSION,
                protocol
            ));
        }
        let identity = answer;

        let reader_conn = connection.clone();
        let writer_conn = connection;

        let inner = std::sync::Arc::new(RuntimeClientInner {
            writer: Mutex::new(writer_conn),
            in_flight: Mutex::new(HashMap::new()),
            on_frame_sinks: Mutex::new(HashMap::new()),
            next_sink: AtomicU64::new(0),
            connections: Mutex::new(HashMap::new()),
            identity,
            alive: AtomicBool::new(true),
            http_cache: Mutex::new(HashMap::new()),
        });

        let pump_inner = std::sync::Arc::clone(&inner);
        std::thread::Builder::new()
            .name("orion-runtime-pump".to_string())
            .spawn(move || {
                let mut reader = reader_conn;
                while let Ok(Some(frame)) = reader.read_frame() {
                    if let Some(id) = frame.get("id").and_then(Value::as_str) {
                        let sender = pump_inner.in_flight.lock().remove(id);
                        if let Some(sender) = sender {
                            let _ = sender.send(frame);
                        } else {
                            pump_inner.fan(frame);
                        }
                    } else {
                        pump_inner.fan(frame);
                    }
                }
                pump_inner.alive.store(false, Ordering::SeqCst);
                let mut in_flight = pump_inner.in_flight.lock();
                for (_, sender) in in_flight.drain() {
                    drop(sender);
                }
            })
            .map_err(|error| error.to_string())?;

        Ok(RuntimeClient { inner })
    }

    /// Sends one request and waits for its answer.
    pub fn request(&self, request: Value) -> Result<Value, String> {
        let id = Uuid::new_v4().to_string();
        let mut request = request;
        if let Some(object) = request.as_object_mut() {
            object.insert("id".to_string(), Value::String(id.clone()));
        }
        let (sender, receiver) = mpsc::channel();
        self.inner.in_flight.lock().insert(id.clone(), sender);

        {
            let mut writer = self.inner.writer.lock();
            writer
                .write_frame(&request)
                .map_err(|error| error.to_string())?;
        }

        match receiver.recv_timeout(std::time::Duration::from_secs(30)) {
            Ok(frame) => Ok(frame),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                Err("runtime did not answer in time".to_string())
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err("connection closed".to_string()),
        }
    }

    /// Sends one frame without waiting for an answer.
    pub fn send(&self, frame: Value) {
        let mut writer = self.inner.writer.lock();
        let _ = writer.write_frame(&frame);
    }

    /// Registers a sink for the frames the daemon pushes. The returned
    /// function removes the sink again.
    pub fn on_frame(&self, sink: FrameSink) -> Box<dyn FnOnce() + Send> {
        let key = self.inner.next_sink.fetch_add(1, Ordering::Relaxed);
        self.inner.on_frame_sinks.lock().insert(key, sink);
        let inner = std::sync::Arc::clone(&self.inner);
        Box::new(move || {
            inner.on_frame_sinks.lock().remove(&key);
        })
    }

    fn identity_value(&self) -> Value {
        self.inner.identity.clone()
    }
}

impl Runtime for RuntimeClient {
    fn identity(&self) -> Value {
        self.identity_value()
    }

    fn http(&self, method: &str, path: &str, body: &Value) -> (u16, Value) {
        // Cache GET responses for 100ms so repeated calls (polls, render)
        // return instantly without a socket round-trip.
        let cache_key = format!("{method} {path}");
        if method == "GET" {
            let cache = self.inner.http_cache.lock();
            if let Some((status, body, when)) = cache.get(&cache_key) {
                if when.elapsed() < std::time::Duration::from_millis(1000) {
                    return (*status, body.clone());
                }
            }
            drop(cache);
        }

        let answer = match self.request(json!({
            "type": "http",
            "method": method,
            "path": path,
            "body": body,
        })) {
            Ok(answer) => answer,
            Err(_) => return (0, Value::Null),
        };
        let status = answer.get("status").and_then(Value::as_u64).unwrap_or(0) as u16;
        let body = answer.get("body").cloned().unwrap_or(Value::Null);

        if method == "GET" {
            self.inner
                .http_cache
                .lock()
                .insert(cache_key, (status, body.clone(), std::time::Instant::now()));
        } else {
            // Mutations invalidate the cache.
            self.inner.http_cache.lock().clear();
        }

        (status, body)
    }

    fn pane_by_id(&self, pane_id: &str) -> Option<std::sync::Arc<dyn PaneHandle>> {
        let answer = self
            .request(json!({ "type": "pane-open", "pane_id": pane_id }))
            .ok()?;
        if answer.get("ok").and_then(Value::as_bool) != Some(true) {
            return None;
        }
        let cwd = answer
            .get("cwd")
            .and_then(Value::as_str)
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        Some(std::sync::Arc::new(RemotePane {
            client: std::sync::Arc::new(self.clone()),
            pane_id: pane_id.to_string(),
            cwd,
            attachments: Mutex::new(HashMap::new()),
        }))
    }

    fn connect(&self, sink: FrameSink) -> String {
        let id = format!("conn-{}", Uuid::new_v4());
        self.inner.connections.lock().insert(id.clone(), sink);
        id
    }

    fn send_to(&self, connection: &str, frame: Value) {
        if let Some(sink) = self.inner.connections.lock().get(connection).cloned() {
            sink(frame);
        }
    }

    fn disconnect(&self, connection: &str) {
        self.inner.connections.lock().remove(connection);
    }

    fn dispatch_from(
        &self,
        _connection: Option<&str>,
        request: &Value,
    ) -> Result<Option<Value>, String> {
        match self.request(request.clone()) {
            Ok(answer) => Ok(Some(answer)),
            Err(error) => Err(error),
        }
    }

    fn snapshot_value(&self) -> Value {
        match self.request(json!({ "type": "snapshot" })) {
            Ok(answer) => answer.get("snapshot").cloned().unwrap_or(answer),
            Err(_) => Value::Null,
        }
    }

    fn restore(&self) {
        self.send(json!({ "type": "restore" }));
    }
}

/// A pane that lives in another process, reached through a [`RuntimeClient`].
pub struct RemotePane {
    client: std::sync::Arc<RuntimeClient>,
    pane_id: String,
    cwd: std::path::PathBuf,
    attachments: Mutex<HashMap<String, Box<dyn FnOnce() + Send>>>,
}

impl PaneHandle for RemotePane {
    fn id(&self) -> &str {
        &self.pane_id
    }

    fn cwd(&self) -> &std::path::Path {
        &self.cwd
    }

    fn write(&self, data: &str) -> Result<()> {
        self.client.send(json!({
            "type": "input",
            "pane_id": self.pane_id,
            "text": data,
        }));
        Ok(())
    }

    fn keys(&self, keys: &[String]) -> Result<()> {
        self.client.send(json!({
            "type": "keys",
            "pane_id": self.pane_id,
            "keys": keys,
        }));
        Ok(())
    }

    fn interrupt(&self) -> Result<()> {
        self.client.send(json!({
            "type": "keys",
            "pane_id": self.pane_id,
            "keys": ["ctrl+c"],
        }));
        Ok(())
    }

    fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.client.send(json!({
            "type": "resize",
            "pane_id": self.pane_id,
            "cols": cols,
            "rows": rows,
        }));
        Ok(())
    }

    fn clear(&self) {
        self.client
            .send(json!({ "type": "clear", "pane_id": self.pane_id }));
    }

    fn status(&self) -> AgentStatus {
        match self
            .client
            .request(json!({ "type": "status", "pane_id": self.pane_id }))
        {
            Ok(answer) => answer
                .get("agent_status")
                .and_then(Value::as_str)
                .and_then(AgentStatus::from_wire)
                .unwrap_or(AgentStatus::Idle),
            Err(_) => AgentStatus::Idle,
        }
    }

    fn prompt(&self) -> Option<DetectedPrompt> {
        let answer = self
            .client
            .request(json!({ "type": "prompt", "pane_id": self.pane_id }))
            .ok()?;
        // DetectedPrompt is not (de)serializable; prompts arrive as pushed
        // frames in practice. A round-trip query is not supported yet.
        let _ = answer;
        None
    }

    fn attach(
        &self,
        connection: &str,
        sink: FrameSink,
        role: Role,
        cols: u16,
        rows: u16,
        keep_size: bool,
    ) -> Result<()> {
        let pane_id = self.pane_id.clone();
        let user_sink = sink;
        let filter = std::sync::Arc::new(move |frame: Value| {
            if frame.get("type").and_then(Value::as_str) == Some("pty-data")
                && frame.get("pane_id").and_then(Value::as_str) == Some(pane_id.as_str())
            {
                user_sink(frame);
            }
        });
        let remover = self.client.on_frame(filter);
        self.attachments
            .lock()
            .insert(connection.to_string(), remover);

        self.client.send(json!({
            "type": "role",
            "mode": if role == Role::Observe { "observe" } else { "interact" },
        }));
        self.client.send(json!({
            "type": "attach",
            "pane_id": self.pane_id,
            "cols": cols,
            "rows": rows,
            "keep_size": keep_size,
        }));
        Ok(())
    }

    fn detach(&self, connection: &str) {
        if let Some(remover) = self.attachments.lock().remove(connection) {
            remover();
        }
        self.client
            .send(json!({ "type": "detach", "pane_id": self.pane_id }));
    }

    fn detach_all(&self, connection: &str) {
        self.detach(connection);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::sync::mpsc::channel;
    use std::sync::Arc;

    #[derive(Clone)]
    struct FakeConn {
        out_tx: std::sync::mpsc::Sender<Value>,
        in_rx: Arc<parking_lot::Mutex<std::sync::mpsc::Receiver<Value>>>,
    }

    impl FrameConnection for FakeConn {
        fn read_frame(&mut self) -> io::Result<Option<Value>> {
            let rx = self.in_rx.lock();
            match rx.recv() {
                Ok(value) => Ok(Some(value)),
                Err(_) => Ok(None),
            }
        }
        fn write_frame(&mut self, frame: &Value) -> io::Result<()> {
            self.out_tx
                .send(frame.clone())
                .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
        }
        fn clone_box(&self) -> Box<dyn FrameConnection> {
            Box::new(self.clone())
        }
    }

    fn make_pair() -> (FakeConn, FakeConn) {
        let (a_tx, a_rx) = channel::<Value>();
        let (b_tx, b_rx) = channel::<Value>();
        let client = FakeConn {
            out_tx: a_tx,
            in_rx: Arc::new(parking_lot::Mutex::new(b_rx)),
        };
        let daemon = FakeConn {
            out_tx: b_tx,
            in_rx: Arc::new(parking_lot::Mutex::new(a_rx)),
        };
        (client, daemon)
    }

    #[test]
    fn the_client_speaks_to_a_fake_daemon_over_the_framing() {
        let (client_conn, daemon_conn) = make_pair();
        std::thread::spawn(move || {
            let mut d = daemon_conn;
            // handshake
            let _ = d.read_frame();
            let _ = d.write_frame(&json!({
                "ok": true,
                "version": "0",
                "protocol": framing::PROTOCOL_VERSION,
            }));
            // http
            if let Ok(Some(frame)) = d.read_frame() {
                let id = frame.get("id").cloned().unwrap_or(Value::Null);
                let _ = d.write_frame(&json!({
                    "id": id,
                    "type": "http-result",
                    "status": 200u16,
                    "body": { "ok": true },
                }));
            }
        });

        let client = RuntimeClient::new(Box::new(client_conn)).expect("client");
        let (status, body) = client.http("GET", "/api/health", &Value::Null);
        assert_eq!(status, 200);
        assert_eq!(body, json!({ "ok": true }));
    }

    #[test]
    fn a_daemon_that_speaks_another_protocol_is_refused() {
        let (client_conn, daemon_conn) = make_pair();
        std::thread::spawn(move || {
            let mut d = daemon_conn;
            let _ = d.read_frame();
            let _ = d.write_frame(&json!({
                "ok": true,
                "version": "0",
                "protocol": 999u32,
            }));
        });

        let result = RuntimeClient::new(Box::new(client_conn));
        let error = match result {
            Err(e) => e,
            Ok(_) => panic!("mismatched protocol must be refused"),
        };
        assert!(
            error.contains(&framing::PROTOCOL_VERSION.to_string()),
            "error must name the client version: {error}"
        );
        assert!(
            error.contains("999"),
            "error must name the daemon version: {error}"
        );
    }

    #[test]
    fn a_pushed_frame_reaches_a_registered_sink() {
        let (client_conn, daemon_conn) = make_pair();
        std::thread::spawn(move || {
            let mut d = daemon_conn;
            // handshake
            let _ = d.read_frame();
            let _ = d.write_frame(&json!({
                "ok": true,
                "version": "0",
                "protocol": framing::PROTOCOL_VERSION,
            }));
            // wait for the http request, push a frame first, then answer
            if let Ok(Some(frame)) = d.read_frame() {
                let id = frame.get("id").cloned().unwrap_or(Value::Null);
                let _ =
                    d.write_frame(&json!({ "type": "pty-data", "pane_id": "p1", "data": "hi" }));
                let _ = d.write_frame(&json!({
                    "id": id,
                    "type": "http-result",
                    "status": 200u16,
                    "body": { "ok": true },
                }));
            }
        });

        let client = RuntimeClient::new(Box::new(client_conn)).expect("client");
        let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let seen_sink = Arc::clone(&seen);
        let _remover = client.on_frame(Arc::new(move |frame| {
            seen_sink.lock().push(frame);
        }));

        let (status, _) = client.http("GET", "/api/health", &Value::Null);
        assert_eq!(status, 200);
        // give the pump a moment to fan the pushed frame
        std::thread::sleep(std::time::Duration::from_millis(50));
        let frames = seen.lock();
        assert!(
            frames
                .iter()
                .any(|f| f.get("type").and_then(Value::as_str) == Some("pty-data")),
            "the pushed frame must reach the sink: {frames:?}"
        );
    }

    #[test]
    fn a_closed_connection_fails_the_requests_that_were_waiting() {
        let (client_conn, daemon_conn) = make_pair();
        std::thread::spawn(move || {
            let mut d = daemon_conn;
            let _ = d.read_frame();
            let _ = d.write_frame(&json!({
                "ok": true,
                "version": "0",
                "protocol": framing::PROTOCOL_VERSION,
            }));
            // close by dropping
            drop(d);
        });

        let client = RuntimeClient::new(Box::new(client_conn)).expect("client");
        // wait for pump to observe close
        std::thread::sleep(std::time::Duration::from_millis(50));
        let result = client
            .request(json!({ "type": "http", "method": "GET", "path": "/", "body": Value::Null }));
        assert!(result.is_err(), "a request after close must fail");
    }

    #[test]
    fn the_client_answers_the_routes_the_engine_answers() {
        let (client_conn, daemon_conn) = make_pair();
        std::thread::spawn(move || {
            let mut d = daemon_conn;
            let _ = d.read_frame();
            let _ = d.write_frame(&json!({
                "ok": true,
                "version": "0",
                "protocol": framing::PROTOCOL_VERSION,
            }));
            if let Ok(Some(frame)) = d.read_frame() {
                let id = frame.get("id").cloned().unwrap_or(Value::Null);
                let _ = d.write_frame(&json!({
                    "id": id,
                    "type": "http-result",
                    "status": 201u16,
                    "body": { "created": true },
                }));
            }
        });

        let client = RuntimeClient::new(Box::new(client_conn)).expect("client");
        let runtime: &dyn Runtime = &client;
        let (status, body) = runtime.http("POST", "/api/x", &Value::Null);
        assert_eq!(status, 201);
        assert_eq!(body, json!({ "created": true }));
    }
}
