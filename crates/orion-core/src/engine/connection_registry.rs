//! Connection registry: owns push connections and their presence.
//!
//! A connection is one client's delivery sink (`FrameSink`), its role (interact vs observe),
//! and the panes it is attached to. Presence tracks whether its page is live: a present
//! connection already sees the UI, so `pane-status` and `session-changed` frames are not
//! duplicated to it.
//!
//! The registry is a deep module: all mutation goes through its interface, and transports
//! never touch the map directly.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::Value;

use crate::engine::pane::{FrameSink, Role};

/// One client connection: where its frames go, what it may do, and what it is attached to.
pub(crate) struct Connection {
    sink: FrameSink,
    role: Role,
    attached: HashSet<String>,
}

/// Owns every push connection and presence state.
pub struct ConnectionRegistry {
    connections: Mutex<HashMap<String, Connection>>,
    next_connection: AtomicU64,
    presence: Mutex<HashMap<String, Instant>>,
}

impl ConnectionRegistry {
    pub fn new() -> Self {
        Self {
            connections: Mutex::new(HashMap::new()),
            next_connection: AtomicU64::new(0),
            presence: Mutex::new(HashMap::new()),
        }
    }

    /// Registers a push connection and returns its id.
    pub fn connect(&self, sink: FrameSink) -> String {
        let id = format!(
            "conn-{}",
            self.next_connection.fetch_add(1, Ordering::Relaxed) + 1
        );
        self.connections.lock().insert(
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
        if let Some(entry) = self.connections.lock().get(connection) {
            (entry.sink)(frame);
        }
    }

    /// Forgets a connection and returns the pane ids it was attached to.
    pub fn disconnect(&self, connection: &str) -> Vec<String> {
        match self.connections.lock().remove(connection) {
            Some(entry) => entry.attached.into_iter().collect(),
            None => Vec::new(),
        }
    }

    /// Whether the connection's page is active right now (within 30 seconds).
    pub fn is_present(&self, connection_id: &str) -> bool {
        let presence = self.presence.lock();
        let Some(when) = presence.get(connection_id) else {
            return false;
        };
        when.elapsed() <= Duration::from_secs(30)
    }

    /// Fans one frame out to every connection, skipping present ones for UI-only kinds.
    pub fn broadcast(&self, frame: Value) {
        let kind = frame.get("type").and_then(Value::as_str).unwrap_or("");
        let suppress = kind == "pane-status" || kind == "session-changed";
        let connections = self.connections.lock();
        for (connection_id, connection) in connections.iter() {
            if suppress && self.is_present(connection_id) {
                continue;
            }
            (connection.sink)(frame.clone());
        }
    }

    /// The role a connection currently has.
    pub fn role(&self, connection: &str) -> Result<Role, String> {
        Ok(self
            .connections
            .lock()
            .get(connection)
            .map(|entry| entry.role)
            .unwrap_or(Role::Interact))
    }

    /// Refuses with `read_only` when the connection only watches.
    pub fn require_interact(&self, connection: Option<&str>) -> Result<(), String> {
        let Some(connection) = connection else {
            return Ok(());
        };
        match self.role(connection)? {
            Role::Interact => Ok(()),
            Role::Observe => Err("read_only".to_string()),
        }
    }

    /// Sets a connection's role (interact vs observe).
    pub fn set_role(&self, connection: &str, role: Role) {
        if let Some(entry) = self.connections.lock().get_mut(connection) {
            entry.role = role;
        }
    }

    /// Marks a pane attached to a connection; returns the role and sink for replay.
    pub fn attach(&self, connection: &str, pane_id: &str) -> Result<(Role, FrameSink), String> {
        let mut connections = self.connections.lock();
        let entry = connections
            .get_mut(connection)
            .ok_or_else(|| format!("no such connection: {connection}"))?;
        entry.attached.insert(pane_id.to_string());
        Ok((entry.role, std::sync::Arc::clone(&entry.sink)))
    }

    /// Detaches a pane from a connection.
    pub fn detach(&self, connection: &str, pane_id: &str) {
        if let Some(entry) = self.connections.lock().get_mut(connection) {
            entry.attached.remove(pane_id);
        }
    }

    /// Records presence for a connection.
    pub fn set_presence(&self, connection: &str, active: bool) {
        let mut presence = self.presence.lock();
        if active {
            presence.insert(connection.to_string(), Instant::now());
        } else {
            presence.remove(connection);
        }
    }
}

impl Default for ConnectionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn connect_returns_unique_ids_and_send_to_delivers() {
        let registry = ConnectionRegistry::new();
        let received = std::sync::Arc::new(Mutex::new(Vec::new()));
        let sink_received = std::sync::Arc::clone(&received);
        let sink: FrameSink = std::sync::Arc::new(move |frame| {
            sink_received.lock().push(frame);
        });
        let id_one = registry.connect(sink);
        let id_two = registry.connect(std::sync::Arc::new(|_| {}));
        assert_ne!(id_one, id_two);
        registry.send_to(&id_one, serde_json::json!({ "type": "ping" }));
        assert_eq!(received.lock().len(), 1);
    }

    #[test]
    fn broadcast_suppresses_present_connections_for_status_frames() {
        let registry = ConnectionRegistry::new();
        let count = std::sync::Arc::new(AtomicUsize::new(0));
        let sink_count = std::sync::Arc::clone(&count);
        let sink: FrameSink = std::sync::Arc::new(move |_| {
            sink_count.fetch_add(1, Ordering::SeqCst);
        });
        let id = registry.connect(sink);
        registry.set_presence(&id, true);
        registry.broadcast(serde_json::json!({ "type": "pane-status" }));
        assert_eq!(count.load(Ordering::SeqCst), 0);
        registry.broadcast(serde_json::json!({ "type": "pty-data" }));
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn role_gates_writes_for_observe_connections() {
        let registry = ConnectionRegistry::new();
        let id = registry.connect(std::sync::Arc::new(|_| {}));
        registry.set_role(&id, Role::Observe);
        assert_eq!(
            registry.require_interact(Some(&id)).unwrap_err(),
            "read_only"
        );
        assert!(registry.require_interact(None).is_ok());
        registry.set_role(&id, Role::Interact);
        assert!(registry.require_interact(Some(&id)).is_ok());
    }

    #[test]
    fn attach_and_detach_track_pane_ownership() {
        let registry = ConnectionRegistry::new();
        let id = registry.connect(std::sync::Arc::new(|_| {}));
        registry.attach(&id, "pane-1").unwrap();
        let attached = registry.disconnect(&id);
        assert_eq!(attached, vec!["pane-1".to_string()]);
    }
}
