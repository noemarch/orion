//! Reading Codex's own transcript (rollout JSONL).
//!
//! Codex writes one JSONL rollout per thread under `<CODEX_HOME>/sessions/<year>/<month>/<day>/`.
//! The parser below mirrors Orbit's `codex.ts` for the conversation surface: `event_msg` display
//! records and `response_item` model records both become turns, deduplicated when they show the
//! same text, and tool calls are attached by `call_id`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::{json, Map, Value};

/// A rollout file plus everything parsed from it so far.
pub struct CodexTranscript {
    path: PathBuf,
    segments: Vec<Segment>,
    session_id: String,
    history_id: String,
    turns: Vec<Value>,
    tools: HashMap<String, (usize, usize)>,
    messages: Vec<SentMessage>,
    started_at: Option<String>,
    active_turn_id: Option<String>,
    signature: String,
}

struct Segment {
    path: PathBuf,
    end: u64,
    offset: u64,
}

#[derive(Clone)]
struct SentMessage {
    role: String,
    text: String,
    source: String,
    ts: String,
    paired: bool,
    phase: Option<String>,
    turn: usize,
    part: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedQuestion {
    pub key: String,
    pub title: String,
    pub options: Vec<String>,
}

impl CodexTranscript {
    /// The questions this rollout asked with `request_user_input_async` and holds no answer for.
    pub fn unanswered_questions(&self) -> Vec<QueuedQuestion> {
        let Ok(text) = fs::read_to_string(&self.path) else {
            return Vec::new();
        };
        let mut asked = Vec::new();
        let mut answered = std::collections::HashSet::new();
        for line in text.lines() {
            if !line.contains("request_user_input")
                && !line.contains("send_user_message_question_reply")
            {
                continue;
            }
            let Ok(entry) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if entry.get("type").and_then(Value::as_str) != Some("response_item") {
                continue;
            }
            let payload = entry.get("payload").cloned().unwrap_or(Value::Null);
            match payload
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default()
            {
                "function_call"
                    if payload.get("name").and_then(Value::as_str)
                        == Some("request_user_input_async") =>
                {
                    let call_id = payload
                        .get("call_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let args_text = payload
                        .get("arguments")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let Ok(args) = serde_json::from_str::<Value>(args_text) else {
                        continue;
                    };
                    let Some(questions) = args.get("questions").and_then(Value::as_array) else {
                        continue;
                    };
                    for (index, question) in questions.iter().enumerate() {
                        let title = question
                            .get("title")
                            .or_else(|| question.get("question"))
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        let options = question
                            .get("options")
                            .and_then(Value::as_array)
                            .map(|options| {
                                options
                                    .iter()
                                    .filter_map(|option| {
                                        option.as_str().map(str::to_string).or_else(|| {
                                            option
                                                .get("label")
                                                .and_then(Value::as_str)
                                                .map(str::to_string)
                                        })
                                    })
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default();
                        asked.push(QueuedQuestion {
                            key: format!("{call_id}:{index}"),
                            title,
                            options,
                        });
                    }
                }
                "message" if payload.get("role").and_then(Value::as_str) == Some("user") => {
                    let text = content_text(payload.get("content").unwrap_or(&Value::Null), true);
                    let Some(inner) = text
                        .trim()
                        .strip_prefix("<send_user_message_question_reply>")
                        .and_then(|rest| rest.strip_suffix("</send_user_message_question_reply>"))
                    else {
                        continue;
                    };
                    let Ok(items) = serde_json::from_str::<Value>(inner.trim()) else {
                        continue;
                    };
                    let Some(items) = items.as_array() else {
                        continue;
                    };
                    for item in items {
                        let Some(id_text) = item.get("questionItemId").and_then(Value::as_str)
                        else {
                            continue;
                        };
                        let Ok(id) = serde_json::from_str::<Value>(id_text) else {
                            continue;
                        };
                        if let (Some(call), Some(index)) = (
                            id.get(1).and_then(Value::as_str),
                            id.get(2).and_then(Value::as_u64),
                        ) {
                            answered.insert(format!("{call}:{index}"));
                        }
                    }
                }
                _ => {}
            }
        }
        asked
            .into_iter()
            .filter(|question| !answered.contains(&question.key))
            .collect()
    }
}

impl CodexTranscript {
    pub fn open(cwd: &Path, since: Option<SystemTime>) -> Option<Self> {
        let home = codex_home()?;
        let path = newest_rollout(&home, cwd, since)?;
        Self::from_path(&path, &home)
    }

    /// Opens the rollout this screen is showing, when exactly one candidate does.
    pub fn open_for_screen(cwd: &Path, screen: &str, since: Option<SystemTime>) -> Option<Self> {
        let home = codex_home()?;
        let candidates = rollout_candidates(&home, cwd, since);
        if candidates.len() > 1 {
            let texts: Vec<(PathBuf, String)> = candidates
                .iter()
                .map(|path| (path.clone(), fs::read_to_string(path).unwrap_or_default()))
                .collect();
            if let Some(path) = match_codex_transcript(screen, &texts) {
                return Self::from_path(&path, &home);
            }
        }
        let path = newest_rollout(&home, cwd, since)?;
        Self::from_path(&path, &home)
    }

    fn from_path(path: &Path, home: &Path) -> Option<Self> {
        let (session_id, history_id) = rollout_ids(path);
        let segments = history_segments(path, home)
            .into_iter()
            .map(|(segment_path, end)| Segment {
                path: segment_path,
                end,
                offset: 0,
            })
            .collect();
        let mut transcript = Self {
            path: path.to_path_buf(),
            segments,
            session_id,
            history_id,
            turns: Vec::new(),
            tools: HashMap::new(),
            messages: Vec::new(),
            started_at: None,
            active_turn_id: None,
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

    pub fn refresh(&mut self) -> bool {
        let mut changed = false;
        for index in 0..self.segments.len() {
            let (bytes, offset) = {
                let segment = &mut self.segments[index];
                let Ok(bytes) = read_range(&segment.path, segment.offset, segment.end) else {
                    continue;
                };
                let Some(end) = bytes.iter().rposition(|byte| *byte == b'\n') else {
                    continue;
                };
                let consumed = bytes[..=end].to_vec();
                segment.offset += consumed.len() as u64;
                (consumed, segment.offset)
            };
            let _ = offset;
            let text = String::from_utf8_lossy(&bytes);
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let Ok(entry) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                if ingest(self, &entry) {
                    changed = true;
                }
            }
        }
        if changed {
            self.signature = format!("{}:{}", self.history_id, self.turns.len());
            while self.turns.len() > 100 {
                self.turns.remove(0);
            }
        }
        changed
    }

    pub fn response(&self) -> Value {
        let mut body = Map::new();
        body.insert(
            "source".to_string(),
            Value::String("codex-transcript".to_string()),
        );
        body.insert("turns".to_string(), Value::Array(self.turns.clone()));
        body.insert(
            "history_id".to_string(),
            Value::String(self.history_id.clone()),
        );
        Value::Object(body)
    }
}

fn codex_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("CODEX_HOME") {
        return Some(PathBuf::from(home));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))?;
    Some(home.join(".codex"))
}

fn newest_rollout(home: &Path, cwd: &Path, since: Option<SystemTime>) -> Option<PathBuf> {
    if let Some(path) = sqlite_rollout(home, cwd, since) {
        return Some(path);
    }
    let sessions = home.join("sessions");
    let floor = since.and_then(|since| since.checked_sub(std::time::Duration::from_secs(5)));
    let mut best: Option<(SystemTime, PathBuf)> = None;
    collect_rollouts(&sessions, cwd, floor, &mut best);
    best.map(|(_, path)| path)
}

fn rollout_candidates(home: &Path, cwd: &Path, since: Option<SystemTime>) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = sqlite_rollout(home, cwd, since) {
        candidates.push(path);
    }
    let sessions = home.join("sessions");
    let floor = since.and_then(|since| since.checked_sub(std::time::Duration::from_secs(5)));
    let mut scanned = Vec::new();
    collect_all_rollouts(&sessions, cwd, floor, &mut scanned);
    scanned.sort_by_key(|left| std::cmp::Reverse(left.0));
    for (_, path) in scanned {
        if !candidates.contains(&path) {
            candidates.push(path);
        }
    }
    candidates.truncate(32);
    candidates
}

fn collect_all_rollouts(
    dir: &Path,
    cwd: &Path,
    floor: Option<SystemTime>,
    best: &mut Vec<(SystemTime, PathBuf)>,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_all_rollouts(&path, cwd, floor, best);
            continue;
        }
        if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if floor.is_some_and(|floor| modified < floor) {
            continue;
        }
        if !rollout_cwd(&path)
            .is_some_and(|rollout_cwd| same_directory(&rollout_cwd, &cwd.to_string_lossy()))
        {
            continue;
        }
        best.push((modified, path));
    }
}

/// The newest thread in this cwd from `state_5.sqlite`, if the store has one. The scan fallback
/// stays for stores without the database or without the columns this query needs.
fn sqlite_rollout(home: &Path, cwd: &Path, since: Option<SystemTime>) -> Option<PathBuf> {
    use rusqlite::Connection;

    let db_path = home.join("state_5.sqlite");
    if !db_path.is_file() {
        return None;
    }
    let db =
        Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let columns: Vec<String> = db
        .prepare("SELECT name FROM pragma_table_info('threads')")
        .ok()?
        .query_map([], |row| row.get::<_, String>(0))
        .ok()?
        .filter_map(|column| column.ok())
        .collect();
    if !["rollout_path", "cwd", "archived"]
        .iter()
        .all(|needed| columns.contains(&needed.to_string()))
    {
        return None;
    }
    let created = if columns.iter().any(|column| column == "created_at_ms") {
        "COALESCE(created_at_ms, created_at * 1000)"
    } else {
        "created_at * 1000"
    };
    let interactive = if columns.iter().any(|column| column == "source") {
        " AND source IN ('cli', 'vscode')"
    } else {
        ""
    };
    let cwd_text = cwd.to_string_lossy().to_string();
    let verbatim = format!("\\\\?\\{cwd_text}");
    let since_ms = since
        .and_then(|since| since.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    let query = format!(
        "SELECT rollout_path FROM threads WHERE cwd IN (?1, ?2) AND archived = 0 AND agent_role IS NULL{interactive} AND {created} >= ?3 ORDER BY {created} DESC LIMIT 1"
    );
    let rollout: Option<String> = db
        .query_row(
            &query,
            rusqlite::params![cwd_text, verbatim, since_ms],
            |row| row.get(0),
        )
        .ok();
    let rollout = rollout?;
    let path = PathBuf::from(&rollout);
    if !path.is_file() {
        return None;
    }
    // Only rollouts inside this store: a thread may name a file elsewhere.
    let sessions = home.join("sessions");
    let canonical_path = fs::canonicalize(&path).ok()?;
    let canonical_sessions = fs::canonicalize(&sessions).ok()?;
    canonical_path
        .starts_with(&canonical_sessions)
        .then_some(path)
}

fn collect_rollouts(
    dir: &Path,
    cwd: &Path,
    floor: Option<SystemTime>,
    best: &mut Option<(SystemTime, PathBuf)>,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rollouts(&path, cwd, floor, best);
            continue;
        }
        if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if floor.is_some_and(|floor| modified < floor) {
            continue;
        }
        if !rollout_cwd(&path)
            .is_some_and(|rollout_cwd| same_directory(&rollout_cwd, &cwd.to_string_lossy()))
        {
            continue;
        }
        if best
            .as_ref()
            .map(|(when, _)| modified > *when)
            .unwrap_or(true)
        {
            *best = Some((modified, path));
        }
    }
}

fn rollout_cwd(path: &Path) -> Option<String> {
    let header = read_header(path)?;
    header
        .get("payload")
        .and_then(|payload| payload.get("cwd"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn rollout_ids(path: &Path) -> (String, String) {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string();
    let header = read_header(path);
    let id = header
        .as_ref()
        .and_then(|header| header.get("payload"))
        .and_then(|payload| payload.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| name.clone());
    (id, name)
}

fn read_header(path: &Path) -> Option<Value> {
    let text = fs::read_to_string(path).ok()?;
    let first = text.lines().next()?;
    let value: Value = serde_json::from_str(first).ok()?;
    (value.get("type").and_then(Value::as_str) == Some("session_meta")).then_some(value)
}

fn read_range(path: &Path, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = fs::File::open(path)?;
    if start > 0 {
        file.seek(SeekFrom::Start(start))?;
    }
    let mut bytes = Vec::new();
    let want = end.saturating_sub(start).min(u32::MAX as u64);
    (&mut file).take(want).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// The rollout files this conversation spans, oldest first, each with the byte count that
/// belongs to it. A paginated rollout continues its `history_base`; bytes past the cut are
/// discarded turns and are never read.
fn history_segments(path: &Path, home: &Path) -> Vec<(PathBuf, u64)> {
    let size = fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let chain = history_chain(path, home);
    let mut segments: Vec<(PathBuf, u64)> = chain.into_iter().rev().collect();
    segments.push((path.to_path_buf(), size));
    segments
}

fn history_chain(path: &Path, home: &Path) -> Vec<(PathBuf, u64)> {
    let mut chain = Vec::new();
    let mut current = path.to_path_buf();
    for _ in 0..32 {
        let Some(base) = history_base(&current) else {
            break;
        };
        let mut holders: Vec<PathBuf> = thread_rollouts(home, &base.0)
            .into_iter()
            .filter(|candidate| {
                candidate != &current
                    && !chain.iter().any(|(existing, _)| existing == candidate)
                    && holds_cut(candidate, base.1, base.2)
            })
            .collect();
        if holders.len() != 1 {
            break;
        }
        let holder = holders.remove(0);
        chain.push((holder.clone(), base.2));
        current = holder;
    }
    chain
}

fn history_base(path: &Path) -> Option<(String, u64, u64)> {
    let header = read_header(path)?;
    let base = header.get("payload")?.get("history_base")?;
    if base.as_object().is_some_and(|object| object.is_empty()) {
        return None;
    }
    let thread = base.get("thread_id")?.as_str()?.to_string();
    if thread.is_empty() {
        return None;
    }
    let ordinal = base.get("end_ordinal_exclusive")?.as_u64()?;
    let end = base.get("end_byte_offset")?.as_u64()?;
    (ordinal > 0 && end > 0).then_some((thread, ordinal, end))
}

fn first_ordinal(path: &Path) -> u64 {
    read_header(path)
        .and_then(|header| {
            header
                .get("payload")?
                .get("history_base")?
                .get("end_ordinal_exclusive")?
                .as_u64()
        })
        .filter(|ordinal| *ordinal > 0)
        .unwrap_or(0)
}

fn holds_cut(path: &Path, ordinal: u64, end: u64) -> bool {
    if read_header(path).is_none() {
        return false;
    }
    let lines = ordinal.saturating_sub(first_ordinal(path));
    if lines == 0 {
        return false;
    }
    let Ok(bytes) = read_range(path, 0, end) else {
        return false;
    };
    if bytes.len() as u64 != end || bytes.last() != Some(&b'\n') {
        return false;
    }
    bytes.iter().filter(|byte| **byte == b'\n').count() as u64 == lines
}

fn thread_rollouts(home: &Path, thread_id: &str) -> Vec<PathBuf> {
    let sessions = home.join("sessions");
    let mut rollouts = Vec::new();
    collect_thread_rollouts(&sessions, thread_id, &mut rollouts);
    rollouts
}

fn collect_thread_rollouts(dir: &Path, thread_id: &str, rollouts: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_thread_rollouts(&path, thread_id, rollouts);
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.ends_with(&format!("-{thread_id}.jsonl"))
            || name.contains(&format!("-{thread_id}_"))
        {
            rollouts.push(path);
        }
    }
}

fn same_directory(left: &str, right: &str) -> bool {
    normalize_dir(left) == normalize_dir(right)
}

/// Normalize display text the way the matcher compares it: letters and digits only.
fn normalize_display(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// The anchor of an answer as the screen would show it: its last 160 letters/digits, once
/// markdown link targets are cut. Short or repetitive answers anchor nothing.
fn answer_anchor(text: &str) -> Option<String> {
    let pieces: Vec<String> = {
        let mut pieces = Vec::new();
        let mut start = 0usize;
        let bytes = text.as_bytes();
        let mut search = 0usize;
        while search < bytes.len() {
            let Some(relative) = text[search..].find("](") else {
                break;
            };
            let at = search + relative;
            let mut end = at + 2;
            while end < bytes.len() && bytes[end] != b')' && !bytes[end].is_ascii_whitespace() {
                end += 1;
            }
            if end >= bytes.len() || bytes[end] != b')' {
                search = end.max(at + 1);
                continue;
            }
            pieces.push(text[start..at].to_string());
            start = end + 1;
            search = end + 1;
        }
        pieces.push(text[start..].to_string());
        pieces
    };
    for piece in pieces.iter().rev() {
        let anchor: String = normalize_display(piece)
            .chars()
            .rev()
            .take(160)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        if anchor.len() >= 64
            && anchor
                .chars()
                .collect::<std::collections::HashSet<char>>()
                .len()
                >= 12
        {
            return Some(anchor);
        }
    }
    None
}

/// Parse one rollout's text into assistant text parts (the matcher's evidence).
fn assistant_texts(text: &str) -> Vec<String> {
    let mut transcript = CodexTranscript {
        path: PathBuf::from("candidate.jsonl"),
        segments: Vec::new(),
        session_id: String::new(),
        history_id: String::new(),
        turns: Vec::new(),
        tools: HashMap::new(),
        messages: Vec::new(),
        started_at: None,
        active_turn_id: None,
        signature: String::new(),
    };
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        ingest(&mut transcript, &entry);
    }
    transcript
        .turns
        .iter()
        .filter(|turn| turn.get("role").and_then(Value::as_str) == Some("assistant"))
        .flat_map(|turn| {
            turn.get("parts")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .filter(|part| part.get("kind").and_then(Value::as_str) == Some("text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str).map(str::to_string))
        .collect()
}

/// The rollout whose assistant answer shows on this screen, when exactly one does.
pub fn match_codex_transcript(screen: &str, candidates: &[(PathBuf, String)]) -> Option<PathBuf> {
    let display = match screen.rfind("OpenAI Codex (v") {
        Some(index) => normalize_display(&screen[index..]),
        None => normalize_display(screen),
    };
    let mut matching = Vec::new();
    for (path, text) in candidates {
        let prose = assistant_texts(text);
        if prose
            .iter()
            .rev()
            .take(8)
            .any(|part| answer_anchor(part).is_some_and(|anchor| display.contains(&anchor)))
        {
            matching.push(path.clone());
        }
    }
    (matching.len() == 1).then(|| matching.remove(0))
}

fn normalize_dir(value: &str) -> String {
    value.trim_end_matches(['\\', '/']).to_ascii_lowercase()
}

fn ingest(state: &mut CodexTranscript, entry: &Value) -> bool {
    let payload = entry.get("payload").cloned().unwrap_or(Value::Null);
    let ts = entry
        .get("timestamp")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    match entry
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "event_msg" => match payload
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "task_started" => {
                state.started_at = Some(
                    payload
                        .get("started_at")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| ts.clone()),
                );
                state.active_turn_id = payload
                    .get("turn_id")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                false
            }
            "task_complete" | "turn_aborted" => {
                state.started_at = None;
                if let Some(turn) = state.turns.last_mut() {
                    if turn.get("role").and_then(Value::as_str) == Some("assistant")
                        && !ts.is_empty()
                    {
                        turn["end_ts"] = json!(ts);
                        return true;
                    }
                }
                false
            }
            "user_message" => {
                if payload
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("plain")
                    != "plain"
                {
                    return false;
                }
                let text = payload
                    .get("message")
                    .map(|message| content_text(message, true))
                    .unwrap_or_default();
                message(state, "user", &text, "event", &ts, None)
            }
            "agent_message" => {
                let text = payload
                    .get("message")
                    .map(|message| content_text(message, false))
                    .unwrap_or_default();
                let phase = match payload.get("phase").and_then(Value::as_str) {
                    Some("commentary") => Some("commentary"),
                    Some("final_answer") => Some("final_answer"),
                    _ => None,
                };
                message(state, "assistant", &text, "event", &ts, phase)
            }
            _ => false,
        },
        "response_item" => match payload
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "message" => {
                let role = payload
                    .get("role")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let content = payload.get("content").cloned().unwrap_or(Value::Null);
                if role == "user" {
                    message(
                        state,
                        "user",
                        &content_text(&content, true),
                        "response",
                        &ts,
                        None,
                    )
                } else if role == "assistant" {
                    let body = content_text(&content, false);
                    if payload.get("channel").and_then(Value::as_str) == Some("analysis") {
                        if body.trim().is_empty() {
                            return false;
                        }
                        let turn = assistant_turn(state, &ts);
                        push_part(state, turn, json!({ "kind": "thinking", "text": body }));
                        true
                    } else if payload
                        .get("recipient")
                        .and_then(Value::as_str)
                        .is_none_or(|recipient| recipient == "all")
                    {
                        let phase = match payload.get("phase").and_then(Value::as_str) {
                            Some("commentary") => Some("commentary"),
                            Some("final_answer") => Some("final_answer"),
                            _ => None,
                        };
                        message(state, "assistant", &body, "response", &ts, phase)
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
            "reasoning" => {
                let body = payload
                    .get("summary")
                    .map(|summary| content_text(summary, false))
                    .unwrap_or_default();
                if body.trim().is_empty() {
                    return false;
                }
                let turn = assistant_turn(state, &ts);
                push_part(state, turn, json!({ "kind": "thinking", "text": body }));
                true
            }
            "function_call" | "custom_tool_call" => {
                let name = payload
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
                    .to_string();
                let raw = if payload.get("type").and_then(Value::as_str) == Some("function_call") {
                    payload.get("arguments").cloned().unwrap_or(Value::Null)
                } else {
                    payload.get("input").cloned().unwrap_or(Value::Null)
                };
                let args = if raw.is_string() {
                    serde_json::from_str::<Value>(raw.as_str().unwrap_or_default())
                        .unwrap_or(Value::Null)
                } else {
                    raw.clone()
                };
                let summary = tool_summary(&name, &args, &raw);
                let input =
                    if args.is_object() && !args.as_object().is_some_and(|map| map.is_empty()) {
                        serde_json::to_string_pretty(&args).unwrap_or_default()
                    } else if raw.is_string() {
                        raw.as_str().unwrap_or_default().to_string()
                    } else {
                        String::new()
                    };
                let mut part = json!({
                    "kind": "tool",
                    "name": name,
                    "summary": summary,
                    "input": input,
                    "output": "",
                });
                if part["summary"]
                    .as_str()
                    .is_none_or(|summary| summary.is_empty())
                {
                    part["summary"] = json!(name);
                }
                let turn = assistant_turn(state, &ts);
                let part_index = push_part(state, turn, part);
                if let Some(call_id) = payload.get("call_id").and_then(Value::as_str) {
                    state.tools.insert(call_id.to_string(), (turn, part_index));
                }
                true
            }
            "function_call_output" | "custom_tool_call_output" => {
                let call_id = payload
                    .get("call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let Some((turn, part)) = state.tools.remove(call_id) else {
                    return false;
                };
                let Some(part_value) = state
                    .turns
                    .get_mut(turn)
                    .and_then(|turn| turn.get_mut("parts"))
                    .and_then(|parts| parts.get_mut(part))
                else {
                    return false;
                };
                let output = payload
                    .get("output")
                    .map(|output| content_text(output, false))
                    .unwrap_or_default();
                part_value["output"] = json!(output);
                if codex_call_failed(&output) {
                    part_value["error"] = json!(true);
                }
                if let Some(turn) = state.turns.last_mut() {
                    if turn.get("role").and_then(Value::as_str) == Some("assistant")
                        && !ts.is_empty()
                    {
                        turn["end_ts"] = json!(ts);
                    }
                }
                true
            }
            _ => false,
        },
        _ => false,
    }
}

fn assistant_turn(state: &mut CodexTranscript, ts: &str) -> usize {
    if let Some(index) = state.turns.len().checked_sub(1) {
        if state.turns[index].get("role").and_then(Value::as_str) == Some("assistant") {
            state.turns[index]["end_ts"] = json!(ts);
            return index;
        }
    }
    let created = state.started_at.clone().unwrap_or_else(|| ts.to_string());
    state
        .turns
        .push(json!({ "role": "assistant", "ts": created, "parts": [] }));
    state.turns.len() - 1
}

fn push_part(state: &mut CodexTranscript, turn: usize, part: Value) -> usize {
    let parts = state.turns[turn]["parts"]
        .as_array_mut()
        .expect("parts is an array");
    parts.push(part);
    parts.len() - 1
}

fn message(
    state: &mut CodexTranscript,
    role: &str,
    text: &str,
    source: &str,
    ts: &str,
    phase: Option<&str>,
) -> bool {
    let body = if role == "user" {
        question_reply(text).unwrap_or_else(|| text.to_string())
    } else {
        without_memory_citations(text)
    };
    if body.trim().is_empty() {
        return false;
    }
    if let Some(duplicate) = state.messages.iter_mut().rev().find(|other| {
        !other.paired
            && other.role == role
            && other.text == body
            && other.source != source
            && same_instant(&other.ts, ts)
    }) {
        duplicate.paired = true;
        if let Some(phase) = phase {
            duplicate.phase = Some(phase.to_string());
            if let Some(part_value) = state
                .turns
                .get_mut(duplicate.turn)
                .and_then(|turn| turn.get_mut("parts"))
                .and_then(|parts| parts.get_mut(duplicate.part))
            {
                part_value["phase"] = json!(phase);
            }
        }
        return true;
    }

    let mut part = json!({ "kind": "text", "text": body });
    if let Some(phase) = phase {
        part["phase"] = json!(phase);
    }
    let (turn_index, part_index) = if role == "user" {
        state
            .turns
            .push(json!({ "role": "user", "ts": ts, "parts": [part] }));
        (state.turns.len() - 1, 0)
    } else {
        let turn = assistant_turn(state, ts);
        (turn, push_part(state, turn, part))
    };
    state.messages.push(SentMessage {
        role: role.to_string(),
        text: body,
        source: source.to_string(),
        ts: ts.to_string(),
        paired: false,
        phase: phase.map(str::to_string),
        turn: turn_index,
        part: part_index,
    });
    if state.messages.len() > 8 {
        state.messages.remove(0);
    }
    true
}

fn same_instant(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }
    let Ok(left) = chrono::DateTime::parse_from_rfc3339(left) else {
        return false;
    };
    let Ok(right) = chrono::DateTime::parse_from_rfc3339(right) else {
        return false;
    };
    (left.timestamp_millis() - right.timestamp_millis()).abs() <= 1000
}

fn content_text(value: &Value, user: bool) -> String {
    match value {
        Value::String(text) => {
            if user && context_only(text) {
                String::new()
            } else {
                text.clone()
            }
        }
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| {
                let block_type = block
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !matches!(
                    block_type,
                    "input_text" | "output_text" | "text" | "summary_text"
                ) {
                    return None;
                }
                let text = block.get("text").and_then(Value::as_str)?;
                if user && context_only(text) {
                    return None;
                }
                if user && (text.starts_with("<image name=") || text == "</image>") {
                    return None;
                }
                Some(text.to_string())
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn context_only(text: &str) -> bool {
    let value = text.trim();
    if value.starts_with("# AGENTS.md instructions")
        && value.contains("<INSTRUCTIONS>")
        && value.contains("</INSTRUCTIONS>")
    {
        return true;
    }
    [
        "environment_context",
        "permissions instructions",
        "turn_aborted",
        "subagent_notification",
    ]
    .iter()
    .any(|tag| value.starts_with(&format!("<{tag}>")) && value.ends_with(&format!("</{tag}>")))
}

fn question_reply(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let inner = trimmed
        .strip_prefix("<send_user_message_question_reply>")?
        .strip_suffix("</send_user_message_question_reply>")?
        .trim();
    let items: Value = serde_json::from_str(inner).ok()?;
    let answers = items
        .as_array()?
        .iter()
        .filter_map(|item| item.get("answer").and_then(Value::as_str))
        .map(str::trim)
        .filter(|answer| !answer.is_empty())
        .collect::<Vec<_>>();
    (!answers.is_empty()).then(|| answers.join("\n"))
}

fn without_memory_citations(text: &str) -> String {
    let opening = "<oai-mem-citation>";
    let closing = "</oai-mem-citation>";
    if !text.contains(opening) {
        return text.trim_end().to_string();
    }
    let mut out = String::new();
    let mut rest = text;
    while let Some(index) = rest.find(opening) {
        let before = &rest[..index];
        if inside_fence(before) {
            out.push_str(before);
            out.push_str(opening);
            rest = &rest[index + opening.len()..];
            continue;
        }
        out.push_str(before);
        let after_open = &rest[index + opening.len()..];
        match after_open.find(closing) {
            Some(end) => rest = &after_open[end + closing.len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.trim_end().to_string()
}

fn inside_fence(chunk: &str) -> bool {
    let mut in_fence = false;
    let mut marker: Option<(char, usize)> = None;
    for line in chunk.lines() {
        let trimmed = line.trim_start();
        let first = trimmed.chars().next();
        match (in_fence, first) {
            (false, Some(c)) if c == '`' || c == '~' => {
                let count = trimmed
                    .chars()
                    .take_while(|candidate| *candidate == c)
                    .count();
                if count >= 3 {
                    in_fence = true;
                    marker = Some((c, count));
                }
            }
            (true, Some(c)) => {
                if let Some((marker_char, count)) = marker {
                    if c == marker_char {
                        let here = trimmed
                            .chars()
                            .take_while(|candidate| *candidate == c)
                            .count();
                        if here >= count {
                            in_fence = false;
                            marker = None;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    in_fence
}

fn codex_call_failed(output: &str) -> bool {
    let head: String = output.chars().take(2000).collect();
    if head
        .lines()
        .any(|line| line.starts_with("Script completed"))
    {
        return false;
    }
    if head.lines().any(|line| line.starts_with("Script failed"))
        || head.contains("apply_patch verification failed")
    {
        return true;
    }
    for marker in ["Process exited with code ", "Exit code: ", "\"exit_code\":"] {
        let mut search = head.as_str();
        while let Some(index) = search.find(marker) {
            let after = search[index + marker.len()..].trim_start();
            let digits = after
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>();
            if let Ok(code) = digits.parse::<i64>() {
                return code != 0;
            }
            search = &search[index + marker.len()..];
        }
    }
    false
}

fn tool_summary(name: &str, args: &Value, raw: &Value) -> String {
    if name.starts_with("request_user_input") {
        if let Some(titles) = args
            .get("questions")
            .and_then(Value::as_array)
            .map(|questions| {
                questions
                    .iter()
                    .filter_map(|question| {
                        question
                            .get("title")
                            .or_else(|| question.get("question"))
                            .and_then(Value::as_str)
                    })
                    .collect::<Vec<_>>()
            })
            .filter(|titles| !titles.is_empty())
        {
            return titles.join(" · ");
        }
    }
    if name == "apply_patch" {
        if let Some(text) = raw.as_str() {
            if text.contains("*** Begin Patch") || text.contains("*** Update File") {
                return "apply_patch".to_string();
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

    fn entry(kind: &str, payload: Value) -> Value {
        json!({ "type": kind, "timestamp": "2026-09-22T01:00:00.000Z", "payload": payload })
    }

    fn message(role: &str, text: &str) -> Value {
        let content_type = if role == "user" {
            "input_text"
        } else {
            "output_text"
        };
        entry(
            "response_item",
            json!({
                "type": "message",
                "role": role,
                "content": [{ "type": content_type, "text": text }],
            }),
        )
    }

    #[test]
    fn parses_user_assistant_and_tool_output() {
        let mut transcript = CodexTranscript {
            path: PathBuf::from("rollout.jsonl"),
            segments: Vec::new(),
            session_id: "t1".into(),
            history_id: "h1".into(),
            turns: Vec::new(),
            tools: HashMap::new(),
            messages: Vec::new(),
            started_at: None,
            active_turn_id: None,
            signature: String::new(),
        };
        assert!(ingest(&mut transcript, &message("user", "Inspect this")));
        assert!(ingest(
            &mut transcript,
            &entry(
                "response_item",
                json!({
                    "type": "function_call",
                    "call_id": "read",
                    "name": "exec_command",
                    "arguments": "{\"cmd\":\"git status\"}",
                }),
            )
        ));
        assert!(ingest(
            &mut transcript,
            &entry(
                "response_item",
                json!({ "type": "function_call_output", "call_id": "read", "output": [{ "type": "text", "text": "clean" }] }),
            )
        ));
        assert!(ingest(&mut transcript, &message("assistant", "Clean.")));

        assert_eq!(transcript.turns.len(), 2);
        assert_eq!(transcript.turns[0]["role"], "user");
        assert_eq!(transcript.turns[1]["role"], "assistant");
        let part = &transcript.turns[1]["parts"][0];
        assert_eq!(part["kind"], "tool");
        assert_eq!(part["output"], "clean");
        assert_eq!(transcript.turns[1]["parts"][1]["text"], "Clean.");
    }

    #[test]
    fn pairs_event_and_response_duplicates() {
        let mut transcript = CodexTranscript {
            path: PathBuf::from("rollout.jsonl"),
            segments: Vec::new(),
            session_id: "t1".into(),
            history_id: "h1".into(),
            turns: Vec::new(),
            tools: HashMap::new(),
            messages: Vec::new(),
            started_at: None,
            active_turn_id: None,
            signature: String::new(),
        };
        assert!(ingest(&mut transcript, &message("assistant", "Done")));
        assert!(ingest(
            &mut transcript,
            &entry(
                "event_msg",
                json!({ "type": "agent_message", "message": "Done" })
            )
        ));
        assert_eq!(transcript.turns.len(), 1);
        assert_eq!(
            transcript.turns[0]["parts"]
                .as_array()
                .expect("parts")
                .len(),
            1
        );
    }

    #[test]
    fn sqlite_thread_wins_over_newer_scan_candidate() {
        use rusqlite::Connection;
        let home =
            std::env::temp_dir().join(format!("orion-codex-sqlite-{}", uuid::Uuid::new_v4()));
        let sessions = home.join("sessions").join("2026").join("09").join("22");
        std::fs::create_dir_all(&sessions).expect("sessions dir");
        let cwd = std::env::temp_dir().join("orion-codex-cwd");
        std::fs::create_dir_all(&cwd).expect("cwd dir");

        let sqlite_path = sessions.join("rollout-sqlite.jsonl");
        let newer_rollout = sessions.join("rollout-newer.jsonl");
        let header = |thread: &str| {
            json!({
                "type": "session_meta",
                "payload": { "id": thread, "cwd": cwd.to_string_lossy() },
            })
            .to_string()
        };
        std::fs::write(&sqlite_path, format!("{}\n", header("sqlite-thread"))).expect("write");
        std::fs::write(&newer_rollout, format!("{}\n", header("newer-thread"))).expect("write");
        let now = std::time::SystemTime::now();
        let _ = filetime_set(&sqlite_path, now - std::time::Duration::from_secs(60));
        let _ = filetime_set(&newer_rollout, now);

        let db = Connection::open(home.join("state_5.sqlite")).expect("db");
        db.execute_batch(
            "CREATE TABLE threads (id TEXT, cwd TEXT, rollout_path TEXT, archived INTEGER, agent_role TEXT, source TEXT, created_at INTEGER, created_at_ms INTEGER);",
        )
        .expect("schema");
        db.execute(
            "INSERT INTO threads (id, cwd, rollout_path, archived, agent_role, source, created_at, created_at_ms) VALUES (?, ?, ?, 0, NULL, 'cli', ?, ?)",
            rusqlite::params![
                "sqlite-thread",
                cwd.to_string_lossy(),
                sqlite_path.to_string_lossy(),
                1_700_000_000,
                1_700_000_000_000i64,
            ],
        )
        .expect("insert");
        drop(db);

        let path = sqlite_rollout(&home, &cwd, None).expect("sqlite rollout");
        assert_eq!(path, sqlite_path);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn matches_the_unique_rollout_whose_answer_shows_on_screen() {
        let answer =
            "The rollout parser now reads native session records and keeps the history intact.";
        let screen = format!("OpenAI Codex (v0.1)\nSome earlier output\n{answer}\n");
        let wrong = vec![message(
            "assistant",
            "A completely different answer that is quite long and detailed too.",
        )];
        let right = vec![message("assistant", answer)];
        let candidates = vec![
            (
                PathBuf::from("wrong.jsonl"),
                wrong
                    .into_iter()
                    .map(|line| line.to_string())
                    .collect::<String>(),
            ),
            (
                PathBuf::from("right.jsonl"),
                right
                    .into_iter()
                    .map(|line| line.to_string())
                    .collect::<String>(),
            ),
        ];
        // The candidate text is already JSONL; rebuild with newlines for the matcher.
        let candidates = candidates
            .into_iter()
            .map(|(path, text)| (path, format!("{}\n", text)))
            .collect::<Vec<_>>();
        assert_eq!(
            match_codex_transcript(&screen, &candidates),
            Some(PathBuf::from("right.jsonl"))
        );
    }

    #[test]
    fn parses_orbit_codex_fixture() {
        let path = std::env::var("ORBIT_CODEX_FIXTURE")
            .unwrap_or_else(|_| "fixtures/codex.jsonl".to_string());
        let fixture = std::path::Path::new(&path);
        let Ok(text) = fs::read_to_string(fixture) else {
            eprintln!("skipped: Orbit codex fixture not present");
            return;
        };
        let mut transcript = CodexTranscript {
            path: PathBuf::from("fixture.jsonl"),
            segments: Vec::new(),
            session_id: "t".into(),
            history_id: "h".into(),
            turns: Vec::new(),
            tools: HashMap::new(),
            messages: Vec::new(),
            started_at: None,
            active_turn_id: None,
            signature: String::new(),
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(entry) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            ingest(&mut transcript, &entry);
        }
        let texts = transcript
            .turns
            .iter()
            .flat_map(|turn| {
                turn.get("parts")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            })
            .filter_map(|part| part.get("text").and_then(Value::as_str).map(str::to_string))
            .collect::<Vec<_>>();
        assert!(
            texts.contains(&"$probe-skill hello".to_string()),
            "user prompt: {texts:?}"
        );
        assert!(
            texts.contains(&"PROBED".to_string()),
            "final answer: {texts:?}"
        );
    }

    #[test]
    fn follows_history_base_to_the_parent_rollout() {
        let home = std::env::temp_dir().join(format!("orion-codex-chain-{}", uuid::Uuid::new_v4()));
        let sessions = home.join("sessions").join("2026").join("09").join("22");
        std::fs::create_dir_all(&sessions).expect("sessions dir");
        let parent = "01a09a35-5c5f-7830-94f7-4a1854613531";
        let thread = "01a0a337-19e8-7712-92f5-aa0883392afd";

        let parent_path = sessions.join(format!("rollout-parent-{parent}.jsonl"));
        let parent_lines = [
            json!({ "type": "session_meta", "payload": { "source": "cli", "thread_source": "user" } }),
            message("user", "parent question"),
            message("assistant", "parent answer"),
        ];
        let parent_text = parent_lines
            .iter()
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        std::fs::write(&parent_path, &parent_text).expect("parent");

        let base = json!({
            "thread_id": parent,
            "end_ordinal_exclusive": 3,
            "end_byte_offset": parent_text.len(),
        });
        let child_path = sessions.join(format!("rollout-child-{thread}.jsonl"));
        let child_lines = [
            json!({ "type": "session_meta", "payload": { "source": "cli", "thread_source": "user", "history_base": base } }),
            message("user", "later question"),
            message("assistant", "later answer"),
        ];
        std::fs::write(
            &child_path,
            child_lines
                .iter()
                .map(|line| format!("{line}\n"))
                .collect::<String>(),
        )
        .expect("child");

        let segments = history_segments(&child_path, &home);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].0, parent_path);
        assert_eq!(segments[0].1 as usize, parent_text.len());
        assert_eq!(segments[1].0, child_path);

        let mut transcript = CodexTranscript {
            path: child_path.clone(),
            segments: segments
                .into_iter()
                .map(|(path, end)| Segment {
                    path,
                    end,
                    offset: 0,
                })
                .collect(),
            session_id: thread.into(),
            history_id: "h".into(),
            turns: Vec::new(),
            tools: HashMap::new(),
            messages: Vec::new(),
            started_at: None,
            active_turn_id: None,
            signature: String::new(),
        };
        assert!(transcript.refresh());
        let texts = transcript
            .turns
            .iter()
            .map(|turn| {
                turn["parts"][0]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            texts,
            vec![
                "parent question",
                "parent answer",
                "later question",
                "later answer"
            ]
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    fn filetime_set(path: &Path, when: SystemTime) -> std::io::Result<()> {
        let file = fs::File::options().write(true).open(path)?;
        file.set_modified(when)
    }
}
