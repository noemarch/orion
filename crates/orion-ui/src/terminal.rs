//! Terminal pane view — renders the PTY output using the vt100 parser grid.
//!
//! PTY output arrives from the engine via `feed()`; each feed mutates the grid
//! and the caller notifies the view so GPUI repaints.

use gpui::prelude::*;
use gpui::{
    div, px, rems, rgb, white, Context, IntoElement, Render, StyledText, TextStyle, Window,
};
use orion_core::screen::ScreenState;

pub struct TerminalPane {
    screen: ScreenState,
}

impl TerminalPane {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            screen: ScreenState::new(32, 120),
        }
    }

    /// Feed a chunk of PTY text into the grid.
    pub fn feed(&mut self, text: &str) {
        self.screen.process(text);
    }

    fn render_grid(&self) -> impl IntoElement {
        let rows = self.screen.rows_with_wrap();
        let mut elements = Vec::new();

        for (row_text, _wrapped) in rows {
            let style = TextStyle {
                color: white(),
                font_size: rems(0.8125).into(), // 13px
                line_height: px(1.0).into(),
                ..Default::default()
            };
            elements.push(
                div()
                    .flex_row()
                    .child(StyledText::new(row_text).with_default_highlights(&style, [])),
            );
        }

        div().flex_col().children(elements)
    }
}

impl Render for TerminalPane {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("terminal-pane")
            .flex_col()
            .size_full()
            .bg(rgb(0x0d0d0d))
            .font_family("JetBrains Mono")
            .text_size(px(13.0))
            .line_height(px(1.0))
            .child(self.render_grid())
    }
}
