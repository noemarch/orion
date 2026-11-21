//! The one UI projection of `SessionState`.
//!
//! The sidebar badge, the Working/Settled groups, the duplicate-row label and
//! the terminal footer all read through this module. No other UI module maps a
//! state to a word, a color, a group or a row label.

use crate::agents::AgentRow;
use crate::theme::Theme;
use gpui_kit::Rgba;
use orion_core::protocol::AgentStatus;
use orion_core::session_state::SessionState;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// Which side of the sidebar's status section a row belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusGroup {
    Working,
    Settled,
}

/// The single state for a wire status carried by a snapshot row.
pub fn state_of(status: AgentStatus) -> SessionState {
    SessionState::from_agent_status(status, None)
}

/// The single state for a raw wire status. An unreported status is `Unknown`,
/// which the core maps to `Starting` — never silently `Idle`.
pub fn state_from_wire(raw: Option<&str>) -> SessionState {
    state_of(
        raw.and_then(AgentStatus::from_wire)
            .unwrap_or(AgentStatus::Unknown),
    )
}

/// The one badge word, shared verbatim by the sidebar and the footer.
pub fn badge_word(state: SessionState) -> &'static str {
    state.status_word()
}

/// The one badge color. `NeedsInput` is amber attention, never the rose accent.
pub fn badge_color(state: SessionState, theme: &Theme, pulse: u32) -> Rgba {
    let base = match state {
        SessionState::Starting => theme.indicator_unknown,
        SessionState::Running => theme.status_working,
        SessionState::NeedsInput => theme.state_attention(),
        SessionState::Idle => theme.status_idle,
        SessionState::Exited { code: 0 } => theme.status_done,
        SessionState::Exited { .. } => theme.indicator_blocked,
    };
    match state {
        SessionState::Running | SessionState::NeedsInput if pulse > 0 => {
            let phase = (pulse % 16) as f32 / 16.0;
            let wave = if phase < 0.5 {
                phase * 2.0
            } else {
                2.0 - phase * 2.0
            };
            Rgba {
                r: base.r,
                g: base.g,
                b: base.b,
                a: 0.55 + wave * 0.45,
            }
        }
        _ => base,
    }
}

/// Group membership for the Working / Settled sections.
pub fn group(state: SessionState) -> StatusGroup {
    if state.is_working() {
        StatusGroup::Working
    } else {
        StatusGroup::Settled
    }
}

/// A duplicate row is labelled `agent name · pane title`; a unique row keeps
/// its pane title.
pub fn row_label(agent_name: &str, pane_title: &str, duplicate: bool) -> String {
    if duplicate {
        format!("{} · {}", agent_name, pane_title)
    } else {
        pane_title.to_string()
    }
}

/// (workspace id, agent name) pairs that occur more than once in one workspace.
pub fn duplicate_names(rows: &[AgentRow]) -> HashSet<(String, String)> {
    let mut counts: HashMap<(String, String), usize> = HashMap::new();
    for row in rows {
        *counts
            .entry((row.workspace_id.clone(), row.agent_name.clone()))
            .or_insert(0) += 1;
    }
    counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(key, _)| key)
        .collect()
}

/// The sidebar projection: the badge word for the first pane in a snapshot.
pub fn sidebar_word(snapshot: &Value) -> &'static str {
    let raw = snapshot
        .get("panes")
        .and_then(Value::as_array)
        .and_then(|panes| panes.first())
        .and_then(|pane| pane.get("agent_status").and_then(Value::as_str));
    badge_word(state_from_wire(raw))
}

/// The footer projection: the state word for the focused workspace in a snapshot.
pub fn footer_word(snapshot: &Value) -> &'static str {
    let raw = snapshot
        .get("workspaces")
        .and_then(Value::as_array)
        .and_then(|workspaces| {
            workspaces
                .iter()
                .find(|ws| ws.get("focused").and_then(Value::as_bool) == Some(true))
                .or_else(|| workspaces.first())
        })
        .and_then(|ws| ws.get("agent_status").and_then(Value::as_str));
    badge_word(state_from_wire(raw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sidebar_and_footer_derive_the_same_word_from_one_snapshot() {
        let snapshot = json!({
            "workspaces": [{
                "workspace_id": "ws-1",
                "label": "demo",
                "focused": true,
                "agent_status": "blocked"
            }],
            "tabs": [],
            "panes": [{
                "pane_id": "p1",
                "workspace_id": "ws-1",
                "label": "claude",
                "agent_status": "blocked"
            }]
        });
        let sidebar = sidebar_word(&snapshot);
        let footer = footer_word(&snapshot);
        assert_eq!(
            sidebar, footer,
            "one session must read the same word in the sidebar and the footer"
        );
        assert_eq!(sidebar, "Waiting");
    }

    #[test]
    fn needs_input_badge_is_amber_not_rose() {
        let theme = Theme::dark();
        let color = badge_color(SessionState::NeedsInput, &theme, 0);
        assert_eq!(color, theme.state_attention());
        assert_ne!(color, theme.accent, "rose is reserved for focus");
        assert_ne!(color, theme.primary, "rose is reserved for focus");
    }

    #[test]
    fn badge_word_comes_from_the_single_model() {
        assert_eq!(badge_word(SessionState::NeedsInput), "Waiting");
        assert_eq!(badge_word(SessionState::Running), "Working");
        assert_eq!(badge_word(state_from_wire(Some("blocked"))), "Waiting");
        assert_eq!(badge_word(state_from_wire(None)), "Starting");
        assert_eq!(badge_word(state_of(AgentStatus::Blocked)), "Waiting");
    }

    #[test]
    fn group_membership_follows_the_single_model() {
        assert_eq!(group(SessionState::Starting), StatusGroup::Working);
        assert_eq!(group(SessionState::Running), StatusGroup::Working);
        assert_eq!(group(SessionState::NeedsInput), StatusGroup::Working);
        assert_eq!(group(SessionState::Idle), StatusGroup::Settled);
        assert_eq!(
            group(SessionState::Exited { code: 0 }),
            StatusGroup::Settled
        );
    }

    #[test]
    fn duplicate_rows_get_agent_name_and_pane_title() {
        assert_eq!(row_label("Orion", "orion-2", true), "Orion · orion-2");
        assert_eq!(row_label("Orion", "orion-2", false), "orion-2");
    }

    #[test]
    fn duplicate_names_are_scoped_to_one_workspace() {
        let rows = vec![
            AgentRow {
                pane_id: "p1".into(),
                workspace_id: "ws-1".into(),
                agent_name: "Orion".into(),
                title: "a".into(),
                context: String::new(),
                status: AgentStatus::Working,
                selected: false,
            },
            AgentRow {
                pane_id: "p2".into(),
                workspace_id: "ws-1".into(),
                agent_name: "Orion".into(),
                title: "b".into(),
                context: String::new(),
                status: AgentStatus::Working,
                selected: false,
            },
            AgentRow {
                pane_id: "p3".into(),
                workspace_id: "ws-2".into(),
                agent_name: "Orion".into(),
                title: "c".into(),
                context: String::new(),
                status: AgentStatus::Working,
                selected: false,
            },
        ];
        let duplicates = duplicate_names(&rows);
        assert!(duplicates.contains(&("ws-1".to_string(), "Orion".to_string())));
        assert!(!duplicates.contains(&("ws-2".to_string(), "Orion".to_string())));
    }
}
