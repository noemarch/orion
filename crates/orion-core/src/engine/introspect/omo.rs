//! Reading OmO's own transcript (and omp's, which keeps the same senpi session shape).
//!
//! `omo` and `omp` are thin launchers around the senpi engine. A session is one JSONL file under
//! `<agent dir>/sessions/<cwd slug>/`: a `session` header naming the cwd and id, then entries — a
//! `message` per step (`user`, `assistant` with content blocks, `toolResult`), and `custom_message`
//! entries for skill prompts, runtime notices, compaction, and the wake that reports a background
//! task. Turns are folded exactly as the chat draws them: assistant steps merge until one stops for
//! good, tool calls adopt the output of the result that answers them, and a runtime message that
//! starts work sits in the user's seat as a notice or a task card.
//!
//! A pane is bound to the newest session in its cwd's folder whose header names that cwd. Orion
//! has no external process/session report, so no held-session evidence is available; a pane
//! launched with an unrecognized `--session-dir` falls back to scrollback.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::{json, Value};

use crate::protocol::{AgentKind, AgentStatus};

/// Enough turns for a conversation; older turns fall away as a session grows.
const MAX_TURNS: usize = 100;
/// A tool output longer than this is cut; the whole text still lives in the agent's file.
const TOOL_OUTPUT_CHARS: usize = 64_000;
/// A task result longer than this is cut, matching the protocol's card limit.
const TASK_RESULT_CHARS: usize = 16_000;
/// Runtime messages that start a turn nobody typed.
const TURN_STARTS: [&str; 9] = [
    "goal-continuation",
    "omo-senpi:wake",
    "senpi-monitor:notification",
    "senpi-terminal:notification",
    "senpi-codemode:notification",
    "senpi.todo-owed",
    "omo-init-deep-advisor:run",
    "omo-onboarding:bootstrap",
    "ttsr-injection",
];

/// The turns a senpi-shaped session writes. `omo` and `omp` are launchers around that engine, and
/// pi keeps the same message shape in an entry-tree file: its reader picks the live branch and
/// feeds these lines, so the folding rules stay in one place.
pub(crate) struct SenpiTurns {
    turns: Vec<Value>,
    /// Tool calls still waiting for their result: id -> (turn index, part index).
    pending: HashMap<String, (usize, usize)>,
    /// The summaries OmO's `task` calls gave their tasks, by task id.
    task_titles: HashMap<String, String>,
    /// The last assistant message stopped for good: the next one starts its own turn.
    settled: bool,
    status: Option<AgentStatus>,
}

/// One session file plus everything parsed from it so far.
pub struct OmoTranscript {
    path: PathBuf,
    session_id: String,
    source: &'static str,
    /// Bytes of complete lines already parsed; a torn tail is retried next refresh.
    offset: u64,
    turns: SenpiTurns,
}

impl OmoTranscript {
    /// Opens the newest session of `cwd` in the store the agent kind writes.
    pub fn open(kind: AgentKind, cwd: &Path, _since: Option<SystemTime>) -> Option<Self> {
        Self::open_at(kind, cwd, &agent_dir(kind)?)
    }

    /// Opens a store rooted at `agent_dir`, which is how tests replay a recorded store.
    pub fn open_in(
        kind: AgentKind,
        cwd: &Path,
        agent_dir: &Path,
        _since: Option<SystemTime>,
    ) -> Option<Self> {
        Self::open_at(kind, cwd, agent_dir)
    }

    fn open_at(kind: AgentKind, cwd: &Path, agent_dir: &Path) -> Option<Self> {
        let source = match kind {
            AgentKind::Omo => "omo-transcript",
            AgentKind::Omp => "omp-transcript",
            _ => return None,
        };
        let cwd_text = cwd.to_string_lossy().to_string();
        let dir = agent_dir.join("sessions").join(session_folder(&cwd_text));
        let mut best: Option<(i64, PathBuf, String)> = None;
        for entry in fs::read_dir(&dir).ok()? {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(header) = first_line(&path) else {
                continue;
            };
            let Ok(header) = serde_json::from_str::<Value>(&header) else {
                continue;
            };
            if header.get("type").and_then(Value::as_str) != Some("session")
                || header.get("cwd").and_then(Value::as_str) != Some(cwd_text.as_str())
            {
                continue;
            }
            let Some(id) = header.get("id").and_then(Value::as_str) else {
                continue;
            };
            let written = header
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
                .map(|time| time.timestamp_millis())
                .or_else(|| {
                    fs::metadata(&path)
                        .ok()
                        .and_then(|meta| meta.modified().ok())
                        .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
                        .map(|time| time.as_millis() as i64)
                })
                .unwrap_or(0);
            if best.as_ref().is_none_or(|(at, _, _)| written > *at) {
                best = Some((written, path, id.to_string()));
            }
        }
        let (_, path, session_id) = best?;
        let mut transcript = Self {
            path,
            session_id: session_id.clone(),
            source,
            offset: 0,
            turns: SenpiTurns::new(),
        };
        transcript.refresh();
        Some(transcript)
    }

    /// Parses everything appended since the last call. Returns whether the transcript changed.
    pub fn refresh(&mut self) -> bool {
        let Ok(mut file) = fs::File::open(&self.path) else {
            return false;
        };
        let length = file.metadata().map(|meta| meta.len()).unwrap_or(0);
        if length < self.offset {
            // The file was replaced or truncated: read it again from the start.
            self.offset = 0;
            self.turns.reset();
        }
        if length == self.offset {
            return false;
        }
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return false;
        }
        let mut bytes = Vec::new();
        if file.read_to_end(&mut bytes).is_err() {
            return false;
        }
        // Complete lines are parsed; a last line still without its newline is parsed too, and
        // kept only when it is whole JSON. omo appends the newline with its next write, so
        // waiting for it would hide a finished answer until the session grew again.
        let whole = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map(|at| at + 1)
            .unwrap_or(0);
        let mut text = String::from_utf8_lossy(&bytes[..whole]).to_string();
        let mut consumed = whole;
        if whole < bytes.len() {
            let tail = String::from_utf8_lossy(&bytes[whole..]);
            if serde_json::from_str::<Value>(tail.trim()).is_ok() {
                text.push_str(&tail);
                consumed = bytes.len();
            }
        }
        if consumed == 0 {
            return false;
        }
        self.offset += consumed as u64;
        let mut changed = false;
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(entry) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if self.turns.ingest(&entry) {
                changed = true;
            }
        }
        if changed {
            self.turns.settle();
        }
        changed
    }
}

impl SenpiTurns {
    pub(crate) fn new() -> Self {
        Self {
            turns: Vec::new(),
            pending: HashMap::new(),
            task_titles: HashMap::new(),
            settled: false,
            status: None,
        }
    }

    /// Forgets every parsed turn, as when the file was replaced.
    pub(crate) fn reset(&mut self) {
        *self = Self::new();
    }

    pub(crate) fn turns(&self) -> &[Value] {
        &self.turns
    }

    pub(crate) fn status(&self) -> Option<AgentStatus> {
        self.status
    }

    /// Drops what draws nothing and keeps the window bounded.
    pub(crate) fn settle(&mut self) {
        // A failed step with no error text draws nothing; dropping it moves the turns a
        // pending tool call points at, so those are dropped rather than misdirected.
        if self.turns.iter().any(|turn| {
            turn["parts"]
                .as_array()
                .is_some_and(|parts| parts.is_empty())
        }) {
            self.turns.retain(|turn| {
                turn["parts"]
                    .as_array()
                    .is_some_and(|parts| !parts.is_empty())
            });
            self.pending.clear();
        }
        if self.turns.len() > MAX_TURNS {
            self.turns.drain(..self.turns.len() - MAX_TURNS);
            self.pending.clear();
        }
    }

    /// Applies one recorded line. Returns whether it produced or completed a turn.
    pub(crate) fn ingest(&mut self, entry: &Value) -> bool {
        let timestamp = entry.get("timestamp").and_then(Value::as_str);
        if is_context_clear(entry) {
            self.turns.clear();
            self.pending.clear();
            self.settled = false;
            self.status = None;
            return true;
        }
        if let Some(parts) = skill_prompt(entry) {
            self.push_user(timestamp, parts);
            self.status = Some(AgentStatus::Working);
            return true;
        }
        if let Some(notice) = runtime_notice(entry) {
            self.push_user(timestamp, vec![notice]);
            // only the runtime messages that start work begin a turn; a spare notice does not
            if entry
                .get("customType")
                .and_then(Value::as_str)
                .is_some_and(|kind| TURN_STARTS.contains(&kind))
            {
                self.status = Some(AgentStatus::Working);
            }
            return true;
        }
        if let Some(tasks) = task_results(entry, &self.task_titles) {
            self.push_user(
                timestamp,
                vec![json!({ "kind": "task_result", "tasks": tasks })],
            );
            self.status = Some(AgentStatus::Working);
            return true;
        }
        if entry.get("type").and_then(Value::as_str) == Some("compaction") {
            if let Some(summary) = entry
                .get("summary")
                .and_then(Value::as_str)
                .filter(|summary| !summary.trim().is_empty())
            {
                self.push_user(
                    timestamp,
                    vec![json!({ "kind": "compact", "text": summary })],
                );
                return true;
            }
            return false;
        }
        let Some(message) = pi_message(entry) else {
            return false;
        };
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let content = message
            .get("content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if role != "assistant" {
            self.apply_results(&message);
        }
        self.learn_task_titles(&message);
        if role == "user" {
            let prompt = content
                .iter()
                .filter_map(|part| {
                    if part.get("type").and_then(Value::as_str) == Some("text") {
                        part.get("text").and_then(Value::as_str)
                    } else {
                        None
                    }
                })
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            if prompt.is_empty() {
                return false;
            }
            self.push_user(timestamp, vec![json!({ "kind": "text", "text": prompt })]);
            self.status = Some(AgentStatus::Working);
            return true;
        }
        if role != "assistant" {
            return false;
        }
        let stop = message.get("stopReason").and_then(Value::as_str);
        let before = self.status;
        let index = if matches!(
            self.turns
                .last()
                .and_then(|turn| turn.get("role"))
                .and_then(Value::as_str),
            Some("assistant")
        ) && !self.settled
        {
            self.turns.len() - 1
        } else {
            self.turns
                .push(json!({ "role": "assistant", "ts": timestamp, "parts": [] }));
            self.turns.len() - 1
        };
        if let Some(timestamp) = timestamp {
            self.turns[index]["end_ts"] = json!(timestamp);
        }
        let mut produced = false;
        for block in &content {
            let kind = block.get("type").and_then(Value::as_str);
            match kind {
                Some("text") => {
                    if let Some(text) = block
                        .get("text")
                        .and_then(Value::as_str)
                        .filter(|text| !text.is_empty())
                    {
                        self.push_part(index, json!({ "kind": "text", "text": text }));
                        produced = true;
                    }
                }
                Some("thinking") => {
                    let thinking = block
                        .get("thinking")
                        .and_then(Value::as_str)
                        .or_else(|| block.get("text").and_then(Value::as_str))
                        .unwrap_or_default();
                    if !thinking.is_empty() {
                        self.push_part(index, json!({ "kind": "thinking", "text": thinking }));
                        produced = true;
                    }
                }
                Some("toolCall") => {
                    let Some(name) = block.get("name").and_then(Value::as_str) else {
                        continue;
                    };
                    let input = block.get("arguments").cloned().unwrap_or_else(|| json!({}));
                    let summary = block
                        .get("intent")
                        .and_then(Value::as_str)
                        .filter(|intent| !intent.is_empty())
                        .map(str::to_string)
                        .unwrap_or_else(|| tool_summary(name, &input));
                    let part = json!({
                        "kind": "tool",
                        "name": name,
                        "summary": summary.chars().take(120).collect::<String>(),
                        "input": serde_json::to_string_pretty(&input).unwrap_or_else(|_| "{}".to_string()),
                        "output": "",
                    });
                    let part_index = self.turns[index]["parts"]
                        .as_array()
                        .map(|parts| parts.len())
                        .unwrap_or(0);
                    self.push_part(index, part);
                    if let Some(id) = block.get("id").and_then(Value::as_str) {
                        self.pending.insert(id.to_string(), (index, part_index));
                    }
                    produced = true;
                }
                _ => {}
            }
        }
        // A provider can place a result beside its call in the same assistant record.
        self.apply_results(&message);
        if stop == Some("error") {
            if let Some(error) = message
                .get("errorMessage")
                .and_then(Value::as_str)
                .filter(|error| !error.is_empty())
            {
                self.push_part(
                    index,
                    json!({ "kind": "text", "text": format!("Error: {error}") }),
                );
                produced = true;
            }
        }
        self.settled = stop == Some("stop");
        self.status = Some(if stop == Some("toolUse") {
            AgentStatus::Working
        } else {
            AgentStatus::Idle
        });
        if self.status != before {
            produced = true;
        }
        // An empty step (a failed request with no error text) draws nothing.
        if self.turns[index]
            .get("parts")
            .and_then(Value::as_array)
            .is_some_and(|parts| parts.is_empty())
        {
            self.turns.remove(index);
        }
        produced
    }

    fn push_user(&mut self, timestamp: Option<&str>, parts: Vec<Value>) {
        self.turns.push(json!({
            "role": "user",
            "ts": timestamp,
            "parts": parts,
        }));
        self.settled = false;
        while self.turns.len() > MAX_TURNS {
            self.turns.remove(0);
            self.pending.clear();
        }
    }

    fn push_part(&mut self, turn: usize, part: Value) {
        if let Some(parts) = self.turns[turn]
            .get_mut("parts")
            .and_then(Value::as_array_mut)
        {
            parts.push(part);
        }
    }

    /// Fills each pending tool call with the result that answers it.
    fn apply_results(&mut self, message: &Value) {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let blocks = if role == "toolResult" {
            vec![message.clone()]
        } else {
            message
                .get("content")
                .and_then(Value::as_array)
                .map(|content| {
                    content
                        .iter()
                        .filter(|block| {
                            block.get("type").and_then(Value::as_str) == Some("toolResult")
                        })
                        .cloned()
                        .collect()
                })
                .unwrap_or_default()
        };
        for block in blocks {
            let Some(id) = block
                .get("toolCallId")
                .and_then(Value::as_str)
                .or_else(|| block.get("callId").and_then(Value::as_str))
            else {
                continue;
            };
            let Some((turn, part)) = self.pending.remove(id) else {
                continue;
            };
            let raw = block
                .get("output")
                .or_else(|| block.get("content"))
                .or_else(|| block.get("result"));
            let mut text = raw.map(result_text).unwrap_or_default();
            if text.chars().count() > TOOL_OUTPUT_CHARS {
                text = text.chars().take(TOOL_OUTPUT_CHARS).collect::<String>() + "\n… trimmed";
            }
            let target = self.turns.get_mut(turn).and_then(|turn| {
                turn.get_mut("parts")
                    .and_then(Value::as_array_mut)
                    .and_then(|parts| parts.get_mut(part))
            });
            if let Some(target) = target {
                target["output"] = json!(text);
                if block.get("isError").and_then(Value::as_bool) == Some(true) {
                    target["error"] = json!(true);
                }
            }
        }
    }

    /// The summaries OmO's `task` calls gave their tasks, by task id.
    fn learn_task_titles(&mut self, message: &Value) {
        if message.get("role").and_then(Value::as_str) != Some("toolResult")
            || message.get("toolName").and_then(Value::as_str) != Some("task")
        {
            return;
        }
        let Some(details) = message.get("details") else {
            return;
        };
        let items = details
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| vec![details.clone()]);
        for item in items {
            let id = item.get("task_id").and_then(label);
            let title = item
                .get("task_summary")
                .and_then(label)
                .or_else(|| item.get("description").and_then(label));
            if let (Some(id), Some(title)) = (id, title) {
                self.task_titles.insert(id, title);
            }
        }
    }
}

impl OmoTranscript {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn turns(&self) -> &[Value] {
        self.turns.turns()
    }

    /// The turn the session is in: working after a prompt or a tool step, idle after an answer.
    pub fn status(&self) -> Option<AgentStatus> {
        self.turns.status()
    }

    fn history_id(&self) -> String {
        format!("omo-{}", self.session_id)
    }

    /// The wire answer (`ConversationResponse`).
    pub fn response(&self) -> Value {
        json!({
            "source": self.source,
            "turns": self.turns.turns(),
            "history_id": self.history_id(),
        })
    }
}

/// `<agent dir>/sessions/--<cwd without its leading slash, separators as dashes>--`
pub fn session_folder(cwd: &str) -> String {
    let stripped = cwd.trim_start_matches(['/', '\\']);
    let slug = stripped.replace(['/', '\\', ':'], "-");
    format!("--{slug}--")
}

/// The store the agent kind writes, from the environment its launcher honors.
pub(crate) fn agent_dir(kind: AgentKind) -> Option<PathBuf> {
    match kind {
        AgentKind::Omo => omo_agent_dir(),
        AgentKind::Omp => omp_agent_dir(),
        _ => None,
    }
}

fn home() -> Option<PathBuf> {
    env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(PathBuf::from))
}

fn expand_tilde(value: &str) -> PathBuf {
    if value == "~" {
        return home().unwrap_or_else(|| PathBuf::from(value));
    }
    if let Some(rest) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        if let Some(home) = home() {
            return home.join(rest);
        }
    }
    PathBuf::from(value)
}

/// senpi's agent directory: the first environment override that is set, else `~/.omo/agent`.
pub(crate) fn omo_agent_dir() -> Option<PathBuf> {
    for name in [
        "OMO_CODING_AGENT_DIR",
        "SENPI_CODING_AGENT_DIR",
        "PI_CODING_AGENT_DIR",
    ] {
        if let Some(value) = env::var_os(name) {
            let value = value.to_string_lossy();
            if !value.trim().is_empty() {
                return Some(expand_tilde(&value));
            }
        }
    }
    Some(home()?.join(".omo").join("agent"))
}

/// omp's agent directory: a named profile, else `~/.omp/agent`.
fn omp_agent_dir() -> Option<PathBuf> {
    let home = home()?;
    match env::var("OMP_PROFILE")
        .ok()
        .filter(|profile| !profile.is_empty() && profile != "." && profile != "..")
    {
        Some(profile)
            if profile.chars().all(|character| {
                character.is_alphanumeric() || matches!(character, '_' | '-' | '.')
            }) =>
        {
            Some(
                home.join(".omp")
                    .join("profiles")
                    .join(profile)
                    .join("agent"),
            )
        }
        Some(_) => None,
        None => Some(home.join(".omp").join("agent")),
    }
}

fn first_line(path: &Path) -> Option<String> {
    use std::io::BufRead;
    let file = fs::File::open(path).ok()?;
    let mut line = String::new();
    std::io::BufReader::new(file).read_line(&mut line).ok()?;
    Some(line)
}

fn label(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn is_context_clear(entry: &Value) -> bool {
    entry.get("type").and_then(Value::as_str) == Some("custom")
        && entry.get("customType").and_then(Value::as_str) == Some("context_clear")
}

/// The chat's own record of a skill invocation (`custom_message` with `customType: skill-prompt`):
/// the request the user typed, with the skill as a chip on it.
fn skill_prompt(entry: &Value) -> Option<Vec<Value>> {
    if entry.get("type").and_then(Value::as_str) != Some("custom_message")
        || entry.get("customType").and_then(Value::as_str) != Some("skill-prompt")
        || entry.get("display").and_then(Value::as_bool) == Some(false)
    {
        return None;
    }
    let details = entry.get("details")?;
    let name = details.get("name").and_then(label)?;
    if details.get("args").is_some_and(|args| !args.is_string()) {
        return None;
    }
    let args = details
        .get("args")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let request = if args.is_empty() {
        format!("/skill:{name}")
    } else {
        format!("/skill:{name} {args}")
    };
    let mut skill = json!({
        "name": name,
        "evidence": "instructions",
        "status": "loaded",
    });
    if let Some(path) = details.get("path").and_then(label) {
        skill["path"] = json!(path);
    }
    Some(vec![
        json!({ "kind": "text", "text": request }),
        json!({ "kind": "skill", "skill": skill }),
    ])
}

/// A runtime message the agent itself put in the user's seat.
fn runtime_notice(entry: &Value) -> Option<Value> {
    if entry.get("type").and_then(Value::as_str) != Some("custom_message")
        || entry.get("display").and_then(Value::as_bool) == Some(false)
    {
        return None;
    }
    let content = entry.get("content").and_then(Value::as_str)?;
    let text = content
        .trim()
        .trim_start_matches("<system-notice>")
        .trim_end_matches("</system-notice>")
        .trim();
    if text.is_empty() {
        return None;
    }
    let mut part = json!({ "kind": "notice", "text": text });
    if let Some(source) = entry.get("customType").and_then(Value::as_str) {
        part["source"] = json!(source);
    }
    Some(part)
}

/// OmO's wake after background tasks ended: one `task_result` card per report.
fn task_results(entry: &Value, titles: &HashMap<String, String>) -> Option<Vec<Value>> {
    if entry.get("type").and_then(Value::as_str) != Some("custom_message")
        || entry.get("customType").and_then(Value::as_str) != Some("omo-senpi:wake")
    {
        return None;
    }
    let details = entry.get("details").and_then(Value::as_array)?;
    let mut tasks: Vec<Value> = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for group in details {
        if group.get("customType").and_then(Value::as_str) != Some("senpi-task.completion") {
            continue;
        }
        let Some(reports) = group.get("details").and_then(Value::as_array) else {
            continue;
        };
        for report in reports {
            let Some(id) = report.get("task_id").and_then(label) else {
                continue;
            };
            let Some(raw) = report.get("status").and_then(label) else {
                continue;
            };
            let stats = report.get("run_stats").cloned().unwrap_or(Value::Null);
            let agent = report
                .get("agent_type")
                .and_then(label)
                .or_else(|| report.get("category").and_then(label))
                .or_else(|| report.get("subagent_type").and_then(label));
            let name = report.get("name").and_then(label);
            let result = report
                .get("final_response")
                .and_then(label)
                .or_else(|| report.get("error").and_then(label))
                .unwrap_or_default();
            let title = titles
                .get(&id)
                .cloned()
                .or_else(|| name.filter(|name| *name != id))
                .or_else(|| agent.clone())
                .unwrap_or_else(|| id.clone());
            let status = match raw.as_str() {
                "completed" => "completed",
                "cancelled" | "canceled" | "aborted" => "cancelled",
                _ => "failed",
            };
            let duration = report
                .get("duration_ms")
                .and_then(number)
                .or_else(|| stats.get("runtime_ms").and_then(number));
            let task = json!({
                "id": id,
                "title": title,
                "agent": agent,
                "model": report.get("resolved_model").and_then(|model| model.get("display")).and_then(label).or_else(|| report.get("model").and_then(label)),
                "status": status,
                "duration_ms": duration,
                "turns": stats.get("turns").and_then(number),
                "tool_calls": stats.get("tool_calls").and_then(number),
                "tokens": stats.get("total_tokens").and_then(number).or_else(|| report.get("total_tokens").and_then(number)).or_else(|| report.get("tokens").and_then(number)),
                "result": result.chars().take(TASK_RESULT_CHARS).collect::<String>(),
            });
            let mut task = task;
            if result.chars().count() > TASK_RESULT_CHARS {
                task["result_cut"] = json!(true);
            }
            match seen.get(&id) {
                Some(index) => tasks[*index] = task,
                None => {
                    seen.insert(id, tasks.len());
                    tasks.push(task);
                }
            }
        }
    }
    if tasks.is_empty() {
        None
    } else {
        Some(tasks)
    }
}

fn number(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        value
            .as_f64()
            .filter(|value| value.is_finite() && *value >= 0.0)
            .map(|value| value.round() as u64)
    })
}

/// senpi providers spell the same message fields differently; one normalized row.
fn pi_message(entry: &Value) -> Option<Value> {
    if entry.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    let message = entry.get("message")?;
    if message.get("display").and_then(Value::as_bool) == Some(false)
        || entry.get("display").and_then(Value::as_bool) == Some(false)
    {
        return None;
    }
    let raw = match message.get("content") {
        Some(Value::String(text)) => vec![json!({ "type": "text", "text": text })],
        Some(Value::Array(blocks)) => blocks.clone(),
        _ => Vec::new(),
    };
    let content = raw
        .into_iter()
        .map(|block| {
            match block.get("type").and_then(Value::as_str) {
                Some("toolCall") => {
                    let name = block
                        .get("toolName")
                        .and_then(Value::as_str)
                        .or_else(|| block.get("name").and_then(Value::as_str));
                    let id = block
                        .get("toolCallId")
                        .and_then(Value::as_str)
                        .or_else(|| block.get("id").and_then(Value::as_str))
                        .or_else(|| block.get("callId").and_then(Value::as_str));
                    let arguments = block
                        .get("toolInput")
                        .or_else(|| block.get("input"))
                        .or_else(|| block.get("arguments"))
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    json!({
                        "type": "toolCall",
                        "name": name,
                        "id": id,
                        "arguments": arguments,
                        "intent": block.get("intent"),
                    })
                }
                Some("toolResult") => json!({
                    "type": "toolResult",
                    "toolCallId": block.get("toolCallId").and_then(Value::as_str).or_else(|| block.get("callId").and_then(Value::as_str)).or_else(|| block.get("id").and_then(Value::as_str)),
                    "content": block.get("output").or_else(|| block.get("content")).or_else(|| block.get("result")),
                    "isError": block.get("isError"),
                }),
                _ => block,
            }
        })
        .collect::<Vec<_>>();
    let mut normalized = message.clone();
    normalized["content"] = json!(content);
    if let Some(id) = message
        .get("toolCallId")
        .and_then(Value::as_str)
        .or_else(|| message.get("callId").and_then(Value::as_str))
    {
        normalized["toolCallId"] = json!(id);
    }
    Some(normalized)
}

/// The text of a tool result: a string, or the text parts of a content array.
fn result_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<String>(),
        _ => String::new(),
    }
}

/// The one-line summary a collapsed tool chip shows.
pub(crate) fn tool_summary(name: &str, input: &Value) -> String {
    // an OmO or omp `task` call: the summary it gave the person, one per task of a batch
    if name == "task" {
        let items = input
            .get("tasks")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| vec![input.clone()]);
        let titles = items
            .iter()
            .filter_map(|item| {
                item.get("task_summary")
                    .and_then(label)
                    .or_else(|| item.get("description").and_then(label))
            })
            .collect::<Vec<_>>();
        if !titles.is_empty() {
            return titles.join(" · ").chars().take(120).collect();
        }
    }
    // what the agent itself said the call is for
    let said = if input.get("code").is_some() {
        input.get("summary").and_then(label)
    } else if input.get("command").is_some() || input.get("prompt").is_some() {
        input.get("description").and_then(label)
    } else {
        None
    };
    if let Some(said) = said {
        return said.chars().take(120).collect();
    }
    for key in [
        "command",
        "file_path",
        "notebook_path",
        "path",
        "pattern",
        "description",
        "url",
    ] {
        if let Some(value) = input.get(key).and_then(Value::as_str) {
            return value.chars().take(120).collect();
        }
    }
    name.to_string()
}

#[cfg(test)]
mod tests;
