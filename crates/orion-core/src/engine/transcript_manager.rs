//! Transcript manager: one reader per pane, behind a common seam.
//!
//! The engine keeps per-pane transcript readers alive and asks the right one by agent kind.
//! Each agent's reader (`ClaudeTranscript`, `CodexTranscript`, …) translates that agent's store
//! into the wire's `ConversationResponse`. The manager unifies them behind [`TranscriptReader`]
//! so callers never switch on eight maps: `conversation()`, `session_id()`, and
//! `codex_unanswered()` all go through one lock and one map.

use std::collections::HashMap;
use std::path::Path;
use std::time::SystemTime;

use parking_lot::Mutex;
use serde_json::Value;
use tracing::info;

use crate::engine::introspect::claude::ClaudeTranscript;
use crate::engine::introspect::codex::{CodexTranscript, QueuedQuestion};
use crate::engine::introspect::cursor::CursorTranscript;
use crate::engine::introspect::grok::GrokTranscript;
use crate::engine::introspect::hermes::HermesTranscript;
use crate::engine::introspect::omo::OmoTranscript;
use crate::engine::introspect::opencode::OpencodeTranscript;
use crate::engine::introspect::pi::PiTranscript;
use crate::protocol::{AgentKind, AgentStatus};

/// The common surface every per-agent transcript reader presents.
pub trait TranscriptReader: Send {
    /// Ingests everything appended since the last call.
    fn refresh(&mut self) -> bool;
    /// The wire answer (`ConversationResponse`).
    fn response(&self) -> Value;
    /// The agent's session identifier for this store.
    fn session_id(&self) -> &str;
    /// The turn the session is in, when the agent's store reports one.
    fn status(&self) -> Option<AgentStatus> {
        None
    }
    /// Unanswered structured questions (Codex only; others have none).
    fn unanswered_questions(&self) -> Vec<QueuedQuestion> {
        Vec::new()
    }
}

// Adapters: one per agent kind. Each wrapper is a real seam — the reader is
// constructed exactly as it was when each agent had its own map.
struct Claude(ClaudeTranscript);
impl TranscriptReader for Claude {
    fn refresh(&mut self) -> bool {
        self.0.refresh()
    }
    fn response(&self) -> Value {
        self.0.response()
    }
    fn session_id(&self) -> &str {
        self.0.session_id()
    }
}

struct Codex(CodexTranscript);
impl TranscriptReader for Codex {
    fn refresh(&mut self) -> bool {
        self.0.refresh()
    }
    fn response(&self) -> Value {
        self.0.response()
    }
    fn session_id(&self) -> &str {
        self.0.session_id()
    }
    fn unanswered_questions(&self) -> Vec<QueuedQuestion> {
        self.0.unanswered_questions()
    }
}

struct Opencode(OpencodeTranscript);
impl TranscriptReader for Opencode {
    fn refresh(&mut self) -> bool {
        self.0.refresh()
    }
    fn response(&self) -> Value {
        self.0.response()
    }
    fn session_id(&self) -> &str {
        self.0.session_id()
    }
}

struct Omo(OmoTranscript);
impl TranscriptReader for Omo {
    fn refresh(&mut self) -> bool {
        self.0.refresh()
    }
    fn response(&self) -> Value {
        self.0.response()
    }
    fn session_id(&self) -> &str {
        self.0.session_id()
    }
    fn status(&self) -> Option<AgentStatus> {
        self.0.status()
    }
}

struct Pi(PiTranscript);
impl TranscriptReader for Pi {
    fn refresh(&mut self) -> bool {
        self.0.refresh()
    }
    fn response(&self) -> Value {
        self.0.response()
    }
    fn session_id(&self) -> &str {
        self.0.session_id()
    }
    fn status(&self) -> Option<AgentStatus> {
        self.0.status()
    }
}

struct Hermes(HermesTranscript);
impl TranscriptReader for Hermes {
    fn refresh(&mut self) -> bool {
        self.0.refresh()
    }
    fn response(&self) -> Value {
        self.0.response()
    }
    fn session_id(&self) -> &str {
        self.0.session_id()
    }
    fn status(&self) -> Option<AgentStatus> {
        self.0.status()
    }
}

struct Grok(GrokTranscript);
impl TranscriptReader for Grok {
    fn refresh(&mut self) -> bool {
        self.0.refresh()
    }
    fn response(&self) -> Value {
        self.0.response()
    }
    fn session_id(&self) -> &str {
        self.0.session_id()
    }
    fn status(&self) -> Option<AgentStatus> {
        self.0.status()
    }
}

struct Cursor(CursorTranscript);
impl TranscriptReader for Cursor {
    fn refresh(&mut self) -> bool {
        self.0.refresh()
    }
    fn response(&self) -> Value {
        self.0.response()
    }
    fn session_id(&self) -> &str {
        self.0.session_id()
    }
    fn status(&self) -> Option<AgentStatus> {
        self.0.status()
    }
}

struct Null;
impl TranscriptReader for Null {
    fn refresh(&mut self) -> bool {
        false
    }
    fn response(&self) -> Value {
        serde_json::json!({})
    }
    fn session_id(&self) -> &str {
        ""
    }
}

/// Owns one reader per pane behind a single lock.
pub struct TranscriptManager {
    readers: Mutex<HashMap<String, Box<dyn TranscriptReader>>>,
}

impl TranscriptManager {
    pub fn new() -> Self {
        Self {
            readers: Mutex::new(HashMap::new()),
        }
    }

    /// Forgets a pane's reader. Every teardown path calls this once.
    pub fn remove_pane(&self, pane_id: &str) {
        self.readers.lock().remove(pane_id);
    }

    /// Opens the reader for a pane if it is not already open.
    fn ensure_open(
        &self,
        pane_id: &str,
        kind: AgentKind,
        cwd: &Path,
        started: SystemTime,
        visible_text: &str,
    ) -> bool {
        let mut readers = self.readers.lock();
        if readers.contains_key(pane_id) {
            return true;
        }
        let reader: Box<dyn TranscriptReader> = match kind {
            AgentKind::Claude => match ClaudeTranscript::open(cwd, None, Some(started)) {
                Some(t) => {
                    info!("claude transcript for {pane_id}: {}", t.path().display());
                    Box::new(Claude(t))
                }
                None => return false,
            },
            AgentKind::Codex => {
                match CodexTranscript::open_for_screen(cwd, visible_text, Some(started)) {
                    Some(t) => {
                        info!("codex transcript for {pane_id}: {}", t.path().display());
                        Box::new(Codex(t))
                    }
                    None => return false,
                }
            }
            AgentKind::OpenCode => match OpencodeTranscript::open(cwd, Some(started)) {
                Some(t) => {
                    info!("opencode transcript for {pane_id}: {}", t.path().display());
                    Box::new(Opencode(t))
                }
                None => return false,
            },
            AgentKind::Omo | AgentKind::Omp => {
                match OmoTranscript::open(kind, cwd, Some(started)) {
                    Some(t) => {
                        info!("omo transcript for {pane_id}: {}", t.path().display());
                        Box::new(Omo(t))
                    }
                    None => return false,
                }
            }
            AgentKind::Pi => match PiTranscript::open(cwd, Some(started)) {
                Some(t) => {
                    info!("pi transcript for {pane_id}: {}", t.path().display());
                    Box::new(Pi(t))
                }
                None => return false,
            },
            AgentKind::Hermes => match HermesTranscript::open(cwd, Some(started)) {
                Some(t) => {
                    info!("hermes transcript for {pane_id}: {}", t.path().display());
                    Box::new(Hermes(t))
                }
                None => return false,
            },
            AgentKind::Grok => match GrokTranscript::open(cwd, Some(started)) {
                Some(t) => {
                    info!("grok transcript for {pane_id}: {}", t.path().display());
                    Box::new(Grok(t))
                }
                None => return false,
            },
            AgentKind::Cursor => match CursorTranscript::open(cwd, Some(started)) {
                Some(t) => {
                    info!("cursor transcript for {pane_id}: {}", t.path().display());
                    Box::new(Cursor(t))
                }
                None => return false,
            },
            _ => Box::new(Null),
        };
        readers.insert(pane_id.to_string(), reader);
        true
    }

    /// Refreshes a pane's transcript and returns its wire answer and status.
    /// Returns `None` when no reader could be opened for the pane.
    pub fn conversation(
        &self,
        pane_id: &str,
        agent: AgentKind,
        cwd: &Path,
        started: SystemTime,
        visible_text: &str,
    ) -> Option<(Value, Option<AgentStatus>)> {
        if !self.ensure_open(pane_id, agent, cwd, started, visible_text) {
            return None;
        }
        let mut readers = self.readers.lock();
        let reader = readers.get_mut(pane_id)?;
        reader.refresh();
        let response = reader.response();
        let status = reader.status();
        Some((response, status))
    }

    /// The session id a pane's transcript is reading, or `None` when none is open.
    pub fn session_id(
        &self,
        pane_id: &str,
        agent: AgentKind,
        cwd: &Path,
        started: SystemTime,
        visible_text: &str,
    ) -> Option<String> {
        if agent != AgentKind::Omo && agent != AgentKind::Omp {
            return None;
        }
        if !self.ensure_open(pane_id, agent, cwd, started, visible_text) {
            return None;
        }
        let readers = self.readers.lock();
        readers.get(pane_id).map(|r| r.session_id().to_string())
    }

    /// Unanswered Codex questions for a pane (empty when the pane has no Codex reader).
    #[allow(dead_code)]
    pub fn codex_unanswered(
        &self,
        pane_id: &str,
        cwd: &Path,
        started: SystemTime,
        visible_text: &str,
    ) -> Vec<QueuedQuestion> {
        if !self.ensure_open(pane_id, AgentKind::Codex, cwd, started, visible_text) {
            return Vec::new();
        }
        let mut readers = self.readers.lock();
        let Some(reader) = readers.get_mut(pane_id) else {
            return Vec::new();
        };
        reader.refresh();
        reader.unanswered_questions()
    }

    /// How many readers are currently held (for tests).
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.readers.lock().len()
    }

    /// Whether a reader is held for a pane (for tests).
    #[allow(dead_code)]
    pub fn contains(&self, pane_id: &str) -> bool {
        self.readers.lock().contains_key(pane_id)
    }
}

impl Default for TranscriptManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_pane_forgets_a_reader() {
        let manager = TranscriptManager::new();
        assert_eq!(manager.len(), 0);
        manager.remove_pane("missing");
        assert_eq!(manager.len(), 0);
    }

    #[test]
    fn conversation_without_a_store_returns_none() {
        let manager = TranscriptManager::new();
        let dir = std::env::temp_dir().join("orion-transcript-none");
        let started = SystemTime::now();
        assert!(manager
            .conversation("pane-missing", AgentKind::Claude, &dir, started, "")
            .is_none());
        assert!(manager
            .codex_unanswered("pane-missing", &dir, started, "")
            .is_empty());
    }

    #[test]
    fn null_reader_reports_no_session() {
        let mut reader: Box<dyn TranscriptReader> = Box::new(Null);
        assert!(!reader.refresh());
        assert_eq!(reader.session_id(), "");
        assert!(reader.status().is_none());
        assert!(reader.unanswered_questions().is_empty());
        let _ = reader.response();
    }
}
