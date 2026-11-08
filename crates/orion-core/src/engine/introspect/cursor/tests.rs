//! Tests for the Cursor transcript reader.
//!
//! Cursor stores AI chat data in SQLite:
//! - Global: `~/.cursor/globalStorage/state.vscdb` (ItemTable / cursorDiskKV key-value)
//! - Workspace: `~/.cursor/workspaceStorage/<md5(path+birthtime)>/state.vscdb`
//!   Sessions have `composer_id`, `title`, `created_at_ms`.
//!
//! Cursor is not installed on this machine, so fixtures are synthesized from the
//! cursor-helper crate's documented storage layout.

#![allow(dead_code)]

use std::fs;
use std::path::PathBuf;

use rusqlite::{params, Connection};
use serde_json::{json, Value};

use super::*;
use crate::protocol::AgentStatus;

/// A Cursor workspace storage: `state.vscdb` with ItemTable key-value pairs.
struct Store {
    path: PathBuf,
    conn: Connection,
}

impl Store {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("orion-cursor-{tag}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("state.vscdb");
        let conn = Connection::open(&path).expect("db");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS ItemTable (key TEXT PRIMARY KEY, value TEXT);
             CREATE TABLE IF NOT EXISTS cursorDiskKV (key TEXT PRIMARY KEY, value TEXT);",
        )
        .expect("schema");
        Self { path, conn }
    }

    fn path(&self) -> PathBuf {
        self.path.clone()
    }

    fn insert_item(&self, key: &str, value: &str) {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO ItemTable (key, value) VALUES (?1, ?2)",
                params![key, value],
            )
            .expect("insert item");
    }

    fn insert_disk_kv(&self, key: &str, value: &str) {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
                params![key, value],
            )
            .expect("insert disk kv");
    }
}

fn ai_chat_value(session_id: &str, messages: &[Value]) -> String {
    json!({
        "composer_id": session_id,
        "title": "Cursor chat",
        "created_at_ms": 1760000000000i64,
        "messages": messages,
    })
    .to_string()
}

fn user_message(text: &str) -> Value {
    json!({
        "type": "user",
        "message": {
            "type": "text",
            "text": text
        }
    })
}

fn assistant_message(text: &str) -> Value {
    json!({
        "type": "assistant",
        "message": {
            "type": "text",
            "text": text
        }
    })
}

fn tool_call_message(call_id: &str, name: &str, input: &str) -> Value {
    json!({
        "type": "assistant",
        "message": {
            "type": "tool_use",
            "id": call_id,
            "name": name,
            "input": input
        }
    })
}

fn tool_result_message(call_id: &str, output: &str) -> Value {
    json!({
        "type": "user",
        "message": {
            "type": "tool_result",
            "tool_use_id": call_id,
            "content": output
        }
    })
}

fn text(turn: &Value) -> String {
    turn["parts"]
        .as_array()
        .expect("parts")
        .iter()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn kinds(turn: &Value) -> Vec<String> {
    turn["parts"]
        .as_array()
        .expect("parts")
        .iter()
        .map(|part| part["kind"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn parses_user_and_assistant_messages() {
    let store = Store::new("basic");
    let session_id = "cursor-session-1";
    let messages = json!([user_message("hello"), assistant_message("hi there")]);
    store.insert_item(
        &format!("cursor:ai/chat/{}", session_id),
        &ai_chat_value(session_id, messages.as_array().unwrap()),
    );

    let transcript = CursorTranscript::open_at(&store.path).expect("transcript");
    let turns = transcript.turns();
    assert_eq!(turns.len(), 2, "{turns:?}");
    assert_eq!(turns[0]["role"], "user");
    assert_eq!(text(&turns[0]), "hello");
    assert_eq!(turns[1]["role"], "assistant");
    assert_eq!(text(&turns[1]), "hi there");
}

#[test]
fn pairs_tool_call_with_result() {
    let store = Store::new("tools");
    let session_id = "cursor-tools-1";
    let messages = json!([
        user_message("run the command"),
        tool_call_message("call-1", "bash", r#"{"command":"echo hi"}"#),
        tool_result_message("call-1", "hi\n"),
        assistant_message("done")
    ]);
    store.insert_item(
        &format!("cursor:ai/chat/{}", session_id),
        &ai_chat_value(session_id, messages.as_array().unwrap()),
    );

    let transcript = CursorTranscript::open_at(&store.path).expect("transcript");
    let turns = transcript.turns();
    assert_eq!(turns.len(), 2);
    let assistant = &turns[1];
    assert_eq!(assistant["role"], "assistant");
    let tool = &assistant["parts"].as_array().unwrap()[0];
    assert_eq!(tool["kind"], "tool");
    assert_eq!(tool["name"], "bash");
    assert_eq!(tool["input"], r#"{"command":"echo hi"}"#);
    assert_eq!(tool["output"], "hi\n");
}

#[test]
fn empty_database_returns_none() {
    let dir = std::env::temp_dir().join(format!("orion-cursor-empty-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("state.vscdb");
    assert!(CursorTranscript::open_at(&path).is_none());
}

#[test]
fn status_is_working_while_turn_open() {
    let store = Store::new("status");
    let session_id = "cursor-status-1";
    let messages = json!([
        user_message("thinking"),
        tool_call_message("call-1", "bash", r#"{"command":"sleep"}"#)
    ]);
    store.insert_item(
        &format!("cursor:ai/chat/{}", session_id),
        &ai_chat_value(session_id, messages.as_array().unwrap()),
    );

    let transcript = CursorTranscript::open_at(&store.path).expect("transcript");
    assert_eq!(transcript.status(), Some(AgentStatus::Working));
}

#[test]
fn status_is_idle_after_completion() {
    let store = Store::new("status2");
    let session_id = "cursor-status-2";
    let messages = json!([user_message("hello"), assistant_message("hi")]);
    store.insert_item(
        &format!("cursor:ai/chat/{}", session_id),
        &ai_chat_value(session_id, messages.as_array().unwrap()),
    );

    let transcript = CursorTranscript::open_at(&store.path).expect("transcript");
    assert_eq!(transcript.status(), Some(AgentStatus::Idle));
}

#[test]
fn history_id_and_metadata() {
    let store = Store::new("meta");
    let session_id = "cursor-meta-1";
    let messages = json!([user_message("hi")]);
    store.insert_item(
        &format!("cursor:ai/chat/{}", session_id),
        &ai_chat_value(session_id, messages.as_array().unwrap()),
    );

    let transcript = CursorTranscript::open_at(&store.path).expect("transcript");
    let response = transcript.response();
    assert_eq!(response["source"], "cursor-transcript");
    assert!(response["history_id"].as_str().is_some());
}
