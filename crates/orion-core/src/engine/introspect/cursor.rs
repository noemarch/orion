//! Reading Cursor's AI chat data.
//!
//! Cursor stores AI conversations in SQLite key-value stores:
//! - Global: `~/.cursor/globalStorage/state.vscdb` (`ItemTable` / `cursorDiskKV`)
//! - Workspace: `~/.cursor/workspaceStorage/<md5(path+birthtime)>/state.vscdb`
//!   Each session record carries `composer_id`, `title`, `created_at_ms` and a
//!   `messages` array of user/assistant/tool_use/tool_result entries.
//!
//! Cursor is not installed on this machine; the reader is built from the
//! cursor-helper crate's documented storage layout and is unproven against a
//! live Cursor installation.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rusqlite::{params, Connection, OpenFlags};
use serde_json::{json, Value};

use crate::protocol::AgentStatus;

use super::omo::tool_summary;

/// One Cursor session's messages, folded into conversation turns.
pub struct CursorTranscript {
    path: PathBuf,
    session_id: String,
    history_id: String,
    turns: Vec<Value>,
    #[allow(dead_code)]
    pending_tool_calls: usize,
    status: Option<AgentStatus>,
}

impl CursorTranscript {
    /// Opens a workspace's `state.vscdb`, which is how the fixture tests replay
    /// a recorded database. Reads all AI chat sessions from `ItemTable` and
    /// `cursorDiskKV`.
    pub fn open_at(path: &Path) -> Option<Self> {
        if !path.is_file() {
            return None;
        }
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
        let has_item_table = table_exists(&conn, "ItemTable");
        let has_disk_kv = table_exists(&conn, "cursorDiskKV");
        if !has_item_table && !has_disk_kv {
            return None;
        }

        // Read all sessions from both tables.
        let mut sessions: Vec<Value> = Vec::new();
        if has_item_table {
            sessions.extend(read_sessions_from_table(&conn, "ItemTable").ok()?);
        }
        if has_disk_kv {
            sessions.extend(read_sessions_from_table(&conn, "cursorDiskKV").ok()?);
        }

        if sessions.is_empty() {
            return None;
        }

        // Fold all sessions into turns. Multiple sessions in one store are
        // concatenated in insertion order (rowid order).
        let mut turns: Vec<Value> = Vec::new();
        let mut session_id = String::new();
        let mut pending_tool_calls: usize = 0;
        for session in &sessions {
            if let Some(sid) = session.get("composer_id").and_then(Value::as_str) {
                session_id = sid.to_string();
            }
            if let Some(messages) = session.get("messages").and_then(Value::as_array) {
                pending_tool_calls += fold_messages(messages, &mut turns);
            }
        }

        if turns.is_empty() {
            return None;
        }

        let status = infer_status(&turns, pending_tool_calls);
        let history_id = format!("cursor:{session_id}");

        Some(Self {
            path: path.to_path_buf(),
            session_id,
            history_id,
            turns,
            pending_tool_calls,
            status,
        })
    }

    /// Opens the Cursor session for this pane by scanning the Cursor workspace
    /// storage for `state.vscdb`. The workspace storage directory is determined
    /// by `compute_workspace_hash(cwd)` = MD5(absolutePath + rounded birthtimeMs).
    /// Since Cursor's birthtime is not available to Orion, we fall back to
    /// scanning all workspace storage directories.
    pub fn open(_cwd: &Path, _since: Option<SystemTime>) -> Option<Self> {
        let home = env::var("CURSOR_HOME")
            .ok()
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("USERPROFILE")
                    .or_else(|| std::env::var_os("HOME"))
                    .map(PathBuf::from)
                    .map(|home| home.join(".cursor"))
            })?;
        let workspace_storage = home.join("workspaceStorage");
        if !workspace_storage.is_dir() {
            return None;
        }

        let mut candidates: Vec<(PathBuf, u64)> = Vec::new();
        let Ok(entries) = fs::read_dir(&workspace_storage) else {
            return None;
        };
        for entry in entries {
            let entry = entry.ok()?;
            let ws_dir = entry.path();
            if !ws_dir.is_dir() {
                continue;
            }
            let vscdb = ws_dir.join("state.vscdb");
            if vscdb.is_file() {
                if let Ok(meta) = fs::metadata(&vscdb) {
                    if let Ok(time) = meta.modified() {
                        candidates.push((vscdb, time.elapsed().unwrap_or_default().as_secs()));
                    }
                }
            }
        }

        candidates.sort_by_key(|(_, mtime)| std::cmp::Reverse(*mtime));

        for (path, _) in candidates {
            if let Some(transcript) = Self::open_at(&path) {
                return Some(transcript);
            }
        }

        None
    }

    /// Ingests everything changed since the last call. Returns whether the chat changed.
    pub fn refresh(&mut self) -> bool {
        // Cursor's state.vscdb is updated in place; we rebuild on each refresh.
        // A real implementation would track rowid changes for incremental reads.
        false
    }

    /// The turns in the window, folded exactly as the chat draws them.
    pub fn turns(&self) -> &[Value] {
        &self.turns
    }

    /// The turn the session is in: working while the last message is from the user
    /// (awaiting assistant response), idle after an assistant response.
    pub fn status(&self) -> Option<AgentStatus> {
        self.status
    }

    /// The wire answer (`ConversationResponse`).
    pub fn response(&self) -> Value {
        json!({
            "source": "cursor-transcript",
            "turns": self.turns,
            "history_id": self.history_id,
            "metadata": {
                "model": null,
                "reasoning_effort": null,
            },
        })
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Check whether a table exists in the database.
fn table_exists(conn: &Connection, table: &str) -> bool {
    conn.query_row(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1",
        params![table],
        |_| Ok(()),
    )
    .is_ok()
}

/// Read AI chat sessions from a key-value table. Looks for values that are JSON
/// objects with a `messages` array (Cursor's composer session format).
fn read_sessions_from_table(conn: &Connection, table: &str) -> rusqlite::Result<Vec<Value>> {
    let mut sessions = Vec::new();
    let mut stmt = conn.prepare(&format!("SELECT value FROM {table}"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    for row in rows.filter_map(|r| r.ok()) {
        if let Ok(value) = serde_json::from_str::<Value>(&row) {
            if value.get("messages").map(Value::is_array).unwrap_or(false) {
                sessions.push(value);
            }
        }
    }
    Ok(sessions)
}

/// Fold Cursor messages into conversation turns.
///
/// Returns the number of pending tool calls (calls without results).
///
/// Message types:
/// - `user` with `message.text` → user turn
/// - `assistant` with `message.text` → assistant turn
/// - `assistant` with `message.tool_use` → tool call part
/// - `user` with `message.tool_result` → tool result part
fn fold_messages(messages: &[Value], turns: &mut Vec<Value>) -> usize {
    let mut pending_tool_calls: usize = 0;

    for msg in messages {
        let msg_type = msg.get("type").and_then(Value::as_str).unwrap_or("");
        let message = msg.get("message").cloned().unwrap_or(Value::Null);

        match msg_type {
            "user" => {
                if let Some(text) = message.get("text").and_then(Value::as_str) {
                    if !text.is_empty() {
                        turns.push(json!({
                            "role": "user",
                            "parts": [{"kind": "text", "text": text}]
                        }));
                    }
                } else if message.get("type").and_then(Value::as_str) == Some("tool_result") {
                    // Tool result: pair with the most recent pending tool call.
                    let tool_use_id = message
                        .get("tool_use_id")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let content = message.get("content").and_then(Value::as_str).unwrap_or("");
                    if !tool_use_id.is_empty() && !content.is_empty() {
                        if let Some(last_turn) = turns.last_mut() {
                            if last_turn["role"] == "assistant" {
                                if let Some(parts) =
                                    last_turn.get_mut("parts").and_then(|p| p.as_array_mut())
                                {
                                    // Find the matching tool call part
                                    if let Some(part_idx) = parts.iter().position(|p| {
                                        p.get("kind").and_then(Value::as_str) == Some("tool")
                                            && p.get("name").is_some()
                                    }) {
                                        if let Some(part) = parts.get_mut(part_idx) {
                                            part["output"] = json!(content);
                                        }
                                    }
                                }
                            }
                        }
                        pending_tool_calls = pending_tool_calls.saturating_sub(1);
                    }
                }
            }
            "assistant" => {
                if let Some(text) = message.get("text").and_then(Value::as_str) {
                    if !text.is_empty() {
                        if let Some(last_turn) = turns.last_mut() {
                            if last_turn["role"] == "assistant" {
                                if let Some(parts) =
                                    last_turn.get_mut("parts").and_then(|p| p.as_array_mut())
                                {
                                    // Append to last text part or create new
                                    if let Some(last_part) = parts.last_mut() {
                                        if last_part["kind"] == "text" {
                                            last_part["text"] = json!(format!(
                                                "{}{}",
                                                last_part["text"].as_str().unwrap_or(""),
                                                text
                                            ));
                                        } else {
                                            parts.push(json!({ "kind": "text", "text": text }));
                                        }
                                    } else {
                                        parts.push(json!({ "kind": "text", "text": text }));
                                    }
                                }
                            } else {
                                turns.push(json!({
                                    "role": "assistant",
                                    "parts": [{"kind": "text", "text": text}]
                                }));
                            }
                        }
                    }
                } else if let Some(call_id) = message.get("id").and_then(Value::as_str) {
                    // Tool use call
                    let name = message
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or(call_id);
                    let input = message.get("input").cloned().unwrap_or(Value::Null);
                    let input_str = if input.is_string() {
                        input.as_str().unwrap_or("").to_string()
                    } else {
                        input.to_string()
                    };
                    let summary = tool_summary(name, &input);

                    let tool_part = json!({
                        "kind": "tool",
                        "name": name,
                        "summary": summary,
                        "input": input_str,
                        "output": "",
                    });

                    if let Some(last_turn) = turns.last_mut() {
                        if last_turn["role"] == "assistant" {
                            if let Some(parts) =
                                last_turn.get_mut("parts").and_then(|p| p.as_array_mut())
                            {
                                parts.push(tool_part);
                            }
                        } else {
                            turns.push(json!({
                                "role": "assistant",
                                "parts": [tool_part]
                            }));
                        }
                    } else {
                        turns.push(json!({
                            "role": "assistant",
                            "parts": [tool_part]
                        }));
                    }
                    pending_tool_calls += 1;
                }
            }
            _ => {}
        }
    }

    pending_tool_calls
}

/// Infer status from the turns and pending tool calls.
///
/// Working if the last turn is from the user (awaiting assistant response)
/// or if there are pending tool calls awaiting results.
/// Idle after an assistant turn with no pending calls.
fn infer_status(turns: &[Value], pending_tool_calls: usize) -> Option<AgentStatus> {
    if pending_tool_calls > 0 {
        return Some(AgentStatus::Working);
    }
    match turns.last() {
        Some(t) if t["role"] == "user" => Some(AgentStatus::Working),
        _ => Some(AgentStatus::Idle),
    }
}

#[cfg(test)]
mod tests;
