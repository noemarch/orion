//! Pure agent roster projection for the Orbit port.

use orion_core::protocol::AgentStatus;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRow {
    pub pane_id: String,
    pub workspace_id: String,
    pub agent_name: String,
    pub title: String,
    pub context: String,
    pub status: AgentStatus,
    pub selected: bool,
    pub model_label: Option<String>,
}

impl AgentRow {
    pub fn status_word(&self) -> &'static str {
        crate::session_status::badge_word(crate::session_status::state_of(self.status))
    }
}

fn parse_status(raw: Option<&str>) -> AgentStatus {
    raw.and_then(AgentStatus::from_wire)
        .unwrap_or(AgentStatus::Unknown)
}

fn str_field<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value.get(field).and_then(Value::as_str)
}

pub fn agent_rows(snapshot: &Value) -> Vec<AgentRow> {
    let focused_pane_id = str_field(snapshot, "focused_pane_id");
    let workspaces = snapshot.get("workspaces").and_then(Value::as_array);
    let tabs = snapshot.get("tabs").and_then(Value::as_array);
    let Some(panes) = snapshot.get("panes").and_then(Value::as_array) else {
        return Vec::new();
    };
    panes
        .iter()
        .filter_map(|pane| {
            let pane_id = str_field(pane, "pane_id")?.to_string();
            let title = str_field(pane, "label")
                .or_else(|| str_field(pane, "title"))
                .unwrap_or(&pane_id)
                .to_string();
            let workspace_id = str_field(pane, "workspace_id");
            let tab_id = str_field(pane, "tab_id");
            let workspace_label = workspaces
                .and_then(|rows| {
                    rows.iter()
                        .find(|w| str_field(w, "workspace_id") == workspace_id)
                })
                .and_then(|w| str_field(w, "label"));
            let tab_label = tabs
                .and_then(|rows| rows.iter().find(|t| str_field(t, "tab_id") == tab_id))
                .and_then(|t| str_field(t, "label"));
            let status = parse_status(pane.get("agent_status").and_then(Value::as_str));
            let agent_name = str_field(pane, "display_agent")
                .or_else(|| str_field(pane, "agent"))
                .unwrap_or(&title)
                .to_string();
            let context = [workspace_label, tab_label, str_field(pane, "cwd")]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
            let model_label = str_field(pane, "model").map(str::to_string);
            Some(AgentRow {
                selected: focused_pane_id == Some(pane_id.as_str()),
                status,
                pane_id,
                workspace_id: workspace_id.unwrap_or_default().to_string(),
                agent_name,
                title,
                context,
                model_label,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn status_word_is_written_not_color_only() {
        let snapshot = serde_json::json!({
            "panes": [{ "pane_id": "p1", "agent_status": "working" }],
            "tabs": [],
            "workspaces": [],
        });
        let rows = agent_rows(&snapshot);
        assert_eq!(rows[0].status_word(), "Working");
        assert!(
            !rows[0].context.contains("Working"),
            "the status is written by its own label, not repeated in the context"
        );
    }

    #[::core::prelude::v1::test]
    fn missing_agent_fields_are_unavailable_not_defaulted() {
        let snapshot =
            serde_json::json!({ "panes": [{ "pane_id": "p1" }], "tabs": [], "workspaces": [] });
        let rows = agent_rows(&snapshot);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "p1");
        assert_eq!(rows[0].status, AgentStatus::Unknown);
    }

    #[::core::prelude::v1::test]
    fn model_label_is_none_when_no_model_is_reported() {
        let snapshot = serde_json::json!({
            "panes": [{ "pane_id": "p1", "display_agent": "claude", "agent": "claude" }],
            "tabs": [],
            "workspaces": [],
        });
        let rows = agent_rows(&snapshot);
        assert_eq!(rows[0].model_label, None);
        assert_eq!(rows[0].agent_name, "claude");
    }

    #[::core::prelude::v1::test]
    fn model_label_uses_only_the_explicit_model_field() {
        let snapshot = serde_json::json!({
            "panes": [{
                "pane_id": "p1",
                "display_agent": "claude",
                "model": "claude-sonnet-4-5",
            }],
            "tabs": [],
            "workspaces": [],
        });
        let rows = agent_rows(&snapshot);
        assert_eq!(rows[0].model_label.as_deref(), Some("claude-sonnet-4-5"));
    }
}
