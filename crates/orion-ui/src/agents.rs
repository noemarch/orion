//! Pure agent roster projection for the Orbit port.

use orion_core::protocol::AgentStatus;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRow {
    pub pane_id: String,
    pub title: String,
    pub context: String,
    pub status: AgentStatus,
    pub selected: bool,
}

fn parse_status(raw: Option<&str>) -> AgentStatus {
    match raw {
        Some("working") => AgentStatus::Working,
        Some("blocked") => AgentStatus::Blocked,
        Some("done") => AgentStatus::Done,
        Some("error") => AgentStatus::Error,
        Some("idle") => AgentStatus::Idle,
        _ => AgentStatus::Unknown,
    }
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
                .and_then(|rows| rows.iter().find(|w| str_field(w, "workspace_id") == workspace_id))
                .and_then(|w| str_field(w, "label"));
            let tab_label = tabs
                .and_then(|rows| rows.iter().find(|t| str_field(t, "tab_id") == tab_id))
                .and_then(|t| str_field(t, "label"));
            let context = [workspace_label, tab_label, str_field(pane, "cwd")]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
            Some(AgentRow {
                selected: focused_pane_id == Some(pane_id.as_str()),
                status: parse_status(pane.get("agent_status").and_then(Value::as_str)),
                pane_id,
                title,
                context,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_agent_fields_are_unavailable_not_defaulted() {
        let snapshot = serde_json::json!({ "panes": [{ "pane_id": "p1" }], "tabs": [], "workspaces": [] });
        let rows = agent_rows(&snapshot);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "p1");
        assert_eq!(rows[0].status, AgentStatus::Unknown);
    }
}
