//! Prompt card: the agent's on-screen menu, answered without the terminal.
//!
//! The card reads one [`ChatPrompt`] (parsed from `DetectedPrompt::to_wire()`
//! via `GET /api/pane/prompt`) and answers through the [`PaneSeam`] — no REST
//! round-trip, so the id check in
//! [`orion_core::engine::pane::Pane::answer_prompt`] is the authority.
//! `prompt_changed` maps to "The question on screen changed; check it and
//! answer again."; other errors surface verbatim. Pending disables the buttons
//! while an answer sends.
//!
//! The card blocks the conversation while it waits: it takes the focus when it
//! appears, the number keys pick the options the agent numbered, and the card
//! is gone once the answer is out.

use crate::chat_entities::ChatPrompt;
use crate::composer::typed_character;
use crate::pane_seam::{PaneSeam, PROMPT_CHANGED};
use crate::theme::Theme;
use gpui::prelude::*;
use gpui::{div, px, white, Context, FocusHandle, IntoElement, KeyDownEvent, Render, Window};
use std::sync::Arc;

/// What the card tells the shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptCardEvent {
    /// The answer went out: the card has nothing left to ask.
    Answered,
}

impl gpui::EventEmitter<PromptCardEvent> for PromptCard {}

pub struct PromptCard {
    seam: Arc<dyn PaneSeam>,
    prompt: ChatPrompt,
    pending: bool,
    answered: bool,
    error: Option<String>,
    custom_text: String,
    custom_open: bool,
    focus_handle: FocusHandle,
}

impl PromptCard {
    pub fn new(seam: Arc<dyn PaneSeam>, prompt: ChatPrompt, cx: &mut Context<Self>) -> Self {
        Self {
            seam,
            prompt,
            pending: false,
            answered: false,
            error: None,
            custom_text: String::new(),
            custom_open: false,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn prompt_id(&self) -> &str {
        &self.prompt.id
    }

    /// Whether the answer went out and the card has nothing left to ask.
    pub fn is_answered(&self) -> bool {
        self.answered
    }

    /// The handle the card takes the focus with.
    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus_handle
    }

    /// Give the card the focus.
    pub fn focus(&self, window: &mut Window) {
        self.focus_handle.focus(window);
    }

    fn answer_option(&mut self, option_index: usize, cx: &mut Context<Self>) {
        self.pending = true;
        self.error = None;
        match self.seam.answer(&self.prompt.id, option_index) {
            Ok(()) => {
                self.pending = false;
                self.answered = true;
                cx.emit(PromptCardEvent::Answered);
            }
            Err(code) => {
                self.pending = false;
                self.error = Some(answer_error_message(&code));
            }
        }
    }

    fn answer_custom(&mut self, text: &str, cx: &mut Context<Self>) {
        self.pending = true;
        self.error = None;
        match self.seam.answer_typed(&self.prompt.id, text) {
            Ok(()) => {
                self.pending = false;
                self.answered = true;
                self.custom_open = false;
                cx.emit(PromptCardEvent::Answered);
            }
            Err(code) => {
                self.pending = false;
                self.error = Some(answer_error_message(&code));
            }
        }
    }

    /// One key on the card: the number of an option picks it, and the custom
    /// answer line takes the keyboard while it is open.
    fn on_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        if self.pending || self.answered {
            return;
        }
        let keystroke = &ev.keystroke;
        let key = keystroke.key.to_ascii_lowercase();

        if self.custom_open {
            match key.as_str() {
                "backspace" => {
                    self.custom_text.pop();
                    cx.notify();
                }
                "enter" => {
                    let text = std::mem::take(&mut self.custom_text);
                    if !text.is_empty() {
                        self.answer_custom(&text, cx);
                    }
                    cx.notify();
                }
                "escape" => {
                    self.custom_open = false;
                    cx.notify();
                }
                _ => {
                    if let Some(character) = typed_character(
                        &keystroke.key,
                        keystroke.key_char.as_deref(),
                        keystroke.modifiers.shift,
                    ) {
                        self.custom_text.push_str(&character);
                        cx.notify();
                    }
                }
            }
            return;
        }

        // The agent numbers its own menu: "1." is the first option. There is
        // no "0." option, so the zero key answers nothing.
        let Some(digit) = key
            .chars()
            .next()
            .and_then(|character| character.to_digit(10))
        else {
            return;
        };
        if digit == 0 {
            return;
        }
        let index = digit as usize - 1;
        if index < self.prompt.options.len() {
            self.answer_option(index, cx);
            cx.notify();
        }
    }
}

/// How the composer says to answer this menu.
pub fn answer_hint(prompt: &ChatPrompt) -> String {
    let choices: Vec<usize> = prompt
        .options
        .iter()
        .enumerate()
        .filter(|(index, _)| Some(*index) != prompt.custom_option_index)
        .map(|(index, _)| index)
        .collect();
    if choices.is_empty() {
        // A free-form question (the agent's queue) has no options to number.
        return "Type your reply…".to_string();
    }
    let range = if choices.len() > 1 {
        format!("{}–{}", choices[0] + 1, choices[choices.len() - 1] + 1)
    } else {
        format!("{}", choices[0] + 1)
    };
    if prompt.custom_option_index.is_some() {
        format!("Type {range} or your own reply…")
    } else {
        format!("Type {range} to choose…")
    }
}

/// Map an engine error code onto the card's error line: one sentence, never a
/// code.
pub fn answer_error_message(code: &str) -> String {
    match code {
        PROMPT_CHANGED => "The question on screen changed; check it and answer again.".to_string(),
        "invalid_option" => {
            "That option is no longer there. Check the question and answer again.".to_string()
        }
        _ => "The answer did not go through. Try again.".to_string(),
    }
}

impl Render for PromptCard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let prompt = self.prompt.clone();
        let pending = self.pending;
        let mut card = div()
            .id("prompt-card")
            .debug_selector(|| "prompt-card".to_string())
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
                    .debug_selector(|| "prompt-title".to_string())
                    .text_size(px(12.0 * Theme::current().density_scale()))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(format!("{} · {}", prompt.agent, prompt.title)),
            )
            .child(
                div()
                    .id("prompt-question")
                    .debug_selector(|| "prompt-question".to_string())
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
                    .debug_selector(|| format!("prompt-option-{index}"))
                    .py_2()
                    .px_3()
                    .rounded_md()
                    .bg(if pending {
                        Theme::current().surface_input
                    } else {
                        Theme::current().surface_hover
                    })
                    .child(text)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !pending {
                            this.answer_option(index, cx);
                            cx.notify();
                        }
                    })),
            );
        }

        if prompt.custom_option_index.is_some() || prompt.options.is_empty() {
            let custom_open = self.custom_open;
            card = card.child(
                div()
                    .id("prompt-custom-toggle")
                    .debug_selector(|| "prompt-custom-toggle".to_string())
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
                .debug_selector(|| "prompt-custom-input".to_string())
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
                        .debug_selector(|| "prompt-custom-send".to_string())
                        .py_2()
                        .px_3()
                        .rounded_md()
                        .bg(Theme::current().accent)
                        .child("Answer")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let text = std::mem::take(&mut this.custom_text);
                            if !text.is_empty() {
                                this.answer_custom(&text, cx);
                                cx.notify();
                            }
                        })),
                );
            card = card.child(input);
        }

        if let Some(error) = &self.error {
            card = card.child(
                div()
                    .id("prompt-error")
                    .debug_selector(|| "prompt-error".to_string())
                    .text_size(px(12.0 * Theme::current().density_scale()))
                    .text_color(Theme::current().danger_text)
                    .child(error.clone()),
            );
        }

        // The card owns the keyboard while it waits: it takes the focus when it
        // appears, and the number keys pick the options the agent numbered.
        card = card
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, ev, _, cx| {
                this.on_key(ev, cx);
            }));
        card
    }
}

#[cfg(test)]
mod tests {
    use super::answer_hint;
    use super::*;
    use crate::pane_seam::test_doubles::{RecordingPane, SeamCall};
    use gpui::{Entity, TestAppContext};
    use serde_json::json;
    use std::sync::Arc;

    fn prompt() -> ChatPrompt {
        ChatPrompt::from_value(&json!({
            "id": "p1", "agent": "claude", "kind": "approval", "title": "Approve?",
            "question": "Run it?", "body": null,
            "options": [{"label": "Yes", "description": null}, {"label": "No", "description": null}],
            "custom_option_index": null,
        }))
        .expect("the wire shape parses")
    }

    fn card(cx: &mut TestAppContext, pane: Arc<RecordingPane>) -> Entity<PromptCard> {
        let seam: Arc<dyn PaneSeam> = pane;
        cx.update(|app| app.new(|cx| PromptCard::new(seam, prompt(), cx)))
    }

    #[test]
    fn the_error_line_is_plain_language() {
        assert_eq!(
            answer_error_message(PROMPT_CHANGED),
            "The question on screen changed; check it and answer again."
        );
        assert_eq!(
            answer_error_message("invalid_option"),
            "That option is no longer there. Check the question and answer again."
        );
        assert_eq!(
            answer_error_message("something_else"),
            "The answer did not go through. Try again.",
            "a code the shell does not know is still not shown to the reader"
        );
    }

    #[gpui::test]
    fn a_number_key_answers_the_option_the_agent_numbered(cx: &mut TestAppContext) {
        let pane = Arc::new(RecordingPane::new());
        let card = card(cx, pane.clone());

        cx.update(|app| {
            card.update(app, |card, cx| {
                card.on_key(
                    &gpui::KeyDownEvent {
                        keystroke: gpui::Keystroke {
                            key: "2".to_string(),
                            key_char: Some("2".to_string()),
                            modifiers: gpui::Modifiers::none(),
                        },
                        is_held: false,
                    },
                    cx,
                );
            });
        });

        assert_eq!(
            pane.calls(),
            vec![SeamCall::Answer("p1".to_string(), 1)],
            "the second option was chosen"
        );
    }

    #[gpui::test]
    fn a_number_key_outside_the_options_answers_nothing(cx: &mut TestAppContext) {
        let pane = Arc::new(RecordingPane::new());
        let card = card(cx, pane.clone());

        cx.update(|app| {
            card.update(app, |card, cx| {
                card.on_key(
                    &gpui::KeyDownEvent {
                        keystroke: gpui::Keystroke {
                            key: "9".to_string(),
                            key_char: Some("9".to_string()),
                            modifiers: gpui::Modifiers::none(),
                        },
                        is_held: false,
                    },
                    cx,
                );
            });
        });

        assert!(
            pane.calls().is_empty(),
            "the agent numbered two options; nine is not one of them"
        );
    }

    #[gpui::test]
    fn the_zero_key_answers_nothing(cx: &mut TestAppContext) {
        let pane = Arc::new(RecordingPane::new());
        let card = card(cx, pane.clone());

        cx.update(|app| {
            card.update(app, |card, cx| {
                card.on_key(
                    &gpui::KeyDownEvent {
                        keystroke: gpui::Keystroke {
                            key: "0".to_string(),
                            key_char: Some("0".to_string()),
                            modifiers: gpui::Modifiers::none(),
                        },
                        is_held: false,
                    },
                    cx,
                );
            });
        });

        assert!(
            pane.calls().is_empty(),
            "the agent numbered two options; there is no zero"
        );
    }

    #[test]
    fn the_hint_says_how_to_answer_the_menu() {
        let mut prompt = prompt();
        assert_eq!(answer_hint(&prompt), "Type 1–2 to choose…");

        prompt.custom_option_index = Some(1);
        assert_eq!(
            answer_hint(&prompt),
            "Type 1 or your own reply…",
            "a menu with a free-form row says so"
        );

        prompt.options.clear();
        prompt.custom_option_index = Some(0);
        assert_eq!(
            answer_hint(&prompt),
            "Type your reply…",
            "a free-form question has no options to number"
        );
    }

    #[gpui::test]
    fn an_answered_card_says_it_is_answered(cx: &mut TestAppContext) {
        let pane = Arc::new(RecordingPane::new());
        let card = card(cx, pane.clone());

        cx.update(|app| {
            card.update(app, |card, cx| {
                card.answer_option(0, cx);
            });
        });

        cx.update(|app| {
            assert!(card.read(app).is_answered());
        });
    }

    #[gpui::test]
    fn a_failed_answer_keeps_the_card_and_says_why(cx: &mut TestAppContext) {
        let pane = Arc::new(RecordingPane::new());
        pane.fail_with("prompt_changed");
        let card = card(cx, pane.clone());

        cx.update(|app| {
            card.update(app, |card, cx| {
                card.answer_option(0, cx);
            });
        });

        cx.update(|app| {
            let card = card.read(app);
            assert!(
                !card.is_answered(),
                "the answer did not go out: the card stays"
            );
        });
        assert_eq!(
            pane.calls(),
            vec![SeamCall::Answer("p1".to_string(), 0)],
            "the card did try"
        );
    }
}
