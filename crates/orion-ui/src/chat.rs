//! Chat lens view: the Engine's Transcript as a conversation.
//!
//! `ChatView` polls `Engine::http("GET", "/api/pane/conversation?pane_id=…")`
//! on a 2s timer (the wire seam — `engine.rs` untouched) and rerenders when the
//! body changes. What the transcript means for the surface — which phase the
//! column is in, which thinking rows are open, whether new turns still follow
//! the bottom — lives in [`crate::chat_model`], so the rules are testable
//! without a window and this file only draws.
//!
//! Rendering is a plain scrollable column with a recent-N cap (last 200
//! turns): GPUI virtualization is deliberately not used; the cap mirrors the
//! terminal scrollback bound and keeps oversized tool outputs truncated with a
//! "View full in Terminal" control (see [`crate::tool_card::clipped_output`]).
//! A [`PromptCard`] sits at the top whenever `pane.prompt()` is `Some`.
//!
//! The chat asks the shell for two things through [`ChatEvent`]: the Terminal
//! Lens, which has the whole output, and a file, which the Changes Lens
//! opens.
//!
//! The terminal grid never becomes a turn: the chat reads the agent's own
//! transcript, and an agent that keeps none shows the empty state, not its
//! screen.

use crate::chat_entities::{
    cap_turns, ChatPart, ChatPrompt, ChatRole, ChatTurn, ConversationResponse,
};
use crate::chat_model::{at_bottom, format_timestamp, ChatModel};
use crate::markdown::{
    block_style, list_item_parts, opens_in_browser, parse_markdown, spans, BlockKind, MarkdownLine,
    TextSpan,
};
use crate::prompt_card::PromptCard;
use crate::theme::{Theme, CHAT_INLINE_CODE_RATIO, CHAT_LINE_HEIGHT};
use crate::tool_card::{
    clipped_output, lines_with_paths, part_card, tool_object, tool_verb, DiffLine, PartCard,
    ToolCard, ToolStatus, MAX_FILE_LINKS,
};
use gpui::prelude::*;
use gpui::{
    div, px, Context, Entity, FontStyle, FontWeight, IntoElement, Render, ScrollHandle,
    ScrollWheelEvent, StyledText, TextStyle, Window,
};
use orion_core::engine::pane::Pane;
use orion_core::Engine;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

/// Recent-turn cap: chat renders the newest `MAX_RECENT_TURNS` turns only.
pub const MAX_RECENT_TURNS: usize = 200;
/// What the chat asks the shell for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatEvent {
    /// The reader wants the whole output: the Terminal Lens has it.
    ShowTerminal,
    /// The reader clicked a file path.
    OpenFile(String),
}

impl gpui::EventEmitter<ChatEvent> for ChatView {}

pub struct ChatView {
    engine: Option<Arc<Engine>>,
    pane: Option<Arc<Pane>>,
    pane_id: String,
    /// What the transcript column shows: phase, turns, thinking rows, follow.
    model: ChatModel,
    last_body: Option<String>,
    prompt_card: Option<Entity<PromptCard>>,
    scroll_handle: ScrollHandle,
    focus_handle: gpui::FocusHandle,
    row_seq: usize,
}

/// The message a failed read shows: one sentence, and the Retry beside it
/// does the rest (DESIGN.md §1). Never a status code, never a raw body.
fn conversation_error_message(status: u16) -> String {
    if status == 404 {
        // The pane is gone: no retry will bring its transcript back.
        return "This pane is no longer open.".to_string();
    }
    "Couldn't load the conversation.".to_string()
}

/// The message a transcript the shell cannot read shows.
fn parse_error_message(_error: &str) -> String {
    "Couldn't read the conversation.".to_string()
}

/// The transcript's source in plain language: the wire names are internals.
fn source_label(source: &str) -> &'static str {
    match source {
        // The agent keeps no transcript: the chat reads its live output.
        "scrollback" => "Live output",
        _ => "Transcript",
    }
}

impl ChatView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let scroll_handle = ScrollHandle::new();
        let view = Self {
            engine: None,
            pane: None,
            pane_id: "pane-1".to_string(),
            model: ChatModel::new(),
            last_body: None,
            prompt_card: None,
            scroll_handle,
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
        // A new pane is a new transcript: neither the old answer nor the old
        // thinking rows may linger.
        self.last_body = None;
        self.model.reset();
        self.refresh();
    }

    pub fn set_pane(&mut self, pane: Arc<Pane>) {
        self.pane = Some(pane);
        self.refresh();
    }

    /// The transcript the pane's agent wrote, as the chat draws it.
    pub fn model(&self) -> &ChatModel {
        &self.model
    }

    /// A read answered: the turns are on screen.
    pub fn apply_conversation(&mut self, response: ConversationResponse) {
        self.model.apply(response);
    }

    /// A read failed: the message is plain language and a Retry is offered.
    pub fn fail_conversation(&mut self, message: String) {
        self.model.fail(message);
    }

    /// The reader asked for the transcript again.
    pub fn retry(&mut self) {
        self.model.retry();
        self.last_body = None;
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
            // Forget the last good answer: the next good answer must show again
            // even when it is the same body as before.
            self.last_body = None;
            self.fail_conversation(conversation_error_message(status));
            return;
        }
        let encoded = body.to_string();
        if self.last_body.as_deref() == Some(encoded.as_str()) {
            return;
        }
        self.last_body = Some(encoded);
        match ConversationResponse::parse(&body) {
            Ok(response) => self.apply_conversation(response),
            Err(error) => self.fail_conversation(parse_error_message(&error)),
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

    /// The agent whose transcript this is, for the role label.
    fn agent_label(&self) -> String {
        self.pane
            .as_ref()
            .map(|pane| pane.agent.display_name().to_string())
            .unwrap_or_else(|| "Agent".to_string())
    }
}

/// The bound on the output a tool card shows, and the marker that says where
/// the rest is.
pub use crate::tool_card::MAX_TOOL_CHARS;

/// The text style of one span: the Quiet Slate type scale, plus the span's own
/// character (inline code, a link, bold, italic).
fn span_style(base: &TextStyle, span: &TextSpan, theme: Theme, inline_code_size: f32) -> TextStyle {
    let mut style = base.clone();
    if span.code {
        style.font_family = "JetBrains Mono".into();
        // Inline code is `0.87em` of its line (DESIGN.md §Faces).
        style.font_size = px(inline_code_size).into();
        style.background_color = Some(theme.surface_hover.into());
    }
    if span.link && span.target.as_deref().is_some_and(opens_in_browser) {
        style.color = theme.accent.into();
        style.underline = Some(gpui::UnderlineStyle {
            thickness: px(1.0),
            ..Default::default()
        });
    }
    if span.bold {
        style.font_weight = FontWeight::BOLD;
    }
    if span.italic {
        style.font_style = FontStyle::Italic;
    }
    style
}

/// One markdown line as a row of the Quiet Slate type scale.
fn render_markdown_line(
    line: &MarkdownLine,
    theme: Theme,
    row_seq: &mut usize,
) -> impl IntoElement {
    let style = block_style(&line.style);
    let scale = theme.density_scale();
    // The reading scale (DESIGN.md §Scale): prose on `--fs-chat`, a code
    // block on `--fs-sm`, headings on `--fs-xl`/`--fs-lg`.
    let size = match &line.style {
        BlockKind::Heading(level) => theme.chat_heading_size(*level),
        BlockKind::Code => theme.chat_code_size(),
        _ => theme.chat_prose_size(),
    };
    let mut base = TextStyle {
        color: crate::theme::hsla(if style.quote {
            theme.text_dim
        } else if style.bold {
            theme.text_strong
        } else {
            theme.text
        }),
        font_size: px(size).into(),
        line_height: gpui::relative(CHAT_LINE_HEIGHT),
        ..Default::default()
    };
    if style.bold {
        base.font_weight = FontWeight::BOLD;
    }
    if style.mono {
        base.font_family = "JetBrains Mono".into();
    }

    // A list item draws its own bullet; the marker is not part of the prose.
    let (text, shift) = if line.style.list_depth().is_some() {
        let (body, shift) = list_item_parts(&line.text);
        (body.to_string(), shift)
    } else {
        (line.text.clone(), 0)
    };

    let selector = match line.style {
        BlockKind::Heading(_) => "chat-heading",
        BlockKind::ListItem(_) => "chat-list-item",
        _ => "",
    }
    .to_string();
    let mut row = div()
        .flex()
        .flex_row()
        .items_start()
        .debug_selector(move || selector.clone());
    if style.indent > 0.0 {
        row = row.pl(px(style.indent * scale));
    }
    if style.quote {
        row = row
            .border_l_2()
            .border_color(theme.border_strong)
            .pl(px(style.indent * scale + 8.0));
    }
    if line.style.list_depth().is_some() {
        row = row.child(
            div()
                .text_color(theme.text_dim)
                .text_size(px(size))
                .child("• "),
        );
    }

    // The spans are the line's own runs: inline code, links, bold, italic.
    for span in spans(line) {
        let start = span.range.start.saturating_sub(shift);
        let end = span.range.end.saturating_sub(shift);
        let Some(slice) = text.get(start..end) else {
            continue;
        };
        if slice.is_empty() {
            continue;
        }
        let span_style = span_style(&base, &span, theme, size * CHAT_INLINE_CODE_RATIO);
        *row_seq += 1;
        let mut element = div().id(*row_seq).child(
            StyledText::new(slice.to_string()).with_default_highlights(&span_style, Vec::new()),
        );
        // A web link opens in the browser; anything else stays in the app.
        if let Some(target) = span.target.as_deref() {
            if opens_in_browser(target) {
                let target = target.to_string();
                element = element
                    .debug_selector(|| "chat-link".to_string())
                    .on_click(move |_, _window, app| app.open_url(&target));
            }
        }
        row = row.child(element);
    }
    row
}

/// A fenced code block: one quiet panel, mono lines, no syntax colours.
fn render_code_block(lines: &[MarkdownLine], theme: Theme) -> impl IntoElement {
    let text_style = TextStyle {
        color: crate::theme::hsla(theme.text),
        font_size: px(theme.chat_code_size()).into(),
        font_family: "JetBrains Mono".into(),
        line_height: gpui::relative(CHAT_LINE_HEIGHT),
        ..Default::default()
    };
    let mut block = div()
        .flex()
        .flex_col()
        .rounded_md()
        .bg(theme.surface_elevated)
        .px_2()
        .py_1()
        .debug_selector(|| "chat-code-block".to_string());
    for line in lines {
        block = block.child(
            StyledText::new(line.text.clone()).with_default_highlights(&text_style, Vec::new()),
        );
    }
    block
}

/// A horizontal rule: a quiet line, not three dashes.
fn render_rule(theme: Theme) -> impl IntoElement {
    div().h(px(1.0)).w_full().mt_1().mb_1().bg(theme.border)
}

/// Markdown prose, in the Quiet Slate type scale.
fn render_text_block(text: &str, theme: Theme, row_seq: &mut usize) -> Vec<gpui::AnyElement> {
    let lines = parse_markdown(text);
    let mut blocks: Vec<gpui::AnyElement> = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let line = &lines[index];
        if line.style.is_code() {
            // A fenced block is one panel, not one panel per line.
            let start = index;
            while index < lines.len() && lines[index].style.is_code() {
                index += 1;
            }
            blocks.push(render_code_block(&lines[start..index], theme).into_any_element());
            continue;
        }
        if line.style.is_rule() {
            blocks.push(render_rule(theme).into_any_element());
        } else {
            blocks.push(render_markdown_line(line, theme, row_seq).into_any_element());
        }
        index += 1;
    }
    blocks
}

impl Render for ChatView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.row_seq = 0;
        let theme = Theme::current();

        // Where the reader is: the follow rule follows the scroll position,
        // so a scrollbar drag pins the view just like the wheel does.
        let offset = f32::from(self.scroll_handle.offset().y);
        let max_offset = f32::from(self.scroll_handle.max_offset().height);
        self.model.sync_scroll(offset, max_offset);

        let chat = div()
            .id("chat-view")
            .debug_selector(|| "chat-view".to_string())
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.surface_base)
            .text_color(theme.text)
            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, window, cx| {
                let delta = f32::from(ev.delta.pixel_delta(window.line_height()).y);
                let offset = f32::from(this.scroll_handle.offset().y);
                let max_offset = f32::from(this.scroll_handle.max_offset().height);
                // Scrolling up pins the view; the scroll position itself says
                // when the reader is back at the bottom.
                this.model.on_scroll(delta, at_bottom(offset, max_offset));
                cx.notify();
            }));

        let mut root = chat.child(self.render_status_bar());

        // A failed read says so in plain language and offers a Retry.
        if let Some(message) = self.model.failure().map(str::to_string) {
            root = root.child(self.render_failure(&message, cx));
        }

        let content: gpui::AnyElement = if self.model.is_loading() {
            self.render_loading()
        } else if self.model.is_empty() {
            self.render_empty()
        } else {
            self.render_turns(cx)
        };

        let list = div()
            .id("chat-scroll")
            .flex()
            .flex_col()
            .flex_1()
            .overflow_y_scroll()
            .track_scroll(&self.scroll_handle)
            .child(content);

        // New turns follow the bottom only while the reader is there.
        if self.model.follows_bottom() {
            self.scroll_handle.scroll_to_bottom();
        }

        root.child(list)
    }
}

impl ChatView {
    fn render_status_bar(&self) -> impl IntoElement {
        let source = if self.model.is_loading() {
            "loading…".to_string()
        } else {
            source_label(self.model.source()).to_string()
        };
        let count = self.model.turns().len();
        let status = self
            .pane
            .as_ref()
            .map(|p| p.status().as_wire().to_string())
            .unwrap_or_default();
        div()
            .id("chat-status")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .h(px(28.0))
            .px_3()
            .text_size(px(11.0 * Theme::current().density_scale()))
            .text_color(Theme::current().text_dim)
            .child(format!("{source} · {count} turns · {status}"))
    }

    /// The loading state: a skeleton of the turns that are on their way.
    fn render_loading(&self) -> gpui::AnyElement {
        let theme = Theme::current();
        let mut column = div()
            .id("chat-loading")
            .debug_selector(|| "chat-loading".to_string())
            .flex()
            .flex_col()
            .gap(px(12.0))
            .p_3();
        for width in [220.0, 320.0, 180.0] {
            column = column.child(
                div()
                    .h(px(12.0))
                    .w(px(width))
                    .rounded_md()
                    .bg(theme.surface_elevated),
            );
        }
        column.into_any_element()
    }

    /// The empty state: the transcript is readable and holds no turns.
    fn render_empty(&self) -> gpui::AnyElement {
        let theme = Theme::current();
        div()
            .id("chat-empty")
            .debug_selector(|| "chat-empty".to_string())
            .flex()
            .flex_col()
            .items_start()
            .gap(px(4.0))
            .p_3()
            .child(
                div()
                    .text_size(px(13.0 * theme.density_scale()))
                    .text_color(theme.text)
                    .child("No conversation yet."),
            )
            .child(
                div()
                    .text_size(px(12.0 * theme.density_scale()))
                    .text_color(theme.text_dim)
                    .child("This agent has not written a transcript. The terminal lens shows its live output."),
            )
            .into_any_element()
    }

    /// The failed state: a plain-language message and a Retry.
    fn render_failure(&self, message: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current();
        div()
            .id("chat-error")
            .debug_selector(|| "chat-error".to_string())
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(px(12.0))
            .px_3()
            .py_2()
            .bg(theme.danger_tint)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .text_size(px(12.0 * theme.density_scale()))
                    .text_color(theme.danger_text)
                    .child(message.to_string()),
            )
            .child(
                div()
                    .id("chat-retry")
                    .debug_selector(|| "chat-retry".to_string())
                    .flex()
                    .items_center()
                    .justify_center()
                    .min_h(px(44.0))
                    .px_3()
                    .rounded_md()
                    .bg(theme.primary)
                    .text_color(theme.primary_text)
                    .text_size(px(12.0 * theme.density_scale()))
                    .child("Try again")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.retry();
                        cx.notify();
                    })),
            )
    }

    fn render_turns(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let turns = cap_turns(self.model.turns().to_vec(), MAX_RECENT_TURNS);
        let total = self.model.turns().len();
        let mut column = div().id("chat-turns").flex().flex_col().gap(px(12.0)).p_3();
        for (offset, turn) in turns.iter().enumerate() {
            let index = total - turns.len() + offset;
            column = column.child(self.render_turn(index, turn, cx));
        }
        column.into_any_element()
    }

    fn render_turn(
        &mut self,
        index: usize,
        turn: &ChatTurn,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = Theme::current();
        let is_user = turn.role == ChatRole::User;
        let mut card = div()
            .id(self.next_row_id())
            .debug_selector(|| "chat-turn".to_string())
            .flex()
            .flex_col()
            .gap(px(6.0))
            .p_3()
            .rounded_md()
            .bg(if is_user {
                theme.surface_elevated
            } else {
                theme.surface_panel
            })
            .border_1()
            .border_color(if is_user {
                theme.border_strong
            } else {
                theme.border
            });

        // The role and the time it happened: a turn without them is a rumour.
        let stamp = format_timestamp(turn.ts.as_deref().or(turn.end_ts.as_deref()));
        let role_label = if is_user {
            "You".to_string()
        } else {
            self.agent_label()
        };
        card = card.child(
            div()
                .id(self.next_row_id())
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .text_size(px(11.0 * theme.density_scale()))
                .text_color(theme.text_dim)
                .child(role_label)
                .child(stamp),
        );

        for (part_index, part) in turn.parts.iter().enumerate() {
            card = card.child(self.render_part(index, part_index, part, cx));
        }
        card
    }

    /// Prose: the markdown of one part, in the Quiet Slate scale.
    fn render_prose(&mut self, text: &str, theme: Theme) -> gpui::AnyElement {
        let mut column = div().id(self.next_row_id()).flex().flex_col();
        let mut row_seq = self.row_seq;
        for line in render_text_block(text, theme, &mut row_seq) {
            column = column.child(line);
        }
        self.row_seq = row_seq;
        column.into_any_element()
    }

    fn render_part(
        &mut self,
        turn_index: usize,
        part_index: usize,
        part: &ChatPart,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = Theme::current();
        match part_card(part) {
            PartCard::Text { text } => self.render_prose(&text, theme),
            PartCard::Thinking { text } => {
                let open = self.model.is_thinking_open(turn_index, part_index);
                let mut folded = div()
                    .id(self.next_row_id())
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .rounded_md()
                    .bg(theme.surface_elevated);
                folded = folded.child(
                    div()
                        .id(self.next_row_id())
                        .debug_selector(|| "chat-thinking".to_string())
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.0))
                        .px_2()
                        .py_1()
                        .min_h(px(44.0))
                        .cursor_pointer()
                        .text_size(px(12.0 * theme.density_scale()))
                        .text_color(theme.text_dim)
                        .child(if open { "\u{25be}" } else { "\u{25b8}" })
                        .child(if open {
                            "Thinking (collapse)"
                        } else {
                            "Thinking (expand)"
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.model.toggle_thinking(turn_index, part_index);
                            cx.notify();
                        })),
                );
                if open {
                    let mut body = div()
                        .id(self.next_row_id())
                        .debug_selector(|| "chat-thinking-body".to_string())
                        .px_2()
                        .py_1();
                    let mut row_seq = self.row_seq;
                    for line in render_text_block(&text, theme, &mut row_seq) {
                        body = body.child(line);
                    }
                    self.row_seq = row_seq;
                    folded = folded.child(body);
                }
                folded.into_any_element()
            }
            PartCard::Tool(card) => self.render_tool_row(turn_index, part_index, &card, &[], cx),
            PartCard::Diff { card, lines } => {
                self.render_tool_row(turn_index, part_index, &card, &lines, cx)
            }
            PartCard::Notice { text, source } => {
                let source = source.unwrap_or_default();
                div()
                    .id(self.next_row_id())
                    .text_size(px(12.0 * theme.density_scale()))
                    .text_color(theme.notice)
                    .child(if source.is_empty() {
                        text
                    } else {
                        format!("[{source}] {text}")
                    })
                    .into_any_element()
            }
            PartCard::Compact { text } => div()
                .id(self.next_row_id())
                .text_size(px(12.0 * theme.density_scale()))
                .text_color(theme.text_dim)
                .child(format!("Compacted: {text}"))
                .into_any_element(),
            PartCard::Other | PartCard::Unknown => self.render_other_part(part, theme),
        }
    }

    /// The parts this shell does not draw as a card of their own: background
    /// tasks, skills, images.
    fn render_other_part(&mut self, part: &ChatPart, theme: Theme) -> gpui::AnyElement {
        match part {
            ChatPart::TaskResult { tasks } => {
                let mut column = div().id(self.next_row_id()).flex().flex_col().gap(px(2.0));
                for task in tasks {
                    column = column.child(
                        div()
                            .id(self.next_row_id())
                            .text_size(px(12.0 * theme.density_scale()))
                            .child(format!(
                                "Task {} [{}]: {}",
                                task.title, task.status, task.result
                            )),
                    );
                }
                column.into_any_element()
            }
            ChatPart::Skill { skill } => div()
                .id(self.next_row_id())
                .text_size(px(12.0 * theme.density_scale()))
                .text_color(theme.text_dim)
                .child(format!(
                    "Skill {} ({}/{})",
                    skill.name, skill.evidence, skill.status
                ))
                .into_any_element(),
            ChatPart::Image {
                media_type,
                image_ref,
            } => div()
                .id(self.next_row_id())
                .text_size(px(12.0 * theme.density_scale()))
                .child(format!("Image ({media_type}) \u{2014} {image_ref}"))
                .into_any_element(),
            _ => div()
                .id(self.next_row_id())
                .text_size(px(12.0 * theme.density_scale()))
                .text_color(theme.text_dim)
                .child("(unsupported part)")
                .into_any_element(),
        }
    }

    /// A tool call: a row that folds away, with its work inside.
    fn render_tool_row(
        &mut self,
        turn_index: usize,
        part_index: usize,
        card: &ToolCard,
        lines: &[DiffLine],
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = Theme::current();
        let open = self.model.is_tool_open(turn_index, part_index);
        let failed = card.status == ToolStatus::Failed;

        let mut row = div()
            .id(self.next_row_id())
            .debug_selector(|| "chat-tool".to_string())
            .flex()
            .flex_col()
            .gap(px(4.0))
            .rounded_md()
            .bg(theme.surface_panel)
            .p_2();

        // The header: caret, verb, object (DESIGN.md §5).
        let verb = tool_verb(&card.name);
        let object = tool_object(&card.summary, &card.name);
        row = row.child(
            div()
                .id(self.next_row_id())
                .debug_selector(|| "chat-tool-header".to_string())
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .min_h(px(44.0))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.model.toggle_tool(turn_index, part_index);
                    cx.notify();
                }))
                .child(div().text_color(theme.text_dim).child(if open {
                    "\u{25be}"
                } else {
                    "\u{25b8}"
                }))
                .child(
                    div()
                        .text_size(px(13.0 * theme.density_scale()))
                        .text_color(if failed {
                            theme.status_blocked
                        } else {
                            theme.text_dim
                        })
                        .child(match verb {
                            Some(verb) => verb.to_string(),
                            None => card.name.clone(),
                        }),
                )
                .child(
                    div()
                        .text_size(px(11.0 * theme.density_scale()))
                        .font_family("JetBrains Mono")
                        .text_color(if failed {
                            theme.status_blocked
                        } else {
                            theme.text
                        })
                        .child(object),
                )
                .child(
                    div()
                        .text_size(px(11.0 * theme.density_scale()))
                        .text_color(theme.status_blocked)
                        .child(if failed { "failed" } else { "" }),
                ),
        );

        if open {
            if !card.input.is_empty() {
                let (input, clipped) = clipped_output(&card.input);
                row = row.child(self.render_tool_text(&input, theme, cx));
                if clipped {
                    row = row.child(self.render_terminal_path(cx));
                }
            }
            if card.arguments_omitted {
                row = row.child(
                    div()
                        .id(self.next_row_id())
                        .text_size(px(11.0 * theme.density_scale()))
                        .text_color(theme.text_dim)
                        .child("Arguments omitted (too large for the transcript page)."),
                );
            }
            if !lines.is_empty() {
                row = row.child(self.render_diff(lines, theme));
            } else {
                let (output, clipped) = clipped_output(&card.output);
                if !output.is_empty() {
                    row = row.child(self.render_tool_text(&output, theme, cx));
                }
                if clipped {
                    // The rest of the output is in the Terminal Lens.
                    row = row.child(self.render_terminal_path(cx));
                }
            }
            if failed && card.output.is_empty() {
                row = row.child(
                    div()
                        .id(self.next_row_id())
                        .text_size(px(11.0 * theme.density_scale()))
                        .text_color(theme.status_blocked)
                        .child("This tool call failed."),
                );
            }
        }
        row.into_any_element()
    }

    /// A tool's input or output: mono, line by line, with its file paths as
    /// controls inside the line they sit on.
    fn render_tool_text(
        &mut self,
        text: &str,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let text_style = TextStyle {
            color: crate::theme::hsla(theme.text),
            font_size: px(theme.chat_code_size()).into(),
            font_family: "JetBrains Mono".into(),
            ..Default::default()
        };
        let mut block = div()
            .id(self.next_row_id())
            .debug_selector(|| "chat-tool-output".to_string())
            .flex()
            .flex_col()
            .rounded_md()
            .bg(theme.surface_elevated)
            .px_2()
            .py_1()
            // A long line stays inside the panel; the whole output is in the
            // Terminal Lens.
            .overflow_x_hidden();

        for line in lines_with_paths(text) {
            let mut row = div()
                .id(self.next_row_id())
                .flex()
                .flex_row()
                .w_full()
                // A long line stays inside the panel.
                .overflow_hidden();
            let mut cursor = 0;
            for (range, path) in line.paths.iter().take(MAX_FILE_LINKS) {
                if range.start > cursor {
                    if let Some(before) = line.text.get(cursor..range.start) {
                        row = row.child(
                            StyledText::new(before.to_string())
                                .with_default_highlights(&text_style, Vec::new()),
                        );
                    }
                }
                // The path is a control: it opens the file.
                let target = path.clone();
                row = row.child(
                    div()
                        .id(self.next_row_id())
                        .debug_selector(|| "chat-file-path".to_string())
                        .text_color(theme.accent)
                        .underline()
                        .cursor_pointer()
                        .child(path.clone())
                        .on_click(cx.listener(move |_this, _, _, cx| {
                            cx.emit(ChatEvent::OpenFile(target.clone()));
                            cx.notify();
                        })),
                );
                cursor = range.end;
            }
            if let Some(rest) = line.text.get(cursor..) {
                if !rest.is_empty() {
                    row = row.child(
                        StyledText::new(rest.to_string())
                            .with_default_highlights(&text_style, Vec::new()),
                    );
                }
            }
            block = block.child(row);
        }
        block.into_any_element()
    }

    /// An edit diff: the added lines in `--syntax-inserted`, the deleted ones
    /// in `--syntax-deleted`, with the context between them.
    fn render_diff(&mut self, lines: &[DiffLine], theme: Theme) -> gpui::AnyElement {
        let mut block = div()
            .id(self.next_row_id())
            .debug_selector(|| "chat-diff".to_string())
            .flex()
            .flex_col()
            .rounded_md()
            .bg(theme.surface_elevated)
            .px_2()
            .py_1();
        for line in lines {
            let color = match line {
                DiffLine::Added(_) => theme.status_done,
                DiffLine::Removed(_) => theme.status_blocked,
                DiffLine::Context(_) => theme.text_dim,
            };
            let text_style = TextStyle {
                color: crate::theme::hsla(color),
                font_size: px(theme.chat_code_size()).into(),
                font_family: "JetBrains Mono".into(),
                ..Default::default()
            };
            block = block.child(
                div()
                    .id(self.next_row_id())
                    .flex()
                    .flex_row()
                    .gap(px(6.0))
                    .child(
                        StyledText::new(line.sign().to_string())
                            .with_default_highlights(&text_style, Vec::new()),
                    )
                    .child(
                        StyledText::new(line.text().to_string())
                            .with_default_highlights(&text_style, Vec::new()),
                    ),
            );
        }
        block.into_any_element()
    }

    /// The path to the whole output: the Terminal Lens has it.
    fn render_terminal_path(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = Theme::current();
        div()
            .id(self.next_row_id())
            .debug_selector(|| "chat-terminal-path".to_string())
            .flex()
            .flex_row()
            .items_center()
            .min_h(px(44.0))
            .px_2()
            .text_size(px(11.0 * theme.density_scale()))
            .text_color(theme.accent)
            .cursor_pointer()
            .child("View full in Terminal")
            .on_click(cx.listener(|_this, _, _, cx| {
                cx.emit(ChatEvent::ShowTerminal);
                cx.notify();
            }))
            .into_any_element()
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_model::ChatPhase;
    use gpui::Window;
    use serde_json::json;

    /// The window's root view: the chat the test drives.
    struct ChatRoot {
        view: Entity<ChatView>,
    }

    impl Render for ChatRoot {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            self.view.clone()
        }
    }

    fn response(value: Value) -> ConversationResponse {
        ConversationResponse::parse(&value).expect("the wire shape parses")
    }

    #[test]
    fn conversation_fetch_failure_is_plain_language_with_retry() {
        let message = conversation_error_message(500);
        assert_eq!(message, "Couldn't load the conversation.");
        assert!(
            !message.contains("500"),
            "a status code is not something the reader needs"
        );
        assert!(
            !message.contains("Try again"),
            "the Retry button says it; the sentence need not repeat it"
        );
    }

    #[test]
    fn a_pane_that_is_gone_says_so() {
        assert_eq!(
            conversation_error_message(404),
            "This pane is no longer open.",
            "a retry will not bring a closed pane back"
        );
    }

    #[gpui::test]
    fn the_chat_shows_its_surface_turn_rows_and_error_state(cx: &mut gpui::TestAppContext) {
        use gpui::{point, size};
        let cx = cx.add_empty_window();
        let view = cx.update(|_window, app| app.new(ChatView::new));

        // A real transcript: a user turn and an agent turn.
        cx.draw(point(px(0.), px(0.)), size(px(900.), px(700.)), |_, cx| {
            view.update(cx, |view, _cx| {
                view.apply_conversation(response(json!({
                    "source": "claude-transcript",
                    "turns": [
                        { "role": "user", "ts": "2026-01-01T00:00:00Z",
                          "parts": [{ "kind": "text", "text": "hello" }] },
                        { "role": "assistant", "ts": "2026-01-01T00:00:05Z",
                          "parts": [{ "kind": "text", "text": "hi" }] },
                    ],
                })));
            });
            view.clone()
        });
        assert!(
            cx.debug_bounds("chat-view").is_some(),
            "the chat surface is on screen"
        );
        assert!(
            cx.debug_bounds("chat-turn").is_some(),
            "the turns are on screen"
        );
        assert!(
            cx.debug_bounds("chat-error").is_none(),
            "a good read shows no failure"
        );

        // A failed read: a plain-language message and a Retry.
        cx.draw(point(px(0.), px(0.)), size(px(900.), px(700.)), |_, cx| {
            view.update(cx, |view, _cx| {
                view.fail_conversation("Couldn't load the conversation. Try again.".to_string());
            });
            view.clone()
        });
        assert!(
            cx.debug_bounds("chat-error").is_some(),
            "the failure is on screen"
        );
        assert!(
            cx.debug_bounds("chat-retry").is_some(),
            "a Retry is offered"
        );
    }

    #[gpui::test]
    fn the_chat_draws_markdown_in_the_quiet_slate_scale(cx: &mut gpui::TestAppContext) {
        use gpui::{point, size};
        let cx = cx.add_empty_window();
        let view = cx.update(|_window, app| app.new(ChatView::new));
        cx.draw(point(px(0.), px(0.)), size(px(900.), px(700.)), |_, cx| {
            view.update(cx, |view, _cx| {
                view.apply_conversation(response(json!({
                    "source": "claude-transcript",
                    "turns": [
                        { "role": "assistant", "ts": "2026-01-01T00:00:00Z",
                          "parts": [{ "kind": "text", "text":
                            "# Title

- one
- two

Inline `code` and [docs](https://example.com).

```
let x = 1;
```
"
                          }] },
                    ],
                })));
            });
            view.clone()
        });
        assert!(
            cx.debug_bounds("chat-heading").is_some(),
            "a heading is drawn"
        );
        assert!(
            cx.debug_bounds("chat-list-item").is_some(),
            "a list item is drawn"
        );
        assert!(
            cx.debug_bounds("chat-code-block").is_some(),
            "a fenced code block is drawn"
        );
        assert!(cx.debug_bounds("chat-link").is_some(), "a link is drawn");
    }

    #[gpui::test]
    fn a_thinking_row_opens_and_closes_on_a_click(cx: &mut gpui::TestAppContext) {
        let (root, cx) = cx.add_window_view(|_, cx| ChatRoot {
            view: cx.new(ChatView::new),
        });
        cx.update(|_window, app| {
            let chat = root.read(app).view.clone();
            chat.update(app, |view, _cx| {
                view.apply_conversation(response(json!({
                    "source": "claude-transcript",
                    "turns": [
                        { "role": "assistant", "ts": "2026-01-01T00:00:00Z",
                          "parts": [{ "kind": "thinking", "text": "hmm" }] },
                    ],
                })));
            });
        });
        cx.update(|window, cx| window.draw(cx).clear());
        assert!(
            cx.debug_bounds("chat-thinking").is_some(),
            "the thinking row is drawn"
        );
        assert!(
            cx.debug_bounds("chat-thinking-body").is_none(),
            "a thinking row starts closed"
        );

        let bounds = cx.debug_bounds("chat-thinking").expect("the thinking row");
        cx.simulate_click(bounds.center(), gpui::Modifiers::none());
        cx.update(|_window, app| {
            let chat = root.read(app).view.clone();
            assert!(
                chat.read(app).model().is_thinking_open(0, 0),
                "the click opened the thinking row"
            );
        });
        cx.update(|window, cx| window.draw(cx).clear());
        assert!(
            cx.debug_bounds("chat-thinking-body").is_some(),
            "the thinking row shows its body"
        );

        let bounds = cx.debug_bounds("chat-thinking").expect("the thinking row");
        cx.simulate_click(bounds.center(), gpui::Modifiers::none());
        cx.update(|_window, app| {
            let chat = root.read(app).view.clone();
            assert!(
                !chat.read(app).model().is_thinking_open(0, 0),
                "the second click closed the thinking row"
            );
        });
    }

    #[gpui::test]
    fn a_retry_recovers_the_conversation_without_a_restart(cx: &mut gpui::TestAppContext) {
        let (root, cx) = cx.add_window_view(|_, cx| ChatRoot {
            view: cx.new(ChatView::new),
        });
        cx.update(|_window, app| {
            let chat = root.read(app).view.clone();
            chat.update(app, |view, _cx| {
                view.fail_conversation("Couldn't load the conversation. Try again.".to_string());
            });
        });
        cx.update(|window, cx| window.draw(cx).clear());
        assert!(
            cx.debug_bounds("chat-error").is_some(),
            "the failure is on screen"
        );
        assert!(
            cx.debug_bounds("chat-retry").is_some(),
            "a Retry is offered"
        );

        let bounds = cx.debug_bounds("chat-retry").expect("the Retry");
        cx.simulate_click(bounds.center(), gpui::Modifiers::none());
        cx.update(|_window, app| {
            let chat = root.read(app).view.clone();
            assert_eq!(
                chat.read(app).model().failure(),
                None,
                "the Retry cleared the failure"
            );
            assert_eq!(chat.read(app).model().phase(), &ChatPhase::Loading);
        });

        // The next good answer fills the column again.
        cx.update(|_window, app| {
            let chat = root.read(app).view.clone();
            chat.update(app, |view, _cx| {
                view.apply_conversation(response(json!({
                    "source": "claude-transcript",
                    "turns": [
                        { "role": "user", "ts": "2026-01-01T00:00:00Z",
                          "parts": [{ "kind": "text", "text": "hello" }] },
                    ],
                })));
            });
        });
        cx.update(|window, cx| window.draw(cx).clear());
        assert!(
            cx.debug_bounds("chat-turn").is_some(),
            "the recovered transcript is on screen"
        );
    }

    #[gpui::test]
    fn scrolling_up_pins_the_view(cx: &mut gpui::TestAppContext) {
        let (root, cx) = cx.add_window_view(|_, cx| ChatRoot {
            view: cx.new(ChatView::new),
        });
        // A long transcript, so the column really scrolls.
        let turns: Vec<Value> = (0..80)
            .map(|index| {
                json!({
                    "role": "user",
                    "ts": "2026-01-01T00:00:00Z",
                    "parts": [{ "kind": "text", "text": format!("turn {index}") }],
                })
            })
            .collect();
        cx.update(|_window, app| {
            let chat = root.read(app).view.clone();
            chat.update(app, |view, _cx| {
                view.apply_conversation(response(json!({
                    "source": "claude-transcript",
                    "turns": turns,
                })));
            });
        });
        cx.update(|window, cx| window.draw(cx).clear());
        cx.update(|_window, app| {
            let chat = root.read(app).view.clone();
            assert!(
                chat.read(app).model().follows_bottom(),
                "a fresh chat follows the bottom"
            );
        });

        cx.simulate_event(gpui::ScrollWheelEvent {
            position: gpui::point(px(400.), px(300.)),
            // A positive wheel delta scrolls up.
            delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(120.))),
            modifiers: gpui::Modifiers::none(),
            touch_phase: gpui::TouchPhase::default(),
        });
        cx.update(|_window, app| {
            let chat = root.read(app).view.clone();
            assert!(
                !chat.read(app).model().follows_bottom(),
                "scrolling up pins the view: the transcript must not jump away"
            );
        });
    }

    #[gpui::test]
    fn a_chat_that_has_not_read_yet_shows_a_skeleton(cx: &mut gpui::TestAppContext) {
        use gpui::{point, size};
        let cx = cx.add_empty_window();
        let view = cx.update(|_window, app| app.new(ChatView::new));
        cx.draw(point(px(0.), px(0.)), size(px(900.), px(700.)), |_, _cx| {
            view.clone()
        });
        assert!(
            cx.debug_bounds("chat-loading").is_some(),
            "the first load shows a skeleton"
        );
        assert!(
            cx.debug_bounds("chat-empty").is_none(),
            "a chat that has not read yet is not empty"
        );
        assert!(
            cx.debug_bounds("chat-turn").is_none(),
            "a chat that has not read yet has no turns"
        );
    }

    /// A transcript with one tool call, as the tool card reads it.
    fn tool_turn(input: &str, output: &str, error: bool) -> Value {
        json!({
            "source": "claude-transcript",
            "turns": [
                { "role": "assistant", "ts": "2026-01-01T00:00:00Z",
                  "parts": [
                    { "kind": "tool", "name": "Bash", "summary": "run a command",
                      "input": input, "output": output, "error": error },
                  ],
                },
            ],
        })
    }

    #[gpui::test]
    fn a_tool_call_is_a_row_with_its_work_inside(cx: &mut gpui::TestAppContext) {
        let (root, cx) = cx.add_window_view(|_, cx| ChatRoot {
            view: cx.new(ChatView::new),
        });
        cx.update(|_window, app| {
            let chat = root.read(app).view.clone();
            chat.update(app, |view, _cx| {
                view.apply_conversation(response(tool_turn(
                    "{\"command\":\"ls\"}",
                    "src\nCargo.toml\n",
                    false,
                )));
            });
        });
        cx.update(|window, cx| window.draw(cx).clear());
        assert!(
            cx.debug_bounds("chat-tool").is_some(),
            "the tool row is drawn"
        );
        assert!(
            cx.debug_bounds("chat-tool-header").is_some(),
            "the row has a header"
        );
        assert!(
            cx.debug_bounds("chat-tool-output").is_some(),
            "a tool's work starts open"
        );
    }

    #[gpui::test]
    fn a_tool_row_folds_away_on_a_click(cx: &mut gpui::TestAppContext) {
        let (root, cx) = cx.add_window_view(|_, cx| ChatRoot {
            view: cx.new(ChatView::new),
        });
        cx.update(|_window, app| {
            let chat = root.read(app).view.clone();
            chat.update(app, |view, _cx| {
                view.apply_conversation(response(tool_turn(
                    "{\"command\":\"ls\"}",
                    "src\n",
                    false,
                )));
            });
        });
        cx.update(|window, cx| window.draw(cx).clear());
        let before = cx.debug_bounds("chat-tool").expect("the tool row");
        let bounds = cx
            .debug_bounds("chat-tool-header")
            .expect("the tool row's header");
        cx.simulate_click(bounds.center(), gpui::Modifiers::none());
        cx.update(|window, cx| window.draw(cx).clear());
        let after = cx.debug_bounds("chat-tool").expect("the tool row");
        assert!(
            after.size.height < before.size.height,
            "the click folded the work away: {before:?} then {after:?}"
        );
        cx.update(|_window, app| {
            let chat = root.read(app).view.clone();
            assert!(
                !chat.read(app).model().is_tool_open(0, 0),
                "the row's state is the model's"
            );
        });
    }

    #[gpui::test]
    fn a_failed_tool_call_says_so_and_stays(cx: &mut gpui::TestAppContext) {
        let (root, cx) = cx.add_window_view(|_, cx| ChatRoot {
            view: cx.new(ChatView::new),
        });
        cx.update(|_window, app| {
            let chat = root.read(app).view.clone();
            chat.update(app, |view, _cx| {
                view.apply_conversation(response(tool_turn("{}", "boom", true)));
            });
        });
        cx.update(|window, cx| window.draw(cx).clear());
        assert!(
            cx.debug_bounds("chat-tool").is_some(),
            "a failed tool call does not disappear"
        );
        assert!(
            cx.debug_bounds("chat-tool-output").is_some(),
            "and it keeps what it said"
        );
    }

    #[gpui::test]
    fn an_edit_shows_its_diff(cx: &mut gpui::TestAppContext) {
        let (root, cx) = cx.add_window_view(|_, cx| ChatRoot {
            view: cx.new(ChatView::new),
        });
        let input = json!({
            "file_path": "C:/work/src/main.rs",
            "old_string": "one\ntwo\n",
            "new_string": "one\nthree\n",
        })
        .to_string();
        cx.update(|_window, app| {
            let chat = root.read(app).view.clone();
            chat.update(app, |view, _cx| {
                view.apply_conversation(response(tool_turn(&input, "updated", false)));
            });
        });
        cx.update(|window, cx| window.draw(cx).clear());
        assert!(
            cx.debug_bounds("chat-diff").is_some(),
            "the edit shows its diff"
        );
    }

    #[gpui::test]
    fn a_file_path_in_a_tool_card_is_a_control(cx: &mut gpui::TestAppContext) {
        let (root, cx) = cx.add_window_view(|_, cx| ChatRoot {
            view: cx.new(ChatView::new),
        });
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let collected = events.clone();
        // The subscription must outlive the test, or the events go nowhere.
        let subscription = cx.update(|_window, app| {
            app.subscribe(
                &root.read(app).view.clone(),
                move |_entity, event: &ChatEvent, _cx| {
                    collected.borrow_mut().push(event.clone());
                },
            )
        });
        cx.update(|_window, app| {
            let chat = root.read(app).view.clone();
            chat.update(app, |view, _cx| {
                view.apply_conversation(response(tool_turn(
                    "{\"file_path\":\"C:/work/src/main.rs\"}",
                    "wrote C:/work/src/main.rs",
                    false,
                )));
            });
        });
        cx.update(|window, cx| window.draw(cx).clear());
        assert!(
            cx.debug_bounds("chat-file-path").is_some(),
            "the path is its own control"
        );

        let bounds = cx.debug_bounds("chat-file-path").expect("the path");
        cx.simulate_click(bounds.center(), gpui::Modifiers::none());
        assert!(
            events
                .borrow()
                .contains(&ChatEvent::OpenFile("C:/work/src/main.rs".to_string())),
            "the click asked for the file: {:?}",
            events.borrow()
        );
        drop(subscription);
    }

    #[gpui::test]
    fn output_beyond_the_bound_points_at_the_terminal(cx: &mut gpui::TestAppContext) {
        let (root, cx) = cx.add_window_view(|_, cx| ChatRoot {
            view: cx.new(ChatView::new),
        });
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let collected = events.clone();
        // The subscription must outlive the test, or the events go nowhere.
        let subscription = cx.update(|_window, app| {
            app.subscribe(
                &root.read(app).view.clone(),
                move |_entity, event: &ChatEvent, _cx| {
                    collected.borrow_mut().push(event.clone());
                },
            )
        });
        cx.update(|_window, app| {
            let chat = root.read(app).view.clone();
            chat.update(app, |view, _cx| {
                view.apply_conversation(response(tool_turn(
                    "{}",
                    &"x".repeat(crate::tool_card::MAX_TOOL_CHARS + 100),
                    false,
                )));
            });
        });
        cx.update(|window, cx| window.draw(cx).clear());
        assert!(
            cx.debug_bounds("chat-terminal-path").is_some(),
            "the cut output points at the Terminal Lens"
        );

        let bounds = cx.debug_bounds("chat-terminal-path").expect("the path");
        cx.simulate_click(bounds.center(), gpui::Modifiers::none());
        assert!(
            events.borrow().contains(&ChatEvent::ShowTerminal),
            "the click asked for the Terminal Lens: {:?}",
            events.borrow()
        );
        drop(subscription);
    }

    #[gpui::test]
    fn an_empty_transcript_shows_the_empty_state_not_the_grid(cx: &mut gpui::TestAppContext) {
        use gpui::{point, size};
        let cx = cx.add_empty_window();
        let view = cx.update(|_window, app| app.new(ChatView::new));
        cx.draw(point(px(0.), px(0.)), size(px(900.), px(700.)), |_, cx| {
            view.update(cx, |view, _cx| {
                // The Engine answers "scrollback" with no turns when the agent
                // keeps no transcript: the chat must not show the terminal's
                // own text as a turn.
                view.apply_conversation(response(json!({
                    "source": "scrollback",
                    "turns": [],
                })));
            });
            view.clone()
        });
        assert!(
            cx.debug_bounds("chat-empty").is_some(),
            "an empty transcript shows the empty state"
        );
        assert!(
            cx.debug_bounds("chat-turn").is_none(),
            "an empty transcript shows no turns"
        );
    }

    #[gpui::test]
    fn a_failed_read_shows_the_failure_and_a_retry(cx: &mut gpui::TestAppContext) {
        let cx = cx.add_empty_window();
        let view = cx.update(|_window, app| app.new(ChatView::new));
        cx.update(|_window, app| {
            view.update(app, |view, _cx| {
                view.fail_conversation("Couldn't load the conversation. Try again.".to_string());
            });
            assert_eq!(
                view.read(app).model().failure(),
                Some("Couldn't load the conversation. Try again.")
            );
            assert!(view.read(app).model().phase().is_failed());
        });
    }

    #[gpui::test]
    fn a_retry_clears_the_failure(cx: &mut gpui::TestAppContext) {
        let cx = cx.add_empty_window();
        let view = cx.update(|_window, app| app.new(ChatView::new));
        cx.update(|_window, app| {
            view.update(app, |view, _cx| {
                view.fail_conversation("Couldn't load the conversation. Try again.".to_string());
                view.retry();
            });
            assert_eq!(view.read(app).model().failure(), None);
            assert_eq!(view.read(app).model().phase(), &ChatPhase::Loading);
        });
    }
}
