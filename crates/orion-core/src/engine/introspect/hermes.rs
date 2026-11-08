//! Reading Hermes's own transcript.
//!
//! Hermes keeps one SQLite store per installation: `<HERMES_HOME>/state.db` (default
//! `~/.hermes/state.db`). A session is a row in `sessions` naming its `cwd`; its transcript is the
//! `messages` rows that are `active` or were folded into a compaction (`compacted`). An assistant
//! row can carry reasoning and a list of tool calls, a `tool` row answers one by `tool_call_id`,
//! and a user row holds whatever was typed — including the whole scaffold Hermes records when a
//! skill is invoked.
//!
//! A pane is bound to the newest session of its cwd whose `started_at` is at or after the pane
//! started. Hermes's `terminal-sessions` breadcrumb (Orbit's fallback when no session is reported)
//! is keyed by a terminal device Orion's Windows PTYs do not have, so a store without a `cwd`
//! column is left to the terminal rather than guessed at.

use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rusqlite::types::ValueRef;
use rusqlite::{params, Connection, OpenFlags, Row};
use serde_json::{json, Value};

use crate::protocol::AgentStatus;

use super::omo::tool_summary;

/// Enough turns for a conversation; older turns fall away as a session grows.
const MAX_TURNS: usize = 100;
/// The newest rows the first read folds; a session longer than this keeps its recent turns.
const MAX_ROWS: usize = 5_000;
/// A tool output longer than this is cut; the whole text still lives in the database.
const TOOL_OUTPUT_CHARS: usize = 64_000;
/// A tool-call list the page budget would have omitted, leaving only the fact that it was large.
const PAGE_TOOL_CALLS_BYTES: usize = 4 * 1024 * 1024;
/// Tools whose output is a JSON envelope `{output, exit_code, error}` rather than plain text.
const TERMINAL_TOOLS: [&str; 6] = ["bash", "terminal", "sh", "exec", "execute_command", "shell"];

/// One message row, read whole: the page budget Orbit applies is not needed for a local store.
struct HermesRow {
    id: i64,
    role: String,
    content: Option<String>,
    tool_call_id: Option<String>,
    tool_calls: Option<String>,
    tool_name: Option<String>,
    reasoning: Option<String>,
    timestamp: f64,
}

/// One Hermes session's rows, folded into the chat's turns.
pub struct HermesTranscript {
    path: PathBuf,
    conn: Connection,
    session_id: String,
    history_id: String,
    model: Option<String>,
    reasoning_effort: Option<String>,
    turns: Vec<Value>,
    /// Tool calls still waiting for their result: id -> (turn index, part index).
    pending: HashMap<String, (usize, usize)>,
    /// The last visible row and the shape of the visible set, so an append reads on and a
    /// compaction (which deactivates rows) rebuilds.
    last_id: i64,
    visible_count: i64,
    visible_sum: i64,
    last_role: Option<String>,
    status: Option<AgentStatus>,
}

/// The `state.db` inside a Hermes home: `HERMES_HOME` when set, else `~/.hermes`.
pub(crate) fn db_path(home: Option<String>, user_home: Option<PathBuf>) -> Option<PathBuf> {
    let home = match home.filter(|home| !home.is_empty()) {
        Some(home) => PathBuf::from(home),
        None => user_home?.join(".hermes"),
    };
    Some(home.join("state.db"))
}

/// The reasoning effort a session's `model_config` records, if it records one.
pub(crate) fn reasoning_effort(model_config: Option<&str>) -> Option<String> {
    let config: Value = serde_json::from_str(model_config?).ok()?;
    match config.get("reasoning_config") {
        Some(Value::Object(settings)) => {
            if settings.get("enabled").and_then(Value::as_bool) == Some(false) {
                Some("off".to_string())
            } else {
                settings
                    .get("effort")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            }
        }
        Some(Value::String(effort)) => Some(effort.clone()),
        _ => config
            .get("reasoning_effort")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

impl HermesTranscript {
    /// Opens the session of `cwd` that this pane is in, out of the store Hermes writes.
    pub fn open(cwd: &Path, since: Option<SystemTime>) -> Option<Self> {
        let home = env::var("HERMES_HOME").ok().filter(|home| !home.is_empty());
        Self::open_at(&db_path(home, user_home())?, cwd, since)
    }

    /// Opens a specific store, which is how the fixture tests replay a recorded database.
    pub fn open_at(db_path: &Path, cwd: &Path, since: Option<SystemTime>) -> Option<Self> {
        let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
        if !has_table(&conn, "sessions") || !has_table(&conn, "messages") {
            return None;
        }
        // Hermes records the directory a session ran in. Without that evidence, binding a
        // session to a pane would be a guess shared by every pane in the store.
        if !has_column(&conn, "sessions", "cwd") {
            return None;
        }
        let floor_ms = since
            .and_then(|since| since.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|since| since.as_millis() as i64)
            .unwrap_or(0)
            // Hermes writes `started_at` in whole seconds; a pane's start is finer than that.
            .saturating_sub(2_000);
        let cwd_text = cwd.to_string_lossy().to_string();
        let mut statement = conn
            .prepare(
                "SELECT id, cwd, started_at, model, model_config FROM sessions
                 WHERE started_at * 1000 >= ?1 ORDER BY started_at DESC LIMIT 500",
            )
            .ok()?;
        let mut rows = statement.query(params![floor_ms]).ok()?;
        let mut chosen: Option<(String, f64, Option<String>, Option<String>)> = None;
        while let Ok(Some(row)) = rows.next() {
            let stored = row.get::<_, String>(1).unwrap_or_default();
            if !same_directory(&stored, &cwd_text) {
                continue;
            }
            let id = row.get::<_, String>(0).unwrap_or_default();
            if id.is_empty() {
                continue;
            }
            let started_at = numeric(row, 2).unwrap_or(0.0);
            let model = row.get::<_, Option<String>>(3).ok().flatten();
            let model_config = row.get::<_, Option<String>>(4).ok().flatten();
            chosen = Some((id, started_at, model, model_config));
            break;
        }
        drop(rows);
        drop(statement);
        let (session_id, started_at, model, model_config) = chosen?;

        let visible: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id = ?1 AND (active = 1 OR compacted = 1)",
                params![&session_id],
                |row| row.get(0),
            )
            .ok()?;
        if visible == 0 {
            return None;
        }

        let mut transcript = Self {
            path: db_path.to_path_buf(),
            conn,
            history_id: format!(
                "hermes:{session_id}:{}",
                (started_at * 1000.0).round() as i64
            ),
            session_id,
            model,
            reasoning_effort: reasoning_effort(model_config.as_deref()),
            turns: Vec::new(),
            pending: HashMap::new(),
            last_id: 0,
            visible_count: 0,
            visible_sum: 0,
            last_role: None,
            status: None,
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

    /// Ingests everything changed since the last call. Returns whether the chat changed.
    pub fn refresh(&mut self) -> bool {
        let Some((max_id, count, sum)) = self.visible() else {
            return false;
        };
        let mut changed = false;
        let appended = self.last_id > 0
            && max_id > self.last_id
            && count >= self.visible_count
            && sum >= self.visible_sum;
        if appended {
            let rows = self.rows_after(self.last_id);
            let added = rows.len() as i64;
            let added_sum: i64 = rows.iter().map(|row| row.id).sum();
            if count == self.visible_count + added && sum == self.visible_sum + added_sum {
                changed = self.ingest_rows(rows);
            } else {
                // Rows were deactivated (or rewritten) while new ones landed: fold the visible set
                // again rather than leave the hidden turns on the chat.
                changed = self.rebuild(count);
            }
        } else if max_id != self.last_id
            || count != self.visible_count
            || sum != self.visible_sum
            || self.turns.is_empty()
        {
            changed = self.rebuild(count);
        }
        self.last_id = max_id;
        self.visible_count = count;
        self.visible_sum = sum;
        changed
    }

    /// The turns in the window, folded exactly as the chat draws them.
    pub fn turns(&self) -> &[Value] {
        &self.turns
    }

    /// The turn the session is in: working while a prompt or a tool result waits for the model,
    /// idle after a step that stopped without a call to answer.
    pub fn status(&self) -> Option<AgentStatus> {
        self.status
    }

    /// The wire answer (`ConversationResponse`).
    pub fn response(&self) -> Value {
        json!({
            "source": "hermes-transcript",
            "turns": self.turns,
            "history_id": self.history_id,
            "metadata": {
                "model": self.model,
                "reasoning_effort": self.reasoning_effort,
            },
        })
    }

    /// `(last visible id, visible rows, sum of their ids)`: an append grows all three in a way the
    /// read rows can account for, while a deactivation leaves the count or the sum behind.
    fn visible(&self) -> Option<(i64, i64, i64)> {
        self.conn
            .query_row(
                "SELECT COALESCE(MAX(id), 0), COUNT(*), COALESCE(SUM(id), 0) FROM messages
                 WHERE session_id = ?1 AND (active = 1 OR compacted = 1)",
                params![&self.session_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0).unwrap_or(0),
                        row.get::<_, i64>(1).unwrap_or(0),
                        row.get::<_, i64>(2).unwrap_or(0),
                    ))
                },
            )
            .ok()
    }

    fn rebuild(&mut self, count: i64) -> bool {
        let rows = self.rows_window(MAX_ROWS);
        let truncated = count > MAX_ROWS as i64;
        self.pending.clear();
        self.turns.clear();
        self.last_role = None;
        let changed = self.ingest_rows(rows);
        // A window that starts mid-turn would open the chat on an answer whose question is gone.
        if truncated
            && self
                .turns
                .first()
                .is_some_and(|turn| turn["role"] == "assistant")
        {
            self.turns.remove(0);
            self.remap_pending(1);
        }
        changed
    }

    fn rows_after(&self, id: i64) -> Vec<HermesRow> {
        let Ok(mut statement) = self.conn.prepare(
            "SELECT id, role, content, tool_call_id, tool_calls, tool_name, reasoning, timestamp
             FROM messages
             WHERE session_id = ?1 AND (active = 1 OR compacted = 1) AND id > ?2
             ORDER BY id ASC",
        ) else {
            return Vec::new();
        };
        let Ok(mut rows) = statement.query(params![&self.session_id, id]) else {
            return Vec::new();
        };
        collect_rows(&mut rows)
    }

    fn rows_window(&self, limit: usize) -> Vec<HermesRow> {
        let Ok(mut statement) = self.conn.prepare(
            "SELECT id, role, content, tool_call_id, tool_calls, tool_name, reasoning, timestamp
             FROM messages
             WHERE session_id = ?1 AND (active = 1 OR compacted = 1)
             ORDER BY id DESC LIMIT ?2",
        ) else {
            return Vec::new();
        };
        let Ok(mut rows) = statement.query(params![&self.session_id, limit as i64]) else {
            return Vec::new();
        };
        let mut rows = collect_rows(&mut rows);
        rows.reverse();
        rows
    }

    fn ingest_rows(&mut self, rows: Vec<HermesRow>) -> bool {
        let before = self.turns.clone();
        for row in rows {
            self.ingest(row);
        }
        self.settle();
        self.update_status();
        self.turns != before
    }

    fn ingest(&mut self, row: HermesRow) {
        self.last_role = Some(row.role.clone());
        let ts = iso_timestamp(row.timestamp);
        match row.role.as_str() {
            "user" => {
                let text = row.content.as_deref().unwrap_or_default();
                let (request, skills) = match skill_prompt(text) {
                    Some((request, skills)) => (request, skills),
                    None => (text.to_string(), Vec::new()),
                };
                let mut parts = vec![json!({ "kind": "text", "text": request })];
                parts.extend(skills);
                self.turns
                    .push(json!({ "role": "user", "ts": ts, "parts": parts }));
            }
            "assistant" => {
                let index = match self.turns.last() {
                    Some(turn) if turn["role"] == "assistant" => self.turns.len() - 1,
                    _ => {
                        self.turns
                            .push(json!({ "role": "assistant", "ts": ts, "parts": [] }));
                        self.turns.len() - 1
                    }
                };
                if let Some(ts) = &ts {
                    self.turns[index]["end_ts"] = json!(ts);
                }
                if let Some(reasoning) = row
                    .reasoning
                    .as_deref()
                    .map(str::trim)
                    .filter(|reasoning| !reasoning.is_empty())
                {
                    push_part(
                        &mut self.turns[index],
                        json!({ "kind": "thinking", "text": reasoning }),
                    );
                }
                if let Some(calls) = row.tool_calls.as_deref().filter(|calls| !calls.is_empty()) {
                    let calls = parse_tool_calls(calls);
                    if calls.is_empty()
                        && row.tool_calls.as_deref().map(str::len).unwrap_or(0)
                            > PAGE_TOOL_CALLS_BYTES
                    {
                        push_part(
                            &mut self.turns[index],
                            json!({
                                "kind": "tool", "name": "tool", "summary": "",
                                "input": "", "output": "", "arguments_omitted": true,
                            }),
                        );
                    } else {
                        for call in calls {
                            let part = json!({
                                "kind": "tool",
                                "name": call.name,
                                "summary": call.summary,
                                "input": call.input,
                                "output": "",
                            });
                            let part_index = push_part(&mut self.turns[index], part);
                            if let Some(id) = call.id {
                                self.pending.insert(id, (index, part_index));
                            }
                        }
                    }
                }
                if let Some(content) = row
                    .content
                    .as_deref()
                    .filter(|content| !content.trim().is_empty())
                {
                    push_part(
                        &mut self.turns[index],
                        json!({ "kind": "text", "text": content }),
                    );
                }
            }
            "tool" => {
                let call_id = row.tool_call_id.as_deref().unwrap_or_default();
                let target = self.pending.remove(call_id);
                let name = target
                    .as_ref()
                    .and_then(|(turn, part)| {
                        self.turns
                            .get(*turn)
                            .and_then(|turn| turn["parts"].as_array())
                            .and_then(|parts| parts.get(*part))
                            .and_then(|part| part["name"].as_str())
                    })
                    .map(str::to_string)
                    .or_else(|| row.tool_name.clone())
                    .unwrap_or_else(|| "tool".to_string());
                let raw = row.content.as_deref().unwrap_or_default();
                let decoded = if is_terminal_tool(&name) {
                    decode_terminal_envelope(raw)
                } else {
                    None
                };
                let (output, failed) = match decoded {
                    Some((output, failed)) => (output, failed),
                    None => (raw.to_string(), output_looks_failed(raw)),
                };
                let output = trim_output(&output);
                match target {
                    Some((turn, part)) => {
                        if let Some(target) = self
                            .turns
                            .get_mut(turn)
                            .and_then(|turn| turn["parts"].as_array_mut())
                            .and_then(|parts| parts.get_mut(part))
                        {
                            target["output"] = json!(output);
                            if failed {
                                target["error"] = json!(true);
                            }
                        }
                    }
                    None => {
                        // A result whose call is off the window still happened: say so rather
                        // than hide it, as the chat does for every agent's orphan result.
                        let index = match self.turns.last() {
                            Some(turn) if turn["role"] == "assistant" => self.turns.len() - 1,
                            _ => {
                                self.turns
                                    .push(json!({ "role": "assistant", "ts": ts, "parts": [] }));
                                self.turns.len() - 1
                            }
                        };
                        let mut part = json!({
                            "kind": "tool",
                            "name": name,
                            "summary": name,
                            "input": "",
                            "output": output,
                        });
                        if failed {
                            part["error"] = json!(true);
                        }
                        push_part(&mut self.turns[index], part);
                    }
                }
            }
            _ => {}
        }
    }

    /// Keeps the window bounded and the pending tool calls pointing at what they pointed at.
    fn settle(&mut self) {
        if self.turns.len() > MAX_TURNS {
            let excess = self.turns.len() - MAX_TURNS;
            self.turns.drain(..excess);
            self.remap_pending(excess);
        }
    }

    fn remap_pending(&mut self, removed: usize) {
        let removed = removed as i64;
        self.pending
            .retain(|_, (turn, _)| (*turn as i64) >= removed);
        for (turn, _) in self.pending.values_mut() {
            *turn -= removed as usize;
        }
    }

    fn update_status(&mut self) {
        self.status = match self.last_role.as_deref() {
            None => None,
            Some("user" | "tool") => Some(AgentStatus::Working),
            Some(_) if !self.pending.is_empty() => Some(AgentStatus::Working),
            Some(_) => Some(AgentStatus::Idle),
        };
    }
}

fn collect_rows(rows: &mut rusqlite::Rows<'_>) -> Vec<HermesRow> {
    let mut collected = Vec::new();
    while let Ok(Some(row)) = rows.next() {
        collected.push(HermesRow {
            id: row.get::<_, i64>(0).unwrap_or(0),
            role: row.get::<_, String>(1).unwrap_or_default(),
            content: row.get::<_, Option<String>>(2).ok().flatten(),
            tool_call_id: row.get::<_, Option<String>>(3).ok().flatten(),
            tool_calls: row.get::<_, Option<String>>(4).ok().flatten(),
            tool_name: row.get::<_, Option<String>>(5).ok().flatten(),
            reasoning: row.get::<_, Option<String>>(6).ok().flatten(),
            timestamp: numeric(row, 7).unwrap_or(0.0),
        });
    }
    collected
}

fn has_table(conn: &Connection, table: &str) -> bool {
    conn.query_row(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1",
        params![table],
        |_| Ok(()),
    )
    .is_ok()
}

fn has_column(conn: &Connection, table: &str, column: &str) -> bool {
    let Ok(mut statement) = conn.prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
    else {
        return false;
    };
    let names: Vec<String> = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default();
    names.iter().any(|name| name == column)
}

fn user_home() -> Option<PathBuf> {
    env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .map(PathBuf::from)
}

fn numeric(row: &Row<'_>, index: usize) -> Option<f64> {
    match row.get_ref(index).ok()? {
        ValueRef::Real(value) => Some(value),
        ValueRef::Integer(value) => Some(value as f64),
        ValueRef::Text(bytes) => std::str::from_utf8(bytes).ok()?.parse().ok(),
        _ => None,
    }
}

fn iso_timestamp(seconds: f64) -> Option<String> {
    if !seconds.is_finite() || seconds <= 0.0 {
        return None;
    }
    chrono::DateTime::from_timestamp_millis((seconds * 1000.0).round() as i64)
        .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

/// Case- and separator-insensitive on Windows; exact elsewhere, with a trailing separator ignored.
fn same_directory(left: &str, right: &str) -> bool {
    fn normalize(value: &str) -> String {
        let value = value.strip_prefix(r"\\?\").unwrap_or(value);
        let trimmed = value.trim_end_matches(['\\', '/']);
        if cfg!(windows) {
            trimmed.replace('/', "\\").to_ascii_lowercase()
        } else {
            trimmed.to_string()
        }
    }
    normalize(left) == normalize(right)
}

fn is_terminal_tool(name: &str) -> bool {
    TERMINAL_TOOLS
        .iter()
        .any(|tool| tool.eq_ignore_ascii_case(name))
}

/// A terminal result is `{output, exit_code, error}`; anything else is the text itself.
fn decode_terminal_envelope(raw: &str) -> Option<(String, bool)> {
    let parsed: Value = serde_json::from_str(raw).ok()?;
    let object = parsed.as_object()?;
    let output = object.get("output")?.as_str()?.to_string();
    let failed = object
        .get("exit_code")
        .and_then(Value::as_f64)
        .is_some_and(|code| code != 0.0)
        || object
            .get("error")
            .and_then(Value::as_str)
            .is_some_and(|error| !error.trim().is_empty());
    Some((output, failed))
}

/// A non-envelope result that starts with the word an error starts with is a failure.
fn output_looks_failed(output: &str) -> bool {
    let trimmed = output.trim().to_ascii_lowercase();
    for word in ["error", "failed", "exception"] {
        let Some(rest) = trimmed.strip_prefix(word) else {
            continue;
        };
        if rest
            .chars()
            .next()
            .is_none_or(|next| !next.is_alphanumeric() && next != '_')
        {
            return true;
        }
    }
    false
}

fn trim_output(output: &str) -> String {
    if output.chars().count() > TOOL_OUTPUT_CHARS {
        output.chars().take(TOOL_OUTPUT_CHARS).collect::<String>() + "\n… trimmed"
    } else {
        output.to_string()
    }
}

struct ToolCall {
    id: Option<String>,
    name: String,
    input: String,
    summary: String,
}

/// Hermes records OpenAI-shaped calls (`function.name`/`function.arguments`); a launcher may
/// flatten them. Either way the summary is about the arguments, not the wrapper.
fn parse_tool_calls(text: &str) -> Vec<ToolCall> {
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    let mut calls = Vec::new();
    for item in items {
        let Some(object) = item.as_object() else {
            continue;
        };
        let function = object
            .get("function")
            .filter(|function| function.is_object())
            .unwrap_or(&item);
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| object.get("name").and_then(Value::as_str))
            .unwrap_or("tool")
            .to_string();
        let raw = function
            .get("arguments")
            .or_else(|| object.get("arguments"))
            .cloned()
            .unwrap_or_else(|| json!("{}"));
        let input = match &raw {
            Value::String(text) => text.clone(),
            Value::Null => "null".to_string(),
            other => other.to_string(),
        };
        let arguments = match &raw {
            Value::Object(_) => raw.clone(),
            Value::String(text) => serde_json::from_str(text).unwrap_or(Value::Null),
            _ => Value::Null,
        };
        let id = object
            .get("id")
            .or_else(|| object.get("tool_call_id"))
            .and_then(scalar);
        calls.push(ToolCall {
            id,
            summary: hermes_tool_summary(&name, &arguments),
            name,
            input,
        });
    }
    calls
}

fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// Hermes's code tool puts its script in `code`; its first nonblank line is the call's purpose.
fn hermes_tool_summary(name: &str, input: &Value) -> String {
    if let Some(code) = input.get("code").and_then(Value::as_str) {
        if let Some(line) = code.lines().map(str::trim).find(|line| !line.is_empty()) {
            return line.chars().take(120).collect();
        }
    }
    tool_summary(name, input)
}

fn push_part(turn: &mut Value, part: Value) -> usize {
    let parts = turn["parts"].as_array_mut().expect("a turn's parts");
    parts.push(part);
    parts.len() - 1
}

/// The prompt Hermes records when a skill is invoked: the whole SKILL.md the user never typed,
/// with the request they did type at the tail. Returns the request and one chip per skill.
fn skill_prompt(text: &str) -> Option<(String, Vec<Value>)> {
    const SINGLE: &str = "[IMPORTANT: The user has invoked the \"";
    const BUNDLE: &str = "[IMPORTANT: The user has invoked the \"";
    const INSTRUCTION: &str =
        "\n\nThe user has provided the following instruction alongside the skill invocation: ";
    let rest = text.strip_prefix(SINGLE)?;
    if let Some((name, _)) = rest.split_once(
        "\" skill, indicating they want you to follow its instructions. The full skill content is loaded below.]\n\n",
    ) {
        let name = label(name)?;
        let marker = text.rfind(INSTRUCTION);
        let with_instruction = marker.is_some_and(|index| skill_directory(&text[..index]).is_some());
        let index = if with_instruction {
            marker.unwrap_or(0)
        } else {
            text.len()
        };
        let directory = skill_directory(&text[..index])?;
        let instruction = if with_instruction {
            text[index + INSTRUCTION.len()..].trim()
        } else {
            ""
        };
        let request = if instruction.is_empty() {
            format!("/{name}")
        } else {
            format!("/{name} {instruction}")
        };
        return Some((
            request,
            vec![skill_chip(&name, &format!("{directory}/SKILL.md"))?],
        ));
    }
    let rest = text.strip_prefix(BUNDLE)?;
    let (name, tail) = rest.split_once("\" skill bundle, loading ")?;
    let (count, tail) = tail.split_once(
        " skills together. Treat every skill below as active guidance for this turn.]\n\n",
    )?;
    let count: usize = count.parse().ok()?;
    let name = label(name)?;
    let marker = format!("\n\n[Loaded as part of the \"{name}\" skill bundle.]\n\n");
    let mut blocks = tail.split(marker.as_str());
    let header = blocks.next()?;
    let metadata = bundle_header(header)?;
    if metadata.name != name || metadata.skills.len() != count {
        return None;
    }
    let skills = blocks.collect::<Vec<_>>();
    if skills.len() != metadata.skills.len() {
        return None;
    }
    let mut chips = Vec::new();
    for (block, skill) in skills.iter().zip(&metadata.skills) {
        let directory = skill_directory(block)?;
        chips.push(skill_chip(
            &label(skill)?,
            &format!("{directory}/SKILL.md"),
        )?);
    }
    let instruction = metadata.instruction.trim();
    let request = if instruction.is_empty() {
        format!("/{name}")
    } else {
        format!("/{name} {instruction}")
    };
    Some((request, chips))
}

struct BundleHeader {
    name: String,
    skills: Vec<String>,
    instruction: String,
}

/// `Bundle: NAME\nSkills loaded: a, b\n\nUser instruction: ...`
fn bundle_header(header: &str) -> Option<BundleHeader> {
    let rest = header.strip_prefix("Bundle: ")?;
    let (name, rest) = rest.split_once('\n')?;
    let rest = rest.strip_prefix("Skills loaded: ")?;
    let (skills, instruction) = match rest.split_once("\n\nUser instruction: ") {
        Some((skills, instruction)) => (skills, instruction),
        None => (rest, ""),
    };
    if skills.contains('\n') {
        return None;
    }
    let skills = skills.split(", ").map(str::to_string).collect::<Vec<_>>();
    Some(BundleHeader {
        name: name.to_string(),
        skills,
        instruction: instruction.to_string(),
    })
}

/// The first `[Skill directory: ...]` on a line of its own, without crossing a line.
fn skill_directory(text: &str) -> Option<String> {
    const OPEN: &str = "\n[Skill directory: ";
    let mut search = 0;
    while let Some(relative) = text[search..].find(OPEN) {
        let start = search + relative + OPEN.len();
        let rest = &text[start..];
        let end = rest.find("]\n")?;
        let value = &rest[..end];
        if !value.is_empty() && !value.contains(['\r', '\n']) && value.len() <= 4096 {
            return Some(value.to_string());
        }
        search = start;
    }
    None
}

fn skill_chip(name: &str, path: &str) -> Option<Value> {
    Some(json!({
        "kind": "skill",
        "skill": {
            "name": label(name)?,
            "evidence": "instructions",
            "status": "loaded",
            "path": path,
        },
    }))
}

/// A name or path Hermes would have written: short, one line, no markup.
fn label(value: &str) -> Option<String> {
    (!value.is_empty() && value.len() <= 200 && !value.contains(['\r', '\n', '<', '>']))
        .then(|| value.to_string())
}

#[cfg(test)]
mod tests;
