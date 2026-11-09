//! Chat lens view (P2): structured conversation turns as a scrollable column.
//!
//! `ChatView` polls `Engine::http("GET", "/api/pane/conversation?pane_id=…")` on a
//! 2s timer (the wire seam — `engine.rs` untouched) and rerenders when the body
//! changes. Rendering is a plain scrollable column with a recent-N cap
//! (last 200 turns): GPUI virtualization is deliberately not used; the cap
//! mirrors the terminal scrollback bound and keeps oversized tool outputs
//! truncated with a "View full in Terminal" marker (see `truncate_output`).
//! A [`PromptCard`] sits at the top whenever `pane.prompt()` is `Some`.
//! Scroll-follow: new content scrolls to the bottom until the user scrolls up
//! (wheel sets `at_bottom = false`).

use crate::chat_entities::{ChatPart, ChatPrompt, ChatRole, ChatTurn, ConversationResponse};
use crate::markdown::{parse_markdown, MarkdownLine, RunKind};
use crate::prompt_card::PromptCard;
use crate::theme::Theme;
use gpui::prelude::*;
use gpui::{
    div, px, rems, Context, Entity, FontStyle, FontWeight, HighlightStyle, IntoElement, Render,
    ScrollHandle, ScrollWheelEvent, StyledText, TextStyle, Window,
};
use orion_core::engine::pane::Pane;
use orion_core::Engine;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

/// Recent-turn cap: chat renders the newest `MAX_RECENT_TURNS` turns only.
pub const MAX_RECENT_TURNS: usize = 200;
/// Tool input/output truncation bound (chars), with a marker.
pub const MAX_TOOL_CHARS: usize = 2000;

pub struct ChatView {
    engine: Option<Arc<Engine>>,
    pane: Option<Arc<Pane>>,
    pane_id: String,
    response: Option<ConversationResponse>,
    last_body: Option<String>,
    prompt_card: Option<Entity<PromptCard>>,
    scroll_handle: ScrollHandle,
    at_bottom: bool,
    thinking_open: Vec<bool>,
    error: Option<String>,
    focus_handle: gpui::FocusHandle,
    row_seq: usize,
}

fn conversation_error_message(_status: u16) -> String {
    "Couldn't load the conversation. Try again.".to_string()
}

impl ChatView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let scroll_handle = ScrollHandle::new();
        let view = Self {
            engine: None,
            pane: None,
            pane_id: "pane-1".to_string(),
            response: None,
            last_body: None,
            prompt_card: None,
            scroll_handle,
            at_bottom: true,
            thinking_open: Vec::new(),
            error: None,
            focus_handle: cx.focus_handle(),
            row_seq: 0,
        };
        view.start_poll(cx);
        view
    }

    fn next_row_id(&mut self) -> usize {
        let id = self.row_seq;
        self.row_seq += 1;
        id
    }

    /// Wire the engine + pane once they exist (called from `app.rs` after spawn).
    pub fn attach(&mut self, engine: Arc<Engine>, pane: Arc<Pane>, pane_id: String) {
        self.engine = Some(engine);
        self.pane = Some(pane);
        self.pane_id = pane_id;
        self.refresh();
    }

    pub fn set_pane(&mut self, pane: Arc<Pane>) {
        self.pane = Some(pane);
        self.refresh();
    }

    /// Fetch the conversation through the wire seam (`Engine::http`); the prompt
    /// card re-syncs from `pane.prompt()` so its DetectPrompt id is always fresh.
    pub fn refresh(&mut self) {
        let Some(engine) = self.engine.clone() else {
            return;
        };
        let path = format!("/api/pane/conversation?pane_id={}", self.pane_id);
        let (status, body) = engine.http("GET", &path, &Value::Null);
        if status != 200 {
            self.error = Some(conversation_error_message(status));
            return;
        }
        let encoded = body.to_string();
        if self.last_body.as_deref() == Some(encoded.as_str()) {
            return;
        }
        self.last_body = Some(encoded);
        match ConversationResponse::parse(&body) {
            Ok(response) => {
                self.thinking_open.resize(response.turns.len(), false);
                self.response = Some(response);
                self.error = None;
            }
            Err(error) => self.error = Some(format!("conversation parse failed: {error}")),
        }
    }

    fn start_poll(&self, cx: &mut Context<Self>) {
        let _ = self;
        cx.spawn(async move |view, cx| loop {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            let _ = view.update(cx, |view, _cx| view.refresh());
            if view.upgrade().is_none() {
                break;
            }
        })
        .detach();
    }

    fn scroll_to_bottom(&self) {
        self.scroll_handle.scroll_to_bottom();
    }
}

/// Truncate oversized text; the marker names the terminal as the full surface.
pub fn truncate_output(text: &str) -> String {
    if text.len() <= MAX_TOOL_CHARS {
        return text.to_string();
    }
    let cut = text.floor_char_boundary(MAX_TOOL_CHARS);
    format!(
        "{}\n… (truncated at {MAX_TOOL_CHARS} chars — View full in Terminal)",
        &text[..cut]
    )
}

fn markdown_highlights(
    line: &MarkdownLine,
    theme: Theme,
) -> Vec<(std::ops::Range<usize>, HighlightStyle)> {
    line.runs
        .iter()
        .map(|(range, run)| {
            let style = match run {
                RunKind::Bold => HighlightStyle {
                    font_weight: Some(FontWeight::BOLD),
                    ..Default::default()
                },
                RunKind::Italic => HighlightStyle {
                    font_style: Some(FontStyle::Italic),
                    ..Default::default()
                },
                RunKind::Code => HighlightStyle {
                    background_color: Some(theme.surface_hover.into()),
                    ..Default::default()
                },
                RunKind::Link => HighlightStyle {
                    color: Some(theme.accent.into()),
                    ..Default::default()
                },
            };
            (range.clone(), style)
        })
        .collect()
}

fn render_text_block(text: &str, theme: Theme) -> Vec<impl IntoElement> {
    parse_markdown(text)
        .iter()
        .map(|line| {
            div()
                .flex_row()
                .child(StyledText::new(line.text.clone()).with_default_highlights(
                    &TextStyle {
                        color: crate::theme::hsla(theme.text),
                        font_size: rems(0.875).into(),
                        ..Default::default()
                    },
                    markdown_highlights(line, theme),
                ))
        })
        .collect()
}

fn diff_lines(output: &str) -> Vec<(String, bool)> {
    // Unified-diff-ish output gets green/red rows via `similar`'s line diff;
    // otherwise the caller shows plain truncated output.
    let diff = similar::TextDiff::from_lines("", output);
    diff.iter_all_changes()
        .map(|change| {
            (
                change.to_string(),
                change.tag() == similar::ChangeTag::Insert,
            )
        })
        .collect()
}

fn is_diff_output(output: &str) -> bool {
    output.contains("diff --git") || output.contains("\n@@ ")
}

impl Render for ChatView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.row_seq = 0;
        let theme = Theme::current();
        let chat = div()
            .id("chat-view")
            .track_focus(&self.focus_handle)
            .flex_col()
            .size_full()
            .bg(theme.surface_base)
            .text_color(theme.text)
            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _, cx| {
                let _ = ev;
                this.at_bottom = false;
                cx.notify();
            }));

        let mut root = chat.child(self.render_status_bar());

        if let Some(error) = &self.error {
            root = root.child(
                div()
                    .id("chat-error")
                    .px_3()
                    .py_1()
                    .text_size(px(12.0 * Theme::current().density_scale()))
                    .text_color(Theme::current().danger_text)
                    .child(error.clone()),
            );
        }

        let turn_column = {
            let mut column = div().id("chat-turns").flex_col().gap(px(12.0)).p_3();
            let (turns, total_turns): (Vec<ChatTurn>, usize) = match &self.response {
                Some(response) => (
                    response
                        .recent(MAX_RECENT_TURNS)
                        .into_iter()
                        .cloned()
                        .collect(),
                    response.turns.len(),
                ),
                None => (Vec::new(), 0),
            };
            for (offset, turn) in turns.iter().enumerate() {
                let index = total_turns - turns.len() + offset;
                column = column.child(self.render_turn(index, turn, cx));
            }
            column
        };

        let list = div()
            .id("chat-scroll")
            .flex_col()
            .flex_1()
            .overflow_y_scroll()
            .track_scroll(&self.scroll_handle)
            .child(turn_column);

        if self.at_bottom {
            self.scroll_to_bottom();
        }

        root.child(list)
    }
}

impl ChatView {
    fn render_status_bar(&self) -> impl IntoElement {
        let source = self
            .response
            .as_ref()
            .map(|r| r.source.clone())
            .unwrap_or_else(|| "starting…".to_string());
        let count = self.response.as_ref().map(|r| r.turns.len()).unwrap_or(0);
        let status = self
            .pane
            .as_ref()
            .map(|p| p.status().as_wire().to_string())
            .unwrap_or_default();
        div()
            .id("chat-status")
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .h(px(28.0))
            .px_3()
            .text_size(px(11.0 * Theme::current().density_scale()))
            .text_color(Theme::current().text_dim)
            .child(format!("{source} · {count} turns · {status}"))
    }

    fn render_turn(
        &mut self,
        index: usize,
        turn: &ChatTurn,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_user = turn.role == ChatRole::User;
        let mut card = div()
            .id(self.next_row_id())
            .flex_col()
            .gap(px(6.0))
            .p_3()
            .rounded_md()
            .bg(if is_user {
                Theme::current().surface_elevated
            } else {
                Theme::current().surface_panel
            })
            .border_1()
            .border_color(if is_user {
                Theme::current().border_strong
            } else {
                Theme::current().border
            });

        let ts = turn
            .ts
            .clone()
            .unwrap_or_else(|| turn.end_ts.clone().unwrap_or_default());
        let role_label = if is_user { "You" } else { "Agent" };
        card = card.child(
            div()
                .id(self.next_row_id())
                .flex_row()
                .gap(px(8.0))
                .text_size(px(11.0 * Theme::current().density_scale()))
                .text_color(Theme::current().text_dim)
                .child(role_label.to_string())
                .child(ts),
        );

        for (part_index, part) in turn.parts.iter().enumerate() {
            card = card.child(self.render_part(index, part_index, part, cx));
        }
        card
    }

    fn render_part(
        &mut self,
        turn_index: usize,
        _part_index: usize,
        part: &ChatPart,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        match part {
            ChatPart::Text { text, .. } => {
                let mut column = div().id(self.next_row_id()).flex_col();
                for line in render_text_block(text, Theme::current()) {
                    column = column.child(line);
                }
                column
            }
            ChatPart::Thinking { text } => {
                let open = *self.thinking_open.get(turn_index).unwrap_or(&false);
                let mut folded = div()
                    .id(self.next_row_id())
                    .flex_col()
                    .gap(px(4.0))
                    .rounded_md()
                    .bg(Theme::current().surface_elevated);
                folded = folded.child(
                    div()
                        .id(self.next_row_id())
                        .px_2()
                        .py_1()
                        .text_size(px(12.0 * Theme::current().density_scale()))
                        .text_color(Theme::current().text_dim)
                        .child(if open {
                            "Thinking (collapse)"
                        } else {
                            "Thinking (expand)"
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.thinking_open.len() <= turn_index {
                                this.thinking_open.resize(turn_index + 1, false);
                            }
                            this.thinking_open[turn_index] = !this.thinking_open[turn_index];
                            cx.notify();
                        })),
                );
                if open {
                    let mut body = div().id(self.next_row_id()).px_2().py_1();
                    for line in render_text_block(text, Theme::current()) {
                        body = body.child(line);
                    }
                    folded = folded.child(body);
                }
                folded
            }
            ChatPart::Tool(tool) => {
                let color = if tool.error {
                    Theme::current().danger_text
                } else {
                    Theme::current().status_done
                };
                let mut row = div()
                    .id(self.next_row_id())
                    .flex_col()
                    .gap(px(4.0))
                    .rounded_md()
                    .bg(Theme::current().surface_panel)
                    .p_2();
                row = row.child(
                    div()
                        .id(self.next_row_id())
                        .flex_row()
                        .gap(px(6.0))
                        .text_size(px(12.0 * Theme::current().density_scale()))
                        .child(
                            div()
                                .id(self.next_row_id())
                                .text_color(color)
                                .child(tool.name.clone()),
                        )
                        .child(
                            div()
                                .id(self.next_row_id())
                                .text_color(Theme::current().text_dim)
                                .child(tool.summary.clone()),
                        ),
                );
                if !tool.input.is_empty() {
                    row = row.child(
                        div()
                            .id(self.next_row_id())
                            .text_size(px(11.0 * Theme::current().density_scale()))
                            .text_color(Theme::current().text_dim)
                            .child(truncate_output(&tool.input)),
                    );
                }
                if tool.arguments_omitted {
                    row = row.child(
                        div()
                            .id(self.next_row_id())
                            .text_size(px(11.0 * Theme::current().density_scale()))
                            .child("Arguments omitted (too large for the transcript page)."),
                    );
                }
                if !tool.output.is_empty() {
                    if is_diff_output(&tool.output) {
                        let mut diff_column = div().id(self.next_row_id()).flex_col();
                        for (line, inserted) in diff_lines(&truncate_output(&tool.output)) {
                            diff_column = diff_column.child(
                                div()
                                    .id(self.next_row_id())
                                    .text_size(px(11.0 * Theme::current().density_scale()))
                                    .text_color(if inserted {
                                        Theme::current().status_done
                                    } else {
                                        Theme::current().text
                                    })
                                    .child(line),
                            );
                        }
                        row = row.child(diff_column);
                    } else {
                        row = row.child(
                            div()
                                .id(self.next_row_id())
                                .text_size(px(11.0 * Theme::current().density_scale()))
                                .text_color(if tool.error {
                                    Theme::current().status_blocked
                                } else {
                                    Theme::current().text
                                })
                                .child(truncate_output(&tool.output)),
                        );
                    }
                }
                if tool.error {
                    row = row.child(
                        div()
                            .id(self.next_row_id())
                            .text_size(px(11.0 * Theme::current().density_scale()))
                            .text_color(Theme::current().danger_text)
                            .child("Tool call failed — View full in Terminal"),
                    );
                }
                row
            }
            ChatPart::Compact { text } => div()
                .id(self.next_row_id())
                .text_size(px(12.0 * Theme::current().density_scale()))
                .text_color(Theme::current().text_dim)
                .child(format!("Compacted: {text}")),
            ChatPart::Notice { text, source } => {
                let source = source.clone().unwrap_or_default();
                div()
                    .id(self.next_row_id())
                    .text_size(px(12.0 * Theme::current().density_scale()))
                    .text_color(Theme::current().notice)
                    .child(if source.is_empty() {
                        text.clone()
                    } else {
                        format!("[{source}] {text}")
                    })
            }
            ChatPart::TaskResult { tasks } => {
                let mut column = div().id(self.next_row_id()).flex_col().gap(px(2.0));
                for task in tasks {
                    column = column.child(
                        div()
                            .id(self.next_row_id())
                            .text_size(px(12.0 * Theme::current().density_scale()))
                            .child(format!(
                                "Task {} [{}]: {}",
                                task.title, task.status, task.result
                            )),
                    );
                }
                column
            }
            ChatPart::Skill { skill } => div()
                .id(self.next_row_id())
                .text_size(px(12.0 * Theme::current().density_scale()))
                .text_color(Theme::current().text_dim)
                .child(format!(
                    "Skill {} ({}/{})",
                    skill.name, skill.evidence, skill.status
                )),
            ChatPart::Image {
                media_type,
                image_ref,
            } => div()
                .id(self.next_row_id())
                .text_size(px(12.0 * Theme::current().density_scale()))
                .child(format!("Image ({media_type}) — {image_ref}")),
            ChatPart::Unknown => div()
                .id(self.next_row_id())
                .text_size(px(12.0 * Theme::current().density_scale()))
                .text_color(Theme::current().text_dim)
                .child("(unsupported part)"),
        }
    }

    /// The prompt card slot: a fresh `PromptCard` entity keyed by prompt id so a
    /// new question never inherits a stale answer state.
    pub fn prompt_card_entity(&mut self, cx: &mut Context<Self>) -> Option<Entity<PromptCard>> {
        let pane = self.pane.clone()?;
        let detected = pane.prompt()?;
        let wire = detected.to_wire();
        let prompt = ChatPrompt::from_value(&wire)?;
        let matches = self
            .prompt_card
            .as_ref()
            .map(|card| card.read(&*cx).prompt_id() == prompt.id)
            .unwrap_or(false);
        if matches {
            return self.prompt_card.clone();
        }
        let card = cx.new(|_| PromptCard::new(pane, prompt));
        self.prompt_card = Some(card.clone());
        Some(card)
    }

    pub fn pane(&self) -> Option<&Arc<Pane>> {
        self.pane.as_ref()
    }

    pub fn at_bottom(&self) -> bool {
        self.at_bottom
    }

    pub fn set_at_bottom(&mut self, at_bottom: bool) {
        self.at_bottom = at_bottom;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversation_fetch_failure_is_plain_language_with_retry() {
        let message = conversation_error_message(404);
        assert_eq!(message, "Couldn't load the conversation. Try again.");
        assert!(message.contains("Try again"));
    }

    #[test]
    fn truncates_at_bound_with_terminal_marker() {
        let long = "x".repeat(MAX_TOOL_CHARS + 100);
        let truncated = truncate_output(&long);
        assert!(truncated.contains("View full in Terminal"));
        assert!(truncated.len() <= MAX_TOOL_CHARS + 64);
        assert_eq!(truncate_output("short"), "short");
    }

    #[test]
    fn detects_diff_output() {
        assert!(is_diff_output("diff --git a/x b/x\n@@ -1 +1 @@\n"));
        assert!(!is_diff_output("plain output"));
    }
}
