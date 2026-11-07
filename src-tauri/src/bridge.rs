//! The IPC bridge between the desktop shell and the engine.
//!
//! Two shapes, one engine:
//! - `orion_request` is a single request/answer call for bounded operations (settings, file
//!   reads, session queries) where the client wants a value back.
//! - `orion_connect` registers a channel and returns a connection id; the engine pushes whole
//!   [`serde_json::Value`] frames to it and `orion_send` posts client frames back. This is the
//!   desktop equivalent of the browser's WebSocket, so [`OrionSocket`] runs unchanged on both.
//!
//! The engine owns the connection registry, exactly as it does for the browser gateway. The id
//! exists so a frame can never be delivered to the wrong window: every `orion_send` and
//! `orion_close` names the connection it belongs to.

use std::sync::Arc;

use orion_core::engine::Engine;
use serde_json::{json, Value};
use tauri::ipc::Channel;

/// Opens a push connection and returns its id.
#[tauri::command]
pub fn orion_connect(
    engine: tauri::State<'_, Engine>,
    on_event: Channel<Value>,
) -> Result<String, String> {
    // The snapshot is the first frame, before the connection can receive any broadcast.
    on_event
        .send(engine.snapshot_frame())
        .map_err(|error| error.to_string())?;
    let id = engine.connect(Arc::new(move |frame: Value| {
        // A closed window refuses the send; its connection is dropped on `orion_close`.
        let _ = on_event.send(frame);
    }));
    Ok(id)
}

/// Posts one bounded request/answer round trip. Used by the transport test to move a MiB intact.
#[tauri::command]
pub fn orion_request(engine: tauri::State<'_, Engine>, request: Value) -> Result<Value, String> {
    engine.dispatch(&request)
}

/// Posts one client frame (JSON) to an open connection.
#[tauri::command]
pub fn orion_send(
    engine: tauri::State<'_, Engine>,
    connection: String,
    payload: String,
) -> Result<(), String> {
    let request: Value =
        serde_json::from_str(&payload).map_err(|error| format!("malformed frame: {error}"))?;
    match engine.dispatch_from(Some(&connection), &request) {
        Ok(Some(answer)) => engine.send_to(&connection, answer),
        Ok(None) => {}
        Err(message) => engine.send_to(
            &connection,
            json!({ "type": "error", "code": "bad_request", "message": message }),
        ),
    }
    Ok(())
}

/// Forgets a connection and every pane it was attached to.
#[tauri::command]
pub fn orion_close(engine: tauri::State<'_, Engine>, connection: String) {
    engine.disconnect(&connection);
}
