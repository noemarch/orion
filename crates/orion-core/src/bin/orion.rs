use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

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
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tracing::info;

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

struct ConnectionGuard {
    engine: Engine,
    id: String,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.engine.disconnect(&self.id);
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args = parse_args()?;
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

fn parse_args() -> Result<ServeArgs, Box<dyn std::error::Error>> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("serve") {
        args.remove(0);
    }
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        eprintln!(
            "Usage: orion serve [--host 127.0.0.1] [--port 0] [--state-dir DIR] [--dist dist]"
        );
        std::process::exit(0);
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

async fn events_handler(State(state): State<AppState>) -> impl IntoResponse {
    let (tx, rx) = mpsc::unbounded_channel::<Value>();
    let id = state.engine.connect(Arc::new(move |frame: Value| {
        let _ = tx.send(frame);
    }));
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

    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    let connection = state.engine.connect(Arc::new(move |frame: Value| {
        let _ = tx.send(frame);
    }));
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
