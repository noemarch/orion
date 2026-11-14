use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Json, State};
use axum::http::{Method, StatusCode, Uri};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::{any, get};
use axum::Router;
use futures::{SinkExt, StreamExt};
use orion_core::engine::auth;
use orion_core::engine::store::Store;
use orion_core::engine::Engine;
use orion_core::runtime::{current_session, serve};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tracing::{info, warn};

#[derive(Clone)]
struct AppState {
    engine: Engine,
    token: String,
}

struct ServeArgs {
    host: String,
    port: u16,
    state_dir: Option<PathBuf>,
    dist: PathBuf,
}

/// The runtime daemon's arguments.
struct DaemonArgs {
    state_dir: Option<PathBuf>,
}

/// What this process was asked to run.
enum Command {
    Serve(ServeArgs),
    Daemon(DaemonArgs),
}

struct ConnectionGuard {
    engine: Engine,
    id: String,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.engine.disconnect(&self.id);
    }
}

/// Outbound frames per connection are bounded so a client that stops reading
/// cannot grow server memory without limit. 1024 frames at roughly 4 KiB each
/// is about 4 MiB per connection: enough to absorb a burst of PTY output,
/// small enough to bound a stalled client. A client whose queue stays full
/// for `OUTBOUND_SEND_TIMEOUT` is disconnected as a sustained overflow.
const OUTBOUND_CAPACITY: usize = 1024;

/// How long a push may wait for room in a connection's outbound queue before
/// the connection is declared a sustained overflow and disconnected.
const OUTBOUND_SEND_TIMEOUT: Duration = Duration::from_secs(5);

/// The time source for a push's overflow window.
///
/// Production reads the wall clock and sleeps between attempts. Tests drive a
/// scripted clock, so the threshold is exact and no test sleeps.
trait OverflowClock {
    /// How long this push has been waiting for room.
    fn elapsed(&mut self) -> Duration;
    /// Waits between two attempts.
    fn pause(&mut self, wait: Duration);
}

/// The production clock: wall-clock time since the push started.
struct WallClock {
    started: Instant,
}

impl OverflowClock for WallClock {
    fn elapsed(&mut self) -> Duration {
        self.started.elapsed()
    }

    fn pause(&mut self, wait: Duration) {
        std::thread::sleep(wait);
    }
}

/// Pushes one frame into a connection's bounded outbound queue. On timeout
/// the connection is marked dead and the reaper is notified; the frame is
/// dropped because the connection is being disconnected. Once a connection is
/// marked dead its frames are undeliverable and are dropped here; the reaper
/// forgets the connection right after.
///
/// tokio's bounded `Sender` has no stable timed send, so a full queue is
/// retried against a deadline. The retry only runs while the queue is full
/// (a stalled client); a healthy client never reaches it.
fn push_outbound(
    out_tx: &mpsc::Sender<Value>,
    dead: &AtomicBool,
    dead_tx: &std::sync::mpsc::Sender<()>,
    frame: Value,
    timeout: Duration,
) {
    push_outbound_with(
        out_tx,
        dead,
        dead_tx,
        frame,
        timeout,
        &mut WallClock {
            started: Instant::now(),
        },
    );
}

/// [`push_outbound`] over a caller-supplied clock. The clock only decides when
/// the overflow window ends; production passes [`WallClock`], tests pass a
/// counter.
fn push_outbound_with(
    out_tx: &mpsc::Sender<Value>,
    dead: &AtomicBool,
    dead_tx: &std::sync::mpsc::Sender<()>,
    frame: Value,
    timeout: Duration,
    clock: &mut dyn OverflowClock,
) {
    if dead.load(Ordering::SeqCst) {
        return;
    }
    let mut frame = frame;
    loop {
        match out_tx.try_send(frame) {
            Ok(()) => return,
            Err(mpsc::error::TrySendError::Full(returned)) => {
                frame = returned;
                let waited = clock.elapsed();
                if waited >= timeout {
                    dead.store(true, Ordering::SeqCst);
                    let _ = dead_tx.send(());
                    return;
                }
                clock.pause((timeout - waited).min(Duration::from_millis(1)));
            }
            Err(mpsc::error::TrySendError::Closed(_)) => return,
        }
    }
}

/// Builds the per-connection sink. Frames go into a bounded queue; a client
/// that stays full for `timeout` is marked dead and the reaper disconnects it.
/// The sink never calls `engine.disconnect` itself: the connections lock is
/// not reentrant, so disconnecting from inside a sink would deadlock.
fn outbound_sink(
    out_tx: mpsc::Sender<Value>,
    dead: Arc<AtomicBool>,
    dead_tx: std::sync::mpsc::Sender<()>,
    timeout: Duration,
) -> orion_core::FrameSink {
    Arc::new(move |frame: Value| {
        push_outbound(&out_tx, &dead, &dead_tx, frame, timeout);
    })
}

/// Waits for the sink to report a sustained overflow, then forgets the
/// connection. Runs off the connections lock and after the sink returns, so
/// the disconnect cannot deadlock against an in-flight push. Exits silently
/// when every sink clone is dropped (the connection ended normally).
fn spawn_reaper(
    dead_rx: std::sync::mpsc::Receiver<()>,
    engine: Engine,
    conn_id: String,
    timeout: Duration,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        if dead_rx.recv().is_err() {
            return;
        }
        warn!(
            conn_id = %conn_id,
            "disconnecting slow consumer: outbound queue stayed full for {:?}",
            timeout
        );
        engine.disconnect(&conn_id);
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args = match parse_args()? {
        Command::Daemon(args) => {
            // The daemon resolves its store and its socket from this variable.
            if let Some(dir) = args.state_dir {
                std::env::set_var("ORION_DATA_DIR", dir);
            }
            serve(&current_session()).await?;
            return Ok(());
        }
        Command::Serve(args) => args,
    };
    let store = match args.state_dir {
        Some(dir) => Store::at(dir),
        None => Store::default(),
    };
    let engine = Engine::with_store(store);
    engine.restore();
    let token = std::env::var("ORION_TOKEN").unwrap_or_default();
    let state = AppState { engine, token };

    let mut app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/api/machines/events", get(events_handler))
        .route("/api/auth", any(auth_handler))
        .route("/api/{*path}", any(api_handler))
        .layer(CorsLayer::permissive())
        .with_state(state);

    if args.dist.exists() {
        info!("Serving static web UI from {:?}", args.dist);
        app = app.fallback_service(ServeDir::new(args.dist));
    } else {
        info!("Static dist/ not found; API and WebSocket only.");
    }

    let listener = tokio::net::TcpListener::bind((args.host.as_str(), args.port)).await?;
    let local = listener.local_addr()?;
    let ready = json!({
        "event": "ready",
        "host": local.ip().to_string(),
        "port": local.port(),
        "url": format!("http://{}/", format_socket_addr(local)),
    });
    println!("{ready}");
    use std::io::Write;
    std::io::stdout().flush()?;
    info!("Orion server listening on {local}");

    axum::serve(listener, app).await?;
    Ok(())
}

fn format_socket_addr(addr: SocketAddr) -> String {
    if addr.is_ipv6() {
        format!("[{}]:{}", addr.ip(), addr.port())
    } else {
        format!("{}:{}", addr.ip(), addr.port())
    }
}

fn parse_args() -> Result<Command, Box<dyn std::error::Error>> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().cloned();
    if matches!(command.as_deref(), Some("serve" | "daemon")) {
        args.remove(0);
    }
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        if command.as_deref() == Some("daemon") {
            eprintln!("Usage: orion daemon [--state-dir DIR]");
        } else {
            eprintln!(
                "Usage: orion serve [--host 127.0.0.1] [--port 0] [--state-dir DIR] [--dist dist]"
            );
        }
        std::process::exit(0);
    }
    if command.as_deref() == Some("daemon") {
        return Ok(Command::Daemon(parse_daemon_args(&args)?));
    }

    let mut parsed = ServeArgs {
        host: "127.0.0.1".to_string(),
        port: 7317,
        state_dir: None,
        dist: PathBuf::from("dist"),
    };
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--host" => {
                index += 1;
                parsed.host = args.get(index).cloned().ok_or("--host needs a value")?;
            }
            "--port" => {
                index += 1;
                parsed.port = args
                    .get(index)
                    .ok_or("--port needs a value")?
                    .parse()
                    .map_err(|_| "--port must be a number")?;
            }
            "--state-dir" => {
                index += 1;
                parsed.state_dir = Some(PathBuf::from(
                    args.get(index).ok_or("--state-dir needs a value")?,
                ));
            }
            "--dist" => {
                index += 1;
                parsed.dist = PathBuf::from(args.get(index).ok_or("--dist needs a value")?);
            }
            other => {
                eprintln!("Unknown argument: {other}");
                eprintln!("Usage: orion serve [--host 127.0.0.1] [--port 0] [--state-dir DIR] [--dist dist]");
                std::process::exit(2);
            }
        }
        index += 1;
    }
    Ok(Command::Serve(parsed))
}

fn parse_daemon_args(args: &[String]) -> Result<DaemonArgs, Box<dyn std::error::Error>> {
    let mut parsed = DaemonArgs { state_dir: None };
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--state-dir" => {
                index += 1;
                parsed.state_dir = Some(PathBuf::from(
                    args.get(index).ok_or("--state-dir needs a value")?,
                ));
            }
            other => {
                eprintln!("Unknown argument: {other}");
                eprintln!("Usage: orion daemon [--state-dir DIR]");
                std::process::exit(2);
            }
        }
        index += 1;
    }
    Ok(parsed)
}

async fn api_handler(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: axum::http::HeaderMap,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    let path = uri
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or(uri.path());
    if unauthorized(&state, &headers, path) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": { "code": "unauthorized", "message": "token required" } })),
        )
            .into_response();
    }
    let body = body.map(|value| value.0).unwrap_or(Value::Null);
    let (status, body) = state.engine.http(method.as_str(), path, &body);
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    // A raw-text body lets `/api/fs/file` hand the file itself to the viewer instead of JSON.
    if let Some(text) = body.get("__raw_text__").and_then(Value::as_str) {
        return (
            status,
            [("content-type", "text/plain; charset=utf-8")],
            text.to_string(),
        )
            .into_response();
    }
    (status, Json(body)).into_response()
}

async fn auth_handler(
    State(state): State<AppState>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    let provided = body
        .as_ref()
        .and_then(|body| body.get("token"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if state.token.is_empty() || auth::token_matches(&provided, &state.token) {
        (
            StatusCode::NO_CONTENT,
            [("set-cookie", auth::session_cookie(&provided, false))],
        )
            .into_response()
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": { "code": "unauthorized", "message": "token required" } })),
        )
            .into_response()
    }
}

fn unauthorized(state: &AppState, headers: &axum::http::HeaderMap, path: &str) -> bool {
    if state.token.is_empty() {
        return false;
    }
    let route = path.split('?').next().unwrap_or(path);
    if !auth::requires_auth(route) {
        return false;
    }
    let header_map: std::collections::HashMap<String, String> = headers
        .iter()
        .filter_map(|(name, value)| Some((name.to_string(), value.to_str().ok()?.to_string())))
        .collect();
    match auth::presented_token(&header_map) {
        Some(presented) => {
            !auth::token_matches(&presented, &state.token)
                && !state.engine.device_authenticated(&presented)
        }
        None => true,
    }
}

async fn events_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if unauthorized(&state, &headers, "/api/machines/events") {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": { "code": "unauthorized", "message": "token required" } })),
        )
            .into_response();
    }
    let (tx, rx) = mpsc::channel::<Value>(OUTBOUND_CAPACITY);
    let dead = Arc::new(AtomicBool::new(false));
    let (dead_tx, dead_rx) = std::sync::mpsc::channel::<()>();
    let sink = outbound_sink(tx, dead, dead_tx, OUTBOUND_SEND_TIMEOUT);
    let id = state.engine.connect(sink);
    let _reaper = spawn_reaper(
        dead_rx,
        state.engine.clone(),
        id.clone(),
        OUTBOUND_SEND_TIMEOUT,
    );
    let guard = ConnectionGuard {
        engine: state.engine.clone(),
        id,
    };
    let (status, machines) = state.engine.http("GET", "/api/machines", &Value::Null);
    debug_assert_eq!(status, 200);
    let initial = json!({
        "type": "machines",
        "machines": machines.get("machines").cloned().unwrap_or(Value::Array(Vec::new())),
    });

    // The guard stays in the stream state, so dropping the stream drops the connection.
    let stream = futures::stream::unfold(
        (guard, rx, true, Some(initial)),
        |(guard, mut rx, mut first, mut initial)| async move {
            if first {
                first = false;
                if let Some(frame) = initial.take() {
                    return Some((
                        Ok::<Event, Infallible>(Event::default().data(frame.to_string())),
                        (guard, rx, first, initial),
                    ));
                }
            }
            match rx.recv().await {
                Some(frame) => {
                    let event = json!({
                        "type": "machine-message",
                        "machine_id": "local",
                        "message": frame,
                    });
                    Some((
                        Ok::<Event, Infallible>(Event::default().data(event.to_string())),
                        (guard, rx, first, initial),
                    ))
                }
                None => None,
            }
        },
    );

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    if unauthorized(&state, &headers, "/ws") {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": { "code": "unauthorized", "message": "token required" } })),
        )
            .into_response();
    }
    ws.on_upgrade(move |socket| handle_socket(socket, state))
        .into_response()
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let (tx, mut rx) = mpsc::channel::<Value>(OUTBOUND_CAPACITY);
    let dead = Arc::new(AtomicBool::new(false));
    let (dead_tx, dead_rx) = std::sync::mpsc::channel::<()>();
    let sink = outbound_sink(tx, dead, dead_tx, OUTBOUND_SEND_TIMEOUT);
    let connection = state.engine.connect(sink);
    let _reaper = spawn_reaper(
        dead_rx,
        state.engine.clone(),
        connection.clone(),
        OUTBOUND_SEND_TIMEOUT,
    );
    state
        .engine
        .send_to(&connection, state.engine.snapshot_frame());

    let (mut sender, mut receiver) = socket.split();
    loop {
        tokio::select! {
            incoming = receiver.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        let Ok(request) = serde_json::from_str::<Value>(&text) else {
                            continue;
                        };
                        match state.engine.dispatch_from(Some(&connection), &request) {
                            Ok(Some(answer)) => state.engine.send_to(&connection, answer),
                            Ok(None) => {}
                            Err(message) => state.engine.send_to(
                                &connection,
                                json!({ "type": "error", "code": "bad_request", "message": message }),
                            ),
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
            frame = rx.recv() => {
                match frame {
                    Some(frame) => {
                        if sender.send(Message::Text(frame.to_string().into())).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }
        }
    }

    state.engine.disconnect(&connection);
}

fn _unused(_: Infallible) {}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;

    fn scratch_store(tag: &str) -> Store {
        let dir = std::env::temp_dir().join(format!(
            "orion-serve-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        Store::at(dir)
    }

    #[test]
    fn a_stalled_remote_client_is_disconnected_after_the_threshold() {
        let engine = Engine::with_store(scratch_store("slow-remote"));
        let (out_tx, mut out_rx) = mpsc::channel::<Value>(1);
        let dead = Arc::new(AtomicBool::new(false));
        let (dead_tx, dead_rx) = std::sync::mpsc::channel::<()>();
        let timeout = Duration::from_millis(50);
        let sink = outbound_sink(out_tx, Arc::clone(&dead), dead_tx, timeout);
        let conn_id = engine.connect(sink);
        let reaper = spawn_reaper(dead_rx, engine.clone(), conn_id.clone(), timeout);

        // A second connection that keeps up must be unaffected.
        let received = Arc::new(Mutex::new(Vec::new()));
        let sink_received = Arc::clone(&received);
        let other_id = engine.connect(Arc::new(move |frame| {
            sink_received.lock().push(frame);
        }));

        // Fill the queue (capacity 1), then overflow it: the second push
        // blocks for `timeout`, marks the connection dead, and notifies the reaper.
        engine.send_to(&conn_id, json!({ "type": "pty-data", "data": "a" }));
        engine.send_to(&conn_id, json!({ "type": "pty-data", "data": "b" }));
        assert!(
            dead.load(Ordering::SeqCst),
            "the stalled sink must be marked dead"
        );

        // The reaper disconnects the stalled connection.
        reaper.join().expect("the reaper finishes");

        // The engine keeps serving after the stalled client is dropped.
        let (status, _) = engine.http("GET", "/api/health", &Value::Null);
        assert_eq!(
            status, 200,
            "the engine keeps serving after a stalled client is dropped"
        );

        // The other connection is unaffected.
        engine.send_to(&other_id, json!({ "type": "pty-data", "data": "c" }));
        assert_eq!(received.lock().len(), 1);

        // The stalled connection is forgotten: send_to is a no-op and no
        // further frames reach its queue.
        engine.send_to(&conn_id, json!({ "type": "pty-data", "data": "d" }));
        let mut queued = Vec::new();
        while let Ok(frame) = out_rx.try_recv() {
            queued.push(frame);
        }
        assert_eq!(
            queued.len(),
            1,
            "no further frames reach the stalled connection"
        );
        assert_eq!(queued[0]["data"], "a");
    }
}
