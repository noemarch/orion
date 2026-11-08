use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use rusqlite::{params, Connection};
use serde_json::{json, Value};

use super::*;
use crate::protocol::AgentStatus;

/// A Hermes store (the `state.db` a pane writes) with the schema its sessions and messages tables
/// have: `sessions.cwd` binds the pane, `messages` is the transcript.
struct Store {
    dir: PathBuf,
    conn: Connection,
}

impl Store {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("orion-hermes-{tag}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("temp dir");
        let conn = Connection::open(dir.join("state.db")).expect("db");
        conn.execute_batch(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                model TEXT,
                model_config TEXT,
                started_at REAL NOT NULL,
                input_tokens INTEGER DEFAULT 0,
                output_tokens INTEGER DEFAULT 0,
                reasoning_tokens INTEGER DEFAULT 0,
                cwd TEXT
             );
             CREATE TABLE messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT,
                tool_call_id TEXT,
                tool_calls TEXT,
                tool_name TEXT,
                reasoning TEXT,
                timestamp REAL NOT NULL,
                active INTEGER NOT NULL DEFAULT 1,
                compacted INTEGER NOT NULL DEFAULT 0
             );",
        )
        .expect("schema");
        Self { dir, conn }
    }

    fn path(&self) -> PathBuf {
        self.dir.join("state.db")
    }

    fn session(&self, id: &str, cwd: &str, started_at: f64, model: &str, config: Option<&str>) {
        self.conn
            .execute(
                "INSERT INTO sessions (id, cwd, started_at, model, model_config) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, cwd, started_at, model, config],
            )
            .expect("session");
    }

    /// Inserts one message row; fields left out take their defaults.
    fn message(&self, session: &str, row: Value) -> i64 {
        let role = row["role"].as_str().expect("role");
        let timestamp = row.get("timestamp").and_then(Value::as_f64).unwrap_or(BASE);
        self.conn
            .execute(
                "INSERT INTO messages (session_id, role, content, tool_call_id, tool_calls, tool_name, reasoning, timestamp, active, compacted)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    session,
                    role,
                    row.get("content").and_then(Value::as_str),
                    row.get("tool_call_id").and_then(Value::as_str),
                    row.get("tool_calls").map(|calls| calls.to_string()),
                    row.get("tool_name").and_then(Value::as_str),
                    row.get("reasoning").and_then(Value::as_str),
                    timestamp,
                    row.get("active").and_then(Value::as_i64).unwrap_or(1),
                    row.get("compacted").and_then(Value::as_i64).unwrap_or(0),
                ],
            )
            .expect("message");
        self.conn.last_insert_rowid()
    }

    fn sql(&self, statement: &str) {
        self.conn.execute_batch(statement).expect("sql");
    }
}

const BASE: f64 = 1_760_000_000.0;

fn at(seconds: f64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_millis((seconds * 1000.0) as u64)
}

fn cwd(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orion-hermes-cwd-{tag}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).expect("cwd");
    dir
}

fn text(turn: &Value) -> String {
    turn["parts"]
        .as_array()
        .expect("parts")
        .iter()
        .filter(|part| part["kind"] == "text")
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
fn binds_the_newest_session_of_the_pane_cwd_that_started_with_the_pane() {
    let store = Store::new("bind");
    let project = cwd("bind-project");
    let elsewhere = cwd("bind-elsewhere");
    let project_text = project.to_string_lossy().to_string();
    let elsewhere_text = elsewhere.to_string_lossy().to_string();
    store.session("older", &project_text, BASE - 60.0, "m", None);
    store.session("newer", &project_text, BASE, "m", None);
    // a newer session of another directory is not this pane's
    store.session("foreign", &elsewhere_text, BASE + 2.0, "m", None);
    store.message(
        "newer",
        json!({ "role": "user", "content": "the pane's question" }),
    );
    store.message(
        "older",
        json!({ "role": "user", "content": "yesterday's question" }),
    );

    let transcript = HermesTranscript::open_at(&store.path(), &project, Some(at(BASE - 5.0)))
        .expect("transcript");
    assert_eq!(transcript.session_id(), "newer");
    assert_eq!(text(&transcript.turns()[0]), "the pane's question");
}

#[test]
fn binds_the_newest_matching_session_when_read_without_a_start_time() {
    let store = Store::new("newest");
    let project = cwd("newest-project");
    let project_text = project.to_string_lossy().to_string();
    store.session("older", &project_text, BASE - 60.0, "m", None);
    store.session("newer", &project_text, BASE, "m", None);
    store.message("older", json!({ "role": "user", "content": "older" }));
    store.message("newer", json!({ "role": "user", "content": "newer" }));
    let transcript = HermesTranscript::open_at(&store.path(), &project, None).expect("transcript");
    assert_eq!(transcript.session_id(), "newer");
}

#[test]
fn matches_the_verbatim_windows_form_of_the_cwd() {
    let store = Store::new("verbatim");
    let project = cwd("verbatim-project");
    let project_text = project.to_string_lossy().to_string();
    let verbatim = format!("\\\\?\\{project_text}");
    store.session("verbatim", &verbatim, BASE, "m", None);
    store.message("verbatim", json!({ "role": "user", "content": "hi" }));
    let transcript = HermesTranscript::open_at(&store.path(), &project, None).expect("transcript");
    assert_eq!(transcript.session_id(), "verbatim");
}

#[test]
fn a_session_without_messages_is_not_a_chat_yet() {
    let store = Store::new("empty");
    let project = cwd("empty-project");
    store.session("empty", &project.to_string_lossy(), BASE, "m", None);
    assert!(HermesTranscript::open_at(&store.path(), &project, None).is_none());
}

#[test]
fn folds_user_assistant_reasoning_tool_calls_and_results() {
    let store = Store::new("turns");
    let project = cwd("turns-project");
    store.session(
        "turns",
        &project.to_string_lossy(),
        BASE,
        "claude-3-7-sonnet",
        Some(r#"{"reasoning_effort":"high"}"#),
    );
    store.message(
        "turns",
        json!({ "role": "user", "content": "Please check git status", "timestamp": BASE + 0.0 }),
    );
    store.message(
        "turns",
        json!({
            "role": "assistant",
            "content": "Checking status now.",
            "reasoning": "User wants repository status. I should run git status.",
            "tool_calls": [{"id": "call_1", "function": {"name": "bash", "arguments": "{\"command\":\"git status\"}"}}],
            "timestamp": BASE + 1.0,
        }),
    );
    store.message(
        "turns",
        json!({
            "role": "tool",
            "content": "On branch main\nnothing to commit",
            "tool_call_id": "call_1",
            "tool_name": "bash",
            "timestamp": BASE + 2.0,
        }),
    );
    store.message(
        "turns",
        json!({ "role": "assistant", "content": "Working tree is clean.", "timestamp": BASE + 3.0 }),
    );

    let transcript = HermesTranscript::open_at(&store.path(), &project, None).expect("transcript");
    let turns = transcript.turns();
    assert_eq!(turns.len(), 2, "{turns:?}");
    assert_eq!(turns[0]["role"], "user");
    assert_eq!(text(&turns[0]), "Please check git status");
    assert_eq!(turns[0]["ts"], "2025-10-09T08:53:20.000Z");
    assert_eq!(turns[1]["role"], "assistant");
    assert_eq!(
        kinds(&turns[1]),
        vec!["thinking", "tool", "text", "text"],
        "{:?}",
        turns[1]
    );
    assert_eq!(
        turns[1]["parts"][0]["text"],
        "User wants repository status. I should run git status."
    );
    let tool = &tools(&turns[1])[0];
    assert_eq!(tool["name"], "bash");
    assert_eq!(tool["summary"], "git status");
    assert_eq!(tool["input"], "{\"command\":\"git status\"}");
    assert_eq!(tool["output"], "On branch main\nnothing to commit");
    assert_eq!(turns[1]["end_ts"], "2025-10-09T08:53:23.000Z");

    let response = transcript.response();
    assert_eq!(response["source"], "hermes-transcript");
    assert_eq!(response["history_id"], "hermes:turns:1760000000000");
    assert_eq!(response["metadata"]["model"], "claude-3-7-sonnet");
    assert_eq!(response["metadata"]["reasoning_effort"], "high");
    assert_eq!(transcript.status(), Some(AgentStatus::Idle));
}

#[test]
fn terminal_tools_decode_their_envelopes_and_mark_failures() {
    let store = Store::new("terminal");
    let project = cwd("terminal-project");
    store.session("terminal", &project.to_string_lossy(), BASE, "m", None);
    let calls = [
        ("call-ok", "bash"),
        ("call-exit", "bash"),
        ("call-err", "terminal"),
        ("call-raw", "bash"),
        ("call-other", "web_search"),
    ];
    let tool_calls = calls
        .iter()
        .map(|(id, name)| json!({ "id": id, "function": { "name": name, "arguments": "{}" } }))
        .collect::<Vec<_>>();
    store.message(
        "terminal",
        json!({ "role": "assistant", "tool_calls": tool_calls }),
    );
    let results = [
        (
            "call-ok",
            "bash",
            json!({"output": "command succeeded\n", "exit_code": 0}),
        ),
        (
            "call-exit",
            "bash",
            json!({"output": "command failed\n", "exit_code": 2}),
        ),
        (
            "call-err",
            "terminal",
            json!({"output": "timed out\n", "exit_code": 0, "error": "process timeout"}),
        ),
        ("call-raw", "bash", json!("not a json envelope")),
        (
            "call-other",
            "web_search",
            json!({"output": "json payload", "exit_code": 0}),
        ),
    ];
    for (id, name, content) in results {
        store.message(
            "terminal",
            json!({
                "role": "tool",
                "content": content.as_str().map(str::to_string).unwrap_or_else(|| content.to_string()),
                "tool_call_id": id,
                "tool_name": name,
            }),
        );
    }
    let transcript = HermesTranscript::open_at(&store.path(), &project, None).expect("transcript");
    let tools = tools(&transcript.turns()[0]);
    assert_eq!(tools.len(), 5, "{tools:?}");
    assert_eq!(tools[0]["output"], "command succeeded\n");
    assert!(tools[0].get("error").is_none());
    assert_eq!(tools[1]["output"], "command failed\n");
    assert_eq!(tools[1]["error"], true);
    assert_eq!(tools[2]["output"], "timed out\n");
    assert_eq!(tools[2]["error"], true);
    assert_eq!(tools[3]["output"], "not a json envelope");
    assert!(tools[3].get("error").is_none());
    assert_eq!(
        tools[4]["output"],
        "{\"exit_code\":0,\"output\":\"json payload\"}"
    );
    assert!(tools[4].get("error").is_none());
}

#[test]
fn a_plain_error_prefix_marks_a_failed_tool() {
    let store = Store::new("error-prefix");
    let project = cwd("error-prefix-project");
    store.session("s", &project.to_string_lossy(), BASE, "m", None);
    store.message(
        "s",
        json!({ "role": "assistant", "tool_calls": [{"id": "bad", "function": {"name": "bash", "arguments": "{}"}}] }),
    );
    store.message(
        "s",
        json!({ "role": "tool", "content": "Error: command failed with code 1", "tool_call_id": "bad", "tool_name": "bash" }),
    );
    let transcript = HermesTranscript::open_at(&store.path(), &project, None).expect("transcript");
    assert_eq!(tools(&transcript.turns()[0])[0]["error"], true);
}

#[test]
fn a_skill_invocation_is_a_request_with_a_chip() {
    let fixture = Path::new(
        "C:\\herdr-plugins-project\\orbit\\server\\fixtures\\skill-invocations\\hermes.txt",
    );
    let Ok(prompt) = fs::read_to_string(fixture) else {
        eprintln!("skipped: Orbit hermes fixture not present");
        return;
    };
    let store = Store::new("skill");
    let project = cwd("skill-project");
    store.session("skill", &project.to_string_lossy(), BASE, "m", None);
    store.message("skill", json!({ "role": "user", "content": prompt }));
    let transcript = HermesTranscript::open_at(&store.path(), &project, None).expect("transcript");
    let turn = &transcript.turns()[0];
    assert_eq!(text(turn), "/probe-skill hello");
    let part = turn["parts"]
        .as_array()
        .expect("parts")
        .iter()
        .find(|part| part["kind"] == "skill")
        .expect("skill chip");
    assert_eq!(part["skill"]["name"], "probe-skill");
    assert_eq!(part["skill"]["evidence"], "instructions");
    assert_eq!(part["skill"]["status"], "loaded");
    assert_eq!(
        part["skill"]["path"],
        "/tmp/ulw-allagent.Oyfi/tmp-3/hermes-home/skills/probe-skill/SKILL.md"
    );
}

#[test]
fn a_skill_bundle_is_one_request_with_each_chip() {
    let fixture = Path::new(
        "C:\\herdr-plugins-project\\orbit\\server\\fixtures\\skill-invocations\\hermes-bundle.txt",
    );
    let Ok(prompt) = fs::read_to_string(fixture) else {
        eprintln!("skipped: Orbit hermes bundle fixture not present");
        return;
    };
    let store = Store::new("bundle");
    let project = cwd("bundle-project");
    store.session("bundle", &project.to_string_lossy(), BASE, "m", None);
    store.message("bundle", json!({ "role": "user", "content": prompt }));
    let transcript = HermesTranscript::open_at(&store.path(), &project, None).expect("transcript");
    let turn = &transcript.turns()[0];
    assert_eq!(text(turn), "/probe-bundle hello");
    let skills = turn["parts"]
        .as_array()
        .expect("parts")
        .iter()
        .filter(|part| part["kind"] == "skill")
        .map(|part| {
            part["skill"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect::<Vec<_>>();
    assert_eq!(skills, vec!["probe-skill", "probe-skill-two"]);
}

#[test]
fn an_unrecognized_skill_scaffold_stays_the_user_s_words() {
    let store = Store::new("raw");
    let project = cwd("raw-project");
    store.session("raw", &project.to_string_lossy(), BASE, "m", None);
    let prompt = "[IMPORTANT: The user has invoked the \"probe-skill\" skill, indicating they want you to follow its instructions. The full skill content is loaded below.]\n\nno directory line";
    store.message("raw", json!({ "role": "user", "content": prompt }));
    let transcript = HermesTranscript::open_at(&store.path(), &project, None).expect("transcript");
    assert_eq!(text(&transcript.turns()[0]), prompt);
    assert!(!kinds(&transcript.turns()[0]).contains(&"skill".to_string()));
}

#[test]
fn only_active_or_compacted_rows_are_read() {
    let store = Store::new("active");
    let project = cwd("active-project");
    store.session("active", &project.to_string_lossy(), BASE, "m", None);
    store.message(
        "active",
        json!({ "role": "user", "content": "hidden", "active": 0 }),
    );
    store.message(
        "active",
        json!({ "role": "user", "content": "compacted but shown", "compacted": 1 }),
    );
    store.message("active", json!({ "role": "user", "content": "shown" }));
    let transcript = HermesTranscript::open_at(&store.path(), &project, None).expect("transcript");
    let shown = transcript.turns().iter().map(text).collect::<Vec<_>>();
    assert_eq!(shown, vec!["compacted but shown", "shown"]);
}

#[test]
fn a_finished_session_read_at_open_is_idle() {
    let store = Store::new("idle");
    let project = cwd("idle-project");
    store.session("idle", &project.to_string_lossy(), BASE, "m", None);
    store.message("idle", json!({ "role": "user", "content": "hi" }));
    store.message("idle", json!({ "role": "assistant", "content": "done" }));
    let transcript = HermesTranscript::open_at(&store.path(), &project, None).expect("transcript");
    assert_eq!(transcript.status(), Some(AgentStatus::Idle));
}

#[test]
fn a_turn_waiting_for_its_tool_reads_working_and_ends_on_the_answer() {
    let store = Store::new("status");
    let project = cwd("status-project");
    store.session("status", &project.to_string_lossy(), BASE, "m", None);
    store.message("status", json!({ "role": "user", "content": "do it" }));
    store.message(
        "status",
        json!({ "role": "assistant", "tool_calls": [{"id": "c1", "function": {"name": "bash", "arguments": "{}"}}] }),
    );
    let mut transcript =
        HermesTranscript::open_at(&store.path(), &project, None).expect("transcript");
    assert_eq!(transcript.status(), Some(AgentStatus::Working));

    store.message(
        "status",
        json!({ "role": "tool", "content": "{\"output\":\"ok\",\"exit_code\":0}", "tool_call_id": "c1", "tool_name": "bash" }),
    );
    assert!(transcript.refresh());
    assert_eq!(transcript.status(), Some(AgentStatus::Working));

    store.message(
        "status",
        json!({ "role": "assistant", "content": "finished" }),
    );
    assert!(transcript.refresh());
    assert_eq!(transcript.status(), Some(AgentStatus::Idle));
    assert_eq!(text(&transcript.turns()[1]), "finished");
}

#[test]
fn an_append_is_read_without_rebuilding_the_turns() {
    let store = Store::new("append");
    let project = cwd("append-project");
    store.session("append", &project.to_string_lossy(), BASE, "m", None);
    store.message("append", json!({ "role": "user", "content": "first" }));
    let mut transcript =
        HermesTranscript::open_at(&store.path(), &project, None).expect("transcript");
    assert_eq!(transcript.turns().len(), 1);

    store.message(
        "append",
        json!({ "role": "assistant", "content": "second" }),
    );
    assert!(transcript.refresh());
    assert_eq!(transcript.turns().len(), 2);
    assert_eq!(text(&transcript.turns()[1]), "second");
    assert!(!transcript.refresh(), "an unchanged store is not news");
}

#[test]
fn a_deactivated_row_rebuilds_the_turns() {
    let store = Store::new("rewrite");
    let project = cwd("rewrite-project");
    store.session("rewrite", &project.to_string_lossy(), BASE, "m", None);
    store.message("rewrite", json!({ "role": "user", "content": "question" }));
    let answer = store.message(
        "rewrite",
        json!({ "role": "assistant", "content": "the abandoned answer" }),
    );
    let mut transcript =
        HermesTranscript::open_at(&store.path(), &project, None).expect("transcript");
    assert_eq!(text(&transcript.turns()[1]), "the abandoned answer");

    // a compaction deactivates the old answer and writes the summary in its place
    store.sql(&format!(
        "UPDATE messages SET active = 0 WHERE id = {answer};
         INSERT INTO messages (session_id, role, content, timestamp) VALUES ('rewrite', 'assistant', 'the surviving answer', {BASE});"
    ));
    assert!(transcript.refresh());
    assert_eq!(text(&transcript.turns()[1]), "the surviving answer");
    assert_eq!(transcript.turns().len(), 2);
}

#[test]
fn the_window_keeps_the_newest_hundred_turns() {
    let store = Store::new("cap");
    let project = cwd("cap-project");
    store.session("cap", &project.to_string_lossy(), BASE, "m", None);
    for index in 0..105 {
        store.message(
            "cap",
            json!({ "role": "user", "content": format!("question {index}"), "timestamp": BASE + index as f64 }),
        );
        store.message(
            "cap",
            json!({ "role": "assistant", "content": format!("answer {index}"), "timestamp": BASE + index as f64 + 0.5 }),
        );
    }
    let transcript = HermesTranscript::open_at(&store.path(), &project, None).expect("transcript");
    assert_eq!(transcript.turns().len(), 100);
    assert_eq!(transcript.turns()[0]["role"], "user");
    // 105 question/answer pairs fold to 210 turns; the window keeps the newest 100, so it
    // opens on the question whose answer is the 100th turn.
    assert_eq!(text(&transcript.turns()[0]), "question 55");
    assert_eq!(text(&transcript.turns()[99]), "answer 104");
}

#[test]
fn an_orphan_tool_result_is_shown_on_an_assistant_turn() {
    let store = Store::new("orphan");
    let project = cwd("orphan-project");
    store.session("orphan", &project.to_string_lossy(), BASE, "m", None);
    store.message(
        "orphan",
        json!({ "role": "tool", "content": "a result whose call is off the page", "tool_name": "bash" }),
    );
    let transcript = HermesTranscript::open_at(&store.path(), &project, None).expect("transcript");
    assert_eq!(transcript.turns().len(), 1);
    assert_eq!(transcript.turns()[0]["role"], "assistant");
    assert_eq!(
        tools(&transcript.turns()[0])[0]["output"],
        "a result whose call is off the page"
    );
}

#[test]
fn the_store_follows_hermes_home() {
    let home = PathBuf::from("C:\\Users\\someone");
    assert_eq!(
        db_path(None, Some(home.clone())),
        Some(home.join(".hermes").join("state.db"))
    );
    assert_eq!(
        db_path(Some("D:\\hermes-home".to_string()), Some(home.clone())),
        Some(PathBuf::from("D:\\hermes-home").join("state.db"))
    );
    assert_eq!(db_path(None, None), None);
}

#[test]
fn reads_the_recorded_reasoning_effort() {
    assert_eq!(
        reasoning_effort(Some(r#"{"reasoning_effort":"high"}"#)),
        Some("high".to_string())
    );
    assert_eq!(
        reasoning_effort(Some(r#"{"reasoning_config":{"enabled":false}}"#)),
        Some("off".to_string())
    );
    assert_eq!(
        reasoning_effort(Some(r#"{"reasoning_config":{"effort":"low"}}"#)),
        Some("low".to_string())
    );
    assert_eq!(
        reasoning_effort(Some(r#"{"reasoning_config":"medium"}"#)),
        Some("medium".to_string())
    );
    assert_eq!(reasoning_effort(Some("{}")), None);
    assert_eq!(reasoning_effort(Some("not json")), None);
    assert_eq!(reasoning_effort(None), None);
}

#[test]
fn a_store_without_the_cwd_column_is_not_bound_to_guess_at() {
    let dir = std::env::temp_dir().join(format!("orion-hermes-legacy-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("state.db");
    let conn = Connection::open(&path).expect("db");
    conn.execute_batch(
        "CREATE TABLE sessions (id TEXT PRIMARY KEY, model TEXT, model_config TEXT, started_at REAL NOT NULL);
         CREATE TABLE messages (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, role TEXT NOT NULL, content TEXT, tool_call_id TEXT, tool_calls TEXT, tool_name TEXT, reasoning TEXT, timestamp REAL NOT NULL, active INTEGER NOT NULL DEFAULT 1, compacted INTEGER NOT NULL DEFAULT 0);",
    )
    .expect("schema");
    conn.execute(
        "INSERT INTO sessions (id, model, started_at) VALUES ('s', 'm', ?1)",
        params![BASE],
    )
    .expect("session");
    drop(conn);
    assert!(HermesTranscript::open_at(&path, &cwd("legacy"), None).is_none());
}
