//! Reading pi's own transcript.
//!
//! pi's session file is an append-only entry tree, not a linear log: every entry links to its
//! predecessor by `id`/`parentId`, and `/tree` moves the leaf back to an earlier entry without
//! rewriting the file. The conversation pi shows — and the chat must show — is the path from the
//! last entry written back to its root; entries on a side branch stay in the file, unread. What
//! the branch leaves behind is reported beside the turns so the chat can say it is hidden.
//!
//! The message records themselves keep the senpi shape `omo` and `omp` write, so the folding
//! rules live in [`super::omo`]; this module only indexes the tree and picks the live branch.

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::{json, Value};

use crate::protocol::AgentStatus;

use super::omo::SenpiTurns;

/// Bound the index's work and retention; a page bounds the turns already.
const MAX_ENTRIES: usize = 100_000;
/// One record holding a picture is large; beyond this the file is not read as a tree.
const MAX_ENTRY_BYTES: usize = 16 * 1024 * 1024;
/// The bytes before the scanned offset an append cannot disturb, used to spot a rewrite.
const TAIL_BYTES: u64 = 64;

/// One line of the session tree. The header line carries an id too: a message's `parentId` may
/// name it, and abandoning a branch rooted at it is a navigation like any other.
struct PiEntry {
    id: String,
    parent: Option<String>,
    start: u64,
    end: u64,
    kind: String,
    role: String,
    summary: Option<String>,
}

/// The tree as it stands. Parents precede their children, so one forward pass indexes it; an
/// append extends the scan, and a rewrite or truncation starts it over.
struct PiIndex {
    entries: Vec<PiEntry>,
    children: HashMap<String, Vec<usize>>,
    scanned: u64,
    tail: Vec<u8>,
}

/// The entries a branch can read, and the ids on it.
struct LiveBranch {
    segments: Vec<(u64, u64)>,
    live: HashSet<String>,
}

impl PiIndex {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            children: HashMap::new(),
            scanned: 0,
            tail: Vec::new(),
        }
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    /// Reads the lines appended since the last call. A line still without its newline waits for
    /// it, so `scanned` always names a line boundary and the bytes before it never change.
    fn refresh(&mut self, path: &Path) {
        let Ok(size) = fs::metadata(path).map(|meta| meta.len()) else {
            return;
        };
        if size < self.scanned || !self.tail_matches(path) {
            self.reset();
        }
        if size <= self.scanned {
            return;
        }
        let Ok(mut file) = File::open(path) else {
            return;
        };
        if file.seek(SeekFrom::Start(self.scanned)).is_err() {
            return;
        }
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        let mut position = self.scanned;
        loop {
            line.clear();
            let Ok(read) = reader.read_until(b'\n', &mut line) else {
                break;
            };
            if read == 0 || line.last() != Some(&b'\n') {
                break;
            }
            if line.len() > MAX_ENTRY_BYTES || self.entries.len() >= MAX_ENTRIES {
                // fail closed rather than invent a partial branch
                self.reset();
                return;
            }
            let start = position;
            position += read as u64;
            if let Some(entry) = index_entry(&line, start, position) {
                let index = self.entries.len();
                if let Some(parent) = &entry.parent {
                    self.children.entry(parent.clone()).or_default().push(index);
                }
                self.entries.push(entry);
            }
        }
        self.scanned = position;
        self.tail =
            read_range(path, position.saturating_sub(TAIL_BYTES), position).unwrap_or_default();
    }

    fn tail_matches(&self, path: &Path) -> bool {
        if self.scanned == 0 {
            return true;
        }
        let from = self.scanned.saturating_sub(TAIL_BYTES);
        read_range(path, from, self.scanned).is_some_and(|bytes| bytes == self.tail)
    }

    /// The last entry written, walked back to its root. Null when the tree holds no entry.
    fn live_branch(&self) -> Option<LiveBranch> {
        if self.entries.is_empty() {
            return None;
        }
        let mut by_id: HashMap<&str, usize> = HashMap::new();
        for (index, entry) in self.entries.iter().enumerate() {
            by_id.insert(entry.id.as_str(), index); // a repeated id keeps its last append
        }
        let mut live = HashSet::new();
        let mut chain = Vec::new();
        let mut next = Some(self.entries.len() - 1);
        while let Some(index) = next {
            let entry = &self.entries[index];
            if !live.insert(entry.id.clone()) {
                break;
            }
            chain.push(index);
            // a parent that never appears ends the walk short: what resolves is shown, nothing
            // above the unknown id is invented
            next = entry
                .parent
                .as_deref()
                .and_then(|parent| by_id.get(parent).copied());
        }
        let mut segments: Vec<(u64, u64)> = Vec::new();
        for index in chain.iter().rev() {
            let entry = &self.entries[*index];
            if let Some(last) = segments.last_mut() {
                if last.1 == entry.start {
                    last.1 = entry.end;
                    continue;
                }
            }
            segments.push((entry.start, entry.end));
        }
        Some(LiveBranch { segments, live })
    }

    /// What the live branch hides: the turns on paths a `/tree` walked away from, the number of
    /// separate paths, and pi's own summary of them when the user asked for one.
    fn abandoned(&self, live: &LiveBranch) -> Option<Value> {
        if self.entries.is_empty() {
            return None;
        }
        // a branch is a place the pointer was moved away from: a live entry with a child left
        // behind. A parentless entry off the live path counts only with something under it, so
        // an append-only journal of unrelated lines stays at zero branches
        let mut heads: HashSet<&str> = HashSet::new();
        for id in &live.live {
            let Some(children) = self.children.get(id) else {
                continue;
            };
            for child in children {
                let child = &self.entries[*child];
                if !live.live.contains(&child.id) {
                    heads.insert(child.id.as_str());
                }
            }
        }
        for entry in &self.entries {
            if entry.parent.is_some() || entry.kind == "session" || live.live.contains(&entry.id) {
                continue;
            }
            let left = self.children.get(&entry.id).is_some_and(|children| {
                children
                    .iter()
                    .any(|child| !live.live.contains(&self.entries[*child].id))
            });
            if left {
                heads.insert(entry.id.as_str());
            }
        }
        let branches = heads.len();
        let mut count = 0;
        if branches > 0 {
            // children link to parents, so a subtree's entries were appended after its head: one
            // forward pass marks what each branch owns, and only owned turns are counted
            let mut owned: HashSet<&str> = HashSet::new();
            let mut counted: HashSet<&str> = HashSet::new();
            for entry in &self.entries {
                let mine = heads.contains(entry.id.as_str())
                    || entry
                        .parent
                        .as_deref()
                        .is_some_and(|parent| owned.contains(parent));
                if !mine {
                    continue;
                }
                owned.insert(entry.id.as_str());
                if entry.kind != "message" || !matches!(entry.role.as_str(), "user" | "assistant") {
                    continue;
                }
                if counted.insert(entry.id.as_str()) {
                    count += 1;
                }
            }
        }
        // pi's summary of an abandoned path sits on the branch that replaced it, so only one on
        // the live path describes what the chat is hiding; the newest wins
        let summary = self
            .entries
            .iter()
            .filter(|entry| live.live.contains(&entry.id) && entry.kind == "branch_summary")
            .filter_map(|entry| entry.summary.as_deref())
            .rfind(|summary| !summary.trim().is_empty())
            .map(str::to_string);
        Some(json!({ "count": count, "branches": branches, "summary": summary }))
    }
}

/// One entry's identity and the few fields a reader needs without parsing the line again.
fn index_entry(line: &[u8], start: u64, end: u64) -> Option<PiEntry> {
    let text = std::str::from_utf8(line).ok()?;
    if !text.contains("\"id\"") {
        return None; // a torn fragment or a line that is not a node
    }
    let entry: Value = serde_json::from_str(text).ok()?;
    let id = entry.get("id").and_then(Value::as_str)?;
    if id.is_empty() || id.len() > 256 {
        return None;
    }
    let parent = entry
        .get("parentId")
        .and_then(Value::as_str)
        .filter(|parent| !parent.is_empty() && parent.len() <= 256)
        .map(str::to_string);
    let kind = entry
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if kind.len() > 64 {
        return None;
    }
    let role = entry
        .get("message")
        .and_then(|message| message.get("role"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if role.len() > 64 {
        return None;
    }
    let summary = (kind == "branch_summary")
        .then(|| entry.get("summary").and_then(Value::as_str))
        .flatten()
        .map(str::to_string);
    Some(PiEntry {
        id: id.to_string(),
        parent,
        start,
        end,
        kind: kind.to_string(),
        role: role.to_string(),
        summary,
    })
}

/// One session file's entry tree and the turns folded from its live branch.
pub struct PiTranscript {
    path: PathBuf,
    session_id: String,
    index: PiIndex,
    turns: SenpiTurns,
    /// The branch the turns were folded from, so an append reads on without a restart.
    branch: Vec<(u64, u64)>,
    parsed_through: u64,
}

impl PiTranscript {
    /// Opens the newest session of `cwd` in the store pi writes.
    pub fn open(cwd: &Path, _since: Option<SystemTime>) -> Option<Self> {
        Self::open_in(cwd, &store_dir_from_env()?)
    }

    /// Opens a store rooted at `store` (the `sessions` directory), which is how tests replay one.
    pub fn open_in(cwd: &Path, store: &Path) -> Option<Self> {
        let cwd_text = cwd.to_string_lossy().to_string();
        let dir = store.join(session_folder(&cwd_text));
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
            session_id,
            index: PiIndex::new(),
            turns: SenpiTurns::new(),
            branch: Vec::new(),
            parsed_through: 0,
        };
        transcript.refresh();
        Some(transcript)
    }

    /// Folds the live branch's appended lines. Returns whether the chat changed.
    pub fn refresh(&mut self) -> bool {
        self.index.refresh(&self.path);
        let Some(live) = self.index.live_branch() else {
            let had = !self.turns.turns().is_empty() || !self.branch.is_empty();
            self.turns.reset();
            self.branch.clear();
            self.parsed_through = 0;
            return had;
        };
        let mut changed = false;
        if live.segments != self.branch {
            if extends_branch(&self.branch, &live.segments) {
                self.branch = live.segments.clone();
            } else {
                // /tree moved the leaf, or the file was rewritten: the branch is another path
                self.turns.reset();
                self.branch = live.segments.clone();
                self.parsed_through = 0;
                changed = true;
            }
        }
        let end = self.branch.last().map(|(_, end)| *end).unwrap_or(0);
        if end > self.parsed_through {
            let from = self.parsed_through;
            for (start, segment_end) in &self.branch {
                if *segment_end <= from {
                    continue;
                }
                for line in read_lines(&self.path, (*start).max(from), *segment_end) {
                    if serde_json::from_str::<Value>(&line)
                        .is_ok_and(|entry| self.turns.ingest(&entry))
                    {
                        changed = true;
                    }
                }
            }
            self.parsed_through = end;
            if changed {
                self.turns.settle();
            }
        }
        changed
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn turns(&self) -> &[Value] {
        self.turns.turns()
    }

    pub fn status(&self) -> Option<AgentStatus> {
        self.turns.status()
    }

    /// The wire answer (`ConversationResponse`). `abandoned` is always stated for pi: zero is
    /// every session no `/tree` touched, which the chat may draw as nothing, while unknown is a
    /// tree this reader cannot vouch for.
    pub fn response(&self) -> Value {
        let abandoned = self
            .index
            .live_branch()
            .and_then(|live| self.index.abandoned(&live))
            .unwrap_or(Value::Null);
        json!({
            "source": "pi-transcript",
            "turns": self.turns.turns(),
            "history_id": format!("pi-{}", self.session_id),
            "abandoned": abandoned,
        })
    }
}

/// Whether `next` is `current` with its last segment grown by an append: the parsing can read on
/// from where it stopped instead of folding the whole branch again.
fn extends_branch(current: &[(u64, u64)], next: &[(u64, u64)]) -> bool {
    if current.is_empty() || current.len() != next.len() {
        return false;
    }
    let last = current.len() - 1;
    current
        .iter()
        .zip(next)
        .enumerate()
        .all(|(index, (old, new))| {
            old.0 == new.0 && (old.1 == new.1 || (index == last && new.1 > old.1))
        })
}

/// `<session store>/--<cwd without its leading slash, separators as dashes>--`
pub fn session_folder(cwd: &str) -> String {
    super::omo::session_folder(cwd)
}

/// The session store pi writes: `PI_CODING_AGENT_SESSION_DIR`, else `<agent dir>/sessions`, where
/// the agent dir is `PI_CODING_AGENT_DIR` or `~/.pi/agent`. pi's `--session-dir` flag moves it too,
/// but Orion cannot see its own agent's argv, so a pane started that way keeps the terminal.
pub(crate) fn store_dir(
    agent_dir: Option<String>,
    session_dir: Option<String>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(dir) = session_dir.filter(|dir| !dir.is_empty()) {
        return Some(expand_home(&dir, home.as_deref()));
    }
    let dir = match agent_dir.filter(|dir| !dir.is_empty()) {
        Some(dir) => expand_home(&dir, home.as_deref()),
        None => home?.join(".pi").join("agent"),
    };
    Some(dir.join("sessions"))
}

fn store_dir_from_env() -> Option<PathBuf> {
    let value = |name: &str| env::var(name).ok().filter(|value| !value.is_empty());
    store_dir(
        value("PI_CODING_AGENT_DIR"),
        value("PI_CODING_AGENT_SESSION_DIR"),
        home(),
    )
}

fn home() -> Option<PathBuf> {
    env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(PathBuf::from))
}

/// pi expands a leading `~` against the home its process was started with.
fn expand_home(value: &str, home: Option<&Path>) -> PathBuf {
    if value == "~" {
        if let Some(home) = home {
            return home.to_path_buf();
        }
    }
    if let Some(rest) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        if let Some(home) = home {
            return home.join(rest);
        }
    }
    PathBuf::from(value)
}

fn first_line(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    if line.trim().is_empty() {
        return None;
    }
    Some(line)
}

fn read_range(path: &Path, from: u64, to: u64) -> Option<Vec<u8>> {
    if to <= from {
        return Some(Vec::new());
    }
    let mut file = File::open(path).ok()?;
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut bytes = vec![0; (to - from) as usize];
    file.read_exact(&mut bytes).ok()?;
    Some(bytes)
}

/// The complete lines in one byte range, without their newlines.
fn read_lines(path: &Path, from: u64, to: u64) -> Vec<String> {
    let Ok(mut file) = File::open(path) else {
        return Vec::new();
    };
    if file.seek(SeekFrom::Start(from)).is_err() {
        return Vec::new();
    }
    let mut reader = BufReader::new(file.take(to - from));
    let mut lines = Vec::new();
    let mut line = String::new();
    loop {
        line.clear();
        let Ok(read) = reader.read_line(&mut line) else {
            break;
        };
        if read == 0 {
            break;
        }
        let line = line.strip_suffix('\n').unwrap_or(&line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        if !line.trim().is_empty() {
            lines.push(line.to_string());
        }
    }
    lines
}

#[cfg(test)]
mod tests;
