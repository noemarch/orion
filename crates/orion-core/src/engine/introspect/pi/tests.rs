use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::*;
use crate::protocol::AgentStatus;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orion-pi-{tag}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn entry(id: &str, parent: Option<&str>, extra: Value) -> Value {
    let mut entry = json!({
        "id": id,
        "parentId": parent,
        "timestamp": "2026-10-16T21:05:20.923Z",
    });
    if let Value::Object(fields) = extra {
        for (key, value) in fields {
            entry[key] = value;
        }
    }
    entry
}

fn user(id: &str, parent: &str, text: &str) -> Value {
    entry(
        id,
        Some(parent),
        json!({ "type": "message", "message": { "role": "user", "content": [{ "type": "text", "text": text }] } }),
    )
}

fn answer(id: &str, parent: &str, text: &str) -> Value {
    entry(
        id,
        Some(parent),
        json!({
            "type": "message",
            "message": { "role": "assistant", "stopReason": "stop", "content": [{ "type": "text", "text": text }] },
        }),
    )
}

fn call(id: &str, parent: &str, call_id: &str, name: &str, arguments: Value) -> Value {
    entry(
        id,
        Some(parent),
        json!({
            "type": "message",
            "message": {
                "role": "assistant",
                "stopReason": "toolUse",
                "content": [{ "type": "toolCall", "id": call_id, "name": name, "arguments": arguments }],
            },
        }),
    )
}

fn result(id: &str, parent: &str, call_id: &str, name: &str, output: &str) -> Value {
    entry(
        id,
        Some(parent),
        json!({
            "type": "message",
            "message": {
                "role": "toolResult",
                "toolCallId": call_id,
                "toolName": name,
                "content": [{ "type": "text", "text": output }],
            },
        }),
    )
}

fn compaction(id: &str, parent: &str, summary: &str) -> Value {
    entry(
        id,
        Some(parent),
        json!({ "type": "compaction", "summary": summary, "firstKeptEntryId": parent }),
    )
}

fn branch_summary(id: &str, parent: &str, from: &str, summary: &str) -> Value {
    entry(
        id,
        Some(parent),
        json!({ "type": "branch_summary", "fromId": from, "summary": summary }),
    )
}

/// Writes one pi session into a store: header line naming `cwd`, then `body`.
fn write_session_at(store: &Path, cwd: &Path, id: &str, written: &str, body: &[Value]) -> PathBuf {
    let dir = store.join(session_folder(&cwd.to_string_lossy()));
    fs::create_dir_all(&dir).expect("session dir");
    let path = dir.join(format!("{id}.jsonl"));
    let header = json!({
        "type": "session",
        "version": 3,
        "id": id,
        "timestamp": written,
        "cwd": cwd.to_string_lossy(),
    });
    let text = std::iter::once(header)
        .chain(body.iter().cloned())
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&path, format!("{text}\n")).expect("session file");
    path
}

fn write_session(store: &Path, cwd: &Path, id: &str, body: &[Value]) -> PathBuf {
    write_session_at(store, cwd, id, "2026-10-16T21:05:20.923Z", body)
}

fn text_parts(turn: &Value) -> Vec<String> {
    turn["parts"]
        .as_array()
        .expect("parts")
        .iter()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

#[test]
fn follows_the_branch_the_last_entry_names() {
    let cwd = temp_dir("branch-cwd");
    let store = temp_dir("branch-store");
    write_session(
        &store,
        &cwd,
        "live",
        &[
            user("u1", "live", "the question"),
            answer("a1", "u1", "the abandoned answer"),
            // /tree back to the header, then a new prompt: the answer above is off the branch
            user("u2", "live", "the retried question"),
            answer("a2", "u2", "the answer in play"),
        ],
    );
    let transcript = PiTranscript::open_in(&cwd, &store).expect("transcript");
    let turns = transcript.turns();
    assert_eq!(turns.len(), 2, "{turns:?}");
    assert_eq!(turns[0]["role"], "user");
    assert_eq!(text_parts(&turns[0]), vec!["the retried question"]);
    assert_eq!(turns[1]["role"], "assistant");
    assert_eq!(text_parts(&turns[1]), vec!["the answer in play"]);
    let response = transcript.response();
    assert_eq!(response["source"], "pi-transcript");
    assert_eq!(response["history_id"], "pi-live");
    assert_eq!(
        response["abandoned"],
        json!({ "count": 2, "branches": 1, "summary": null })
    );
}

#[test]
fn carries_pi_s_summary_of_the_abandoned_path() {
    let cwd = temp_dir("summary-cwd");
    let store = temp_dir("summary-store");
    write_session(
        &store,
        &cwd,
        "summary",
        &[
            user("u1", "summary", "the question"),
            answer("a-gone", "u1", "the abandoned answer"),
            branch_summary(
                "bs",
                "summary",
                "a-gone",
                "We tried the first question and it failed.",
            ),
            user("u2", "bs", "the retried question"),
            answer("a-live", "u2", "the answer in play"),
        ],
    );
    let transcript = PiTranscript::open_in(&cwd, &store).expect("transcript");
    let abandoned = &transcript.response()["abandoned"];
    assert_eq!(abandoned["count"], 2);
    assert_eq!(abandoned["branches"], 1);
    assert_eq!(
        abandoned["summary"],
        "We tried the first question and it failed."
    );
}

#[test]
fn a_straight_session_has_nothing_abandoned() {
    let cwd = temp_dir("straight-cwd");
    let store = temp_dir("straight-store");
    write_session(
        &store,
        &cwd,
        "straight",
        &[
            user("u1", "straight", "first"),
            answer("a1", "u1", "answer one"),
        ],
    );
    let transcript = PiTranscript::open_in(&cwd, &store).expect("transcript");
    assert_eq!(
        transcript.response()["abandoned"],
        json!({ "count": 0, "branches": 0, "summary": null })
    );
}

#[test]
fn a_compaction_is_shown_only_while_its_branch_is_live() {
    let cwd = temp_dir("compact-cwd");
    let store = temp_dir("compact-store");
    write_session(
        &store,
        &cwd,
        "compacted",
        &[
            user("u1", "compacted", "the first question"),
            compaction("c1", "u1", "## Session Goal\n- fold the older turns"),
            user("u2", "c1", "and now the next one"),
        ],
    );
    let mut transcript = PiTranscript::open_in(&cwd, &store).expect("transcript");
    let kinds = transcript
        .turns()
        .iter()
        .map(|turn| {
            turn["parts"]
                .as_array()
                .expect("parts")
                .iter()
                .map(|part| part["kind"].as_str().unwrap_or_default().to_string())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(kinds, vec![vec!["text"], vec!["compact"], vec!["text"]]);
    assert_eq!(
        transcript.turns()[1]["parts"][0]["text"],
        "## Session Goal\n- fold the older turns"
    );

    // /tree back to the first prompt: the fold sits on the branch left behind
    let path = transcript.path().to_path_buf();
    let later = answer("a2", "u1", "the answer in play");
    let mut text = fs::read_to_string(&path).expect("session file");
    text.push_str(&format!("{later}\n"));
    fs::write(&path, text).expect("session append");
    assert!(transcript.refresh());
    let live = transcript
        .turns()
        .iter()
        .flat_map(|turn| turn["parts"].as_array().expect("parts").iter())
        .filter(|part| part["kind"] == "compact")
        .count();
    assert_eq!(live, 0, "{:?}", transcript.turns());
    let abandoned = &transcript.response()["abandoned"];
    // the compacted branch holds one user turn; a compaction entry is not a turn
    assert_eq!(abandoned["count"], 1, "{abandoned}");
    assert_eq!(abandoned["branches"], 1);
}

#[test]
fn tool_calls_take_their_results() {
    let cwd = temp_dir("tool-cwd");
    let store = temp_dir("tool-store");
    write_session(
        &store,
        &cwd,
        "tools",
        &[
            user("u1", "tools", "read the file"),
            call("a1", "u1", "c1", "read", json!({ "path": "src/app.ts" })),
            result("r1", "a1", "c1", "read", "export const app = null;"),
            answer("a2", "r1", "It exports null."),
        ],
    );
    let transcript = PiTranscript::open_in(&cwd, &store).expect("transcript");
    let parts = transcript.turns()[1]["parts"].as_array().expect("parts");
    let tool = parts
        .iter()
        .find(|part| part["kind"] == "tool")
        .expect("tool part");
    assert_eq!(tool["name"], "read");
    assert_eq!(tool["summary"], "src/app.ts");
    assert_eq!(tool["output"], "export const app = null;");
    assert_eq!(
        transcript.status(),
        Some(AgentStatus::Idle),
        "the answer that stops ends the turn"
    );
}

#[test]
fn opens_the_newest_session_of_the_pane_cwd() {
    let cwd = temp_dir("newest-cwd");
    let other = temp_dir("newest-other");
    let store = temp_dir("newest-store");
    write_session_at(
        &store,
        &cwd,
        "older",
        "2026-10-16T21:00:00.000Z",
        &[user("u1", "older", "older question")],
    );
    write_session_at(
        &store,
        &cwd,
        "newer",
        "2026-10-16T21:05:00.000Z",
        &[user("u1", "newer", "newer question")],
    );
    // a newer session of another directory in the same slug folder is not this pane's
    write_session_at(
        &store,
        &other,
        "elsewhere",
        "2026-10-16T21:10:00.000Z",
        &[user("u1", "elsewhere", "someone else's question")],
    );
    let transcript = PiTranscript::open_in(&cwd, &store).expect("transcript");
    assert_eq!(transcript.session_id(), "newer");
    assert_eq!(text_parts(&transcript.turns()[0]), vec!["newer question"]);
}

#[test]
fn a_session_whose_header_names_another_cwd_is_not_bound() {
    let cwd = temp_dir("header-cwd");
    let other = temp_dir("header-other");
    let store = temp_dir("header-store");
    // the file sits in this cwd's folder but its header says otherwise
    let path = write_session_at(
        &store,
        &other,
        "foreign",
        "2026-10-16T21:05:00.000Z",
        &[user("u1", "foreign", "elsewhere")],
    );
    let folder = store.join(session_folder(&cwd.to_string_lossy()));
    fs::create_dir_all(&folder).expect("folder");
    fs::rename(&path, folder.join("foreign.jsonl")).expect("move session");
    assert!(PiTranscript::open_in(&cwd, &store).is_none());
}

#[test]
fn the_store_follows_pi_s_environment() {
    let home = PathBuf::from("C:\\Users\\someone");
    assert_eq!(
        store_dir(None, None, Some(home.clone())),
        Some(home.join(".pi").join("agent").join("sessions"))
    );
    assert_eq!(
        store_dir(Some("D:\\pi-agent".to_string()), None, Some(home.clone())),
        Some(PathBuf::from("D:\\pi-agent").join("sessions"))
    );
    assert_eq!(
        store_dir(
            Some("D:\\pi-agent".to_string()),
            Some("E:\\sessions".to_string()),
            Some(home.clone())
        ),
        Some(PathBuf::from("E:\\sessions")),
        "the session directory override wins"
    );
    assert_eq!(
        store_dir(Some("~/agent".to_string()), None, Some(home.clone())),
        Some(home.join("agent").join("sessions"))
    );
}

#[test]
fn an_append_to_the_live_leaf_is_read_on_the_next_poll() {
    let cwd = temp_dir("append-cwd");
    let store = temp_dir("append-store");
    write_session(&store, &cwd, "append", &[user("u1", "append", "first")]);
    let mut transcript = PiTranscript::open_in(&cwd, &store).expect("transcript");
    assert_eq!(transcript.turns().len(), 1);
    let path = transcript.path().to_path_buf();
    let mut text = fs::read_to_string(&path).expect("session file");
    text.push_str(&format!("{}\n", answer("a1", "u1", "the answer")));
    fs::write(&path, text).expect("session append");
    assert!(transcript.refresh());
    assert_eq!(transcript.turns().len(), 2, "{:?}", transcript.turns());
    assert_eq!(text_parts(&transcript.turns()[1]), vec!["the answer"]);
    assert_eq!(transcript.status(), Some(AgentStatus::Idle));
}

#[test]
fn a_tree_move_is_read_on_the_next_poll() {
    let cwd = temp_dir("move-cwd");
    let store = temp_dir("move-store");
    write_session(
        &store,
        &cwd,
        "move",
        &[
            user("u1", "move", "first"),
            answer("a1", "u1", "answer one"),
        ],
    );
    let mut transcript = PiTranscript::open_in(&cwd, &store).expect("transcript");
    assert_eq!(transcript.turns().len(), 2);
    let path = transcript.path().to_path_buf();
    let mut text = fs::read_to_string(&path).expect("session file");
    // /tree back to the prompt: the next prompt branches beside the first answer
    text.push_str(&format!("{}\n", user("u2", "u1", "another question")));
    fs::write(&path, text).expect("session append");
    assert!(transcript.refresh());
    assert_eq!(transcript.turns().len(), 2, "{:?}", transcript.turns());
    assert_eq!(text_parts(&transcript.turns()[1]), vec!["another question"]);
    assert_eq!(transcript.response()["abandoned"]["count"], 1);
}

#[test]
fn a_finished_session_read_at_open_does_not_start_a_turn() {
    let cwd = temp_dir("finished-cwd");
    let store = temp_dir("finished-store");
    write_session(
        &store,
        &cwd,
        "finished",
        &[user("u1", "finished", "first"), answer("a1", "u1", "done")],
    );
    let transcript = PiTranscript::open_in(&cwd, &store).expect("transcript");
    assert_eq!(transcript.status(), Some(AgentStatus::Idle));
}

#[test]
fn a_turn_still_running_reads_working() {
    let cwd = temp_dir("running-cwd");
    let store = temp_dir("running-store");
    write_session(
        &store,
        &cwd,
        "running",
        &[
            user("u1", "running", "do it"),
            call("a1", "u1", "c1", "bash", json!({ "command": "ls" })),
        ],
    );
    let transcript = PiTranscript::open_in(&cwd, &store).expect("transcript");
    assert_eq!(transcript.status(), Some(AgentStatus::Working));
}
