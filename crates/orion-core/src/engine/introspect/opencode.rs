//! Reading OpenCode's own transcript (SQLite store).
//!
//! OpenCode 2 keeps every session in one SQLite database,
//! `<data dir>/opencode/opencode.db`, the data dir being `$XDG_DATA_HOME/opencode`,
//! else `~/.local/share/opencode` (macOS too), and `OPENCODE_DB` naming another file.
//! A session is a `session_v2` row and its conversation the `session_message` rows of its id,
//! ordered by `seq`, each a `type` and a JSON `data`.
//!
//! A session is bound to a pane by its `directory` column matching the pane's cwd — Orion has no
//! external session report, so a pane opens the session whose directory is its cwd.
//!
//! The reader is incremental: it remembers the greatest `seq` consumed and only parses what was
//! appended, so a long session costs the same per poll as a short one.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rusqlite::{params, Connection, OpenFlags};
use serde_json::{json, Value};

/// OpenCode's data dir: `$XDG_DATA_HOME/opencode`, else `~/.local/share/opencode`.
fn opencode_data_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME") {
        return Some(PathBuf::from(dir).join("opencode"));
    }
    home().map(|home| home.join(".local/share/opencode"))
}

fn home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
}

/// The database OpenCode writes to: `$XDG_DATA_HOME/opencode/opencode.db`, or `OPENCODE_DB` when
/// that names a file.
fn opencode_db() -> Option<PathBuf> {
    if let Some(named) = std::env::var_os("OPENCODE_DB") {
        return Some(PathBuf::from(named));
    }
    Some(opencode_data_dir()?.join("opencode.db"))
}

/// One parsed row as the chat needs it.
#[derive(Debug, Clone)]
enum Record {
    User {
        ts: Option<String>,
        parts: Vec<Value>,
    },
    Assistant {
        ts: Option<String>,
        end: Option<String>,
        parts: Vec<Value>,
    },
    Idle,
    Other,
}

/// Which store layout this database uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flavor {
    /// OpenCode 2.x: `session_v2` and `session_message`.
    V2,
    /// OpenCode 1.x: `session`, `message` and `part`.
    V1,
}

/// A session file plus everything parsed from it so far.
pub struct OpencodeTranscript {
    path: PathBuf,
    conn: Connection,
    flavor: Flavor,
    session_id: String,
    history_id: String,
    /// Greatest `seq` consumed; only rows past it are parsed on refresh.
    offset: i64,
    turns: Vec<Value>,
    signature: String,
}

impl OpencodeTranscript {
    /// Opens the session whose `directory` matches `cwd` in OpenCode's store.
    pub fn open(cwd: &Path, _since: Option<SystemTime>) -> Option<Self> {
        Self::open_at(&opencode_db()?, cwd, _since)
    }

    /// Opens a specific store, which is how the fixture tests replay a recorded database.
    pub fn open_at(db_path: &Path, cwd: &Path, _since: Option<SystemTime>) -> Option<Self> {
        let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
        let flavor = store_flavor(&conn)?;
        let cwd_text = cwd.to_string_lossy().to_string();
        let session_id: String = match flavor {
            Flavor::V2 => conn
                .query_row(
                    "SELECT id FROM session_v2 WHERE directory = ?1 LIMIT 1",
                    params![cwd_text],
                    |row| row.get(0),
                )
                .ok()?,
            Flavor::V1 => conn
                .query_row(
                    "SELECT id FROM session WHERE directory = ?1 LIMIT 1",
                    params![cwd_text],
                    |row| row.get(0),
                )
                .ok()?,
        };
        let mut transcript = Self {
            path: db_path.to_path_buf(),
            conn,
            flavor,
            session_id: session_id.clone(),
            history_id: format!("opencode-{}", session_id),
            offset: 0,
            turns: Vec::new(),
            signature: String::new(),
        };
        transcript.refresh();
        Some(transcript)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn signature(&self) -> &str {
        &self.signature
    }

    pub fn turns(&self) -> &[Value] {
        &self.turns
    }

    /// Ingests everything appended since the last call. Returns whether the transcript changed.
    pub fn refresh(&mut self) -> bool {
        match self.flavor {
            Flavor::V2 => self.refresh_v2(),
            Flavor::V1 => self.refresh_v1(),
        }
    }

    fn refresh_v2(&mut self) -> bool {
        let mut records: Vec<Record> = Vec::new();
        let mut max_seq = self.offset;
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, type, data, seq FROM session_message WHERE session_id = ?1 AND seq > ?2 ORDER BY seq",
        ) else {
            return false;
        };
        let Ok(mut rows) = stmt.query(params![&self.session_id, self.offset]) else {
            return false;
        };
        while let Ok(Some(row)) = rows.next() {
            let rtype: String = row.get(1).unwrap_or_default();
            let data: String = row.get(2).unwrap_or_default();
            let seq: i64 = row.get(3).unwrap_or(0);
            if seq > max_seq {
                max_seq = seq;
            }
            if let Some(record) = opencode_record(&rtype, &data) {
                records.push(record);
            }
        }
        drop(rows);
        drop(stmt);
        let mut changed = false;
        for record in records {
            if ingest(&mut self.turns, &record) {
                changed = true;
            }
        }
        self.offset = max_seq;
        if changed {
            self.signature = format!("{}:{}", self.history_id, self.turns.len());
            while self.turns.len() > 100 {
                self.turns.remove(0);
            }
        }
        changed
    }

    /// The 1.x store has no `seq`: a change to any message or part rebuilds the turns from the
    /// pair of tables, and an unchanged store costs one aggregate query.
    fn refresh_v1(&mut self) -> bool {
        let Ok(signature) = self.v1_signature() else {
            return false;
        };
        if signature == self.signature {
            return false;
        }
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, data FROM message WHERE session_id = ?1 ORDER BY time_created, id",
        ) else {
            return false;
        };
        let Ok(mut rows) = stmt.query(params![&self.session_id]) else {
            return false;
        };
        let mut messages: Vec<(String, String)> = Vec::new();
        while let Ok(Some(row)) = rows.next() {
            let id: String = row.get(0).unwrap_or_default();
            let data: String = row.get(1).unwrap_or_default();
            messages.push((id, data));
        }
        drop(rows);
        drop(stmt);
        let mut turns: Vec<Value> = Vec::new();
        for (id, data) in messages {
            let Ok(value) = serde_json::from_str::<Value>(&data) else {
                continue;
            };
            let parts = self.v1_parts(&id);
            if let Some(record) = adapt_v1_message(&value, &parts) {
                ingest(&mut turns, &record);
            }
        }
        while turns.len() > 100 {
            turns.remove(0);
        }
        self.turns = turns;
        self.signature = signature;
        true
    }

    fn v1_parts(&self, message_id: &str) -> Vec<Value> {
        let Ok(mut stmt) = self
            .conn
            .prepare("SELECT data FROM part WHERE message_id = ?1 ORDER BY time_created, id")
        else {
            return Vec::new();
        };
        let Ok(mut rows) = stmt.query(params![message_id]) else {
            return Vec::new();
        };
        let mut parts = Vec::new();
        while let Ok(Some(row)) = rows.next() {
            let data: String = row.get(0).unwrap_or_default();
            if let Ok(value) = serde_json::from_str::<Value>(&data) {
                parts.push(value);
            }
        }
        parts
    }

    fn v1_signature(&self) -> rusqlite::Result<String> {
        self.conn.query_row(
            "SELECT (SELECT count(*) FROM message WHERE session_id = ?1), (SELECT COALESCE(max(time_updated), 0) FROM message WHERE session_id = ?1), (SELECT count(*) FROM part WHERE session_id = ?1), (SELECT COALESCE(max(time_updated), 0) FROM part WHERE session_id = ?1)",
            params![&self.session_id],
            |row| {
                let messages: i64 = row.get(0)?;
                let updated: i64 = row.get(1)?;
                let parts: i64 = row.get(2)?;
                let part_updated: i64 = row.get(3)?;
                Ok(format!(
                    "{}:{messages}:{updated}:{parts}:{part_updated}",
                    self.history_id
                ))
            },
        )
    }

    pub fn response(&self) -> Value {
        let mut body = serde_json::Map::new();
        body.insert(
            "source".to_string(),
            Value::String("opencode-transcript".to_string()),
        );
        body.insert("turns".to_string(), Value::Array(self.turns.clone()));
        body.insert(
            "history_id".to_string(),
            Value::String(self.history_id.clone()),
        );
        Value::Object(body)
    }
}

/// A borrowed JSON value, cloned so it outlives the borrow of `data`.
fn record(value: &Value) -> Value {
    value.clone()
}

/// Which store layout the database has: 2.x (`session_v2`), 1.x (`session`/`message`/`part`), or
/// something this reader does not know.
fn store_flavor(conn: &Connection) -> Option<Flavor> {
    let has = |name: &str| {
        conn.query_row(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![name],
            |row| row.get::<_, String>(0),
        )
        .is_ok()
    };
    if has("session_v2") {
        return Some(Flavor::V2);
    }
    if has("session") && has("message") && has("part") {
        return Some(Flavor::V1);
    }
    None
}

/// A 1.x message and its parts as the 2.x row shape [`opencode_record`] reads.
fn adapt_v1_message(data: &Value, parts: &[Value]) -> Option<Record> {
    match data.get("role").and_then(Value::as_str)? {
        "user" => {
            let mut prompt = Vec::new();
            let mut files = Vec::new();
            for part in parts {
                match part.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            prompt.push(text.to_string());
                        }
                    }
                    Some("file") => {
                        if let Some(url) = part.get("url").and_then(Value::as_str) {
                            files.push(json!({
                                "type": "file",
                                "uri": url,
                                "mime": part.get("mime").cloned().unwrap_or(Value::Null),
                            }));
                        }
                    }
                    _ => {}
                }
            }
            let row = json!({
                "time": data.get("time").cloned().unwrap_or(Value::Null),
                "text": prompt.join("\n"),
                "files": files,
                "skills": [],
            });
            opencode_record("user", &row.to_string())
        }
        "assistant" => {
            let mut content = Vec::new();
            for part in parts {
                match part.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            content.push(json!({ "type": "text", "text": text }));
                        }
                    }
                    Some("reasoning") => {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            content.push(json!({ "type": "reasoning", "text": text }));
                        }
                    }
                    Some("tool") => {
                        let name = part.get("tool").and_then(Value::as_str).unwrap_or("tool");
                        let state = part.get("state").cloned().unwrap_or(Value::Null);
                        content.push(json!({
                            "type": "tool",
                            "name": name,
                            "state": {
                                "status": state.get("status").cloned().unwrap_or(Value::Null),
                                "input": state.get("input").cloned().unwrap_or(Value::Null),
                                "metadata": state.get("metadata").cloned().unwrap_or(Value::Null),
                                "error": state.get("error").cloned().unwrap_or(Value::Null),
                            },
                        }));
                    }
                    _ => {}
                }
            }
            let row = json!({
                "time": data.get("time").cloned().unwrap_or(Value::Null),
                "content": content,
                "finish": data.get("finish").cloned().unwrap_or(Value::Null),
                "error": data.get("error").cloned().unwrap_or(Value::Null),
                "tokens": data.get("tokens").cloned().unwrap_or(Value::Null),
            });
            opencode_record("assistant", &row.to_string())
        }
        _ => None,
    }
}

/// OpenCode 1.17.x records a skill as the expanded body, the base-directory line, the fixed
/// relative-path sentence and the arguments. The chat shows the command the user typed instead;
/// an incomplete scaffold stays raw, and no filesystem path is followed.
pub fn skill_invocation(prompt: &str) -> String {
    const MARKER: &str = "\n\nBase directory for this skill: ";
    const RELATIVE: &str = "Relative paths in this skill (e.g., scripts/, references/) are relative to this base directory.";
    let Some(marker) = prompt.rfind(MARKER) else {
        return prompt.to_string();
    };
    let rest = &prompt[marker + MARKER.len()..];
    let Some((directory, rest)) = next_line(rest) else {
        return prompt.to_string();
    };
    let Some((relative, rest)) = next_line(rest) else {
        return prompt.to_string();
    };
    if relative != RELATIVE {
        return prompt.to_string();
    }
    let Some(args) = rest
        .strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))
    else {
        return prompt.to_string();
    };
    let name = directory
        .replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string();
    if name.is_empty() || name.len() > 64 || !is_skill_name(&name) {
        return prompt.to_string();
    }
    if args.is_empty() {
        format!("/{name}")
    } else {
        format!("/{name} {args}")
    }
}

fn next_line(text: &str) -> Option<(&str, &str)> {
    let end = text.find(['\r', '\n'])?;
    let line = &text[..end];
    let rest = text[end..]
        .strip_prefix("\r\n")
        .or_else(|| text[end..].strip_prefix('\n'))?;
    Some((line, rest))
}

/// `^[a-z0-9]+(-[a-z0-9]+)*$`
fn is_skill_name(name: &str) -> bool {
    let mut start = true;
    for character in name.chars() {
        if character == '-' {
            if start {
                return false;
            }
            start = true;
        } else if character.is_ascii_lowercase() || character.is_ascii_digit() {
            start = false;
        } else {
            return false;
        }
    }
    !start
}

/// Parses one `session_message` row into a record the chat can draw.
fn opencode_record(rtype: &str, data: &str) -> Option<Record> {
    let value: Value = serde_json::from_str(data).ok()?;
    match rtype {
        "user" => {
            let time = value
                .get("time")
                .and_then(|t| t.get("created").and_then(Value::as_i64));
            let ts = time.map(|t| {
                chrono::DateTime::from_timestamp_millis(t)
                    .unwrap()
                    .to_rfc3339()
            });
            let mut parts: Vec<Value> = Vec::new();
            // images the user pasted
            if let Some(files) = value.get("files").and_then(Value::as_array) {
                for (index, file) in files.iter().enumerate() {
                    let file = record(file);
                    let uri = file.get("uri").and_then(Value::as_str)?;
                    let (media_type, base64) = uri
                        .split_once(";base64,")
                        .map(|(m, b)| (m.to_string(), b.to_string()))
                        .unwrap_or_else(|| {
                            (
                                file.get("mime")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string(),
                                String::new(),
                            )
                        });
                    if !base64.is_empty() {
                        parts.push(json!({
                            "kind": "image",
                            "media_type": media_type,
                            "ref": format!("opencode:{}", index),
                        }));
                    }
                }
            }
            // the question text, or the command a recorded skill invocation expands to
            let display = value
                .get("metadata")
                .and_then(|m| m.get("displayText").and_then(Value::as_str));
            let raw = value.get("text").and_then(Value::as_str);
            let text = display
                .map(str::to_string)
                .or_else(|| raw.map(skill_invocation))
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty());
            if let Some(text) = text {
                parts.push(json!({ "kind": "text", "text": text }));
            }
            // skills named in the prompt are loaded with it
            if let Some(skills) = value.get("skills").and_then(Value::as_array) {
                for skill in skills {
                    let skill = record(skill);
                    let name = skill
                        .get("name")
                        .or_else(|| skill.get("id"))
                        .and_then(Value::as_str);
                    if let Some(name) = name.filter(|n| !n.is_empty()) {
                        parts.push(json!({
                            "kind": "skill",
                            "skill": {
                                "name": name,
                                "evidence": "instructions",
                                "status": "loaded",
                            },
                        }));
                    }
                }
            }
            if parts.is_empty() {
                return None;
            }
            Some(Record::User { ts, parts })
        }
        "assistant" => {
            let time = value
                .get("time")
                .and_then(|t| t.get("created").and_then(Value::as_i64));
            let completed = value
                .get("time")
                .and_then(|t| t.get("completed").and_then(Value::as_i64));
            let ts = time.map(|t| {
                chrono::DateTime::from_timestamp_millis(t)
                    .unwrap()
                    .to_rfc3339()
            });
            let end = completed.map(|t| {
                chrono::DateTime::from_timestamp_millis(t)
                    .unwrap()
                    .to_rfc3339()
            });
            let mut parts: Vec<Value> = Vec::new();
            if let Some(content) = value.get("content").and_then(Value::as_array) {
                for block in content {
                    let block = record(block);
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(text) = block.get("text").and_then(Value::as_str) {
                                if !text.is_empty() {
                                    parts.push(json!({ "kind": "text", "text": text }));
                                }
                            }
                        }
                        Some("reasoning") => {
                            if let Some(text) = block.get("text").and_then(Value::as_str) {
                                if !text.is_empty() {
                                    parts.push(json!({ "kind": "thinking", "text": text }));
                                }
                            }
                        }
                        Some("tool") => {
                            let name = block
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("tool")
                                .to_string();
                            let state = record(block.get("state").unwrap_or(&Value::Null));
                            let input = record(state.get("input").unwrap_or(&Value::Null));
                            let summary = tool_summary(&name, &input);
                            let mut part = json!({
                                "kind": "tool",
                                "name": name,
                                "summary": summary,
                                "input": if input.is_object()
                                    && !input.as_object().map(|m| m.is_empty()).unwrap_or(true)
                                {
                                    serde_json::to_string_pretty(&input).unwrap_or_default()
                                } else if input.is_string() {
                                    input.as_str().unwrap_or_default().to_string()
                                } else {
                                    String::new()
                                },
                                "output": "",
                            });
                            if part["summary"].as_str().is_none_or(|s| s.is_empty()) {
                                part["summary"] = json!(name);
                            }
                            parts.push(part);
                        }
                        _ => {}
                    }
                }
            }
            if parts.is_empty() {
                return None;
            }
            Some(Record::Assistant { ts, end, parts })
        }
        "idle" => Some(Record::Idle),
        _ => Some(Record::Other),
    }
}

/// Folds rows into turns: consecutive assistant rows merge into one turn until an `idle` row
/// ends it; whatever woke the model after it starts its own turn.
fn ingest(turns: &mut Vec<Value>, record: &Record) -> bool {
    match record {
        Record::User { ts, parts } => {
            turns.push(json!({ "role": "user", "ts": ts, "parts": parts }));
            true
        }
        Record::Assistant { ts, end, parts } => {
            let merged = turns
                .last()
                .and_then(|t| t.get("role").and_then(Value::as_str))
                == Some("assistant");
            if merged {
                let index = turns.len() - 1;
                let parts_arr = turns[index]
                    .get_mut("parts")
                    .and_then(Value::as_array_mut)
                    .expect("parts");
                parts_arr.extend(parts.clone());
                if let Some(end) = end {
                    turns[index]["end_ts"] = json!(end);
                }
            } else {
                turns.push(json!({ "role": "assistant", "ts": ts, "parts": parts }));
                if let Some(end) = end {
                    turns.last_mut().unwrap()["end_ts"] = json!(end);
                }
            }
            true
        }
        Record::Idle => true,
        Record::Other => false,
    }
}

/// The `tool` call's output summary, same shape as Orbit's `toolSummary`.
fn tool_summary(name: &str, args: &Value) -> String {
    if name.starts_with("request_user_input") {
        if let Some(questions) = args.get("questions").and_then(Value::as_array) {
            let mut titles: Vec<String> = Vec::new();
            for q in questions {
                let title = q
                    .get("title")
                    .and_then(Value::as_str)
                    .or_else(|| q.get("question").and_then(Value::as_str));
                if let Some(title) = title.filter(|t| !t.is_empty()) {
                    titles.push(title.to_string());
                }
            }
            if !titles.is_empty() {
                return titles.join(" · ");
            }
        }
    }
    for key in [
        "cmd",
        "command",
        "file_path",
        "path",
        "pattern",
        "description",
        "url",
    ] {
        if let Some(value) = args.get(key).and_then(Value::as_str) {
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> Value {
        json!({ "time": { "created": 1792184434000i64 }, "text": text, "files": [], "agents": [] })
    }

    fn assistant(text: &str) -> Value {
        json!({
            "time": { "created": 1792184434000i64, "completed": 1792184435000i64 },
            "agent": "build",
            "model": { "id": "claude-haiku-4-5", "providerID": "ccapi", "variant": "default" },
            "content": [{ "type": "text", "text": text }],
            "finish": "stop",
            "tokens": { "input": 10, "output": 5, "reasoning": 0, "cache": { "read": 100, "write": 0 } },
        })
    }

    fn transcript_with_rows(rows: &[(&str, &str, Value)]) -> OpencodeTranscript {
        let dir = std::env::temp_dir().join(format!("orion-opencode-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("dir");
        let db_path = dir.join("opencode.db");
        let conn = Connection::open_with_flags(
            &db_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )
        .expect("db");
        conn.execute_batch(
            "CREATE TABLE session_v2 (id text PRIMARY KEY, project_id text NOT NULL, slug text NOT NULL, directory text NOT NULL, title text NOT NULL, version text NOT NULL, revert text, time_created integer NOT NULL, time_updated integer NOT NULL);
             CREATE TABLE session_message (id text PRIMARY KEY, session_id text NOT NULL, type text NOT NULL, seq integer NOT NULL, time_created integer NOT NULL, time_updated integer NOT NULL, data text NOT NULL);
             CREATE UNIQUE INDEX session_message_session_seq_idx ON session_message (session_id, seq);",
        )
        .expect("schema");
        conn.execute(
            "INSERT INTO session_v2 (id, project_id, slug, directory, title, version, time_created, time_updated) VALUES (?1, 'global', 'slug', ?2, 'Test', '2.0.24', 1792184434000, 1792184434000)",
            ["ses_test", "/tmp/ulw-allagent.Oyfi/tmp-3/probe-opencode"],
        )
        .expect("session");
        for (seq, (id, rtype, data)) in (1i64..).zip(rows.iter()) {
            conn.execute(
                "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data) VALUES (?1, 'ses_test', ?2, ?3, 1792184434000, 1792184434000, ?4)",
                params![id, rtype, seq, serde_json::to_string(data).unwrap()],
                        ).expect("m1");
        }
        drop(conn);
        let conn =
            Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("ro");
        OpencodeTranscript {
            path: db_path.clone(),
            conn,
            flavor: Flavor::V2,
            session_id: "ses_test".to_string(),
            history_id: "opencode-ses_test".to_string(),
            offset: 0,
            turns: Vec::new(),
            signature: String::new(),
        }
    }

    #[test]
    fn parses_user_assistant_and_idle() {
        let mut transcript = transcript_with_rows(&[
            ("m1", "user", user("inspect this")),
            ("m2", "assistant", assistant("Clean.")),
            (
                "m3",
                "idle",
                json!({ "time": { "created": 1792184436000i64 }, "outcome": "succeeded" }),
            ),
        ]);
        assert!(transcript.refresh());
        assert_eq!(transcript.turns.len(), 2);
        assert_eq!(transcript.turns[0]["role"], "user");
        assert_eq!(transcript.turns[1]["role"], "assistant");
        assert_eq!(transcript.turns[1]["parts"][0]["text"], "Clean.");
    }

    #[test]
    fn incremental_read_only_sees_new_rows() {
        let dir = std::env::temp_dir().join(format!("orion-opencode-inc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("dir");
        let db_path = dir.join("opencode.db");
        let conn = Connection::open_with_flags(
            &db_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )
        .expect("db");
        conn.execute_batch(
            "CREATE TABLE session_v2 (id text PRIMARY KEY, project_id text NOT NULL, slug text NOT NULL, directory text NOT NULL, title text NOT NULL, version text NOT NULL, revert text, time_created integer NOT NULL, time_updated integer NOT NULL);
             CREATE TABLE session_message (id text PRIMARY KEY, session_id text NOT NULL, type text NOT NULL, seq integer NOT NULL, time_created integer NOT NULL, time_updated integer NOT NULL, data text NOT NULL);
             CREATE UNIQUE INDEX session_message_session_seq_idx ON session_message (session_id, seq);",
        )
        .expect("schema");
        conn.execute(
            "INSERT INTO session_v2 (id, project_id, slug, directory, title, version, time_created, time_updated) VALUES (?1, 'global', 'slug', ?2, 'Test', '2.0.24', 1792184434000, 1792184434000)",
            ["ses_inc", "/tmp/inc"],
        )
        .expect("session");
        conn.execute(
            "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data) VALUES (?1, 'ses_inc', 'user', 1, 1792184434000, 1792184434000, ?2)",
            params![
                "m1",
                serde_json::to_string(&json!({ "time": { "created": 1792184434000i64 }, "text": "first", "files": [], "agents": [] })).unwrap()
            ],
        )
        .expect("m1");
        drop(conn);
        let conn =
            Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("ro");
        let mut transcript = OpencodeTranscript {
            path: db_path.clone(),
            conn,
            flavor: Flavor::V2,
            session_id: "ses_inc".to_string(),
            history_id: "opencode-ses_inc".to_string(),
            offset: 0,
            turns: Vec::new(),
            signature: String::new(),
        };
        assert!(transcript.refresh());
        assert_eq!(transcript.turns.len(), 1);

        // append a second row
        {
            let conn = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_WRITE)
                .expect("rw");
            conn.execute(
                "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data) VALUES (?1, 'ses_inc', 'assistant', 2, 1792184435000, 1792184435001, ?2)",
                params![
                    "m2",
                    serde_json::to_string(&json!({
                        "time": { "created": 1792184435000i64, "completed": 1792184435001i64 },
                        "agent": "build",
                        "model": { "id": "x", "providerID": "p", "variant": "v" },
                        "content": [{ "type": "text", "text": "second" }],
                        "finish": "stop",
                        "tokens": { "input": 1, "output": 1, "reasoning": 0, "cache": { "read": 0, "write": 0 } },
                    })).unwrap(),
                ],
            )
            .expect("m2");
        }
        let conn =
            Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("ro");
        transcript.conn = conn;
        assert!(transcript.refresh());
        assert_eq!(transcript.turns.len(), 2);
        assert_eq!(transcript.turns[1]["parts"][0]["text"], "second");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn assistant_rows_merge_until_idle() {
        let mut transcript = transcript_with_rows(&[
            ("m1", "assistant", assistant("part one")),
            ("m2", "assistant", assistant("part two")),
            (
                "m3",
                "idle",
                json!({ "time": { "created": 1792184437000i64 }, "outcome": "succeeded" }),
            ),
        ]);
        assert!(transcript.refresh());
        assert_eq!(transcript.turns.len(), 1);
        let parts = transcript.turns[0]["parts"].as_array().expect("parts");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["text"], "part one");
        assert_eq!(parts[1]["text"], "part two");
    }

    #[test]
    fn skill_invocation_restores_the_typed_command() {
        let prefix = "Body\n\nBase directory for this skill: C:\\missing\\probe-skill\nRelative paths in this skill (e.g., scripts/, references/) are relative to this base directory.\n\n";
        assert_eq!(skill_invocation("plain text"), "plain text");
        assert_eq!(skill_invocation("/probe-skill hello"), "/probe-skill hello");
        assert_eq!(skill_invocation(prefix), "/probe-skill");
        assert_eq!(
            skill_invocation(&format!("{prefix}one\ntwo")),
            "/probe-skill one\ntwo"
        );
        // An incomplete scaffold stays raw: no guessing what it was.
        for prompt in [
            "Body\n\nBase directory for this skill: /tmp/probe-skill\n\nhello",
            "Body\n\nBase directory for this skill: /tmp/Bad\nRelative paths in this skill (e.g., scripts/, references/) are relative to this base directory.\n\nhello",
        ] {
            assert_eq!(skill_invocation(prompt), prompt);
        }
    }

    #[test]
    fn parses_orbit_opencode_fixture() {
        let path = std::env::var("ORBIT_OPENCODE_FIXTURE")
            .unwrap_or_else(|_| "fixtures/opencode.jsonl".to_string());
        let fixture = std::path::Path::new(&path);
        let Ok(text) = std::fs::read_to_string(fixture) else {
            eprintln!("skipped: Orbit opencode fixture not present");
            return;
        };
        let dir =
            std::env::temp_dir().join(format!("orion-opencode-fixture-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("dir");
        let db_path = dir.join("opencode.db");
        let conn = Connection::open_with_flags(
            &db_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )
        .expect("db");
        conn.execute_batch(
            "CREATE TABLE session (id text PRIMARY KEY, directory text NOT NULL, revert text, time_created integer NOT NULL, time_updated integer NOT NULL);
             CREATE TABLE message (id text PRIMARY KEY, session_id text NOT NULL, time_created integer NOT NULL, time_updated integer NOT NULL, data text NOT NULL);
             CREATE TABLE part (id text PRIMARY KEY, message_id text NOT NULL, session_id text NOT NULL, time_created integer NOT NULL, time_updated integer NOT NULL, data text NOT NULL);",
        )
        .expect("schema");
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let entry: Value = serde_json::from_str(line).expect("fixture line");
            let table = entry
                .get("table")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let row = entry.get("row").cloned().unwrap_or(Value::Null);
            match table {
                "session" => {
                    conn.execute(
                        "INSERT INTO session (id, directory, revert, time_created, time_updated) VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![
                            row.get("id").and_then(Value::as_str),
                            row.get("directory").and_then(Value::as_str),
                            row.get("revert").and_then(Value::as_str),
                            row.get("time_created").and_then(Value::as_i64),
                            row.get("time_updated").and_then(Value::as_i64),
                        ],
                    )
                    .expect("session");
                }
                "message" => {
                    conn.execute(
                        "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![
                            row.get("id").and_then(Value::as_str),
                            row.get("session_id").and_then(Value::as_str),
                            row.get("time_created").and_then(Value::as_i64),
                            row.get("time_updated").and_then(Value::as_i64),
                            row.get("data").and_then(Value::as_str),
                        ],
                    )
                    .expect("message");
                }
                "part" => {
                    conn.execute(
                        "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![
                            row.get("id").and_then(Value::as_str),
                            row.get("message_id").and_then(Value::as_str),
                            row.get("session_id").and_then(Value::as_str),
                            row.get("time_created").and_then(Value::as_i64),
                            row.get("time_updated").and_then(Value::as_i64),
                            row.get("data").and_then(Value::as_str),
                        ],
                    )
                    .expect("part");
                }
                _ => {}
            }
        }
        drop(conn);

        let session_dir = std::path::Path::new("/tmp/ulw-allagent.Oyfi/tmp-3/probe-opencode");
        let transcript =
            OpencodeTranscript::open_at(&db_path, session_dir, None).expect("transcript");
        let turns = transcript.turns();
        assert_eq!(turns.len(), 2, "turns: {turns:?}");
        assert_eq!(turns[0]["role"], "user");
        assert_eq!(turns[0]["parts"][0]["text"], "/probe-skill hello");
        assert_eq!(turns[1]["role"], "assistant");
        let texts = turns[1]["parts"]
            .as_array()
            .expect("parts")
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert!(texts.contains(&"PROBED"), "assistant parts: {texts:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
