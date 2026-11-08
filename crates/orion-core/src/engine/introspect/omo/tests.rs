use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::*;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orion-omo-{tag}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn session_folder(cwd: &Path) -> String {
    super::session_folder(&cwd.to_string_lossy())
}

/// Writes one session file, with a header naming `cwd`, into `agent_dir`'s store.
fn write_session(agent_dir: &Path, cwd: &Path, id: &str, entries: &[Value]) -> PathBuf {
    let dir = agent_dir.join("sessions").join(session_folder(cwd));
    fs::create_dir_all(&dir).expect("session dir");
    let path = dir.join(format!("{id}.jsonl"));
    let mut lines = vec![json!({
        "type": "session",
        "version": 3,
        "id": id,
        "timestamp": "2026-10-16T21:05:20.923Z",
        "cwd": cwd.to_string_lossy(),
    })];
    lines.extend(entries.iter().cloned());
    let text = lines
        .iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&path, format!("{text}\n")).expect("session file");
    path
}

fn message(role: &str, content: Value, stop: Option<&str>) -> Value {
    let mut message = json!({ "role": role, "content": content });
    if let Some(stop) = stop {
        message["stopReason"] = json!(stop);
    }
    json!({
        "type": "message",
        "id": uuid::Uuid::new_v4().to_string(),
        "timestamp": "2026-10-16T21:20:06.202Z",
        "message": message,
    })
}

fn text_parts(turn: &Value) -> Vec<&str> {
    turn["parts"]
        .as_array()
        .expect("parts")
        .iter()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect()
}

#[test]
fn parses_the_recorded_omp_fixture() {
    let fixture = Path::new(
        "C:\\herdr-plugins-project\\orbit\\server\\fixtures\\skill-invocations\\omp.jsonl",
    );
    let Ok(text) = fs::read_to_string(fixture) else {
        eprintln!("skipped: Orbit omp fixture not present");
        return;
    };
    let cwd = temp_dir("fixture-cwd");
    let agent_dir = temp_dir("fixture-agent");
    let dir = agent_dir.join("sessions").join(session_folder(&cwd));
    fs::create_dir_all(&dir).expect("session dir");
    let mut lines = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let mut entry: Value = serde_json::from_str(line).expect("fixture line");
        match entry["type"].as_str() {
            // a real store's first line is its session header: the recorded fixture keeps a
            // title line ahead of it from a rewrite, and the binding reads the first line
            Some("session") => {
                entry["cwd"] = json!(cwd.to_string_lossy());
                lines.insert(0, entry.to_string());
            }
            Some("title") => {}
            _ => lines.push(entry.to_string()),
        }
    }
    fs::write(dir.join("fixture.jsonl"), lines.join("\n") + "\n").expect("fixture copy");

    let transcript =
        OmoTranscript::open_in(AgentKind::Omp, &cwd, &agent_dir, None).expect("transcript");
    assert_eq!(
        transcript.session_id(),
        "01a14688-9e1b-746f-8a5e-e42818ac0899"
    );
    let turns = transcript.turns();
    assert_eq!(turns.len(), 2, "turns: {turns:?}");
    assert_eq!(turns[0]["role"], "user");
    assert_eq!(turns[0]["parts"][0]["text"], "/skill:probe-skill hello");
    let skill = &turns[0]["parts"][1]["skill"];
    assert_eq!(skill["name"], "probe-skill");
    assert_eq!(skill["evidence"], "instructions");
    assert_eq!(skill["status"], "loaded");
    assert_eq!(
        skill["path"],
        "/tmp/ulw-allagent.Oyfi/tmp-3/probe-omp/.omp/skills/probe-skill/SKILL.md"
    );
    assert_eq!(turns[1]["role"], "assistant");
    assert_eq!(turns[1]["parts"][0]["text"], "PROBED");
    assert_eq!(transcript.response()["source"], "omp-transcript");
    assert_eq!(
        transcript.response()["history_id"],
        "omo-01a14688-9e1b-746f-8a5e-e42818ac0899"
    );
}

#[test]
fn opens_the_newest_session_of_the_pane_cwd() {
    let cwd = temp_dir("cwd");
    let other = temp_dir("other-cwd");
    let agent_dir = temp_dir("agent");
    write_session(
        &agent_dir,
        &other,
        "ses_other",
        &[message("user", json!("another folder"), None)],
    );
    write_session(
        &agent_dir,
        &cwd,
        "ses_new",
        &[message("user", json!("newest"), None)],
    );
    let transcript =
        OmoTranscript::open_in(AgentKind::Omo, &cwd, &agent_dir, None).expect("transcript");
    assert_eq!(transcript.session_id(), "ses_new");
    assert_eq!(transcript.response()["source"], "omo-transcript");
    assert_eq!(transcript.turns()[0]["parts"][0]["text"], "newest");
}

#[test]
fn session_folder_is_omo_and_omp_s_slug() {
    assert_eq!(
        session_folder(Path::new("/tmp/ulw-allagent.Oyfi/tmp-3/probe-omp")),
        "--tmp-ulw-allagent.Oyfi-tmp-3-probe-omp--"
    );
    assert_eq!(
        session_folder(Path::new("C:\\Users\\me\\app")),
        "--C--Users-me-app--"
    );
}

#[test]
fn tool_calls_pick_up_their_results() {
    let cwd = temp_dir("tool-cwd");
    let agent_dir = temp_dir("tool-agent");
    write_session(
        &agent_dir,
        &cwd,
        "ses_tool",
        &[
            message("user", json!("list the files"), None),
            message(
                "assistant",
                json!([{ "type": "toolCall", "id": "t1", "name": "bash", "arguments": { "command": "ls" } }]),
                Some("toolUse"),
            ),
            json!({
                "type": "message",
                "id": "result-1",
                "timestamp": "2026-10-16T21:20:10.000Z",
                "message": {
                    "role": "toolResult",
                    "toolCallId": "t1",
                    "toolName": "bash",
                    "content": "a.txt\nb.txt",
                    "isError": true,
                },
            }),
            message(
                "assistant",
                json!([{ "type": "text", "text": "two files" }]),
                Some("stop"),
            ),
        ],
    );
    let transcript =
        OmoTranscript::open_in(AgentKind::Omo, &cwd, &agent_dir, None).expect("transcript");
    let turn = &transcript.turns()[1];
    assert_eq!(turn["role"], "assistant");
    let parts = turn["parts"].as_array().expect("parts");
    assert_eq!(parts[0]["kind"], "tool");
    assert_eq!(parts[0]["name"], "bash");
    assert_eq!(parts[0]["output"], "a.txt\nb.txt");
    assert_eq!(parts[0]["error"], true);
    assert_eq!(parts[1]["text"], "two files");
}

#[test]
fn thinking_stays_private_to_its_part() {
    let cwd = temp_dir("thinking-cwd");
    let agent_dir = temp_dir("thinking-agent");
    write_session(
        &agent_dir,
        &cwd,
        "ses_thinking",
        &[message(
            "assistant",
            json!([
                { "type": "thinking", "thinking": "consider" },
                { "type": "text", "text": "answer" }
            ]),
            Some("stop"),
        )],
    );
    let transcript =
        OmoTranscript::open_in(AgentKind::Omo, &cwd, &agent_dir, None).expect("transcript");
    let parts = transcript.turns()[0]["parts"].as_array().expect("parts");
    assert_eq!(parts[0]["kind"], "thinking");
    assert_eq!(parts[0]["text"], "consider");
    assert_eq!(parts[1]["kind"], "text");
    assert_eq!(parts[1]["text"], "answer");
}

#[test]
fn an_assistant_stop_ends_the_turn() {
    let cwd = temp_dir("settle-cwd");
    let agent_dir = temp_dir("settle-agent");
    write_session(
        &agent_dir,
        &cwd,
        "ses_settle",
        &[
            message(
                "assistant",
                json!([{ "type": "text", "text": "first" }]),
                Some("stop"),
            ),
            message(
                "assistant",
                json!([{ "type": "text", "text": "second" }]),
                Some("stop"),
            ),
        ],
    );
    let transcript =
        OmoTranscript::open_in(AgentKind::Omo, &cwd, &agent_dir, None).expect("transcript");
    assert_eq!(transcript.turns().len(), 2, "{:?}", transcript.turns());
    assert_eq!(text_parts(&transcript.turns()[0]), vec!["first"]);
    assert_eq!(text_parts(&transcript.turns()[1]), vec!["second"]);
}

#[test]
fn context_clear_empties_the_conversation() {
    let cwd = temp_dir("clear-cwd");
    let agent_dir = temp_dir("clear-agent");
    write_session(
        &agent_dir,
        &cwd,
        "ses_clear",
        &[
            message("user", json!("old question"), None),
            message(
                "assistant",
                json!([{ "type": "text", "text": "old answer" }]),
                Some("stop"),
            ),
            json!({ "type": "custom", "customType": "context_clear" }),
            message("user", json!("new question"), None),
        ],
    );
    let transcript =
        OmoTranscript::open_in(AgentKind::Omo, &cwd, &agent_dir, None).expect("transcript");
    assert_eq!(transcript.turns().len(), 1, "{:?}", transcript.turns());
    assert_eq!(transcript.turns()[0]["parts"][0]["text"], "new question");
}

#[test]
fn a_task_wake_draws_the_finished_task() {
    let cwd = temp_dir("task-cwd");
    let agent_dir = temp_dir("task-agent");
    write_session(
        &agent_dir,
        &cwd,
        "ses_task",
        &[
            message("user", json!("start a task"), None),
            message(
                "assistant",
                json!([{ "type": "toolCall", "id": "t1", "name": "task", "arguments": { "tasks": [{ "task_summary": "probe it" }] } }]),
                Some("toolUse"),
            ),
            json!({
                "type": "message",
                "id": "result-1",
                "timestamp": "2026-10-16T21:20:10.000Z",
                "message": {
                    "role": "toolResult",
                    "toolCallId": "t1",
                    "toolName": "task",
                    "content": "started",
                    "details": { "items": [{ "task_id": "st_1", "task_summary": "probe it" }] },
                },
            }),
            message(
                "assistant",
                json!([{ "type": "text", "text": "waiting" }]),
                Some("stop"),
            ),
            json!({
                "type": "custom_message",
                "customType": "omo-senpi:wake",
                "display": false,
                "timestamp": "2026-10-16T21:30:00.000Z",
                "details": [{
                    "customType": "senpi-task.completion",
                    "details": [{ "task_id": "st_1", "status": "completed", "name": "child", "agent_type": "general", "final_response": "PROBED", "run_stats": { "runtime_ms": 1234, "turns": 2, "tool_calls": 3, "total_tokens": 50 } }]
                }]
            }),
        ],
    );
    let transcript =
        OmoTranscript::open_in(AgentKind::Omo, &cwd, &agent_dir, None).expect("transcript");
    let last = transcript.turns().last().expect("task turn");
    assert_eq!(last["role"], "user");
    let task = &last["parts"][0]["tasks"][0];
    assert_eq!(last["parts"][0]["kind"], "task_result");
    assert_eq!(task["id"], "st_1");
    assert_eq!(task["title"], "probe it");
    assert_eq!(task["status"], "completed");
    assert_eq!(task["duration_ms"], 1234);
    assert_eq!(task["turns"], 2);
    assert_eq!(task["tool_calls"], 3);
    assert_eq!(task["tokens"], 50);
    assert_eq!(task["result"], "PROBED");
}

#[test]
fn a_last_line_still_without_its_newline_is_read() {
    let cwd = temp_dir("tail-cwd");
    let agent_dir = temp_dir("tail-agent");
    let dir = agent_dir.join("sessions").join(session_folder(&cwd));
    fs::create_dir_all(&dir).expect("session dir");
    // omo writes the answer as one line; a poll between the write and its newline must not
    // hide it until the next append
    let text = [
        json!({
            "type": "session",
            "version": 3,
            "id": "ses_tail",
            "timestamp": "2026-10-16T21:05:20.923Z",
            "cwd": cwd.to_string_lossy(),
        })
        .to_string(),
        message("user", json!("work"), None).to_string(),
        message(
            "assistant",
            json!([{ "type": "text", "text": "finished" }]),
            Some("stop"),
        )
        .to_string(),
    ]
    .join("\n");
    fs::write(dir.join("ses_tail.jsonl"), text).expect("session file");
    let transcript =
        OmoTranscript::open_in(AgentKind::Omo, &cwd, &agent_dir, None).expect("transcript");
    assert_eq!(text_parts(&transcript.turns()[1]), vec!["finished"]);
    assert_eq!(transcript.status(), Some(AgentStatus::Idle));
}

#[test]
fn turn_status_reads_the_tail() {
    let cwd = temp_dir("status-cwd");
    let agent_dir = temp_dir("status-agent");
    write_session(
        &agent_dir,
        &cwd,
        "ses_status",
        &[message("user", json!("work"), None)],
    );
    let mut transcript =
        OmoTranscript::open_in(AgentKind::Omo, &cwd, &agent_dir, None).expect("transcript");
    assert_eq!(transcript.status(), Some(AgentStatus::Working));
    // the store grows while the reader is open: only the appended lines are parsed
    let path = transcript.path().to_path_buf();
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("append");
    use std::io::Write;
    writeln!(
        file,
        "{}",
        message(
            "assistant",
            json!([{ "type": "text", "text": "done" }]),
            Some("stop")
        )
    )
    .expect("append answer");
    drop(file);
    assert!(transcript.refresh());
    assert_eq!(transcript.status(), Some(AgentStatus::Idle));
    assert_eq!(transcript.turns().len(), 2);
}
