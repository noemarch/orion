//! Terminal pane view — GPUI rendering of the PTY grid with input.
//!
//! PTY output arrives from the engine via `feed()`; each feed mutates the grid
//! and the caller notifies the view so GPUI repaints. Keyboard input flows back
//! to the pane through `Pane::write` (literal text) and `Pane::keys` (logical
//! chords — the same names `encode_key` in `engine::pane` accepts).
//!
//! Surfaces ported from the web client (P1 — terminal truth):
//! - keyboard input on the focused grid (the soft-keyboard-free desktop path)
//! - a touch-capable key bar (Esc/Tab/Ctrl/Alt/Shift/arrows/Enter, 44px targets)
//! - scrollback navigation (wheel + scrollbar thumb; `ScreenState::scroll_to`)
//! - a find bar (`ScreenState::find`, jump-to-match, match count)
//! - mouse selection (`ScreenState::selection`, Ctrl/Cmd+C copies)

use crate::settings::KeyBarExtras;
use crate::theme::Theme;
use gpui::prelude::*;
use gpui::{
    div, px, rems, white, ClipboardItem, Context, FocusHandle, HighlightStyle, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Render,
    ScrollWheelEvent, StyledText, TextStyle, Window,
};
use orion_core::engine::pane::Pane;
use orion_core::screen::ScreenState;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Sidebar width in app.rs; mouse x-coordinates are window-relative.
pub const SIDEBAR_WIDTH_PX: f32 = 280.0;
const FONT_PX: f32 = 13.0;
const ROW_PITCH_PX: f32 = 16.0;
/// Touch target height for key-bar and find-bar controls (DESIGN.md minimum).
const TARGET_PX: f32 = 44.0;
/// Resize requests are debounced this long before reaching the PTY.
pub const RESIZE_DEBOUNCE_MS: u64 = 50;

fn grid_style() -> TextStyle {
    TextStyle {
        color: white(),
        font_size: rems(0.8125).into(),
        line_height: px(ROW_PITCH_PX).into(),
        ..Default::default()
    }
}

/// One line of grid content plus its highlight ranges (byte offsets).
fn grid_line(
    text: String,
    highlights: Vec<(std::ops::Range<usize>, HighlightStyle)>,
) -> impl IntoElement {
    div()
        .flex_row()
        .child(StyledText::new(text).with_default_highlights(&grid_style(), highlights))
}

pub struct TerminalPane {
    screen: ScreenState,
    focus_handle: FocusHandle,
    pane: Option<Arc<Pane>>,
    // Sticky modifiers (web key-bar semantics: consumed by the next key press).
    sticky_ctrl: bool,
    sticky_alt: bool,
    sticky_shift: bool,
    /// Settings-driven key-bar extras (P4: `Preferences.keyBarExtras`).
    /// Base keys are always present; the extras append extra items.
    key_bar_extras: KeyBarExtras,
    // Find state.
    find_query: String,
    find_matches: Vec<(usize, usize, usize, usize)>, // start_row, start_col, end_row, end_col
    find_current: Option<usize>,
    find_open: bool,
    // Selection state in content coordinates (rows from oldest history row).
    sel_anchor: Option<(usize, usize)>,
    sel_cursor: Option<(usize, usize)>,
    selecting: bool,
    // Geometry measured at render time for mouse→cell mapping.
    char_w: f32,
    last_cols: u16,
    last_rows: u16,
    last_resize: Instant,
}

impl TerminalPane {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            screen: ScreenState::new(32, 120),
            focus_handle: cx.focus_handle(),
            pane: None,
            sticky_ctrl: false,
            sticky_alt: false,
            sticky_shift: false,
            key_bar_extras: KeyBarExtras {
                alt: true,
                ..Default::default()
            },
            find_query: String::new(),
            find_matches: Vec::new(),
            find_current: None,
            find_open: false,
            sel_anchor: None,
            sel_cursor: None,
            selecting: false,
            char_w: 7.81, // JetBrains Mono @ 13px; refined at render.
            last_cols: 120,
            last_rows: 32,
            last_resize: Instant::now() - Duration::from_millis(RESIZE_DEBOUNCE_MS),
        }
    }

    /// Attach the live pane this view drives (set once the engine spawns it).
    pub fn set_pane(&mut self, pane: Arc<Pane>) {
        self.pane = Some(pane);
    }
    /// Drive the key bar from saved settings (P4: replaces the static bar
    /// layout with `Preferences.keyBarExtras`).
    pub fn set_key_bar_extras(&mut self, extras: KeyBarExtras) {
        self.key_bar_extras = extras;
    }

    /// Feed a chunk of PTY text into the grid.
    pub fn feed(&mut self, text: &str) {
        self.screen.process(text);
        // New output scrolls back to the live bottom (pager behavior of the web view).
        if self.screen.offset_from_bottom() > 0 {
            self.screen.scroll_to(0);
        }
    }

    fn send_chord(&self, chord: &str) {
        if let Some(pane) = &self.pane {
            let _ = pane.keys(&[chord.to_string()]);
        }
    }

    fn send_text(&self, text: &str) {
        if let Some(pane) = &self.pane {
            let _ = pane.write(text);
        }
    }

    fn clear_sticky(&mut self) {
        self.sticky_ctrl = false;
        self.sticky_alt = false;
        self.sticky_shift = false;
    }

    fn sticky_prefix(&self) -> String {
        let mut prefix = String::new();
        if self.sticky_ctrl {
            prefix.push_str("ctrl+");
        }
        if self.sticky_alt {
            prefix.push_str("alt+");
        }
        if self.sticky_shift {
            prefix.push_str("shift+");
        }
        prefix
    }

    /// Map a GPUI keystroke to the logical chord `engine::pane::encode_key` accepts.
    fn keystroke_to_chord(ev: &KeyDownEvent) -> Option<String> {
        linear_map_keystroke(ev)
    }

    /// Handle one key event. Returns whether the event was consumed.
    fn on_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        if self.find_open {
            return self.on_find_key(ev);
        }

        let chord = Self::keystroke_to_chord(ev);
        let Some(chord) = chord else { return false };

        // Ctrl/Cmd+C copies the selection instead of sending a chord to the pane.
        if (ev.keystroke.modifiers.control || ev.keystroke.modifiers.platform)
            && chord == "ctrl+c"
            && self.copy_selection(cx)
        {
            self.clear_sticky();
            return true;
        }
        // Ctrl+F opens the find bar.
        if ev.keystroke.modifiers.control && chord == "ctrl+f" {
            self.find_open = true;
            cx.notify();
            return true;
        }
        // Ctrl+L clears the grid.
        if ev.keystroke.modifiers.control && chord == "ctrl+l" {
            self.screen.clear();
            if let Some(pane) = &self.pane {
                pane.clear();
            }
            cx.notify();
            return true;
        }

        let prefix = self.sticky_prefix();
        if prefix.is_empty() && chord.chars().count() == 1 && !chord.contains(['+', ' ']) {
            self.send_text(&chord);
        } else {
            let full = format!("{prefix}{chord}");
            self.send_chord(&full);
        }
        self.clear_sticky();
        cx.notify();
        true
    }

    /// Keys while the find bar owns the input.
    fn on_find_key(&mut self, ev: &KeyDownEvent) -> bool {
        match ev.keystroke.key.to_ascii_lowercase().as_str() {
            "escape" => self.find_open = false,
            "enter" => self.find_next(),
            "backspace" => {
                self.find_query.pop();
                self.refresh_find_matches();
            }
            other
                if other.chars().count() == 1
                    && !ev.keystroke.modifiers.control
                    && !ev.keystroke.modifiers.alt =>
            {
                let c = ev
                    .keystroke
                    .key_char
                    .clone()
                    .unwrap_or_else(|| other.to_string());
                if c.chars().count() == 1 {
                    self.find_query.push_str(&c);
                    self.refresh_find_matches();
                }
            }
            _ => {}
        }
        true
    }

    fn refresh_find_matches(&mut self) {
        self.find_matches = self
            .screen
            .find(&self.find_query, 1024)
            .iter()
            .map(|m| {
                (
                    m["start"]["row"].as_u64().unwrap_or(0) as usize,
                    m["start"]["col"].as_u64().unwrap_or(0) as usize,
                    m["end"]["row"].as_u64().unwrap_or(0) as usize,
                    m["end"]["col"].as_u64().unwrap_or(0) as usize,
                )
            })
            .collect();
        self.find_current = if self.find_matches.is_empty() {
            None
        } else {
            Some(0)
        };
        if let Some(first) = self.find_current {
            self.jump_to_match(first);
        }
    }

    fn jump_to_match(&mut self, index: usize) {
        if let Some(m) = self.find_matches.get(index) {
            let visible = self.screen.size().0 as usize;
            let total = self.screen.scrollback_len() + visible;
            let offset = total.saturating_sub(m.0 + visible / 2);
            self.screen.scroll_to(offset);
        }
    }

    fn find_next(&mut self) {
        if self.find_matches.is_empty() {
            return;
        }
        let next = match self.find_current {
            Some(current) => (current + 1) % self.find_matches.len(),
            None => 0,
        };
        self.find_current = Some(next);
        self.jump_to_match(next);
    }

    fn find_prev(&mut self) {
        if self.find_matches.is_empty() {
            return;
        }
        let prev = match self.find_current {
            Some(current) if current > 0 => current - 1,
            _ => self.find_matches.len() - 1,
        };
        self.find_current = Some(prev);
        self.jump_to_match(prev);
    }

    /// Copy the current selection to the clipboard. Returns whether there was one.
    fn copy_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let (Some(start), Some(end)) = (self.sel_anchor, self.sel_cursor) else {
            return false;
        };
        let text = self.screen.selection(start.0, start.1, end.0, end.1);
        if text.is_empty() {
            return false;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.sel_anchor = None;
        self.sel_cursor = None;
        true
    }

    /// Viewport rows of the current content: (content_row, text) for the visible window.
    fn viewport_rows(&self) -> Vec<(usize, String)> {
        let history = self.screen.history();
        let (_, rows) = self.screen.size();
        let visible = rows as usize;
        let total = history.len();
        let offset = self.screen.offset_from_bottom();
        let end = total.saturating_sub(offset);
        let start = end.saturating_sub(visible);
        history[start..end]
            .iter()
            .enumerate()
            .map(|(i, line)| (start + i, line.clone()))
            .collect()
    }

    /// Map a window-relative position onto a visible (row, col) cell.
    fn pos_to_cell(&self, pos: gpui::Point<gpui::Pixels>) -> (usize, usize) {
        let x = f32::from(pos.x) - SIDEBAR_WIDTH_PX;
        let y = f32::from(pos.y);
        let col = if self.char_w > 0.0 {
            (x / self.char_w).floor().max(0.0) as usize
        } else {
            0
        };
        let row = (y / ROW_PITCH_PX).floor().max(0.0) as usize;
        (row.min(self.last_rows as usize), col.min(10_000))
    }

    /// Map a visible cell onto a content cell (row, col).
    fn cell_to_content(&self, vis_row: usize, col: usize) -> (usize, usize) {
        let (_, rows) = self.screen.size();
        let total = self.screen.history().len();
        let offset = self.screen.offset_from_bottom();
        let end = total.saturating_sub(offset);
        let start = end.saturating_sub(rows as usize);
        (start + vis_row.min(rows as usize), col)
    }
}

/// Map a GPUI keystroke to the logical chord `engine::pane::encode_key` accepts.
/// Returns `None` for keys that carry no terminal meaning (modifiers alone,
/// platform shortcuts).
fn linear_map_keystroke(ev: &KeyDownEvent) -> Option<String> {
    let ks = &ev.keystroke;
    if ks.modifiers.platform {
        return None; // Cmd/Super shortcuts are app-level, not terminal input.
    }
    let key = ks.key.to_ascii_lowercase();
    if matches!(
        key.as_str(),
        "ctrl" | "control" | "alt" | "shift" | "platform" | "function" | "meta" | "command"
    ) {
        return None; // A modifier press by itself is not a chord.
    }
    let mut chord = String::new();
    if ks.modifiers.control {
        chord.push_str("ctrl+");
    }
    if ks.modifiers.alt {
        chord.push_str("alt+");
    }
    if ks.modifiers.shift {
        // Shift is implied by uppercase letters; an explicit shift prefix for a
        // single letter is harmless (the literal path ignores it), so skip it to
        // keep caps typing a plain character.
        let plain_letter = key.chars().count() == 1 && key.chars().all(|c| c.is_ascii_alphabetic());
        if !plain_letter {
            chord.push_str("shift+");
        }
    }
    if let Some(ks_char) = &ks.key_char {
        if ks_char.chars().count() == 1 && ks_char != " " {
            let mut c = ks_char.clone();
            if !ks.modifiers.shift {
                c.make_ascii_lowercase();
            }
            return Some(format!("{chord}{c}"));
        }
    }
    let name: Option<String> = match key.as_str() {
        "escape" => Some("esc".into()),
        "enter" | "return" => Some("enter".into()),
        "tab" => Some("tab".into()),
        "backspace" => Some("backspace".into()),
        "delete" => Some("delete".into()),
        "insert" => Some("insert".into()),
        "home" => Some("home".into()),
        "end" => Some("end".into()),
        "pageup" => Some("pageup".into()),
        "pagedown" => Some("pagedown".into()),
        "up" | "arrowup" => Some("up".into()),
        "down" | "arrowdown" => Some("down".into()),
        "left" | "arrowleft" => Some("left".into()),
        "right" | "arrowright" => Some("right".into()),
        "space" => Some("space".into()),
        other => {
            if other.chars().count() == 1 || (other.starts_with('f') && other.len() <= 3) {
                Some(other.to_string())
            } else {
                None
            }
        }
    };
    let name = name?;
    Some(format!("{chord}{name}"))
}

impl Render for TerminalPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Keep the engine pane's geometry in sync with the window.
        let size = window.bounds().size;
        let avail_w = (f32::from(size.width) - SIDEBAR_WIDTH_PX).max(1.0);
        let avail_h = (f32::from(size.height) - TARGET_PX * 3.0).max(1.0);
        let cols = ((avail_w / self.char_w).floor() as u16).clamp(1, 10_000);
        let rows = ((avail_h / ROW_PITCH_PX).floor() as u16).clamp(1, 10_000);
        if cols != self.last_cols || rows != self.last_rows {
            self.last_cols = cols;
            self.last_rows = rows;
            self.screen.resize(rows, cols);
            if self.last_resize.elapsed() >= Duration::from_millis(RESIZE_DEBOUNCE_MS) {
                self.last_resize = Instant::now();
                if let Some(pane) = &self.pane {
                    let _ = pane.resize(cols, rows);
                }
            }
        }
        self.char_w = avail_w / cols as f32;

        let max_offset = self.screen.scrollback_len();
        let offset = self.screen.offset_from_bottom();
        let (visible_rows, _) = self.screen.size();
        let rows_v = self.viewport_rows();

        // Build highlight ranges per visible row for selection and find matches.
        let sel = match (self.sel_anchor, self.sel_cursor) {
            (Some(a), Some(b)) => {
                let (a, b) = if (a.0, a.1) <= (b.0, b.1) {
                    (a, b)
                } else {
                    (b, a)
                };
                Some((a, b))
            }
            _ => None,
        };
        let theme = Theme::current();
        let sel_style = HighlightStyle {
            color: Some(theme.term_fg.into()),
            background_color: Some(theme.term_selection.into()),
            ..Default::default()
        };
        let find_style = HighlightStyle {
            color: Some(theme.term_bg.into()),
            background_color: Some(theme.accent.into()),
            ..Default::default()
        };

        let grid_rows = rows_v
            .iter()
            .map(|(content_row, text)| {
                let mut ranges: Vec<(std::ops::Range<usize>, HighlightStyle)> = Vec::new();
                if let Some((a, b)) = sel {
                    if *content_row >= a.0 && *content_row <= b.0 {
                        let start = if *content_row == a.0 { a.1 } else { 0 };
                        let end = if *content_row == b.0 {
                            (b.1 + 1).min(text.len())
                        } else {
                            text.len()
                        };
                        if start < end {
                            ranges.push((start..end, sel_style));
                        }
                    }
                }
                for m in &self.find_matches {
                    if m.0 == *content_row && m.2 == *content_row {
                        ranges.push((m.1..(m.3 + 1).min(text.len()), find_style));
                    }
                }
                grid_line(text.clone(), ranges)
            })
            .collect::<Vec<_>>();

        // Scrollbar thumb geometry (visual; wheel is the primary control).
        let total_lines = max_offset + visible_rows as usize;
        let thumb_h = if total_lines > 0 {
            (visible_rows as usize * 100 / total_lines.max(1)).max(4) as f32
        } else {
            100.0
        };
        let thumb_top = if max_offset > 0 {
            offset as f32 / max_offset as f32 * (100.0 - thumb_h)
        } else {
            0.0
        };

        let find_bar = if self.find_open {
            let total = self.find_matches.len();
            let label = match self.find_current {
                Some(current) => format!("{}/{}", current + 1, total),
                None => format!("0/{}", total),
            };
            let ghost_style = TextStyle {
                color: theme.text_dim.into(),
                font_size: rems(0.75).into(),
                ..Default::default()
            };
            div()
                .flex_row()
                .items_center()
                .h(px(TARGET_PX))
                .gap_2()
                .px_2()
                .bg(theme.surface_panel)
                .child(
                    StyledText::new(format!("\"{}\"", self.find_query.clone()))
                        .with_default_highlights(&ghost_style, []),
                )
                .child(StyledText::new(label).with_default_highlights(&ghost_style, []))
                .child(
                    div()
                        .id("find-prev")
                        .flex()
                        .size(px(TARGET_PX))
                        .items_center()
                        .justify_center()
                        .text_color(white())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.find_prev();
                            cx.notify();
                        }))
                        .child("↑"),
                )
                .child(
                    div()
                        .id("find-next")
                        .flex()
                        .size(px(TARGET_PX))
                        .items_center()
                        .justify_center()
                        .text_color(white())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.find_next();
                            cx.notify();
                        }))
                        .child("↓"),
                )
                .child(
                    div()
                        .id("find-close")
                        .flex()
                        .size(px(TARGET_PX))
                        .items_center()
                        .justify_center()
                        .text_color(white())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.find_open = false;
                            cx.notify();
                        }))
                        .child("✕"),
                )
        } else {
            div().h_0()
        };

        let theme = Theme::current();
        let key_bar = {
            let mut bar = div()
                .flex_row()
                .flex_wrap()
                .gap_1()
                .p_1()
                .bg(theme.surface_panel);
            // Base keys (DEFAULT_KEY_BAR_ITEMS): always present.
            let keys: &[(&str, &str)] = &[
                ("Esc", "esc"),
                ("Tab", "tab"),
                ("^C", "ctrl+c"),
                ("Enter", "enter"),
                ("↑", "up"),
                ("↓", "down"),
                ("←", "left"),
                ("→", "right"),
            ];
            let mut all_keys: Vec<(&str, &str)> = keys.to_vec();
            // Settings extras (KEY_BAR_EXTRAS order) append after the base.
            let extra_keys = self.key_bar_extras.keys();
            for (label, chord) in &extra_keys {
                all_keys.push((*label, *chord));
            }
            for (label, chord) in all_keys {
                let chord = chord.to_string();
                bar = bar.child(
                    div()
                        .id(label)
                        .flex()
                        .h(px(TARGET_PX))
                        .min_w(px(TARGET_PX))
                        .px_2()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.0))
                        .bg(theme.surface_elevated)
                        .text_color(theme.text)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let prefix = this.sticky_prefix();
                            let full = if prefix.is_empty() {
                                chord.clone()
                            } else {
                                format!("{prefix}{chord}")
                            };
                            this.send_chord(&full);
                            this.clear_sticky();
                            cx.notify();
                        }))
                        .child(label.to_string()),
                );
            }
            let modifiers: &[&str] = if self.key_bar_extras.alt {
                &["Ctrl", "Alt", "Shift"]
            } else {
                &["Ctrl", "Shift"]
            };
            for &label in modifiers {
                let pressed = match label {
                    "Ctrl" => self.sticky_ctrl,
                    "Alt" => self.sticky_alt,
                    _ => self.sticky_shift,
                };
                bar = bar.child(
                    div()
                        .id(label)
                        .flex()
                        .h(px(TARGET_PX))
                        .min_w(px(TARGET_PX))
                        .px_2()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.0))
                        .bg(if pressed {
                            theme.accent_tint
                        } else {
                            theme.surface_elevated
                        })
                        .text_color(theme.text_strong)
                        .on_click(cx.listener(move |this, _, _, _cx| match label {
                            "Ctrl" => this.sticky_ctrl = !this.sticky_ctrl,
                            "Alt" => this.sticky_alt = !this.sticky_alt,
                            _ => this.sticky_shift = !this.sticky_shift,
                        }))
                        .child(label.to_string()),
                );
            }
            bar
        };

        let status_line = match &self.pane {
            Some(pane) => {
                let status = pane.status().as_wire().to_string();
                if offset > 0 {
                    format!("{status}  scroll {offset}/{max_offset}")
                } else {
                    status.to_string()
                }
            }
            None => "starting agent…".to_string(),
        };
        let status_bar = div()
            .flex_row()
            .items_center()
            .h(px(TARGET_PX * 0.75))
            .px_2()
            .text_size(px(11.0))
            .text_color(theme.text_dim)
            .child(status_line);

        div()
            .id("terminal-pane")
            .track_focus(&self.focus_handle)
            .flex_col()
            .size_full()
            .bg(theme.term_bg)
            .font_family("JetBrains Mono")
            .text_size(px(FONT_PX))
            .line_height(px(ROW_PITCH_PX))
            .on_key_down(cx.listener(|this, ev, _, cx| {
                if this.on_key(ev, cx) {
                    cx.notify();
                }
            }))
            .on_scroll_wheel(cx.listener(move |this, ev: &ScrollWheelEvent, _, cx| {
                let delta = ev.delta.pixel_delta(px(ROW_PITCH_PX));
                let lines = (f32::from(delta.y) / ROW_PITCH_PX).round() as i64;
                let max = this.screen.scrollback_len();
                let next =
                    (this.screen.offset_from_bottom() as i64 - lines).clamp(0, max as i64) as usize;
                if next != this.screen.offset_from_bottom() {
                    this.screen.scroll_to(next);
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    this.focus_handle.focus(window);
                    if ev.button == MouseButton::Left {
                        let (vis_row, col) = this.pos_to_cell(ev.position);
                        this.sel_anchor = Some(this.cell_to_content(vis_row, col));
                        this.sel_cursor = this.sel_anchor;
                        this.selecting = true;
                        cx.notify();
                    }
                }),
            )
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                if this.selecting && ev.dragging() {
                    let (vis_row, col) = this.pos_to_cell(ev.position);
                    this.sel_cursor = Some(this.cell_to_content(vis_row, col));
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseUpEvent, _, cx| {
                    if ev.button == MouseButton::Left && this.selecting {
                        this.selecting = false;
                        cx.notify();
                    }
                }),
            )
            .child(
                div()
                    .flex_row()
                    .flex_1()
                    .child(div().id("grid").flex_1().flex_col().children(grid_rows))
                    .child(
                        div().id("scrollbar").w(px(10.0)).h_full().relative().child(
                            div()
                                .id("thumb")
                                .w_full()
                                .h(px(thumb_h * avail_h / 100.0))
                                .rounded_full()
                                .bg(theme.surface_hover)
                                .absolute()
                                .top(px(thumb_top * avail_h / 100.0)),
                        ),
                    ),
            )
            .child(find_bar)
            .child(key_bar)
            .child(status_bar)
    }
}
/// GPUI keystroke → logical chord name for `Pane::keys` (the same names the
/// web client's `terminalChord` produces).
#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Keystroke, Modifiers};

    fn ev(key: &str, shift: bool, control: bool, alt: bool) -> KeyDownEvent {
        KeyDownEvent {
            keystroke: Keystroke {
                key: key.to_string(),
                modifiers: Modifiers {
                    shift,
                    control,
                    alt,
                    ..Default::default()
                },
                key_char: None,
            },
            is_held: false,
        }
    }

    #[test]
    fn resize_debounce_delay_is_50ms() {
        assert_eq!(RESIZE_DEBOUNCE_MS, 50);
    }

    #[test]
    fn named_keys_map_to_logical_chords() {
        assert_eq!(
            TerminalPane::keystroke_to_chord(&ev("enter", false, false, false)).as_deref(),
            Some("enter")
        );
        assert_eq!(
            TerminalPane::keystroke_to_chord(&ev("escape", false, false, false)).as_deref(),
            Some("esc")
        );
        assert_eq!(
            TerminalPane::keystroke_to_chord(&ev("up", false, false, false)).as_deref(),
            Some("up")
        );
        assert_eq!(
            TerminalPane::keystroke_to_chord(&ev("tab", true, false, false)).as_deref(),
            Some("shift+tab")
        );
        assert_eq!(
            TerminalPane::keystroke_to_chord(&ev("c", false, true, false)).as_deref(),
            Some("ctrl+c")
        );
        assert_eq!(
            TerminalPane::keystroke_to_chord(&ev("backspace", false, false, true)).as_deref(),
            Some("alt+backspace")
        );
    }

    #[test]
    fn modifier_only_and_platform_keys_are_dropped() {
        assert!(TerminalPane::keystroke_to_chord(&ev("control", false, true, false)).is_none());
        let mut platform = ev("c", false, false, false);
        platform.keystroke.modifiers.platform = true;
        assert!(TerminalPane::keystroke_to_chord(&platform).is_none());
    }

    #[test]
    fn plain_letters_stay_plain() {
        assert_eq!(
            TerminalPane::keystroke_to_chord(&ev("a", true, false, false)).as_deref(),
            Some("a")
        );
    }
}
