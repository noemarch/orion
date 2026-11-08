//! Reading Claude Code's own transcript.
//!
//! Claude writes one JSONL file per session under `<config dir>/projects/<project>/<session>.jsonl`
//! and appends to it as a turn runs. That file is the chat lens's source of truth: it has the
//! prompt, the assistant's text, its thinking and every tool call with its output, none of which
//! the terminal grid keeps once it scrolls away.
//!
//! The reader is incremental: it remembers a byte offset and only parses what was appended, so a
//! long session costs the same per poll as a short one. A torn tail line — Claude mid-append — is
//! re-read on the next refresh instead of being parsed as garbage.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::{json, Map, Value};

/// A session file plus everything parsed from it so far.
pub struct ClaudeTranscript {
    path: PathBuf,
    /// Bytes consumed; always just past a newline.
    offset: u64,
    session_id: String,
    turns: Vec<Value>,
    /// Tools whose result has not arrived yet, by `tool_use` id: `(turn index, part index)`.
    pending: Vec<(String, usize, usize)>,
    signature: String,
}

impl ClaudeTranscript {
    /// Finds the newest session file for a directory, or one named by a session id.
    ///
    /// `since` keeps a fresh pane from borrowing a conversation that was already there: a file
    /// last written before the pane started belongs to somebody else in the same folder.
    pub fn open(cwd: &Path, session_id: Option<&str>, since: Option<SystemTime>) -> Option<Self> {
        let config = config_dir()?;
        let path = match session_id {
            Some(id) => find_session(&config, id)?,
            None => newest_session(&config, cwd, since)?,
        };
        let id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_string();
        let mut transcript = Self {
            path,
            offset: 0,
            session_id: id,
            turns: Vec::new(),
            pending: Vec::new(),
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

    /// Ingests everything appended since the last call. Returns whether the transcript changed.
    pub fn refresh(&mut self) -> bool {
        let Ok(bytes) = read_from(&self.path, self.offset) else {
            return false;
        };
        let Some(end) = bytes.iter().rposition(|byte| *byte == b'\n') else {
            return false;
        };
        let consumed = &bytes[..=end];
        self.offset += consumed.len() as u64;
        let text = String::from_utf8_lossy(consumed);
        let mut changed = false;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(entry) = serde_json::from_str::<Value>(line) else {
                continue; // a torn or unknown record: skip it, never fail the lens
            };
            if ingest(&mut self.turns, &mut self.pending, &entry) {
                changed = true;
            }
        }
        if changed {
            self.signature = format!("{}:{}", self.session_id, self.turns.len());
        }
        trim(&mut self.turns);
        changed
    }

    /// The wire answer (`ConversationResponse`).
    pub fn response(&self) -> Value {
        let mut body = Map::new();
        body.insert(
            "source".to_string(),
            Value::String("claude-transcript".to_string()),
        );
        body.insert("turns".to_string(), Value::Array(self.turns.clone()));
        body.insert(
            "history_id".to_string(),
            Value::String(self.session_id.clone()),
        );
        Value::Object(body)
    }

    pub fn turns(&self) -> &[Value] {
        &self.turns
    }
}

fn read_from(path: &Path, offset: u64) -> std::io::Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = fs::File::open(path)?;
    if offset > 0 {
        file.seek(SeekFrom::Start(offset))?;
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Parses one record into the turns it belongs to. Returns whether anything was added.
fn ingest(
    turns: &mut Vec<Value>,
    pending: &mut Vec<(String, usize, usize)>,
    entry: &Value,
) -> bool {
    if entry.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return false;
    }
    if entry.get("isMeta").and_then(Value::as_bool) == Some(true) {
        return false;
    }
    let kind = entry
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let timestamp = entry
        .get("timestamp")
        .and_then(Value::as_str)
        .map(str::to_string);
    let content = entry
        .get("message")
        .and_then(|message| message.get("content"));

    match kind {
        "user" => match content {
            Some(Value::String(text)) => {
                if text.trim().is_empty() || is_command(text) {
                    return false;
                }
                turns.push(json!({
                    "role": "user",
                    "ts": timestamp,
                    "parts": [{ "kind": "text", "text": unwrap_pastes(text) }],
                }));
                true
            }
            Some(Value::Array(blocks)) => {
                let mut prompt = Vec::new();
                let mut touched = false;
                for block in blocks {
                    let block_type = block
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    match block_type {
                        "text" => {
                            if let Some(text) = block.get("text").and_then(Value::as_str) {
                                if !text.trim().is_empty() && !is_command(text) {
                                    prompt.push(unwrap_pastes(text));
                                }
                            }
                        }
                        "tool_result" => {
                            let Some(id) = block.get("tool_use_id").and_then(Value::as_str) else {
                                continue;
                            };
                            let Some((_, turn, part)) =
                                pending.iter().find(|(pending_id, ..)| pending_id == id)
                            else {
                                continue;
                            };
                            let (turn, part) = (*turn, *part);
                            if let Some(part_value) = turns
                                .get_mut(turn)
                                .and_then(|turn| turn.get_mut("parts"))
                                .and_then(|parts| parts.get_mut(part))
                            {
                                part_value["output"] = json!(result_text(block.get("content")));
                                if block.get("is_error").and_then(Value::as_bool) == Some(true) {
                                    part_value["error"] = json!(true);
                                }
                                touched = true;
                            }
                            pending.retain(|(pending_id, ..)| pending_id != id);
                        }
                        _ => {}
                    }
                }
                if !prompt.is_empty() {
                    turns.push(json!({
                        "role": "user",
                        "ts": timestamp,
                        "parts": [{ "kind": "text", "text": prompt.join("\n") }],
                    }));
                    touched = true;
                }
                touched
            }
            _ => false,
        },
        "assistant" => {
            let Some(Value::Array(blocks)) = content else {
                return false;
            };
            let (parts, tool_ids) = assistant_parts(blocks);
            if parts.is_empty() {
                return false;
            }
            // One assistant turn is several records: text, then a tool call, then more text.
            let merged = matches!(
                turns
                    .last()
                    .and_then(|last| last.get("role"))
                    .and_then(Value::as_str),
                Some("assistant")
            );
            let (turn_index, first_part) = if merged {
                let index = turns.len() - 1;
                let Some(existing) = turns[index].get_mut("parts").and_then(Value::as_array_mut)
                else {
                    return false;
                };
                let first = existing.len();
                existing.extend(parts);
                (index, first)
            } else {
                turns.push(json!({
                    "role": "assistant",
                    "ts": timestamp,
                    "parts": parts,
                }));
                (turns.len() - 1, 0)
            };
            if let Some(ts) = timestamp {
                turns[turn_index]["end_ts"] = json!(ts);
            }
            for (id, offset) in tool_ids {
                pending.push((id, turn_index, first_part + offset));
            }
            true
        }
        _ => false,
    }
}

fn assistant_parts(blocks: &[Value]) -> (Vec<Value>, Vec<(String, usize)>) {
    let mut parts = Vec::new();
    let mut tools = Vec::new();
    for block in blocks {
        match block
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "text" => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    if !text.is_empty() {
                        parts.push(json!({ "kind": "text", "text": text }));
                    }
                }
            }
            "thinking" => {
                if let Some(text) = block.get("thinking").and_then(Value::as_str) {
                    if !text.is_empty() {
                        parts.push(json!({ "kind": "thinking", "text": text }));
                    }
                }
            }
            "tool_use" => {
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
                    .to_string();
                let input = block.get("input").cloned().unwrap_or(Value::Null);
                let summary = tool_summary(&name, &input);
                if let Some(id) = block.get("id").and_then(Value::as_str) {
                    tools.push((id.to_string(), parts.len()));
                }
                parts.push(json!({
                    "kind": "tool",
                    "name": name,
                    "summary": summary,
                    "input": input.to_string(),
                    "output": "",
                }));
            }
            _ => {}
        }
    }
    (parts, tools)
}

fn result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| {
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

fn tool_summary(name: &str, input: &Value) -> String {
    let field = match name {
        "Bash" => "command",
        "Read" | "Write" | "Edit" | "NotebookEdit" => "file_path",
        "Glob" | "Grep" => "pattern",
        "WebFetch" => "url",
        "Task" => "description",
        _ => "",
    };
    let detail = input.get(field).and_then(Value::as_str).unwrap_or_default();
    if detail.is_empty() {
        name.to_string()
    } else {
        format!("{name}: {detail}")
    }
}

fn trim(turns: &mut Vec<Value>) {
    const MAX_TURNS: usize = 400;
    if turns.len() > MAX_TURNS {
        turns.drain(..turns.len() - MAX_TURNS);
    }
}

/// Claude's own encoding of a project directory: every character that is not an ASCII letter or
/// digit becomes `-`; a name over 200 characters keeps its first 200 plus `-` and a base-36 hash
/// of the whole path.
pub fn project_dir_name(cwd: &str) -> String {
    const MAX: usize = 200;
    let name: String = cwd
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    if name.len() <= MAX {
        return name;
    }
    let suffix = java_hash(cwd).unsigned_abs();
    format!("{}-{}", &name[..MAX], radix36(suffix))
}

/// Java's `String.hashCode`, which Claude uses: 32-bit signed, over UTF-16 code units.
fn java_hash(text: &str) -> i32 {
    let mut hash: i32 = 0;
    for unit in text.encode_utf16() {
        hash = hash.wrapping_mul(31).wrapping_add(i32::from(unit));
    }
    hash
}

fn radix36(mut value: u32) -> String {
    if value == 0 {
        return "0".to_string();
    }
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    while value > 0 {
        out.push(DIGITS[(value % 36) as usize]);
        value /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    home_dir().map(|home| home.join(".claude"))
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
}

fn newest_session(config: &Path, cwd: &Path, since: Option<SystemTime>) -> Option<PathBuf> {
    let projects = config.join("projects");
    let encoded = project_dir_name(&cwd.to_string_lossy());
    let dir = projects.join(&encoded);
    let mut best: Option<(SystemTime, PathBuf)> = None;
    collect_newest(&dir, since, &mut best);
    best.map(|(_, path)| path)
}

fn collect_newest(dir: &Path, since: Option<SystemTime>, best: &mut Option<(SystemTime, PathBuf)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    // A few seconds of slack: the pane is stamped just before the child writes its first record.
    let floor = since.and_then(|since| since.checked_sub(std::time::Duration::from_secs(5)));
    for entry in entries.flatten() {
        let path = entry.path();
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
        if best
            .as_ref()
            .map(|(when, _)| modified > *when)
            .unwrap_or(true)
        {
            *best = Some((modified, path));
        }
    }
}

fn find_session(config: &Path, session_id: &str) -> Option<PathBuf> {
    let projects = config.join("projects");
    let name = format!("{session_id}.jsonl");
    let entries = fs::read_dir(&projects).ok()?;
    for entry in entries.flatten() {
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        let candidate = entry.path().join(&name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn is_command(text: &str) -> bool {
    let text = text.trim();
    text.starts_with("<command-name>") || text.starts_with("<local-command")
}

/// A pasted block Claude's TUI wraps in markers is shown as the text the user pasted.
fn unwrap_pastes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<paste>") {
        out.push_str(&rest[..start]);
        let after = &rest[start + "<paste>".len()..];
        if let Some(end) = after.find("</paste>") {
            out.push_str(after[..end].trim_end());
            rest = &after[end + "</paste>".len()..];
        } else {
            out.push_str(after);
            return out;
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_project_directory_is_encoded_as_claude_does() {
        assert_eq!(
            project_dir_name("G:\\Temp\\tmp\\opencode\\m1-probe"),
            "G--Temp-tmp-opencode-m1-probe"
        );
        assert_eq!(
            project_dir_name("C:\\Users\\Asep Saepuloh\\my_project"),
            "C--Users-Asep-Saepuloh-my-project"
        );
        let long = "a".repeat(300);
        let encoded = project_dir_name(&long);
        let suffix = radix36(java_hash(&long).unsigned_abs());
        assert_eq!(encoded.len(), 200 + 1 + suffix.len());
        assert!(encoded.starts_with(&"a".repeat(200)));
        assert!(encoded.ends_with(&format!("-{suffix}")));
    }

    #[test]
    fn turns_merge_assistant_records_and_attach_tool_results() {
        let mut turns = Vec::new();
        let mut pending = Vec::new();
        let lines = [
            json!({"type":"user","timestamp":"2026-01-01T00:00:00Z","message":{"role":"user","content":"run it"}}),
            json!({"type":"assistant","timestamp":"2026-01-01T00:00:01Z","message":{"role":"assistant","content":[
                {"type":"text","text":"Running."},
                {"type":"tool_use","id":"tool-1","name":"Bash","input":{"command":"ls"}}
            ]}}),
            json!({"type":"user","timestamp":"2026-01-01T00:00:02Z","message":{"role":"user","content":[
                {"type":"tool_result","tool_use_id":"tool-1","content":"a.txt"}
            ]}}),
            json!({"type":"assistant","timestamp":"2026-01-01T00:00:03Z","message":{"role":"assistant","content":[
                {"type":"text","text":"Done."}
            ]}}),
        ];
        for line in lines {
            ingest(&mut turns, &mut pending, &line);
        }
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0]["role"], "user");
        assert_eq!(turns[1]["parts"].as_array().unwrap().len(), 3);
        let tool = &turns[1]["parts"][1];
        assert_eq!(tool["kind"], "tool");
        assert_eq!(tool["output"], "a.txt");
        assert_eq!(turns[1]["parts"][2]["text"], "Done.");
        // the internal marker never reaches the wire
        assert!(tool.get("__tool_use_id").is_none());
    }

    #[test]
    fn a_sidechain_is_not_part_of_the_conversation() {
        let mut turns = Vec::new();
        let mut pending = Vec::new();
        let entry = json!({"type":"assistant","isSidechain":true,"message":{"content":[{"type":"text","text":"subagent"}]}});
        assert!(!ingest(&mut turns, &mut pending, &entry));
        assert!(turns.is_empty());
    }

    #[test]
    fn a_tool_call_keeps_its_id_out_of_the_wire() {
        let mut turns = Vec::new();
        let mut pending = Vec::new();
        ingest(
            &mut turns,
            &mut pending,
            &json!({
                "type":"assistant",
                "message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"pwd"}}]}
            }),
        );
        assert_eq!(pending[0].0, "t1");
        assert!(turns[0]["parts"][0].get("__tool_use_id").is_none());
        assert_eq!(turns[0]["parts"][0]["summary"], "Bash: pwd");
    }

    #[test]
    fn parses_orbit_claude_fixture() {
        let path = std::env::var("ORBIT_CLAUDE_FIXTURE")
            .unwrap_or_else(|_| "fixtures/claude.jsonl".to_string());
        let fixture = std::path::Path::new(&path);
        let Ok(text) = fs::read_to_string(fixture) else {
            eprintln!("skipped: Orbit claude fixture not present");
            return;
        };
        let mut turns = Vec::new();
        let mut pending = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(entry) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            ingest(&mut turns, &mut pending, &entry);
        }
        let texts = turns
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
            texts.contains(&"PROBED".to_string()),
            "final answer: {texts:?}"
        );
    }
}
