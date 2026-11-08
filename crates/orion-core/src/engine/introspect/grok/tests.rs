//! Tests for the Grok transcript reader.
//!
//! Grok keeps a native update log as JSONL: one `session/update` record per line, with
//! `sessionUpdate` fields like `user_message_chunk`, `agent_message_chunk`, `tool_call`,
//! `tool_call_update`, `turn_completed`, `agent_thought_chunk`, `rewind_marker`.
//!
//! These tests synthesize the recorded event shapes from Orbit's `grok.test.ts` fixtures.

#![allow(dead_code)]

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde_json::{json, Value};

use super::*;
use crate::protocol::AgentStatus;

/// A Grok store: the `updates.jsonl` inside `<grok-home>/sessions/<group>/<session>/`.
struct Store {
    dir: PathBuf,
    path: PathBuf,
}

impl Store {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("orion-grok-{tag}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("updates.jsonl");
        fs::File::create(&path).expect("create updates.jsonl");
        Self { dir, path }
    }

    fn append(&self, line: &str) {
        fs::OpenOptions::new()
            .append(true)
            .open(&self.path)
            .expect("open updates.jsonl")
            .write_all(format!("{line}\n").as_bytes())
            .expect("write line");
    }

    fn append_raw(&self, bytes: &[u8]) {
        fs::OpenOptions::new()
            .append(true)
            .open(&self.path)
            .expect("open updates.jsonl")
            .write_all(bytes)
            .expect("write bytes");
    }
}

fn event(update: &serde_json::Value, session: &str, ts: Option<f64>) -> String {
    let mut obj = serde_json::json!({
        "method": "session/update",
        "params": {
            "sessionId": session,
            "update": update,
        },
    });
    if let Some(t) = ts {
        obj.as_object_mut()
            .unwrap()
            .insert("timestamp".to_string(), json!(t));
    }
    serde_json::to_string(&obj).unwrap()
}

fn user_chunk(session: &str, index: u64, text: &str, ts: Option<f64>) -> String {
    event(
        &json!({
            "sessionUpdate": "user_message_chunk",
            "content": { "type": "text", "text": text },
            "_meta": { "promptIndex": index, "modelId": "grok-3" },
        }),
        session,
        ts,
    )
}

fn answer_chunk(session: &str, text: &str, ts: Option<f64>) -> String {
    event(
        &json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": text },
        }),
        session,
        ts,
    )
}

fn tool_call(session: &str, id: &str, title: &str, raw_input: &serde_json::Value) -> String {
    event(
        &json!({
            "sessionUpdate": "tool_call",
            "toolCallId": id,
            "title": title,
            "rawInput": raw_input,
            "content": [],
            "status": "pending",
        }),
        session,
        None,
    )
}

fn tool_update(session: &str, id: &str, output: &str) -> String {
    event(
        &json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": id,
            "status": "completed",
            "content": [{ "type": "content", "content": { "type": "text", "text": output } }],
        }),
        session,
        None,
    )
}

fn turn_completed(session: &str) -> String {
    event(
        &json!({
            "sessionUpdate": "turn_completed",
        }),
        session,
        None,
    )
}

fn rewind_marker(session: &str, target: u64) -> String {
    event(
        &json!({
            "sessionUpdate": "rewind_marker",
            "target_prompt_index": target,
        }),
        session,
        None,
    )
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

fn tools(turn: &Value) -> Vec<Value> {
    turn["parts"]
        .as_array()
        .expect("parts")
        .iter()
        .filter(|part| part["kind"] == "tool")
        .cloned()
        .collect()
}

#[test]
fn parses_user_and_assistant_chunks_into_turns() {
    let store = Store::new("turns");
    let session = "grok-session";
    store.append(&user_chunk(session, 0, "what is 2+2", None));
    store.append(&answer_chunk(session, "4", None));

    let transcript = GrokTranscript::open_at(&store.path, session).expect("transcript");
    let turns = transcript.turns();
    assert_eq!(turns.len(), 2, "{turns:?}");
    assert_eq!(turns[0]["role"], "user");
    assert_eq!(text(&turns[0]), "what is 2+2");
    assert_eq!(turns[1]["role"], "assistant");
    assert_eq!(text(&turns[1]), "4");
}

#[test]
fn pairs_tool_call_with_its_update() {
    let store = Store::new("tools");
    let session = "grok-tools";
    store.append(&user_chunk(session, 0, "read the file", None));
    store.append(&tool_call(
        session,
        "call-1",
        "Read file",
        &json!({ "path": "src/main.rs" }),
    ));
    store.append(&tool_update(session, "call-1", "fn main() {}\n"));

    let transcript = GrokTranscript::open_at(&store.path, session).expect("transcript");
    let turns = transcript.turns();
    assert_eq!(turns.len(), 2);
    let assistant = &turns[1];
    assert_eq!(assistant["role"], "assistant");
    let tool = &tools(assistant)[0];
    assert_eq!(tool["name"], "call-1");
    assert_eq!(tool["summary"], "Read file");
    assert_eq!(tool["input"], r#"{"path":"src/main.rs"}"#);
    assert_eq!(tool["output"], "fn main() {}\n");
}

#[test]
fn marks_failed_tool_calls_as_error() {
    let store = Store::new("failed");
    let session = "grok-failed";
    store.append(&user_chunk(session, 0, "run it", None));
    store.append(&tool_call(
        session,
        "call-1",
        "Run command",
        &json!({ "cmd": "false" }),
    ));
    store.append(&event(
        &json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call-1",
            "status": "failed",
            "content": [],
        }),
        session,
        None,
    ));

    let transcript = GrokTranscript::open_at(&store.path, session).expect("transcript");
    let turns = transcript.turns();
    let tool = &tools(&turns[1])[0];
    assert!(tool["error"].as_bool().unwrap_or(false));
}

#[test]
fn turn_completed_ends_the_assistant_turn() {
    let store = Store::new("completed");
    let session = "grok-completed";
    store.append(&user_chunk(session, 0, "hi", None));
    store.append(&answer_chunk(session, "hello", None));
    store.append(&turn_completed(session));

    let transcript = GrokTranscript::open_at(&store.path, session).expect("transcript");
    let turns = transcript.turns();
    assert_eq!(turns.len(), 2);
    assert!(turns[1]["end_ts"].as_str().is_some());
}

#[test]
fn rewind_marker_removes_later_prompts() {
    let store = Store::new("rewind");
    let session = "grok-rewind";
    store.append(&user_chunk(session, 0, "first", None));
    store.append(&answer_chunk(session, "first answer", None));
    store.append(&user_chunk(session, 1, "second", None));
    store.append(&answer_chunk(session, "second answer", None));
    // Rewind to prompt 1: removes prompt 2 and its answer
    store.append(&rewind_marker(session, 1));

    let transcript = GrokTranscript::open_at(&store.path, session).expect("transcript");
    let turns = transcript.turns();
    assert_eq!(turns.len(), 2);
    assert_eq!(text(&turns[0]), "first");
    assert_eq!(text(&turns[1]), "first answer");
}

#[test]
fn incremental_append_reads_on() {
    let store = Store::new("append");
    let session = "grok-append";
    store.append(&user_chunk(session, 0, "one", None));
    let mut transcript = GrokTranscript::open_at(&store.path, session).expect("transcript");
    assert_eq!(transcript.turns().len(), 1);

    store.append(&answer_chunk(session, "two", None));
    assert!(transcript.refresh());
    assert_eq!(transcript.turns().len(), 2);
    assert_eq!(text(&transcript.turns()[1]), "two");

    // unchanged store is not news
    assert!(!transcript.refresh());
}

#[test]
fn session_without_events_is_not_a_chat() {
    let store = Store::new("empty");
    assert!(GrokTranscript::open_at(&store.path, "nonexistent").is_none());
}

#[test]
fn unknown_event_type_throws_unavailable() {
    let store = Store::new("unknown");
    let session = "grok-unknown";
    store.append(&event(
        &json!({
            "sessionUpdate": "unknown_update",
        }),
        session,
        None,
    ));
    // unknown sessionUpdate is ignored (returns None from grokEvent), not an error
    assert!(GrokTranscript::open_at(&store.path, session).is_none());
}

#[test]
fn status_is_working_while_turn_is_open() {
    let store = Store::new("status");
    let session = "grok-status";
    store.append(&user_chunk(session, 0, "thinking", None));
    let mut transcript = GrokTranscript::open_at(&store.path, session).expect("transcript");
    assert_eq!(transcript.status(), Some(AgentStatus::Working));

    store.append(&answer_chunk(session, "done", None));
    store.append(&turn_completed(session));
    assert!(transcript.refresh());
    assert_eq!(transcript.status(), Some(AgentStatus::Idle));
}

#[test]
fn history_id_and_metadata() {
    let store = Store::new("meta");
    let session = "grok-meta";
    store.append(&user_chunk(session, 0, "hi", None));
    let transcript = GrokTranscript::open_at(&store.path, session).expect("transcript");
    let response = transcript.response();
    assert_eq!(response["source"], "grok-transcript");
    assert!(response["history_id"].as_str().is_some());
    assert_eq!(response["metadata"]["model"], Value::Null);
}
