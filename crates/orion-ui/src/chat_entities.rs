//! Typed wire structs for the chat lens (P2).
//!
//! The frozen wire contract is `shared/protocol.ts`: `ConversationResponse` with
//! `source`, `history_id?`, `turns: [{role, ts, end_ts?, parts}]`, and parts tagged
//! by `kind` (`text`/`thinking`/`tool`/`compact`/`notice`/`task_result`/`skill`/`image`).
//! These structs parse that wire shape with serde and are the only place the chat
//! view learns the JSON layout. Unknown future fields are ignored; unknown `kind`s
//! deserialize the raw `Value` into [`ChatPart::Unknown`] so a newer server never
//! blanks a turn.

use serde::Deserialize;
use serde_json::Value;

/// `GET /api/pane/conversation` wire shape (subset the chat lens draws).
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ConversationResponse {
    pub source: String,
    #[serde(default)]
    pub history_id: Option<String>,
    #[serde(default)]
    pub turns: Vec<ChatTurn>,
}

impl ConversationResponse {
    /// Parse the route's JSON body (the `Value` returned by [`orion_core::Engine::http`]).
    pub fn parse(value: &Value) -> Result<Self, String> {
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }

    /// Keep only the most recent `cap` turns, newest last. A scrollable column is
    /// used instead of GPUI virtualization (cap documented in `chat.rs`).
    pub fn recent(&self, cap: usize) -> Vec<&ChatTurn> {
        let start = self.turns.len().saturating_sub(cap);
        self.turns[start..].iter().collect()
    }
}

/// Keep the most recent `cap` items, preserving order. Used to bound rendered
/// conversation turns without virtualization.
pub fn cap_turns<T>(mut turns: Vec<T>, cap: usize) -> Vec<T> {
    if turns.len() > cap {
        turns.drain(..turns.len() - cap);
    }
    turns
}

/// One conversation turn: a user message or one assistant response step.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ChatTurn {
    pub role: ChatRole,
    #[serde(default)]
    pub ts: Option<String>,
    #[serde(default)]
    pub end_ts: Option<String>,
    #[serde(default)]
    pub parts: Vec<ChatPart>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    User,
    Assistant,
}

impl ChatRole {
    pub fn as_str(self) -> &'static str {
        match self {
            ChatRole::User => "user",
            ChatRole::Assistant => "assistant",
        }
    }
}

/// One part of a turn, tagged by the wire `kind`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "kind")]
pub enum ChatPart {
    #[serde(rename = "text")]
    Text {
        text: String,
        #[serde(default)]
        phase: Option<String>,
    },
    #[serde(rename = "thinking")]
    Thinking { text: String },
    #[serde(rename = "tool")]
    Tool(ToolPart),
    #[serde(rename = "compact")]
    Compact { text: String },
    #[serde(rename = "notice")]
    Notice {
        text: String,
        #[serde(default)]
        source: Option<String>,
    },
    #[serde(rename = "task_result")]
    TaskResult {
        #[serde(default)]
        tasks: Vec<OmoTaskResult>,
    },
    #[serde(rename = "skill")]
    Skill { skill: SkillActivity },
    #[serde(rename = "image")]
    Image {
        media_type: String,
        #[serde(rename = "ref")]
        image_ref: String,
    },
    #[serde(other)]
    Unknown,
}

/// A tool call inside an assistant turn.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ToolPart {
    pub name: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub input: String,
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub error: bool,
    #[serde(default)]
    pub arguments_omitted: bool,
    #[serde(default)]
    pub skill: Option<SkillActivity>,
    #[serde(default)]
    pub output_ref: Option<String>,
    #[serde(default)]
    pub output_size: Option<u64>,
    #[serde(default)]
    pub images: Vec<ToolImage>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ToolImage {
    pub media_type: String,
    #[serde(rename = "ref")]
    pub image_ref: String,
}

/// Evidence of skill activity (not a claim the skill's workflow completed).
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct SkillActivity {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub evidence: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub path: Option<String>,
}

/// One background task that ended, as reported back to the agent.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct OmoTaskResult {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub duration_ms: Option<i64>,
    #[serde(default)]
    pub turns: Option<i64>,
    #[serde(default)]
    pub tool_calls: Option<i64>,
    #[serde(default)]
    pub tokens: Option<i64>,
    #[serde(default)]
    pub result: String,
    #[serde(default)]
    pub result_cut: bool,
}

/// `InteractivePrompt` wire shape (`DetectedPrompt::to_wire()`): the card the chat
/// lens draws when the agent's menu is on screen.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ChatPrompt {
    pub id: String,
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub question: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub options: Vec<ChatPromptOption>,
    #[serde(default)]
    pub custom_option_index: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ChatPromptOption {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
}

impl ChatPrompt {
    /// Parse one prompt wire object (the `prompt` field of `/api/pane/prompt`).
    pub fn from_value(value: &Value) -> Option<Self> {
        serde_json::from_value(value.clone()).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_all_wire_part_kinds() {
        let value = json!({
            "source": "claude-transcript",
            "history_id": "h1",
            "turns": [
                {"role": "user", "ts": "2026-01-01T00:00:00Z", "parts": [
                    {"kind": "text", "text": "hi"},
                    {"kind": "image", "media_type": "image/png", "ref": "r1"},
                ]},
                {"role": "assistant", "ts": null, "end_ts": "2026-01-01T00:01:00Z", "parts": [
                    {"kind": "thinking", "text": "reasoning"},
                    {"kind": "tool", "name": "Bash", "summary": "run", "input": "ls", "output": "a", "error": true},
                    {"kind": "compact", "text": "summarized"},
                    {"kind": "notice", "text": "job done", "source": "async-result"},
                    {"kind": "task_result", "tasks": []},
                    {"kind": "skill", "skill": {"name": "demo", "evidence": "invocation", "status": "loaded"}},
                    {"kind": "future-kind", "text": "kept"},
                ]},
            ],
        });
        let response = ConversationResponse::parse(&value).expect("parse");
        assert_eq!(response.source, "claude-transcript");
        assert_eq!(response.history_id.as_deref(), Some("h1"));
        assert_eq!(response.turns.len(), 2);
        assert_eq!(response.turns[0].role, ChatRole::User);
        assert!(matches!(response.turns[0].parts[1], ChatPart::Image { .. }));
        assert_eq!(response.turns[1].parts.len(), 7);
        assert!(matches!(&response.turns[1].parts[1], ChatPart::Tool(tool) if tool.error));
        assert!(matches!(response.turns[1].parts[6], ChatPart::Unknown));
    }

    #[test]
    fn large_transcript_caps_rendered_turns() {
        let turns = (0..500)
            .map(|i| ChatTurn {
                role: ChatRole::User,
                ts: Some(i.to_string()),
                end_ts: None,
                parts: vec![],
            })
            .collect::<Vec<_>>();
        let rendered = cap_turns(turns, 200);
        assert_eq!(rendered.len(), 200);
        assert_eq!(rendered[0].ts.as_deref(), Some("300"));
    }

    #[test]
    fn recent_cap_keeps_newest() {
        let response = ConversationResponse {
            source: "scrollback".to_string(),
            history_id: None,
            turns: (0..5)
                .map(|i| ChatTurn {
                    role: ChatRole::User,
                    ts: Some(i.to_string()),
                    end_ts: None,
                    parts: vec![],
                })
                .collect(),
        };
        let recent = response.recent(3);
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].ts.as_deref(), Some("2"));
    }

    #[test]
    fn parses_prompt_wire() {
        let value = json!({
            "id": "p1", "agent": "claude", "kind": "question", "title": "Approve?",
            "question": "Run it?", "body": null,
            "options": [{"label": "Yes", "description": null}],
            "custom_option_index": null,
        });
        let prompt = ChatPrompt::from_value(&value).expect("prompt");
        assert_eq!(prompt.options[0].label, "Yes");
        assert!(ChatPrompt::from_value(&json!(null)).is_none());
    }
}
