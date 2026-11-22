//! The Chat Lens composer: the reader's message box.
//!
//! The composer is the only way the reader talks to the agent from the Chat
//! Lens, so its states are the contract with the reader: an empty draft cannot
//! be sent, a send on its way cannot be sent twice, a failed send keeps the
//! words and offers a retry, and a stop interrupts the agent's turn.
//!
//! The rules live in [`ComposerModel`], which is testable without a window;
//! [`Composer`] draws them and talks to the [`PaneSeam`].

use crate::pane_seam::{PaneSeam, AGENT_GONE};
use crate::theme::{Theme, TOUCH_TARGET_PX};
use gpui_kit::base::component_traits::Disableable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{Icon, IconName, Sizable, ThemeStyled};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use gpui_kit::{
    div, px, App, Context, FocusHandle, IntoElement, KeyDownEvent, MouseButton, Render, Window,
};
use std::sync::Arc;

// Spacing tokens (DESIGN.md §4), as in chat.rs and sidebar.rs.
const SPACE_3: f32 = 12.0; // --space-3: control padding
const CONTENT_W: f32 = 820.0; // --content-w: composer column width

/// What the composer's send is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendState {
    /// Nothing is on its way: the reader may send.
    Idle,
    /// A message is on its way to the agent.
    Pending,
    /// The last action failed. `retry` holds the words to send again, when
    /// there are words to send again: a failed send has them, a failed stop
    /// does not.
    Failed {
        /// What went wrong, in plain language.
        message: String,
        /// The words to send again, when the reader may send them again.
        retry: Option<String>,
    },
}

impl SendState {
    /// Whether a send is on its way.
    pub fn is_pending(&self) -> bool {
        matches!(self, SendState::Pending)
    }

    /// Whether the last send failed.
    pub fn is_failed(&self) -> bool {
        matches!(self, SendState::Failed { .. })
    }
}

/// What one keystroke does to the composer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerKey {
    /// The draft changed.
    Changed,
    /// The reader asked to send the draft.
    Send,
    /// The reader asked to stop the agent.
    Stop,
    /// Not a composer key.
    Ignored,
}

/// The composer's state: the draft, the send on its way, and the failure that
/// keeps the words for a retry.
#[derive(Debug, Clone)]
pub struct ComposerModel {
    draft: String,
    state: SendState,
    /// The words the pending or failed send carries, so a retry sends the same
    /// message rather than whatever the draft has become since.
    sending: Option<String>,
}

impl Default for ComposerModel {
    fn default() -> Self {
        Self::new()
    }
}

impl ComposerModel {
    /// A composer with an empty draft and nothing on its way.
    pub fn new() -> Self {
        Self {
            draft: String::new(),
            state: SendState::Idle,
            sending: None,
        }
    }

    /// The words the reader has typed.
    pub fn draft(&self) -> &str {
        &self.draft
    }

    /// What the send is doing.
    pub fn state(&self) -> &SendState {
        &self.state
    }

    /// The message the pending or failed send carries.
    pub fn sending(&self) -> Option<&str> {
        self.sending.as_deref()
    }

    /// Whether the reader may send: there are words, and no send is on its way.
    pub fn can_send(&self) -> bool {
        !self.draft.trim().is_empty() && !self.state.is_pending()
    }

    /// Whether the agent may be stopped right now.
    pub fn can_stop(&self) -> bool {
        !self.state.is_pending()
    }

    /// The message the failure says, when the last send failed.
    pub fn failure(&self) -> Option<&str> {
        match &self.state {
            SendState::Failed { message, .. } => Some(message),
            _ => None,
        }
    }

    /// Whether there is a message to send again: a failed send keeps its
    /// words, a failed stop has none.
    pub fn can_retry(&self) -> bool {
        match &self.state {
            SendState::Failed { retry, .. } => retry.is_some(),
            _ => false,
        }
    }

    /// The reader typed something.
    pub fn type_text(&mut self, text: &str) {
        self.draft.push_str(text);
    }

    /// The reader typed: one key is applied to the draft.
    pub fn apply_key(
        &mut self,
        key: &str,
        key_char: Option<&str>,
        shift: bool,
        command_modifier: bool,
    ) -> ComposerKey {
        apply_composer_key(&mut self.draft, key, key_char, shift, command_modifier)
    }

    /// A new agent: the old draft belongs to the old agent.
    pub fn reset(&mut self) {
        *self = ComposerModel::new();
    }

    /// The draft becomes a send: the words leave the draft and go on their
    /// way. `None` when there is nothing to send, or a send is already on its
    /// way.
    pub fn begin_send(&mut self) -> Option<String> {
        if !self.can_send() {
            return None;
        }
        let message = self.draft.trim().to_string();
        self.draft.clear();
        self.sending = Some(message.clone());
        self.state = SendState::Pending;
        Some(message)
    }

    /// The send reached the agent, or the stop went through.
    pub fn settled(&mut self) {
        self.state = SendState::Idle;
        self.sending = None;
    }

    /// The send did not reach the agent: the message says why, and the words
    /// stay so the reader can send them again.
    pub fn failed(&mut self, message: String) {
        self.state = SendState::Failed {
            message,
            retry: self.sending.clone(),
        };
    }

    /// The stop did not reach the agent: the message says why. There are no
    /// words to send again, so nothing offers a retry.
    pub fn failed_stop(&mut self, message: String) {
        self.state = SendState::Failed {
            message,
            retry: None,
        };
    }

    /// The reader asks to send the failed message again.
    pub fn retry(&mut self) -> Option<String> {
        let SendState::Failed { retry, .. } = &self.state else {
            return None;
        };
        let message = retry.clone()?;
        self.state = SendState::Pending;
        Some(message)
    }
}

/// Applies one keystroke to the draft, so the composer's input is a pure
/// reducer the tests can drive without a window.
///
/// `key_char` is the character the key produced (a shifted letter, a space);
/// when the platform does not report one, the key name is the character.
/// `shift` turns Enter into a line break: Enter sends, Shift+Enter writes the
/// next line.
pub fn apply_composer_key(
    text: &mut String,
    key: &str,
    key_char: Option<&str>,
    shift: bool,
    command_modifier: bool,
) -> ComposerKey {
    if command_modifier {
        // Ctrl/Cmd chords are shortcuts, never text.
        return ComposerKey::Ignored;
    }
    match key {
        "enter" if shift => {
            text.push('\n');
            ComposerKey::Changed
        }
        "enter" => ComposerKey::Send,
        "escape" => ComposerKey::Stop,
        "backspace" => {
            text.pop();
            ComposerKey::Changed
        }
        "space" => {
            text.push(' ');
            ComposerKey::Changed
        }
        _ => match typed_character(key, key_char, shift) {
            Some(character) => {
                text.push_str(&character);
                ComposerKey::Changed
            }
            None => ComposerKey::Ignored,
        },
    }
}

/// The character one key produced, when it produced one.
pub fn typed_character(key: &str, key_char: Option<&str>, shift: bool) -> Option<String> {
    if let Some(character) = key_char.filter(|character| !character.is_empty()) {
        return Some(character.to_string());
    }
    let mut characters = key.chars();
    let first = characters.next()?;
    if characters.next().is_some() {
        return None;
    }
    if shift && first.is_ascii_alphabetic() {
        return Some(first.to_ascii_uppercase().to_string());
    }
    Some(first.to_string())
}

/// The message a failed send shows: one sentence, never a raw error.
pub fn send_error_message(error: &str) -> String {
    if error == AGENT_GONE {
        "This agent is no longer running.".to_string()
    } else {
        "Couldn't send the message. Try again.".to_string()
    }
}

/// The message a failed stop shows: one sentence, never a raw error.
pub fn stop_error_message(error: &str) -> String {
    if error == AGENT_GONE {
        "This agent is no longer running.".to_string()
    } else {
        "Couldn't stop the agent.".to_string()
    }
}

/// Settles the composer's state after a send: the agent has the message, or
/// the reader is told why it did not go.
pub fn settle_send(model: &mut ComposerModel, result: Result<(), String>) {
    match result {
        Ok(()) => model.settled(),
        Err(error) => model.failed(send_error_message(&error)),
    }
}

/// Settles the composer's state after a stop: the agent's turn was
/// interrupted, or the reader is told why it did not go.
pub fn settle_stop(model: &mut ComposerModel, result: Result<(), String>) {
    match result {
        Ok(()) => model.settled(),
        Err(error) => model.failed_stop(stop_error_message(&error)),
    }
}

/// The Chat Lens composer: the reader's message box.
///
/// The box is the only way the reader drives the conversation from the Chat
/// Lens: it types the draft, sends it through the [`PaneSeam`], and stops the
/// agent's turn. The rules live in [`ComposerModel`]; this draws them and
/// talks to the seam.
pub struct Composer {
    model: ComposerModel,
    seam: Option<Arc<dyn PaneSeam>>,
    focus_handle: FocusHandle,
    /// A prompt card is waiting: the agent wants an answer, not a message.
    blocked: bool,
    /// How the box says to answer the agent's question.
    answer_hint: Option<String>,
    /// The agent is working: the stop control is live.
    running: bool,
}

impl Composer {
    /// A composer with no agent behind it yet.
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            model: ComposerModel::new(),
            seam: None,
            focus_handle: cx.focus_handle().tab_stop(true),
            blocked: false,
            answer_hint: None,
            running: false,
        }
    }

    /// The agent this composer talks to.
    pub fn attach(&mut self, seam: Arc<dyn PaneSeam>) {
        self.seam = Some(seam);
    }

    /// Whether the agent is working: the stop control is live while it is,
    /// and quiet while it does not.
    pub fn set_running(&mut self, running: bool) {
        self.running = running;
    }

    /// A new agent: the old draft belonged to the old agent.
    pub fn reset(&mut self) {
        self.model.reset();
    }

    /// A prompt card is waiting: the composer stops accepting messages.
    pub fn set_blocked(&mut self, blocked: bool) {
        self.blocked = blocked;
    }

    /// How the box says to answer the agent's question.
    pub fn set_answer_hint(&mut self, hint: Option<String>) {
        self.answer_hint = hint;
    }

    /// The state behind the box.
    pub fn model(&self) -> &ComposerModel {
        &self.model
    }

    /// The handle the box takes the focus with.
    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus_handle
    }

    /// Give the box the focus.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.focus_handle.focus(window, cx);
    }

    /// Whether the composer is waiting on a prompt card.
    pub fn is_blocked(&self) -> bool {
        self.blocked
    }

    /// Handle one key event. Returns whether the event was consumed.
    fn on_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        if self.blocked {
            // The agent is waiting for an answer: the card owns the keyboard.
            return false;
        }
        let keystroke = &ev.keystroke;
        // Ctrl+V pastes what the reader copied.
        if (keystroke.modifiers.control || keystroke.modifiers.platform)
            && keystroke.key.eq_ignore_ascii_case("v")
        {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                self.model.type_text(&text);
                cx.notify();
            }
            return true;
        }
        let action = self.model.apply_key(
            &keystroke.key,
            keystroke.key_char.as_deref(),
            keystroke.modifiers.shift,
            keystroke.modifiers.control || keystroke.modifiers.platform,
        );
        match action {
            ComposerKey::Changed => {
                cx.notify();
                true
            }
            ComposerKey::Send => {
                self.send(cx);
                true
            }
            ComposerKey::Stop => {
                // Only a running agent can be stopped: an idle agent must not
                // be sent an interrupt.
                if self.running {
                    self.stop(cx);
                    true
                } else {
                    false
                }
            }
            ComposerKey::Ignored => false,
        }
    }

    /// Send the draft to the agent.
    fn send(&mut self, cx: &mut Context<Self>) {
        let Some(message) = self.model.begin_send() else {
            return;
        };
        self.deliver(message, cx);
    }

    /// Send the failed message again.
    fn retry(&mut self, cx: &mut Context<Self>) {
        let Some(message) = self.model.retry() else {
            return;
        };
        self.deliver(message, cx);
    }

    /// Deliver a message to the agent.
    ///
    /// The write happens off the UI thread, so the box shows that the message
    /// is on its way, and a pane that has gone says so when the answer comes
    /// back.
    fn deliver(&mut self, message: String, cx: &mut Context<Self>) {
        let Some(seam) = self.seam.clone() else {
            self.model
                .failed("This agent is no longer running.".to_string());
            cx.notify();
            return;
        };
        // The agent starts working on the message the moment it goes out.
        self.running = true;
        let sent_to = Arc::clone(&seam);
        cx.spawn(async move |this, cx| {
            let writer = Arc::clone(&sent_to);
            let result = cx
                .background_executor()
                .spawn(async move { writer.submit(&message) })
                .await;
            let _ = this.update(cx, |this, cx| {
                // Another agent took the box meanwhile: this answer belongs to
                // a pane that is no longer on screen.
                if !this
                    .seam
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &sent_to))
                {
                    return;
                }
                settle_send(&mut this.model, result);
                cx.notify();
            });
        })
        .detach();
    }

    /// Interrupt the agent's turn.
    ///
    /// The write happens off the UI thread, like the send: the frame never
    /// waits on the agent's pipe.
    fn stop(&mut self, cx: &mut Context<Self>) {
        let Some(seam) = self.seam.clone() else {
            self.model
                .failed_stop("This agent is no longer running.".to_string());
            cx.notify();
            return;
        };
        let stopped = Arc::clone(&seam);
        cx.spawn(async move |this, cx| {
            let writer = Arc::clone(&stopped);
            let result = cx
                .background_executor()
                .spawn(async move { writer.stop() })
                .await;
            let _ = this.update(cx, |this, cx| {
                // Another agent took the box meanwhile: this answer belongs to
                // a pane that is no longer on screen.
                if !this
                    .seam
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &stopped))
                {
                    return;
                }
                settle_stop(&mut this.model, result);
                cx.notify();
            });
        })
        .detach();
    }

    /// The draft as the reader typed it, for the box to draw.
    fn draft_text(&self) -> String {
        if self.model.draft().is_empty() {
            String::new()
        } else {
            format!("{}▏", self.model.draft())
        }
    }

    /// The note under the box: why a send failed, or what the box is waiting
    /// for.
    fn render_note(&self, cx: &mut Context<Self>) -> Option<gpui_kit::AnyElement> {
        let theme = Theme::current();
        if let Some(failure) = self.model.failure() {
            let mut note = div()
                .id("composer-note-failed")
                .debug_selector(|| "composer-failed".to_string())
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(SPACE_3))
                .px_3()
                .text_size(px(12.0 * theme.density_scale()))
                .text_color(theme.danger_text)
                .child(failure.to_string());
            // Only a send has words to send again: a failed stop offers no
            // retry, because there is nothing to retry.
            if self.model.can_retry() {
                note = note.child(
                    div()
                        .id("composer-retry")
                        .debug_selector(|| "composer-retry".to_string())
                        .flex()
                        .child(
                            Button::new("composer-retry")
                                .label("Try again")
                                .primary()
                                .min_h(px(TOUCH_TARGET_PX))
                                .min_w(px(TOUCH_TARGET_PX))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.retry(cx);
                                    cx.notify();
                                })),
                        ),
                );
            }
            return Some(note.into_any_element());
        }
        if self.blocked {
            return Some(
                div()
                    .id("composer-note-blocked")
                    .debug_selector(|| "composer-blocked".to_string())
                    .px_3()
                    .text_size(px(12.0 * theme.density_scale()))
                    .text_color(theme.text_dim)
                    .child("Answer the question above to continue.")
                    .into_any_element(),
            );
        }
        if self.model.state().is_pending() {
            return Some(
                div()
                    .id("composer-note-pending")
                    .debug_selector(|| "composer-pending".to_string())
                    .px_3()
                    .text_size(px(12.0 * theme.density_scale()))
                    .text_color(theme.text_dim)
                    .child("Sending…")
                    .into_any_element(),
            );
        }
        None
    }
}

impl Render for Composer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current();
        let draft = self.draft_text();
        let placeholder = if self.blocked {
            self.answer_hint
                .clone()
                .unwrap_or_else(|| "Waiting for your answer…".to_string())
        } else {
            "Message the agent…".to_string()
        };

        let input = div()
            .id("composer-input")
            .debug_selector(|| "composer-input".to_string())
            .when(self.focus_handle.is_focused(window), |el| {
                el.focus_ring_style(window, cx)
            })
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .gap(px(theme.space_1()))
            .w_full()
            .min_h(px(50.0))
            .px(px(theme.space_3()))
            .py(px(theme.space_2()))
            .font_family("JetBrains Mono")
            .text_size(px(theme.font_ui_body()))
            .text_color(if self.blocked || draft.is_empty() {
                theme.token_text_dim()
            } else {
                theme.text_primary()
            })
            .on_key_down(cx.listener(|this, ev, _, cx| {
                this.on_key(ev, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    // While the agent waits for an answer the card owns the
                    // keyboard: the box must not take the focus away from it.
                    if !this.blocked {
                        this.focus_handle.focus(window, cx);
                    }
                }),
            )
            .child(if draft.is_empty() {
                placeholder.to_string()
            } else {
                draft
            });

        let can_send = self.model.can_send() && !self.blocked;
        let can_stop = self.running && self.model.can_stop() && !self.blocked;

        // The wrapper stays sized so the click tests can always find it; only
        // the icon button inside comes and goes with the state it belongs to.
        let mut send = div()
            .id("composer-send")
            .debug_selector(|| "composer-send".to_string())
            .flex()
            .items_center()
            .justify_center()
            .w(px(TOUCH_TARGET_PX))
            .h(px(TOUCH_TARGET_PX));
        if can_send {
            send = send.child(
                Button::new("composer-send")
                    .icon(Icon::new(IconName::ArrowUp).size(theme.scaled_icon_size()))
                    .small()
                    .primary()
                    .h(px(TOUCH_TARGET_PX))
                    .min_h(px(TOUCH_TARGET_PX))
                    .min_w(px(TOUCH_TARGET_PX))
                    .accessibility_label("Send")
                    .tooltip("Send")
                    .disabled(!can_send)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.send(cx);
                        cx.notify();
                    })),
            );
        }

        let mut stop = div()
            .id("composer-stop")
            .debug_selector(|| "composer-stop".to_string())
            .flex()
            .items_center()
            .justify_center()
            .w(px(TOUCH_TARGET_PX))
            .h(px(TOUCH_TARGET_PX));
        if can_stop {
            stop = stop.child(
                Button::new("composer-stop")
                    .icon(Icon::new(IconName::Square))
                    .small()
                    .secondary()
                    .h(px(TOUCH_TARGET_PX))
                    .min_h(px(TOUCH_TARGET_PX))
                    .min_w(px(TOUCH_TARGET_PX))
                    .accessibility_label("Stop")
                    .tooltip("Stop")
                    .disabled(!can_stop)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.stop(cx);
                        cx.notify();
                    })),
            );
        }

        let mut column = div()
            .id("chat-composer")
            .debug_selector(|| "chat-composer".to_string())
            .flex()
            .flex_col()
            .gap(px(theme.space_2()))
            .w_full()
            .max_w(px(CONTENT_W))
            .px(px(theme.space_3()))
            .pb(px(theme.space_2()))
            .mb_3();

        column = column.child(div().w_full().h(px(1.0)).bg(theme.border_subtle()));
        column = column.child(input);
        if let Some(note) = self.render_note(cx) {
            column = column.child(note);
        }
        column = column.child(
            div()
                .id("composer-actions")
                .debug_selector(|| "composer-actions".to_string())
                .flex()
                .flex_row()
                .items_center()
                .justify_end()
                .gap(px(theme.space_2()))
                .child(stop)
                .child(send),
        );
        column.child(div().w_full().h(px(1.0)).bg(theme.border_subtle()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[::core::prelude::v1::test]
    fn an_empty_draft_cannot_be_sent() {
        let mut model = ComposerModel::new();
        assert!(!model.can_send());
        model.type_text("   ");
        assert!(!model.can_send(), "a draft of spaces is not a message");
        model.type_text("hello");
        assert!(model.can_send());
    }

    #[::core::prelude::v1::test]
    fn a_send_leaves_the_draft_and_goes_on_its_way() {
        let mut model = ComposerModel::new();
        model.type_text("  hello  ");
        let message = model.begin_send().expect("the draft has words");
        assert_eq!(message, "hello", "the message is trimmed");
        assert_eq!(model.draft(), "", "the draft is empty again");
        assert!(model.state().is_pending());
        assert!(!model.can_send(), "a send on its way cannot be sent twice");
        assert_eq!(model.sending(), Some("hello"));
    }

    #[::core::prelude::v1::test]
    fn a_failed_send_keeps_the_words_and_offers_a_retry() {
        let mut model = ComposerModel::new();
        model.type_text("hello");
        model.begin_send();
        model.failed("Couldn't send the message. Try again.".to_string());
        assert!(model.state().is_failed());
        assert_eq!(
            model.failure(),
            Some("Couldn't send the message. Try again.")
        );
        assert_eq!(
            model.retry(),
            Some("hello".to_string()),
            "a retry sends the same words"
        );
        assert!(model.state().is_pending());
    }

    #[::core::prelude::v1::test]
    fn a_retry_without_a_failed_send_does_nothing() {
        let mut model = ComposerModel::new();
        assert_eq!(model.retry(), None);
        model.type_text("hello");
        model.begin_send();
        assert_eq!(
            model.retry(),
            None,
            "a send on its way is not a failed send"
        );
    }

    #[::core::prelude::v1::test]
    fn enter_sends_and_shift_enter_writes_the_next_line() {
        let mut text = String::new();
        assert_eq!(
            apply_composer_key(&mut text, "enter", None, false, false),
            ComposerKey::Send
        );
        assert_eq!(
            apply_composer_key(&mut text, "enter", None, true, false),
            ComposerKey::Changed
        );
        assert_eq!(text, "\n");
        assert_eq!(
            apply_composer_key(&mut text, "escape", None, false, false),
            ComposerKey::Stop
        );
    }

    #[::core::prelude::v1::test]
    fn the_composer_types_backspaces_and_ignores_chords() {
        let mut text = String::new();
        assert_eq!(
            apply_composer_key(&mut text, "a", None, false, false),
            ComposerKey::Changed
        );
        assert_eq!(
            apply_composer_key(&mut text, "1", None, false, false),
            ComposerKey::Changed
        );
        assert_eq!(text, "a1");
        assert_eq!(
            apply_composer_key(&mut text, "backspace", None, false, false),
            ComposerKey::Changed
        );
        assert_eq!(text, "a");
        assert_eq!(
            apply_composer_key(&mut text, "v", None, false, true),
            ComposerKey::Ignored,
            "ctrl+v is a shortcut, not text"
        );
        assert_eq!(text, "a");
    }

    #[::core::prelude::v1::test]
    fn the_composer_types_the_character_the_key_produced() {
        let mut text = String::new();
        // The platform reports the character a shifted key produced.
        assert_eq!(
            apply_composer_key(&mut text, "h", Some("H"), true, false),
            ComposerKey::Changed
        );
        assert_eq!(text, "H");

        // A platform that reports no character falls back to the key name.
        let mut text = String::new();
        assert_eq!(
            apply_composer_key(&mut text, "h", None, true, false),
            ComposerKey::Changed
        );
        assert_eq!(text, "H", "a shifted letter is a capital");

        // Space is a word of its own.
        let mut text = String::new();
        assert_eq!(
            apply_composer_key(&mut text, "space", None, false, false),
            ComposerKey::Changed
        );
        assert_eq!(text, " ");
    }

    #[::core::prelude::v1::test]
    fn a_send_that_reaches_the_agent_settles_the_box() {
        let mut model = ComposerModel::new();
        model.type_text("hello");
        model.begin_send().expect("the draft has words");
        settle_send(&mut model, Ok(()));
        assert!(!model.state().is_pending(), "the agent has the message");
        assert_eq!(model.sending(), None);
    }

    #[::core::prelude::v1::test]
    fn a_failed_send_says_so_and_keeps_the_words() {
        let mut model = ComposerModel::new();
        model.type_text("hello");
        model.begin_send().expect("the draft has words");
        settle_send(&mut model, Err("pty closed".to_string()));
        assert!(model.state().is_failed());
        assert_eq!(
            model.failure(),
            Some("Couldn't send the message. Try again.")
        );
        assert_eq!(model.sending(), Some("hello"));
    }

    #[::core::prelude::v1::test]
    fn a_stop_that_goes_through_settles_the_box() {
        let mut model = ComposerModel::new();
        settle_stop(&mut model, Ok(()));
        assert!(!model.state().is_failed());
        assert!(!model.state().is_pending());
    }

    #[::core::prelude::v1::test]
    fn a_stop_that_fails_says_so_in_plain_language() {
        let mut model = ComposerModel::new();
        settle_stop(&mut model, Err("pty closed".to_string()));
        assert_eq!(model.failure(), Some("Couldn't stop the agent."));
    }

    #[::core::prelude::v1::test]
    fn a_failed_send_can_be_tried_again_and_a_failed_stop_cannot() {
        let mut model = ComposerModel::new();
        model.type_text("hello");
        model.begin_send().expect("the draft has words");
        settle_send(&mut model, Err("pty closed".to_string()));
        assert!(model.can_retry(), "the words are still there to send");
        assert_eq!(model.retry(), Some("hello".to_string()));

        let mut model = ComposerModel::new();
        settle_stop(&mut model, Err("pty closed".to_string()));
        assert!(
            !model.can_retry(),
            "a stop has no words to send again: no dead retry is offered"
        );
        assert_eq!(model.retry(), None);
    }

    #[::core::prelude::v1::test]
    fn a_new_agent_takes_the_draft_with_it() {
        let mut model = ComposerModel::new();
        model.type_text("for the first agent");
        model.reset();
        assert_eq!(model.draft(), "");
        assert!(!model.state().is_pending());
    }

    crate::orion_test!(
        fn the_composer_input_is_a_keyboard_tab_stop(cx: &mut gpui_kit::TestAppContext) {
            let composer = cx.update(|app| app.new(Composer::new));
            cx.update(|app| {
                assert!(
                    composer.read(app).focus_handle().tab_stop,
                    "the composer input must be reachable by Tab"
                );
            });
        }
    );
}
