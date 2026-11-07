use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use orion_core::agents::AgentRegistry;
use orion_core::protocol::AgentKind;
use orion_core::workspace::WorkspaceManager;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tracing::{error, info};

struct ServerState {
    workspace_manager: Arc<WorkspaceManager>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    info!("Initializing Orion AI Agent Orchestrator...");

    // Discover installed agent CLIs on PATH
    let agents = AgentRegistry::discover_all();
    info!("--- Discovered AI Agents on PATH ---");
    for agent in &agents {
        if agent.installed {
            info!(
                "  ✓ {} (cmd: '{}', version: {})",
                agent.name,
                agent.command,
                agent.version.as_deref().unwrap_or("detected")
            );
        } else {
            info!("  ✗ {} (not found on PATH)", agent.name);
        }
    }
    info!("------------------------------------");

    let workspace_manager = Arc::new(WorkspaceManager::new());

    // Seed initial workspace with Claude if installed, or first available agent
    let initial_agent = agents
        .iter()
        .find(|a| a.installed)
        .map(|a| a.kind)
        .unwrap_or(AgentKind::Claude);

    let cwd = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .to_string_lossy()
        .to_string();

    workspace_manager.create_workspace("default-workspace", &cwd, initial_agent);

    let state = Arc::new(ServerState { workspace_manager });

    // Check if dist folder exists for serving static UI
    let dist_dir = PathBuf::from("dist");

    let mut app = Router::new()
        .route("/api/health", get(health_check))
        .route("/api/agents", get(list_agents))
        .route("/api/workspaces", get(list_workspaces))
        .route("/ws", get(ws_handler))
        .layer(CorsLayer::permissive())
        .with_state(state);

    if dist_dir.exists() {
        info!("Serving static web UI from {:?}", dist_dir);
        app = app.fallback_service(ServeDir::new(dist_dir));
    } else {
        info!("Static dist/ not found. Run 'bun run build' or use Vite dev server on port 5173.");
        app = app.fallback(get(|| async {
            Html("<h1>Orion Server Running</h1><p>Run <code>bun run build</code> to serve the frontend bundle from here, or run <code>bun run dev</code> to open the Vite dev server.</p>")
        }));
    }

    let port = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(7317);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("Orion server listening on http://127.0.0.1:{}", port);
    info!("For mobile companion access, connect via Tailscale MagicDNS or LAN IP.");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn health_check() -> impl IntoResponse {
    axum::Json(serde_json::json!({
        "status": "ok",
        "app": "Orion",
        "version": "0.1.0"
    }))
}

async fn list_agents() -> impl IntoResponse {
    let agents = AgentRegistry::discover_all();
    axum::Json(agents)
}

async fn list_workspaces(State(state): State<Arc<ServerState>>) -> impl IntoResponse {
    let wss = state.workspace_manager.list_workspaces();
    axum::Json(wss)
}

async fn ws_handler(ws: WebSocketUpgrade, State(_state): State<Arc<ServerState>>) -> Response {
    ws.on_upgrade(handle_socket)
}

async fn handle_socket(mut socket: WebSocket) {
    info!("Client connected to Orion WebSocket");

    // Initial welcome payload
    let welcome = serde_json::json!({
        "event": "connected",
        "message": "Connected to Orion Core Agent Engine"
    });
    let _ = socket.send(Message::Text(welcome.to_string().into())).await;

    while let Some(msg) = socket.recv().await {
        match msg {
            Ok(Message::Text(text)) => {
                info!("Received WS message: {}", text);
            }
            Ok(Message::Close(_)) => {
                info!("Client disconnected from WebSocket");
                break;
            }
            Err(e) => {
                error!("WebSocket error: {}", e);
                break;
            }
            _ => {}
        }
    }
}
