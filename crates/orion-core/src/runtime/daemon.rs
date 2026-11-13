//! The runtime daemon: the process that owns the terminals.
//!
//! The daemon listens on the runtime socket and answers the shell's and the
//! CLI's questions with the same engine the app uses. Closing the app closes
//! the window, not the terminals: they belong here.
//!
//! # Verb table
//!
//! Every request is `{"id": "<uuid>", "type": "<verb>", ...}` and the answer
//! carries the same `id`. A request with no `id` (the `health` handshake) is
//! answered without one.
//!
//! | verb | request shape | answer shape |
//! |---|---|---|
//! | `health` | `{"type":"health"}` | identity frame `{"ok":true,"version":..,"protocol":u32}` |
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
//! | `terminal-spawn` | `{"program":..,"args":[..],"cwd":..,"name":..,"parent_id":..,"outlive_parent":..}` | `{"id":..,"ok":bool,"pane_id":<id>,"terminal_id":<id>,"error":..}` |
//! | `terminal-list` | `{"type":"terminal-list"}` | `{"id":..,"terminals":[TerminalInfo...]}` |
//! | `terminal-send` | `{"type":"terminal-send","pane_id":..,"text":..}` | `{"id":..,"ok":true}` |
//! | `terminal-read` | `{"type":"terminal-read","pane_id":..,"lines":..}` | `{"id":..,"text":..}` |
//! | `terminal-kill` | `{"type":"terminal-kill","pane_id":..}` | `{"id":..,"ok":true}` |
//! | `terminal-report` | `{"type":"terminal-report","pane_id":..,"status":"working\|done\|blocked\|error"}` | `{"id":..,"ok":true}` |
//! | `terminal-wait` | `{"type":"terminal-wait","pane_id":..,"timeout_ms":..}` | `{"id":..,"done":bool,"status":..,"exited":..,"address":..}` |
//!
//! Pushed frames (no `id`) fanned to every connection: `pty-data`, `pane-geometry`,
//! `pane-status`, `pty-exit`, `session-changed`, and the registry events
//! `terminal-created`, `terminal-updated`, `terminal-status-changed`,
//! `terminal-exited`, `terminal-removed`.

use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tracing::{debug, warn};

use crate::agents::LaunchSpec;
use crate::engine::store::Store;
use crate::engine::Engine;
use crate::error::Result;
use crate::protocol::{ReportedStatus, TerminalSize};
use crate::runtime::client::FrameConnection;
use crate::runtime::client::FrameSink;
use crate::runtime::lock::RuntimeLock;
use crate::runtime::persist::RuntimeStore;
use crate::runtime::registry::{Registry, RegistryEvent};

use crate::runtime::transport::{socket_path, RuntimeListener, RuntimeStream};

/// The runtime daemon: the process that owns the terminals.
pub struct RuntimeDaemon {
    session: String,
    _lock: RuntimeLock,
    _listener: Arc<RuntimeListener>,
    /// The engine and the registry the accept task works with. They are held
    /// here as well so that the daemon owns the terminals for as long as it
    /// lives, and not only while a connection is being accepted.
    _engine: Engine,
    _registry: Arc<Registry>,
    _conn_ids: Arc<Mutex<Vec<String>>>,
    _registry_watcher: Box<dyn FnOnce() + Send>,
    accept_task: tokio::task::JoinHandle<()>,
}

impl RuntimeDaemon {
    /// Starts the daemon for `session` and starts serving.
    ///
    /// Returns once the socket is bound; the serving continues in the
    /// background.
    pub async fn start(session: &str) -> Result<Self> {
        Self::start_with_store(session, Store::default()).await
    }

    /// Starts the daemon for `session` over a store of the caller's choosing.
    ///
    /// The store decides what the daemon brings back when it starts. A test
    /// passes a store of its own so the daemon does not spend its time
    /// resurrecting somebody else's session.
    pub async fn start_with_store(session: &str, store: Store) -> Result<Self> {
        let lock = RuntimeLock::acquire(session)?;
        let listener = RuntimeListener::bind(session)?;
        let listener = Arc::new(listener);

        let engine = Engine::with_store(store);
        // The saved session comes back before the socket starts answering, so
        // a client that reaches the daemon finds it ready.
        engine.restore();
        // The registry remembers its terminals beside the socket, so a restart can
        // tell the shell what terminals existed before the daemon went away.
        let registry = Arc::new(Registry::with_store(RuntimeStore::in_runtime_dir()));

        let conn_ids: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        // Fan registry events (and terminal output) out to every connection.
        let conn_ids_w = Arc::clone(&conn_ids);
        let engine_w = engine.clone();
        let registry_w = Arc::clone(&registry);
        let watcher_stop = registry.watch(Arc::new(move |event: RegistryEvent| {
            // Subscribe a late reader to a freshly created terminal's output.
            if let RegistryEvent::Created(info) = &event {
                if let Some(term) = registry_w.get(&info.id) {
                    let conn_ids_t = Arc::clone(&conn_ids_w);
                    let engine_t = engine_w.clone();
                    let id = info.id.clone();
                    term.subscribe_output(Arc::new(move |text: String| {
                        let frame = json!({ "type": "pty-data", "pane_id": id, "data": text });
                        let ids: Vec<String> = conn_ids_t.lock().clone();
                        for c in ids {
                            engine_t.send_to(&c, frame.clone());
                        }
                    }));
                }
            }

            let ids: Vec<String> = conn_ids_w.lock().clone();
            let mut frames: Vec<Value> = Vec::new();
            match &event {
                RegistryEvent::Created(info) => {
                    frames.push(json!({ "type": "terminal-created", "terminal": info }));
                }
                RegistryEvent::Updated(info) => {
                    frames.push(json!({ "type": "terminal-updated", "terminal": info }));
                }
                RegistryEvent::StatusChanged { id, status } => {
                    frames.push(json!({
                        "type": "terminal-status-changed",
                        "id": id,
                        "status": status.as_wire(),
                    }));
                }
                RegistryEvent::Exited { id, code } => {
                    frames.push(json!({ "type": "terminal-exited", "id": id, "code": code }));
                    frames.push(json!({ "type": "pty-exit", "pane_id": id, "code": code }));
                }
                RegistryEvent::Removed { id } => {
                    frames.push(json!({ "type": "terminal-removed", "id": id }));
                }
            }
            for frame in frames {
                for c in &ids {
                    engine_w.send_to(c, frame.clone());
                }
            }
        }));

        let accept_listener = Arc::clone(&listener);
        let accept_engine = engine.clone();
        let accept_registry = Arc::clone(&registry);
        let accept_conn_ids = Arc::clone(&conn_ids);
        let accept_session = session.to_string();
        let accept_task = tokio::task::spawn(async move {
            debug!("accept loop running");
            loop {
                debug!("accepting...");
                match accept_listener.accept().await {
                    Ok(stream) => {
                        let engine = accept_engine.clone();
                        let registry = Arc::clone(&accept_registry);
                        let conn_ids = Arc::clone(&accept_conn_ids);
                        let session = accept_session.clone();
                        std::thread::spawn(move || {
                            handle_connection(stream, engine, registry, conn_ids, session);
                        });
                    }
                    Err(error) => {
                        warn!(?error, "accept failed; continuing to serve");
                    }
                }
            }
        });

        tokio::task::yield_now().await;
        std::thread::sleep(std::time::Duration::from_millis(500));
        Ok(Self {
            session: session.to_string(),
            _lock: lock,
            _listener: listener,
            _engine: engine,
            _registry: registry,
            _conn_ids: conn_ids,
            _registry_watcher: watcher_stop,
            accept_task,
        })
    }

    /// The session this daemon serves.
    pub fn session(&self) -> &str {
        &self.session
    }

    /// Stops serving. The terminals keep running.
    pub async fn shutdown(self) {
        self.accept_task.abort();
    }
}

/// Serves the runtime until the process ends.
pub async fn serve(session: &str) -> Result<()> {
    let daemon = RuntimeDaemon::start(session).await?;
    std::future::pending::<()>().await;
    daemon.shutdown().await;
    Ok(())
}

fn handle_connection(
    stream: RuntimeStream,
    engine: Engine,
    registry: Arc<Registry>,
    conn_ids: Arc<Mutex<Vec<String>>>,
    session: String,
) {
    debug!("handle_connection start");
    let (out_tx, out_rx) = std::sync::mpsc::channel::<Value>();
    let sink_tx = out_tx.clone();
    let sink: FrameSink = Arc::new(move |frame: Value| {
        let _ = sink_tx.send(frame);
    });
    let conn_id = engine.connect(sink);
    {
        let mut ids = conn_ids.lock();
        ids.push(conn_id.clone());
    }

    // Guard ensures the connection is forgotten even if the reader thread panics.
    struct ConnGuard {
        engine: Engine,
        conn_ids: Arc<Mutex<Vec<String>>>,
        conn_id: String,
    }
    impl Drop for ConnGuard {
        fn drop(&mut self) {
            self.engine.disconnect(&self.conn_id);
            self.conn_ids.lock().retain(|x| x != &self.conn_id);
        }
    }
    let _guard = ConnGuard {
        engine: engine.clone(),
        conn_ids: Arc::clone(&conn_ids),
        conn_id: conn_id.clone(),
    };

    let mut reader_stream = stream;
    let mut writer_stream = reader_stream.clone_box();

    let answer_tx = out_tx.clone();
    let engine_r = engine.clone();
    let registry_r = Arc::clone(&registry);
    let conn_id_r = conn_id.clone();
    let session_r = session.clone();
    let reader_handle = std::thread::spawn(move || {
        loop {
            match reader_stream.read_frame() {
                Ok(Some(request)) => {
                    let answer = handle_request(
                        &engine_r,
                        &registry_r,
                        Some(&conn_id_r),
                        &request,
                        &session_r,
                    );
                    if let Some(mut ans) = answer {
                        if let Some(id) = request.get("id") {
                            if let Some(obj) = ans.as_object_mut() {
                                if !obj.contains_key("id") {
                                    obj.insert("id".to_string(), id.clone());
                                }
                            }
                        }
                        let _ = answer_tx.send(ans);
                    }
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }
        // Forget the connection so the writer loop exits and frames stop being
        // pushed to it.
        engine_r.disconnect(&conn_id_r);
        conn_ids.lock().retain(|x| x != &conn_id_r);
    });

    for frame in out_rx {
        if writer_stream.write_frame(&frame).is_err() {
            break;
        }
    }

    let _ = reader_handle.join();
    // `_guard` drops here, forgetting the connection.
}

fn handle_request(
    engine: &Engine,
    registry: &Registry,
    connection: Option<&str>,
    request: &Value,
    session: &str,
) -> Option<Value> {
    let kind = request.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "pane-open" => {
            let pane_id = request.get("pane_id").and_then(Value::as_str).unwrap_or("");
            if let Some(info) = registry.info(pane_id) {
                Some(json!({ "ok": true, "cwd": info.cwd.to_string_lossy() }))
            } else {
                Some(json!({ "ok": false, "cwd": Value::Null }))
            }
        }
        "status" => {
            let pane_id = request.get("pane_id").and_then(Value::as_str).unwrap_or("");
            let agent_status = registry
                .get(pane_id)
                .and_then(|t| t.reported_status())
                .map(|s| Value::String(s.as_wire().to_string()))
                .unwrap_or(Value::Null);
            Some(json!({ "agent_status": agent_status }))
        }
        "prompt" => Some(json!({ "prompt": Value::Null })),
        "clear" => None,
        "terminal-spawn" => {
            let program = request.get("program").and_then(Value::as_str).unwrap_or("");
            let args: Vec<String> = request
                .get("args")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let cwd_str = request.get("cwd").and_then(Value::as_str).unwrap_or(".");
            let cwd = PathBuf::from(cwd_str);
            let name = request.get("name").and_then(Value::as_str);
            let parent_id = request.get("parent_id").and_then(Value::as_str);
            let outlive_parent = request
                .get("outlive_parent")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let spec = LaunchSpec {
                program: PathBuf::from(program),
                args,
            };
            let size = TerminalSize::default();

            // Resolve the `orion-rt` binary next to this executable so a program
            // inside the terminal can call back into the runtime without guessing.
            let runtime_cli = std::env::current_exe().ok().and_then(|exe| {
                let dir = exe.parent()?;
                let candidate = if cfg!(windows) {
                    dir.join("orion-rt.exe")
                } else {
                    dir.join("orion-rt")
                };
                if candidate.is_file() {
                    Some(candidate)
                } else {
                    None
                }
            });

            match registry.create(
                &spec,
                &cwd,
                &[],
                size,
                parent_id,
                name,
                outlive_parent,
                Some(session),
                runtime_cli,
            ) {
                Ok(info) => Some(json!({
                    "ok": true,
                    "pane_id": info.id,
                    "terminal_id": info.id,
                    "error": Value::Null,
                })),
                Err(error) => Some(json!({
                    "ok": false,
                    "pane_id": Value::Null,
                    "terminal_id": Value::Null,
                    "error": error.to_string(),
                })),
            }
        }
        "terminal-list" => Some(json!({ "terminals": registry.list() })),
        "terminal-send" => {
            let pane_id = request.get("pane_id").and_then(Value::as_str).unwrap_or("");
            let text = request.get("text").and_then(Value::as_str).unwrap_or("");
            if let Some(term) = registry.get(pane_id) {
                let _ = term.write_input(text);
            }
            Some(json!({ "ok": true }))
        }
        "terminal-read" => {
            let pane_id = request.get("pane_id").and_then(Value::as_str).unwrap_or("");
            let lines = request.get("lines").and_then(Value::as_u64).unwrap_or(100) as usize;
            let text = registry
                .get(pane_id)
                .map(|t| t.read(lines))
                .unwrap_or_default();
            Some(json!({ "text": text }))
        }
        "terminal-kill" => {
            let pane_id = request.get("pane_id").and_then(Value::as_str).unwrap_or("");
            let _ = registry.kill(pane_id);
            Some(json!({ "ok": true }))
        }
        "terminal-report" => {
            let pane_id = request.get("pane_id").and_then(Value::as_str).unwrap_or("");
            let status_str = request.get("status").and_then(Value::as_str).unwrap_or("");
            if let Some(status) = ReportedStatus::from_wire(status_str) {
                let _ = registry.set_status(pane_id, status);
            }
            Some(json!({ "ok": true }))
        }
        "terminal-wait" => {
            let pane_id = request.get("pane_id").and_then(Value::as_str).unwrap_or("");
            let timeout_ms = request
                .get("timeout_ms")
                .and_then(Value::as_u64)
                .unwrap_or(5000);
            let deadline = Instant::now() + Duration::from_millis(timeout_ms);
            let mut done = false;
            let mut status: Option<ReportedStatus> = None;
            let mut exited: Option<i32> = None;
            while Instant::now() < deadline {
                if let Some(term) = registry.get(pane_id) {
                    if let Some(s) = term.reported_status() {
                        status = Some(s);
                        done = true;
                    }
                    if let Some(c) = term.exited() {
                        exited = Some(c);
                        done = true;
                    }
                } else {
                    done = true;
                    break;
                }
                if done {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            if status.is_none() {
                if let Some(info) = registry.info(pane_id) {
                    if info.status.is_some() {
                        status = info.status;
                        done = true;
                    }
                }
            }
            let address = format!("{}:{}", socket_path(session).display(), pane_id);
            Some(json!({
                "done": done,
                "status": status.map(|s| Value::String(s.as_wire().to_string())).unwrap_or(Value::Null),
                "exited": exited,
                "address": address,
            }))
        }
        _ => match engine.dispatch_from(connection, request) {
            Ok(answer) => answer,
            Err(message) => Some(json!({
                "type": "error",
                "code": "bad_request",
                "message": message,
            })),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::runtime::client::RuntimeClient;
    use crate::runtime::framing;
    use crate::runtime::traits::Runtime;
    use crate::runtime::transport::RuntimeStream;

    /// A store of the test's own, so the daemon does not bring back a real session.
    fn scratch_store(tag: &str) -> Store {
        let dir =
            std::env::temp_dir().join(format!("orion-daemon-{}-{}", tag, unique_session(tag)));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        Store::at(dir)
    }

    fn unique_session(tag: &str) -> String {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        format!(
            "{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_daemon_answers_the_health_handshake() {
        let session = unique_session("health");
        let daemon = RuntimeDaemon::start_with_store(&session, scratch_store("start"))
            .await
            .expect("daemon starts");
        let stream = RuntimeStream::connect(&session).await.expect("connect");
        let client = RuntimeClient::new(Box::new(stream)).expect("handshake");
        assert_eq!(
            client.identity()["protocol"],
            serde_json::json!(framing::PROTOCOL_VERSION)
        );
        drop(client);
        daemon.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_daemon_answers_the_routes_the_engine_answers() {
        let session = unique_session("routes");
        let daemon = RuntimeDaemon::start_with_store(&session, scratch_store("start"))
            .await
            .expect("daemon starts");
        let stream = RuntimeStream::connect(&session).await.expect("connect");
        let client = RuntimeClient::new(Box::new(stream)).expect("handshake");
        let (status, body) = client.http("GET", "/api/health", &serde_json::Value::Null);
        assert_eq!(status, 200);
        assert_eq!(body["ok"], true);
        drop(client);
        daemon.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_daemon_serves_a_terminal_it_started() {
        let session = unique_session("term");
        let daemon = RuntimeDaemon::start_with_store(&session, scratch_store("start"))
            .await
            .expect("daemon starts");
        let stream = RuntimeStream::connect(&session).await.expect("connect");
        let client = RuntimeClient::new(Box::new(stream)).expect("handshake");

        let program = if cfg!(windows) { "cmd.exe" } else { "/bin/sh" };
        let args = if cfg!(windows) {
            vec!["/D".to_string(), "/C".to_string(), "echo hi".to_string()]
        } else {
            vec!["-c".to_string(), "echo hi".to_string()]
        };
        let answer = client
            .request(serde_json::json!({
                "type": "terminal-spawn",
                "program": program,
                "args": args,
                "cwd": std::env::temp_dir().to_string_lossy(),
                "name": "t",
                "parent_id": serde_json::Value::Null,
                "outlive_parent": false,
            }))
            .expect("spawn answer");
        assert_eq!(answer["ok"], true);
        let pane_id = answer
            .get("pane_id")
            .and_then(serde_json::Value::as_str)
            .expect("pane id")
            .to_string();

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut text = String::new();
        while Instant::now() < deadline {
            let r = client
                .request(serde_json::json!({
                    "type": "terminal-read",
                    "pane_id": pane_id,
                    "lines": 50,
                }))
                .expect("read answer");
            text = r
                .get("text")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string();
            if text.contains("hi") {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(text.contains("hi"), "text was {text:?}");

        drop(client);
        daemon.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_second_daemon_refuses_the_same_session() {
        let session = unique_session("second");
        let daemon = RuntimeDaemon::start_with_store(&session, scratch_store("start"))
            .await
            .expect("daemon starts");
        let result = RuntimeDaemon::start(&session).await;
        assert!(result.is_err(), "a second daemon must be refused");
        daemon.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_daemon_keeps_serving_after_a_client_leaves() {
        let session = unique_session("keep");
        let daemon = RuntimeDaemon::start_with_store(&session, scratch_store("start"))
            .await
            .expect("daemon starts");
        {
            let stream = RuntimeStream::connect(&session).await.expect("connect");
            let _client = RuntimeClient::new(Box::new(stream)).expect("handshake");
        }
        let stream = RuntimeStream::connect(&session).await.expect("connect");
        let client = RuntimeClient::new(Box::new(stream)).expect("handshake");
        let (status, _) = client.http("GET", "/api/health", &serde_json::Value::Null);
        assert_eq!(status, 200);
        drop(client);
        daemon.shutdown().await;
    }
}
