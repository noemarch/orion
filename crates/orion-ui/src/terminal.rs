//! Terminal pane view — renders the PTY output using the vt100 parser grid.
//!
//! Receives PTY output from an `orion_core::pane::Pane` via an mpsc channel
//! and feeds it into `orion_core::screen::ScreenState`.

use gpui::prelude::*;
use gpui::{
    div, px, rems, rgb, white, Context, IntoElement, Render, StyledText, TextStyle, Window,
};
use orion_core::screen::ScreenState;
use std::sync::mpsc;

pub struct TerminalPane {
    screen: ScreenState,
    tx: mpsc::Sender<Vec<u8>>,
    rx: mpsc::Receiver<Vec<u8>>,
}

impl TerminalPane {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            screen: ScreenState::new(32, 120),
            tx,
            rx,
        }
    }

    /// Get a sender that can be used to feed PTY output into this pane.
    pub fn sender(&self) -> mpsc::Sender<Vec<u8>> {
        self.tx.clone()
    }

    fn process_pty_output(&mut self) {
        while let Ok(data) = self.rx.try_recv() {
            if let Ok(text) = std::str::from_utf8(&data) {
                self.screen.process(text);
            }
        }
    }

    fn render_grid(&self) -> impl IntoElement {
        let rows = self.screen.rows_with_wrap();
        let mut elements = Vec::new();

        for (row_text, _wrapped) in rows {
            let style = TextStyle {
                color: white().into(),
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
        self.process_pty_output();

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
