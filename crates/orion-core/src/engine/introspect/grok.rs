//! Reading Grok's native update log.
//!
//! Grok keeps one JSONL update log per session:
//! `<grok-home>/sessions/<group>/<session>/updates.jsonl`. Each line is a
//! `session/update` envelope with a `sessionUpdate` field naming the event kind
//! (`user_message_chunk`, `agent_message_chunk`, `tool_call`, `tool_call_update`,
//! `turn_completed`, `agent_thought_chunk`, `rewind_marker`). The reader projects these
//! events onto conversation turns: prompts, answers, tool calls, rewinds.
//!
//! A pane is bound to a specific `updates.jsonl` by session id. Orion has no external
//! `agent_session` report, so binding falls back to scanning the Grok home for
//! `updates.jsonl` files; without a match the pane falls back to the terminal.

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::SecondsFormat;
use serde_json::{json, Value};
use sha2::Digest;

use crate::protocol::AgentStatus;

use super::omo::tool_summary;

/// One parsed JSONL event: the parsed envelope, its byte range in the file, and the session.
#[allow(dead_code)]
struct GrokEvent {
    update: Value,
    session: String,
    ts: Option<String>,
    start: u64,
    end: u64,
}

/// One prompt in the index: its index number, byte range, answered state, active tool calls.
#[allow(dead_code)]
struct Prompt {
    index: u64,
    start: u64,
    end: u64,
    answered: bool,
    activity: String,
    calls: HashSet<String>,
}

/// One tool call in the index: which prompt owns it, its current revision/digest.
#[allow(dead_code)]
struct Call {
    owner: u64,
    revision: u64,
    digest: String,
}

/// The projection index for one Grok updates.jsonl: prompts, calls, revision, scan position.
struct GrokIndex {
    prompts: Vec<Prompt>,
    calls: HashMap<String, Call>,
    revision: u64,
    scanned: u64,
    failed: bool,
}

/// One Grok session's update log, projected onto conversation turns.
pub struct GrokTranscript {
    path: PathBuf,
    session_id: String,
    history_id: String,
    index: GrokIndex,
    turns: Vec<Value>,
    status: Option<AgentStatus>,
}

const MAX_LINE: u64 = 16 * 1024 * 1024;
const SCAN_BYTES: u64 = 32 * 1024 * 1024;
const MAX_ENTRIES: usize = 100_000;

impl GrokTranscript {
    /// Opens a specific `updates.jsonl` file, which is how the fixture tests replay a
    /// recorded log. `session` is the session id that must match each event's
    /// `params.sessionId` (empty string matches any session, used by `open`).
    pub fn open_at(path: &Path, session: &str) -> Option<Self> {
        if !path.is_file() {
            return None;
        }
        let mut index = GrokIndex {
            prompts: Vec::new(),
            calls: HashMap::new(),
            revision: 0,
            scanned: 0,
            failed: false,
        };
        let mut turns: Vec<Value> = Vec::new();

        if Self::scan_file(path, session, &mut index, &mut turns, u64::MAX).is_err() {
            return None;
        }

        if turns.is_empty() && index.prompts.is_empty() {
            return None;
        }

        let status = Self::infer_status(&index, &turns);
        let history_id = format!("grok:{session}:{}", index.revision);

        Some(Self {
            path: path.to_path_buf(),
            session_id: session.to_string(),
            history_id,
            index,
            turns,
            status,
        })
    }

    /// Opens the Grok session for this pane by scanning the Grok home for
    /// `updates.jsonl` files. Without an external `agent_session` report, we cannot
    /// disambiguate multiple sessions, so we bind to the newest one that has events.
    pub fn open(_cwd: &Path, _since: Option<SystemTime>) -> Option<Self> {
        let home = env::var("GROK_HOME")
            .ok()
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("USERPROFILE")
                    .or_else(|| std::env::var_os("HOME"))
                    .map(PathBuf::from)
                    .map(|home| home.join(".grok"))
            })?;
        let sessions_dir = home.join("sessions");
        if !sessions_dir.is_dir() {
            return None;
        }

        let mut candidates: Vec<(PathBuf, u64)> = Vec::new();
        let Ok(entries) = fs::read_dir(&sessions_dir) else {
            return None;
        };
        for group_entry in entries {
            let Ok(group_entry) = group_entry else {
                continue;
            };
            let group_dir = group_entry.path();
            if !group_dir.is_dir() {
                continue;
            }
            let Ok(session_entries) = fs::read_dir(&group_dir) else {
                continue;
            };
            for session_entry in session_entries {
                let Ok(session_entry) = session_entry else {
                    continue;
                };
                let session_dir = session_entry.path();
                if !session_dir.is_dir() {
                    continue;
                }
                let updates = session_dir.join("updates.jsonl");
                if updates.is_file() {
                    if let Ok(meta) = fs::metadata(&updates) {
                        if let Ok(time) = meta.modified() {
                            candidates
                                .push((updates, time.elapsed().unwrap_or_default().as_secs()));
                        }
                    }
                }
            }
        }

        candidates.sort_by_key(|(_, mtime)| std::cmp::Reverse(*mtime));

        for (path, _) in candidates {
            if let Some(transcript) = Self::open_at(&path, "") {
                return Some(transcript);
            }
        }

        None
    }

    /// Ingests everything changed since the last call. Returns whether the chat changed.
    pub fn refresh(&mut self) -> bool {
        let metadata = match fs::metadata(&self.path) {
            Ok(m) => m,
            Err(_) => return false,
        };
        let size = metadata.len();

        if self.index.failed {
            return false;
        }

        // File shrank or was replaced: rebuild from scratch.
        if size < self.index.scanned {
            return self.rebuild();
        }

        if size == self.index.scanned {
            return false;
        }

        let mut changed = false;
        if Self::scan_file(
            &self.path,
            &self.session_id,
            &mut self.index,
            &mut self.turns,
            size,
        )
        .is_ok()
        {
            self.index.scanned = size;
            self.status = Self::infer_status(&self.index, &self.turns);
            changed = true;
        }
        changed
    }

    /// Rebuilds the turns from the beginning of the file.
    fn rebuild(&mut self) -> bool {
        self.index = GrokIndex {
            prompts: Vec::new(),
            calls: HashMap::new(),
            revision: 0,
            scanned: 0,
            failed: false,
        };
        let old_len = self.turns.len();
        self.turns.clear();

        if Self::scan_file(
            &self.path,
            &self.session_id,
            &mut self.index,
            &mut self.turns,
            u64::MAX,
        )
        .is_err()
        {
            self.index.failed = true;
            self.turns.clear();
            return false;
        }

        self.index.scanned = fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        self.status = Self::infer_status(&self.index, &self.turns);
        self.turns.len() != old_len
    }

    /// The turns in the window, folded exactly as the chat draws them.
    pub fn turns(&self) -> &[Value] {
        &self.turns
    }

    /// The turn the session is in: working while a prompt is open without completion,
    /// idle after a completed turn.
    pub fn status(&self) -> Option<AgentStatus> {
        self.status
    }

    /// The wire answer (`ConversationResponse`).
    pub fn response(&self) -> Value {
        json!({
            "source": "grok-transcript",
            "turns": self.turns,
            "history_id": self.history_id,
            "metadata": {
                "model": null,
                "reasoning_effort": null,
            },
        })
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn infer_status(index: &GrokIndex, turns: &[Value]) -> Option<AgentStatus> {
        if turns.is_empty() {
            return None;
        }
        match index.prompts.last() {
            Some(p) if !p.answered => Some(AgentStatus::Working),
            _ => Some(AgentStatus::Idle),
        }
    }

    /// Scan the file from position 0 up to `limit` bytes, applying each complete line.
    #[allow(clippy::too_many_arguments)]
    fn scan_file(
        path: &Path,
        session: &str,
        index: &mut GrokIndex,
        turns: &mut Vec<Value>,
        limit: u64,
    ) -> Result<(), std::io::Error> {
        let mut file = fs::File::open(path)?;
        file.seek(SeekFrom::Start(index.scanned))?;

        let mut file_pos = index.scanned;
        let mut carry: Vec<u8> = Vec::new();
        let deadline = file_pos + limit.min(SCAN_BYTES);

        let mut buf = [0u8; 64 * 1024];
        while file_pos < deadline {
            let remaining = (deadline - file_pos) as usize;
            let chunk_size = remaining.min(buf.len());
            let read = file.read(&mut buf[..chunk_size])?;
            if read == 0 {
                break;
            }
            let bytes = &buf[..read];
            let mut buf_offset = 0usize;

            while let Some(newline) = bytes[buf_offset..].iter().position(|&b| b == b'\n') {
                let line_end = file_pos + buf_offset as u64 + newline as u64 + 1;
                if line_end > deadline {
                    break;
                }

                let line_str: String = if carry.is_empty() {
                    String::from_utf8_lossy(&bytes[buf_offset..=buf_offset + newline]).into_owned()
                } else {
                    let mut combined = carry.clone();
                    combined.extend_from_slice(&bytes[buf_offset..=newline]);
                    combined.extend_from_slice(b"\n");
                    String::from_utf8_lossy(&combined).into_owned()
                };

                if line_str.len() as u64 > MAX_LINE {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "line exceeds MAX_LINE",
                    ));
                }

                if let Some(event) = parse_grok_event(&line_str, session) {
                    apply_event(index, turns, &event, file_pos + buf_offset as u64, line_end)?;
                }

                index.scanned = line_end;
                buf_offset = buf_offset + newline + 1;
                carry.clear();
            }

            // Advance file_pos by the bytes we've consumed from this buffer chunk.
            // buf_offset points to the first unprocessed byte (either after the last
            // newline, or at read if all bytes were consumed).
            file_pos += buf_offset as u64;

            if buf_offset < bytes.len() {
                carry.extend_from_slice(&bytes[buf_offset..]);
                if carry.len() as u64 > MAX_LINE {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "line exceeds MAX_LINE",
                    ));
                }
            }

            if file_pos >= deadline {
                break;
            }
        }

        if index.prompts.len() + index.calls.len() > MAX_ENTRIES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "too many entries",
            ));
        }

        Ok(())
    }
}

/// Parse a JSONL line into a GrokEvent. Returns None for blank or non-matching lines.
fn parse_grok_event(line: &str, expected_session: &str) -> Option<GrokEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let value: Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(_) => return None,
    };

    let method = value.get("method")?.as_str()?;
    if method != "session/update" && method != "_x.ai/session/update" {
        return None;
    }

    let params = value.get("params")?;
    let session_id = params.get("sessionId")?.as_str()?;
    if !expected_session.is_empty() && session_id != expected_session {
        return None;
    }

    let update = params.get("update")?;
    let _session_update = update.get("sessionUpdate")?.as_str()?;

    let meta = update
        .get("_meta")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let ms = meta.get("agentTimestampMs").and_then(Value::as_f64);
    let ts_value = value.get("timestamp").and_then(Value::as_f64);
    let time = ms.unwrap_or_else(|| ts_value.unwrap_or(0.0) * 1000.0);
    let ts = if time.is_finite() && time.abs() <= 8.64e15 {
        chrono::DateTime::from_timestamp_millis(time as i64)
            .map(|t| t.to_rfc3339_opts(SecondsFormat::Millis, true))
    } else {
        None
    };

    Some(GrokEvent {
        update: update.clone(),
        session: session_id.to_string(),
        ts,
        start: 0,
        end: 0,
    })
}

/// Apply one parsed event to the index and turns.
fn apply_event(
    index: &mut GrokIndex,
    turns: &mut Vec<Value>,
    event: &GrokEvent,
    start: u64,
    end: u64,
) -> Result<(), std::io::Error> {
    let u = &event.update;
    let session_update = u.get("sessionUpdate").and_then(Value::as_str).unwrap_or("");

    match session_update {
        "rewind_marker" => {
            let target = u
                .get("target_prompt_index")
                .and_then(Value::as_u64)
                .unwrap_or(u64::MAX);
            if !index.prompts.iter().any(|p| p.index == target) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "rewind target not found",
                ));
            }
            while let Some(last) = index.prompts.last() {
                if last.index >= target {
                    let removed = index.prompts.pop().unwrap();
                    for id in &removed.calls {
                        index.calls.remove(id);
                    }
                    // Remove the user turn and, if present, the assistant turn.
                    let turns_to_remove = if turns
                        .last()
                        .map(|t| t["role"] == "assistant")
                        .unwrap_or(false)
                    {
                        2
                    } else {
                        1
                    };
                    if turns.len() >= turns_to_remove {
                        turns.truncate(turns.len() - turns_to_remove);
                    }
                } else {
                    break;
                }
            }
            index.revision = end;
        }
        "user_message_chunk" => {
            let meta = u
                .get("_meta")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let number = meta
                .get("promptIndex")
                .and_then(Value::as_u64)
                .unwrap_or(u64::MAX);
            let hide = meta
                .get("hideFromScrollback")
                .and_then(Value::as_bool)
                .unwrap_or(false);

            if let Some(current) = index.prompts.last() {
                if current.index == number {
                    if current.answered {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "reused prompt index after answer",
                        ));
                    }
                } else if number <= current.index {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "prompt index went backwards",
                    ));
                } else {
                    index.prompts.push(Prompt {
                        index: number,
                        start,
                        end,
                        answered: false,
                        activity: "working".to_string(),
                        calls: HashSet::new(),
                    });
                    if !hide {
                        let content = u
                            .get("content")
                            .and_then(|c| c.get("text"))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        turns.push(json!({
                            "role": "user",
                            "ts": event.ts,
                            "parts": [{"kind": "text", "text": content}],
                        }));
                    }
                }
            } else {
                index.prompts.push(Prompt {
                    index: number,
                    start,
                    end,
                    answered: false,
                    activity: "working".to_string(),
                    calls: HashSet::new(),
                });
                if !hide {
                    let content = u
                        .get("content")
                        .and_then(|c| c.get("text"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    turns.push(json!({
                        "role": "user",
                        "ts": event.ts,
                        "parts": [{"kind": "text", "text": content}],
                    }));
                }
            }
        }
        "tool_call" => {
            let id = u.get("toolCallId").and_then(Value::as_str).unwrap_or("");
            if id.is_empty() || id.len() > 1024 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "invalid tool call id",
                ));
            }
            if index.calls.contains_key(id) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "duplicate tool call id",
                ));
            }
            let prompt_index = index.prompts.last().map(|p| p.index).unwrap_or(0);
            if let Some(p) = index.prompts.iter_mut().find(|p| p.index == prompt_index) {
                p.calls.insert(id.to_string());
            }
            let digest = hash(&serde_json::to_string(u).unwrap_or_default());
            index.calls.insert(
                id.to_string(),
                Call {
                    owner: prompt_index,
                    revision: end,
                    digest,
                },
            );

            // Add tool part to the current assistant turn
            let title = u.get("title").and_then(Value::as_str).unwrap_or("");
            let raw_input = u.get("rawInput").cloned().unwrap_or(Value::Null);
            let input = if raw_input.is_string() {
                raw_input.as_str().unwrap_or("").to_string()
            } else {
                serde_json::to_string(&raw_input).unwrap_or_default()
            };
            let name = u
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(id)
                .to_string();
            let summary = if !title.is_empty() {
                title.to_string()
            } else {
                tool_summary(&name, &raw_input)
            };

            let tool_part = json!({
                "kind": "tool",
                "name": name,
                "summary": summary,
                "input": input,
                "output": "",
            });

            if let Some(last_turn) = turns.last_mut() {
                if last_turn["role"] == "assistant" {
                    let parts_opt = last_turn.get_mut("parts").and_then(|p| p.as_array_mut());
                    if let Some(parts) = parts_opt {
                        parts.push(tool_part);
                    }
                } else {
                    // Create a new assistant turn with just this tool
                    turns.push(json!({
                        "role": "assistant",
                        "ts": event.ts,
                        "parts": [tool_part],
                    }));
                }
            } else {
                turns.push(json!({
                    "role": "assistant",
                    "ts": event.ts,
                    "parts": [tool_part],
                }));
            }
        }
        "tool_call_update" => {
            let id = u.get("toolCallId").and_then(Value::as_str).unwrap_or("");
            if let Some(call) = index.calls.get_mut(id) {
                call.revision = end;
                call.digest = hash(&format!(
                    "{}{}",
                    call.digest,
                    serde_json::to_string(u).unwrap_or_default()
                ));
            }

            // Update the tool part's output if this update carries content
            let status = u.get("status").and_then(Value::as_str).unwrap_or("");
            let content = u
                .get("content")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let output = extract_output(&content);

            if !output.is_empty() || status == "failed" {
                // Find the tool part for this call and update it
                let tool_part_index = find_tool_part_index(turns, id);
                if let Some(part_idx) = tool_part_index {
                    if let Some(turn) = turns.last_mut() {
                        if let Some(parts) = turn.get_mut("parts").and_then(|p| p.as_array_mut()) {
                            if let Some(part) = parts.get_mut(part_idx) {
                                if !output.is_empty() {
                                    part["output"] = json!(output);
                                }
                                if status == "failed" {
                                    part["error"] = json!(true);
                                }
                            }
                        }
                    }
                }
            }
        }
        "turn_completed" => {
            if let Some(p) = index.prompts.last_mut() {
                p.answered = true;
                p.end = end;
            }
            // Mark the last assistant turn's end_ts
            if let Some(last_turn) = turns.last_mut() {
                if last_turn["role"] == "assistant" {
                    last_turn["end_ts"] = json!(event.ts);
                }
            }
        }
        "agent_thought_chunk" => {
            if let Some(p) = index.prompts.last_mut() {
                p.activity = "thinking".to_string();
            }
            // Add thinking part to current assistant turn
            let content = u
                .get("content")
                .and_then(|c| c.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if !content.is_empty() {
                if let Some(last_turn) = turns.last_mut() {
                    if last_turn["role"] == "assistant" {
                        let parts = last_turn.get_mut("parts").and_then(|p| p.as_array_mut());
                        if let Some(parts) = parts {
                            parts.push(json!({ "kind": "thinking", "text": content }));
                        }
                    } else {
                        turns.push(json!({
                            "role": "assistant",
                            "ts": event.ts,
                            "parts": [json!({ "kind": "thinking", "text": content })],
                        }));
                    }
                }
            }
        }
        "agent_message_chunk" => {
            if let Some(p) = index.prompts.last_mut() {
                p.activity = "working".to_string();
                p.answered = true;
                p.end = end;
            }
            let content = u
                .get("content")
                .and_then(|c| c.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if !content.is_empty() {
                if let Some(last_turn) = turns.last_mut() {
                    if last_turn["role"] == "assistant" {
                        let parts_opt = last_turn.get_mut("parts").and_then(|p| p.as_array_mut());
                        if let Some(parts) = parts_opt {
                            // Append to last text part or create new
                            if let Some(last_part) = parts.last_mut() {
                                if last_part["kind"] == "text" {
                                    last_part["text"] = json!(format!(
                                        "{}{content}",
                                        last_part["text"].as_str().unwrap_or("")
                                    ));
                                } else {
                                    parts.push(json!({ "kind": "text", "text": content }));
                                }
                            } else {
                                parts.push(json!({ "kind": "text", "text": content }));
                            }
                        }
                    } else {
                        turns.push(json!({
                            "role": "assistant",
                            "ts": event.ts,
                            "parts": [json!({ "kind": "text", "text": content })],
                        }));
                    }
                }
            }
        }
        _ => {}
    }

    Ok(())
}

/// Extract text output from content blocks.
fn extract_output(content: &[Value]) -> String {
    let mut parts = Vec::new();
    for block in content {
        let empty_map = serde_json::Map::new();
        let block = block.as_object().unwrap_or(&empty_map);
        if block.get("type").and_then(Value::as_str) == Some("content") {
            let content_inner = block
                .get("content")
                .and_then(Value::as_object)
                .unwrap_or(&empty_map);
            if content_inner.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(text) = content_inner.get("text").and_then(Value::as_str) {
                    parts.push(text.to_string());
                }
            }
        }
    }
    parts.join("\n")
}

/// Find the index of a tool part with the given call id in the last assistant turn.
/// Since we don't store the call id in the part, we match by order: the Nth tool part
/// corresponds to the Nth tool call in the current prompt.
fn find_tool_part_index(turns: &[Value], _call_id: &str) -> Option<usize> {
    if let Some(last_turn) = turns.last() {
        if last_turn["role"] == "assistant" {
            let parts = last_turn.get("parts")?.as_array()?;
            for (i, part) in parts.iter().enumerate() {
                if part.get("kind").and_then(Value::as_str) == Some("tool") {
                    return Some(i);
                }
            }
        }
    }
    None
}

fn hash(value: &str) -> String {
    use std::fmt::Write;
    let digest = sha2::Sha256::digest(value.as_bytes());
    let mut out = String::with_capacity(43);
    for byte in &digest[..32] {
        write!(out, "{:02x}", byte).unwrap();
    }
    out.truncate(43);
    out
}

#[cfg(test)]
mod tests;
