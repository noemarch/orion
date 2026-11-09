//! Prompt card entity (P2): the agent's on-screen menu as buttons.
//!
//! The card reads one [`ChatPrompt`] (parsed from `DetectedPrompt::to_wire()` via
//! `GET /api/pane/prompt`) and answers in-process through `Pane::answer_prompt` /
//! `answer_prompt_custom` — no REST round-trip, so the id check in
//! [`orion_core::engine::pane::Pane::answer_prompt`] is the authority. `prompt_changed`
//! maps to "The question on screen changed; check it and answer again."; other
//! errors surface verbatim. Pending disables the buttons while an answer sends.

use crate::chat_entities::ChatPrompt;
use crate::theme::Theme;
use gpui::prelude::*;
use gpui::{div, px, white, Context, IntoElement, KeyDownEvent, Render, Window};
use orion_core::engine::pane::Pane;
use std::sync::Arc;

pub struct PromptCard {
    pane: Arc<Pane>,
    prompt: ChatPrompt,
    pending: bool,
    error: Option<String>,
    custom_text: String,
    custom_open: bool,
}

impl PromptCard {
    pub fn new(pane: Arc<Pane>, prompt: ChatPrompt) -> Self {
        Self {
            pane,
            prompt,
            pending: false,
            error: None,
            custom_text: String::new(),
            custom_open: false,
        }
    }

    pub fn set_prompt(&mut self, prompt: ChatPrompt) {
        self.prompt = prompt;
        self.error = None;
        self.pending = false;
    }

    pub fn prompt_id(&self) -> &str {
        &self.prompt.id
    }

    fn answer_option(&mut self, option_index: usize) {
        self.pending = true;
        self.error = None;
        match self.pane.answer_prompt(&self.prompt.id, option_index) {
            Ok(()) => {
                self.pending = false;
            }
            Err(code) => {
                self.pending = false;
                self.error = Some(answer_error_message(&code));
            }
        }
    }

    fn answer_custom(&mut self, text: &str) {
        self.pending = true;
        self.error = None;
        match self.pane.answer_prompt_custom(&self.prompt.id, text) {
            Ok(()) => {
                self.pending = false;
                self.custom_open = false;
            }
            Err(code) => {
                self.pending = false;
                self.error = Some(answer_error_message(&code));
            }
        }
    }
}

/// Map an engine error code onto the card's error line.
pub fn answer_error_message(code: &str) -> String {
    if code == "prompt_changed" {
        "The question on screen changed; check it and answer again.".to_string()
    } else {
        code.to_string()
    }
}

impl Render for PromptCard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let prompt = self.prompt.clone();
        let pending = self.pending;
        let mut card = div()
            .id("prompt-card")
            .flex_col()
            .gap(px(8.0))
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(Theme::current().accent)
            .bg(Theme::current().surface_elevated)
            .text_color(white())
            .child(
                div()
                    .id("prompt-title")
                    .text_size(px(12.0 * Theme::current().density_scale()))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(format!("{} · {}", prompt.agent, prompt.title)),
            )
            .child(
                div()
                    .id("prompt-question")
                    .text_size(px(13.0))
                    .child(prompt.question.clone()),
            );

        if let Some(body) = prompt.body.as_deref() {
            if !body.is_empty() {
                card = card.child(
                    div()
                        .id("prompt-body")
                        .text_size(px(12.0 * Theme::current().density_scale()))
                        .child(body.to_string()),
                );
            }
        }

        for (index, option) in prompt.options.iter().enumerate() {
            let label = if option.label.is_empty() {
                format!("Option {}", index + 1)
            } else {
                option.label.clone()
            };
            let description = option.description.clone().unwrap_or_default();
            let text = if description.is_empty() {
                label
            } else {
                format!("{label} — {description}")
            };
            card = card.child(
                div()
                    .id(index)
                    .py_2()
                    .px_3()
                    .rounded_md()
                    .bg(if pending {
                        Theme::current().surface_input
                    } else {
                        Theme::current().surface_hover
                    })
                    .child(text)
                    .on_click(cx.listener(move |this, _, _, _cx| {
                        if !pending {
                            this.answer_option(index);
                            _cx.notify();
                        }
                    })),
            );
        }

        if prompt.custom_option_index.is_some() || prompt.options.is_empty() {
            let custom_open = self.custom_open;
            card = card.child(
                div()
                    .id("prompt-custom-toggle")
                    .py_2()
                    .px_3()
                    .rounded_md()
                    .bg(Theme::current().surface_hover)
                    .child(if custom_open {
                        "Custom answer (type below)"
                    } else {
                        "Type your own answer…"
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.custom_open = !this.custom_open;
                        cx.notify();
                    })),
            );
        }

        if self.custom_open && !pending {
            let custom_value = self.custom_text.clone();
            let input = div()
                .id("prompt-custom-input")
                .flex_row()
                .gap(px(8.0))
                .items_center()
                .child(
                    div()
                        .id("prompt-custom-text")
                        .flex_1()
                        .child(format!("> {custom_value}")),
                )
                .child(
                    div()
                        .id("prompt-custom-send")
                        .py_2()
                        .px_3()
                        .rounded_md()
                        .bg(Theme::current().accent)
                        .child("Answer")
                        .on_click(cx.listener(move |this, _, _, _cx| {
                            let text = std::mem::take(&mut this.custom_text);
                            if !text.is_empty() {
                                this.answer_custom(&text);
                                _cx.notify();
                            }
                        })),
                );
            card = card.child(input);
        }

        if let Some(error) = &self.error {
            card = card.child(
                div()
                    .id("prompt-error")
                    .text_size(px(12.0 * Theme::current().density_scale()))
                    .text_color(Theme::current().danger_text)
                    .child(error.clone()),
            );
        }

        // Key handling for the custom answer line (typing appends to `custom_text`).
        card = card
            .track_focus(&cx.focus_handle())
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                if !this.custom_open || this.pending {
                    return;
                }
                let key = ev.keystroke.key.as_str();
                match key {
                    "backspace" => {
                        this.custom_text.pop();
                        cx.notify();
                    }
                    "enter" => {
                        let text = std::mem::take(&mut this.custom_text);
                        if !text.is_empty() {
                            this.answer_custom(&text);
                            cx.notify();
                        }
                    }
                    _ if key.chars().count() == 1 => {
                        this.custom_text.push_str(key);
                        cx.notify();
                    }
                    _ => {}
                }
            }));
        card
    }
}

#[cfg(test)]
mod tests {
    use super::answer_error_message;

    #[test]
    fn prompt_changed_copy() {
        assert_eq!(
            answer_error_message("prompt_changed"),
            "The question on screen changed; check it and answer again."
        );
        assert_eq!(answer_error_message("invalid_option"), "invalid_option");
    }
}
