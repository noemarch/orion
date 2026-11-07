use chrono::Utc;
use serde_json::Value;
use uuid::Uuid;

use crate::protocol::{
    ApprovalRequest, ConversationTurn, PlanStep, PlanStepStatus, PlanSummary, PromptOption,
    PromptType, ToolCallItem, ToolCallStatus, TurnRole,
};

pub struct TranscriptParser;

impl TranscriptParser {
    /// Parse raw JSONL lines (such as Claude Code or omp) into structured ConversationTurns.
    pub fn parse_claude_jsonl(pane_id: &str, line: &str) -> Option<ConversationTurn> {
        let val: Value = serde_json::from_str(line).ok()?;

        let role_str = val.get("role").or_else(|| val.get("type"))?.as_str()?;
        let role = match role_str {
            "user" | "human" => TurnRole::User,
            "assistant" | "model" => TurnRole::Assistant,
            _ => TurnRole::System,
        };

        let content = val
            .get("content")
            .or_else(|| val.get("text"))
            .and_then(|c| {
                if let Some(s) = c.as_str() {
                    Some(s.to_string())
                } else if let Some(arr) = c.as_array() {
                    let text = arr
                        .iter()
                        .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("\n");
                    Some(text)
                } else {
                    None
                }
            })
            .unwrap_or_default();

        let mut tool_calls = Vec::new();
        if let Some(tools) = val.get("tool_calls").and_then(|t| t.as_array()) {
            for tool in tools {
                let tool_name = tool
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let parameters = tool.get("input").cloned().unwrap_or(Value::Null);

                tool_calls.push(ToolCallItem {
                    id: Uuid::new_v4().to_string(),
                    tool_name,
                    parameters,
                    output: None,
                    status: ToolCallStatus::Success,
                    duration_ms: None,
                });
            }
        }

        Some(ConversationTurn {
            id: Uuid::new_v4().to_string(),
            pane_id: pane_id.to_string(),
            role,
            content,
            tool_calls,
            timestamp: Utc::now(),
        })
    }

    /// Extract an interactive approval request from tool invocations or prompts.
    pub fn extract_approval_request(
        pane_id: &str,
        tool_name: &str,
        params: &Value,
    ) -> Option<ApprovalRequest> {
        match tool_name {
            "bash" | "execute_command" | "run_terminal_command" => {
                let cmd = params
                    .get("command")
                    .and_then(|c| c.as_str())
                    .unwrap_or_default()
                    .to_string();

                let is_high_risk = cmd.contains("rm ")
                    || cmd.contains("drop ")
                    || cmd.contains("push ")
                    || cmd.contains("delete");

                Some(ApprovalRequest {
                    id: Uuid::new_v4().to_string(),
                    pane_id: pane_id.to_string(),
                    prompt_type: PromptType::CommandApproval {
                        command: cmd.clone(),
                    },
                    question: format!("Allow command: `{}`?", cmd),
                    description: Some(
                        "Agent requested permission to run a terminal command.".into(),
                    ),
                    options: vec![
                        PromptOption {
                            index: 0,
                            label: "Allow Once".into(),
                            description: Some("Execute this command once".into()),
                            is_danger: false,
                        },
                        PromptOption {
                            index: 1,
                            label: "Deny".into(),
                            description: Some("Block this command and inform agent".into()),
                            is_danger: false,
                        },
                        PromptOption {
                            index: 2,
                            label: "Allow Always in Session".into(),
                            description: Some("Do not ask again for this command pattern".into()),
                            is_danger: is_high_risk,
                        },
                    ],
                    default_option_index: Some(0),
                    created_at: Utc::now(),
                })
            }
            "write_file" | "edit_file" | "str_replace" => {
                let path = params
                    .get("path")
                    .or_else(|| params.get("file_path"))
                    .and_then(|p| p.as_str())
                    .unwrap_or("unknown")
                    .to_string();

                Some(ApprovalRequest {
                    id: Uuid::new_v4().to_string(),
                    pane_id: pane_id.to_string(),
                    prompt_type: PromptType::FileEditApproval {
                        file_path: path.clone(),
                        diff: None,
                    },
                    question: format!("Allow edit to `{}`?", path),
                    description: Some("Agent wants to write or modify a project file.".into()),
                    options: vec![
                        PromptOption {
                            index: 0,
                            label: "Apply Changes".into(),
                            description: None,
                            is_danger: false,
                        },
                        PromptOption {
                            index: 1,
                            label: "Reject".into(),
                            description: None,
                            is_danger: false,
                        },
                    ],
                    default_option_index: Some(0),
                    created_at: Utc::now(),
                })
            }
            _ => None,
        }
    }

    /// Extract active plan steps from TodoWrite or TaskCreate payloads.
    pub fn parse_plan_summary(val: &Value) -> Option<PlanSummary> {
        let tasks = val.get("tasks").or_else(|| val.get("todos"))?.as_array()?;

        let mut steps = Vec::new();
        let mut completed_count = 0;
        let mut active_label = None;

        for (idx, item) in tasks.iter().enumerate() {
            let label = item
                .get("content")
                .or_else(|| item.get("task"))
                .or_else(|| item.get("label"))
                .and_then(|l| l.as_str())
                .unwrap_or("Task")
                .to_string();

            let status_str = item
                .get("status")
                .and_then(|s| s.as_str())
                .unwrap_or("pending");

            let status = match status_str {
                "completed" | "done" => {
                    completed_count += 1;
                    PlanStepStatus::Completed
                }
                "in_progress" | "running" => {
                    if active_label.is_none() {
                        active_label = Some(label.clone());
                    }
                    PlanStepStatus::InProgress
                }
                "failed" => PlanStepStatus::Failed,
                "skipped" => PlanStepStatus::Skipped,
                _ => PlanStepStatus::Pending,
            };

            steps.push(PlanStep {
                id: format!("step-{}", idx + 1),
                label,
                status,
                duration_seconds: None,
            });
        }

        Some(PlanSummary {
            total_steps: steps.len(),
            completed_steps: completed_count,
            active_step_label: active_label,
            steps,
        })
    }
}
