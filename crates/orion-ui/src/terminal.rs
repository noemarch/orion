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
use crate::shell_model::ShellLayout;
use crate::terminal_font::terminal_font_families;
use crate::theme::Theme;
use gpui::prelude::*;
use gpui::{
    div, px, rems, white, Bounds, ClipboardItem, Context, FocusHandle, HighlightStyle, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Render,
    ScrollWheelEvent, StyledText, TextStyle, TextSystem, WhiteSpace, Window,
};
use orion_core::engine::pane::Pane;
use orion_core::screen::ScreenState;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Sidebar width in app.rs; mouse x-coordinates are window-relative.
pub const SIDEBAR_WIDTH_PX: f32 = 280.0;
/// The default terminal font size, from Orbit's `DEFAULT_SETTINGS`.
const DEFAULT_FONT_PX: f32 = 13.0;
/// Touch target height for key-bar and find-bar controls (DESIGN.md minimum).
const TARGET_PX: f32 = 44.0;
/// Resize requests are debounced this long before reaching the PTY.
pub const RESIZE_DEBOUNCE_MS: u64 = 50;

/// The size of one character cell, measured from the font that draws it.
#[derive(Debug, Clone, Copy)]
struct CellSize {
    width: f32,
    height: f32,
}

/// The terminal's font: the monospace family the grid draws with, and the cell
/// it measures from that family's real metrics.
#[derive(Debug, Clone)]
struct TerminalFont {
    family: String,
    cell: CellSize,
}

/// The first family in the stack that is installed and measures monospace.
///
/// gpui resolves a family that is not installed to the system UI font, which is
/// proportional: a grid drawn with it goes ragged, and the agent's menu stops
/// lining up. So a family is only used when it is installed AND every glyph
/// sits on the same advance.
fn measure_terminal_font(
    text_system: &TextSystem,
    families: &[String],
    font_size: Pixels,
) -> Option<TerminalFont> {
    let installed = text_system.all_font_names();
    for family in families {
        if !installed
            .iter()
            .any(|name| name.eq_ignore_ascii_case(family))
        {
            continue;
        }
        let id = text_system.resolve_font(&gpui::font(family.clone()));
        let Ok(narrow) = text_system.advance(id, font_size, 'i') else {
            continue;
        };
        let Ok(wide) = text_system.advance(id, font_size, 'W') else {
            continue;
        };
        if (f32::from(narrow.width) - f32::from(wide.width)).abs() > 0.01 {
            continue;
        }
        let Ok(width) = text_system.ch_advance(id, font_size) else {
            continue;
        };
        let height = text_system.ascent(id, font_size) + text_system.descent(id, font_size);
        return Some(TerminalFont {
            family: family.clone(),
            cell: CellSize {
                width: f32::from(width),
                height: f32::from(height).max(1.0),
            },
        });
    }
    None
}

/// The style the grid's rows are drawn in: the measured font, the terminal's
/// foreground, and no wrapping (a terminal clips a row at its last column, it
/// does not fold it onto the next line).
fn grid_style(font: Option<&TerminalFont>, theme: &Theme, font_size: f32) -> TextStyle {
    TextStyle {
        color: theme.term_fg.into(),
        font_family: font
            .map(|font| font.family.clone())
            .unwrap_or_default()
            .into(),
        font_size: px(font_size).into(),
        line_height: px(font.map(|font| font.cell.height).unwrap_or(font_size)).into(),
        white_space: WhiteSpace::Nowrap,
        ..Default::default()
    }
}

/// One line of grid content plus its highlight ranges (byte offsets).
fn grid_line(
    text: String,
    highlights: Vec<(std::ops::Range<usize>, HighlightStyle)>,
    style: &TextStyle,
) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .w_full()
        .overflow_hidden()
        .child(StyledText::new(text).with_default_highlights(style, highlights))
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
    /// The terminal's font size, from preferences.
    font_size: f32,
    /// The grid's own box, learned from the layout: the pane's size, not the
    /// window's. The layout writes it during prepaint; the next frame draws
    /// with it.
    grid_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The box the last frame drew with.
    drawn_bounds: Option<Bounds<Pixels>>,
    /// The font the grid draws with, measured from the family's metrics.
    font: Option<TerminalFont>,
    /// The font family the reader configured: a comma-separated list in
    /// fallback order. Empty means the built-in stack.
    font_setting: String,
    /// The size the agent still has to be told about: the final size of a
    /// resize must reach it, even when the drag has already quieted down.
    pending_resize: Option<(u16, u16)>,
    /// Whether the device has shown a touch: the key bar is a touch
    /// affordance.
    touch: bool,
    /// The grid takes the focus on the next draw.
    focus_wanted: bool,
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
            char_w: 7.81, // refined from the measured font on the first frame.
            last_cols: 120,
            last_rows: 32,
            last_resize: Instant::now() - Duration::from_millis(RESIZE_DEBOUNCE_MS),
            font_size: DEFAULT_FONT_PX,
            grid_bounds: Rc::new(Cell::new(None)),
            drawn_bounds: None,
            font: None,
            font_setting: String::new(),
            pending_resize: None,
            touch: false,
            focus_wanted: false,
        }
    }

    /// Attach the live pane this view drives (set once the engine spawns it).
    /// Switching panes clears the grid so the replay of the new pane is not
    /// drawn over the previous pane's scrollback.
    pub fn set_pane(&mut self, pane: Arc<Pane>) {
        let changed = self
            .pane
            .as_ref()
            .is_none_or(|current| current.id != pane.id);
        if changed {
            self.screen = ScreenState::new(self.last_rows, self.last_cols);
            self.find_matches.clear();
            self.find_current = None;
            self.sel_anchor = None;
            self.sel_cursor = None;
        }
        self.pane = Some(pane);
    }
    /// Drive the key bar from saved settings (P4: replaces the static bar
    /// layout with `Preferences.keyBarExtras`).
    pub fn set_key_bar_extras(&mut self, extras: KeyBarExtras) {
        self.key_bar_extras = extras;
    }

    /// The font family the reader configured: a comma-separated list in
    /// fallback order. Empty means the built-in stack.
    pub fn set_font_family(&mut self, family: &str) {
        if self.font_setting != family {
            self.font_setting = family.to_string();
            // The grid measures its font again on the next frame.
            self.font = None;
        }
    }

    /// The terminal's font size, in pixels.
    pub fn set_font_size(&mut self, size: u32) {
        let size = size as f32;
        if (self.font_size - size).abs() > f32::EPSILON {
            self.font_size = size;
            self.font = None;
        }
    }

    /// The font the grid draws with: the family the reader configured, or the
    /// first monospace one that is installed.
    fn ensure_font(&mut self, window: &mut Window) {
        if self.font.is_some() {
            return;
        }
        let families = terminal_font_families(&self.font_setting);
        self.font = measure_terminal_font(window.text_system(), &families, px(self.font_size));
    }

    /// Tell the agent about a new size, or hold it back until the drag has
    /// quieted down.
    fn request_resize(&mut self, cols: u16, rows: u16) {
        if self.last_resize.elapsed() >= Duration::from_millis(RESIZE_DEBOUNCE_MS) {
            self.last_resize = Instant::now();
            self.pending_resize = None;
            self.send_resize(cols, rows);
            return;
        }
        // The final size must still reach the agent: the next frame sends it
        // when the window has been quiet for the debounce.
        self.pending_resize = Some((cols, rows));
    }

    /// Send a size to the agent's PTY.
    fn send_resize(&self, cols: u16, rows: u16) {
        if let Some(pane) = &self.pane {
            let _ = pane.resize(cols, rows);
        }
    }

    /// The cell the grid draws with, when the font has been measured.
    fn cell(&self) -> Option<CellSize> {
        self.font.as_ref().map(|font| font.cell)
    }

    /// Take the keyboard: the reader's keystrokes go to the agent without a
    /// click first. The focus lands on the next draw.
    pub fn take_focus(&mut self) {
        self.focus_wanted = true;
    }

    /// Feed a chunk of PTY text into the grid.
    pub fn feed(&mut self, text: &str) {
        self.screen.process(text);
        // The reader's scroll position is theirs: the parser always reads the
        // live screen, and only the live bottom follows the new output.
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

    /// What the reader copied, when the clipboard holds text.
    fn clipboard_text(&self, cx: &mut Context<Self>) -> Option<String> {
        cx.read_from_clipboard().and_then(|item| item.text())
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

    /// Handle one key event. Returns whether the event was consumed.
    fn on_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let has_selection = self.sel_anchor.is_some() && self.sel_cursor.is_some();
        match key_action(ev, self.find_open, has_selection) {
            // The find bar owns the keyboard while it is open.
            KeyAction::FindKey => self.on_find_key(ev),
            KeyAction::Paste => {
                if let Some(text) = self.clipboard_text(cx) {
                    self.send_text(&text);
                }
                true
            }
            KeyAction::Copy => {
                if self.copy_selection(cx) {
                    self.clear_sticky();
                } else if ev.keystroke.modifiers.control {
                    // Nothing to copy: Ctrl+C still interrupts the agent.
                    self.send_chord("ctrl+c");
                }
                true
            }
            KeyAction::Interrupt => {
                self.send_chord("ctrl+c");
                true
            }
            KeyAction::Find => {
                self.find_open = true;
                cx.notify();
                true
            }
            KeyAction::Clear => {
                self.screen.clear();
                if let Some(pane) = &self.pane {
                    pane.clear();
                }
                cx.notify();
                true
            }
            KeyAction::Send(chord) => {
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
            KeyAction::Ignored => false,
        }
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
        let (Some(anchor), Some(cursor)) = (self.sel_anchor, self.sel_cursor) else {
            return false;
        };
        // The reader may drag either way: the text they copy must not depend
        // on which way they dragged.
        let (start, end) = ordered_selection(anchor, cursor);
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
    ///
    /// The rows come from the core's cheap viewport read: it does not clone the
    /// screen or walk the ten-thousand row ring, so the grid can be drawn every
    /// frame.
    fn viewport_rows(&mut self) -> Vec<(usize, String)> {
        let offset = self.screen.offset_from_bottom();
        let base = self.screen.scrollback_len().saturating_sub(offset);
        self.screen
            .viewport_rows(offset)
            .into_iter()
            .enumerate()
            .map(|(index, text)| (base + index, text))
            .collect()
    }

    /// Map a window-relative position onto a visible (row, col) cell.
    ///
    /// The position is relative to the grid's own box, not the window's: the
    /// header, the lens switch, the tab strip, the sidebar and the split pane
    /// all sit above and beside it.
    fn pos_to_cell(&self, pos: gpui::Point<gpui::Pixels>) -> (usize, usize) {
        let Some(bounds) = self.grid_bounds.get() else {
            return (0, 0);
        };
        let x = f32::from(pos.x) - f32::from(bounds.origin.x);
        let y = f32::from(pos.y) - f32::from(bounds.origin.y);
        let cell = self.cell();
        let col = match cell {
            Some(cell) if cell.width > 0.0 => (x / cell.width).floor().max(0.0) as usize,
            _ => 0,
        };
        let row = match cell {
            Some(cell) if cell.height > 0.0 => (y / cell.height).floor().max(0.0) as usize,
            _ => 0,
        };
        (row.min(self.last_rows as usize), col.min(10_000))
    }

    /// Map a visible cell onto a content cell (row, col).
    fn cell_to_content(&self, vis_row: usize, col: usize) -> (usize, usize) {
        let offset = self.screen.offset_from_bottom();
        let base = self.screen.scrollback_len().saturating_sub(offset);
        (base + vis_row.min(self.last_rows as usize), col)
    }
}

/// What one key does in the terminal.
///
/// The decision is pure so it can be tested without a window: the view only
/// carries it out.
#[derive(Debug, Clone, PartialEq, Eq)]
enum KeyAction {
    /// Paste what the reader copied into the agent's input.
    Paste,
    /// Copy the selection to the clipboard.
    Copy,
    /// Interrupt the agent's turn.
    Interrupt,
    /// Open the find bar.
    Find,
    /// Clear the screen.
    Clear,
    /// The find bar owns the keyboard.
    FindKey,
    /// Send this chord to the agent.
    Send(String),
    /// Not terminal input.
    Ignored,
}

/// Decide what one key does.
///
/// `find_open` says the find bar owns the keyboard; `has_selection` says the
/// reader has text selected, which is what turns Ctrl+C from an interrupt
/// into a copy.
fn key_action(ev: &KeyDownEvent, find_open: bool, has_selection: bool) -> KeyAction {
    if find_open {
        return KeyAction::FindKey;
    }
    let keystroke = &ev.keystroke;
    let key = keystroke.key.to_ascii_lowercase();
    let command = keystroke.modifiers.control || keystroke.modifiers.platform;
    if command {
        match key.as_str() {
            "v" => return KeyAction::Paste,
            "c" => {
                return if has_selection {
                    KeyAction::Copy
                } else if keystroke.modifiers.control {
                    // Ctrl+C with nothing selected interrupts the agent.
                    KeyAction::Interrupt
                } else {
                    // Cmd+C is not terminal input.
                    KeyAction::Ignored
                };
            }
            "f" if keystroke.modifiers.control => return KeyAction::Find,
            "l" if keystroke.modifiers.control => return KeyAction::Clear,
            _ => {}
        }
    }
    match linear_map_keystroke(ev) {
        Some(chord) => KeyAction::Send(chord),
        None => KeyAction::Ignored,
    }
}

/// The selection's two ends in reading order: the reader may drag either way,
/// and the text they copy must not depend on which way they dragged.
fn ordered_selection(
    anchor: (usize, usize),
    cursor: (usize, usize),
) -> ((usize, usize), (usize, usize)) {
    if (anchor.0, anchor.1) <= (cursor.0, cursor.1) {
        (anchor, cursor)
    } else {
        (cursor, anchor)
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
        // The font: the family the reader configured, or the first monospace
        // one that is installed. It is measured, not guessed: the cell is the
        // font's own advance.
        self.ensure_font(window);

        // The grid takes the focus when the lens becomes the one on screen.
        if self.focus_wanted {
            self.focus_wanted = false;
            self.focus_handle.focus(window);
        }

        // The grid's own box, learned from the layout. The window's size is
        // not the grid's: the header, the lens switch, the tab strip, the
        // sidebar and the split pane all take their share of it.
        let bounds = self.grid_bounds.get();
        if bounds != self.drawn_bounds {
            self.drawn_bounds = bounds;
            // The box moved: the next frame draws with it.
            cx.notify();
        }

        // The grid's size in cells.
        let cell = self.cell();
        let (cols, rows) = match (bounds, cell) {
            (Some(bounds), Some(cell)) => (
                ((f32::from(bounds.size.width) / cell.width).floor() as u16).clamp(1, 10_000),
                ((f32::from(bounds.size.height) / cell.height).floor() as u16).clamp(1, 10_000),
            ),
            _ => (self.last_cols, self.last_rows),
        };
        if cols != self.last_cols || rows != self.last_rows {
            self.last_cols = cols;
            self.last_rows = rows;
            // The grid follows the box at once; the agent is told after the
            // debounce, so a drag does not flood it with resizes.
            self.screen.resize(rows, cols);
            self.request_resize(cols, rows);
        }
        // A resize that was held back while the drag was still moving goes out
        // now: the final size must reach the agent.
        if let Some((cols, rows)) = self.pending_resize {
            if self.last_resize.elapsed() >= Duration::from_millis(RESIZE_DEBOUNCE_MS) {
                self.pending_resize = None;
                self.last_resize = Instant::now();
                self.send_resize(cols, rows);
            }
        }
        if let Some(cell) = cell {
            self.char_w = cell.width;
        }

        let max_offset = self.screen.scrollback_len();
        let offset = self.screen.offset_from_bottom();
        let (visible_rows, _) = self.screen.size();
        let rows_v = self.viewport_rows();

        // Build highlight ranges per visible row for selection and find matches.
        let sel = match (self.sel_anchor, self.sel_cursor) {
            (Some(a), Some(b)) => Some(ordered_selection(a, b)),
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
        let style = grid_style(self.font.as_ref(), &theme, self.font_size);

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
                grid_line(text.clone(), ranges, &style)
            })
            .collect::<Vec<_>>();

        // Scrollbar thumb geometry (visual; wheel is the primary control).
        let grid_height = bounds
            .map(|bounds| f32::from(bounds.size.height))
            .unwrap_or_else(|| f32::from(window.bounds().size.height));
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
                .id("find-bar")
                .debug_selector(|| "find-bar".to_string())
                .flex()
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
                .into_any_element()
        } else {
            div().h_0().into_any_element()
        };

        let theme = Theme::current();
        // The key bar is a touch affordance: it shows on a device that has
        // shown a touch, or on a layout narrow enough to be a phone. Its keys
        // never take the focus, so the grid keeps the keyboard.
        let touch_layout =
            self.touch || ShellLayout::for_width(f32::from(window.bounds().size.width)).rail;
        let key_bar = if touch_layout {
            let mut bar = div()
                .id("key-bar")
                .debug_selector(|| "key-bar".to_string())
                .flex()
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
            bar.into_any_element()
        } else {
            div().h_0().into_any_element()
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
            .flex()
            .flex_row()
            .items_center()
            .h(px(TARGET_PX * 0.75))
            .px_2()
            .text_size(px(11.0))
            .text_color(theme.text_dim)
            .child(status_line);

        // The cursor cell, on the live screen. It is drawn over the row, in
        // the terminal's cursor token.
        let cursor = match (cell, self.screen.cursor_position(), bounds) {
            (Some(cell), (cursor_row, cursor_col), Some(bounds)) => {
                let visible_row = offset + cursor_row as usize;
                if visible_row < rows as usize {
                    Some(
                        div()
                            .id("terminal-cursor")
                            .debug_selector(|| "terminal-cursor".to_string())
                            .absolute()
                            .left(px(
                                f32::from(bounds.origin.x) + cursor_col as f32 * cell.width
                            ))
                            .top(px(
                                f32::from(bounds.origin.y) + visible_row as f32 * cell.height
                            ))
                            .w(px(cell.width))
                            .h(px(cell.height))
                            .bg(theme.term_cursor),
                    )
                } else {
                    None
                }
            }
            _ => None,
        };

        // The rows, in a column that fills the grid: its bounds ARE the
        // grid's box, which is how the view learns the size to draw at.
        let rows_column = div()
            .id("grid-rows")
            .flex()
            .flex_col()
            .flex_1()
            .w_full()
            .overflow_hidden()
            .children(grid_rows);

        let grid_bounds = Rc::clone(&self.grid_bounds);
        let grid = div()
            .flex_1()
            .flex()
            .flex_col()
            .w_full()
            .overflow_hidden()
            .on_children_prepainted(move |bounds, _window, _cx| {
                if let Some(bounds) = bounds.first() {
                    grid_bounds.set(Some(*bounds));
                }
            })
            .id("grid")
            .debug_selector(|| "grid".to_string())
            .child(rows_column);
        let grid = match cursor {
            Some(cursor) => grid.child(cursor),
            None => grid,
        };

        div()
            .id("terminal-pane")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.term_bg)
            .font_family(
                self.font
                    .as_ref()
                    .map(|font| font.family.clone())
                    .unwrap_or_default(),
            )
            .text_size(px(self.font_size))
            .on_key_down(cx.listener(|this, ev, _, cx| {
                if this.on_key(ev, cx) {
                    cx.notify();
                }
            }))
            .on_scroll_wheel(cx.listener(move |this, ev: &ScrollWheelEvent, _, cx| {
                // A touch that reached the grid means the device has one: the
                // key bar belongs on it.
                if matches!(ev.touch_phase, gpui::TouchPhase::Started) {
                    this.touch = true;
                }
                let row_height = this
                    .cell()
                    .map(|cell| cell.height)
                    .unwrap_or(this.font_size);
                let delta = ev.delta.pixel_delta(px(row_height));
                let lines = (f32::from(delta.y) / row_height).round() as i64;
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
                div().flex().flex_row().flex_1().child(grid).child(
                    div().id("scrollbar").w(px(10.0)).h_full().relative().child(
                        div()
                            .id("thumb")
                            .w_full()
                            .h(px(thumb_h * grid_height / 100.0))
                            .rounded_full()
                            .bg(theme.surface_hover)
                            .absolute()
                            .top(px(thumb_top * grid_height / 100.0)),
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
    use gpui::Entity;
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
            linear_map_keystroke(&ev("enter", false, false, false)).as_deref(),
            Some("enter")
        );
        assert_eq!(
            linear_map_keystroke(&ev("escape", false, false, false)).as_deref(),
            Some("esc")
        );
        assert_eq!(
            linear_map_keystroke(&ev("up", false, false, false)).as_deref(),
            Some("up")
        );
        assert_eq!(
            linear_map_keystroke(&ev("tab", true, false, false)).as_deref(),
            Some("shift+tab")
        );
        assert_eq!(
            linear_map_keystroke(&ev("c", false, true, false)).as_deref(),
            Some("ctrl+c")
        );
        assert_eq!(
            linear_map_keystroke(&ev("backspace", false, false, true)).as_deref(),
            Some("alt+backspace")
        );
    }

    #[test]
    fn modifier_only_and_platform_keys_are_dropped() {
        assert!(linear_map_keystroke(&ev("control", false, true, false)).is_none());
        let mut platform = ev("c", false, false, false);
        platform.keystroke.modifiers.platform = true;
        assert!(linear_map_keystroke(&platform).is_none());
    }

    #[test]
    fn plain_letters_stay_plain() {
        assert_eq!(
            linear_map_keystroke(&ev("a", true, false, false)).as_deref(),
            Some("a")
        );
    }

    /// The same keystroke with the platform (Cmd) modifier held.
    fn command_ev(key: &str) -> KeyDownEvent {
        let mut event = ev(key, false, false, false);
        event.keystroke.modifiers.platform = true;
        event
    }

    #[test]
    fn ctrl_v_pastes_the_clipboard_instead_of_sending_a_control_byte() {
        assert_eq!(
            key_action(&ev("v", false, true, false), false, false),
            KeyAction::Paste,
            "Ctrl+V is a paste, not the SYN byte"
        );
        assert_eq!(
            key_action(&command_ev("v"), false, false),
            KeyAction::Paste,
            "Cmd+V pastes too"
        );
    }

    #[test]
    fn the_find_bar_owns_paste_while_it_is_open() {
        assert_eq!(
            key_action(&ev("v", false, true, false), true, false),
            KeyAction::FindKey,
            "the find bar keeps the keyboard, so nothing reaches the agent"
        );
    }

    #[test]
    fn cmd_c_copies_the_selection_like_ctrl_c_while_a_bare_ctrl_c_interrupts() {
        assert_eq!(
            key_action(&command_ev("c"), false, true),
            KeyAction::Copy,
            "Cmd+C with a selection reaches the clipboard"
        );
        assert_eq!(
            key_action(&ev("c", false, true, false), false, true),
            KeyAction::Copy,
            "Ctrl+C with a selection still copies"
        );
        assert_eq!(
            key_action(&ev("c", false, true, false), false, false),
            KeyAction::Interrupt,
            "Ctrl+C with no selection still interrupts the agent"
        );
        assert_eq!(
            key_action(&command_ev("c"), false, false),
            KeyAction::Ignored,
            "Cmd+C without a selection is not terminal input"
        );
    }

    #[test]
    fn a_selection_dragged_upward_copies_the_same_text_as_one_dragged_downward() {
        let mut screen = ScreenState::new(4, 20);
        screen.process("alpha\r\nbravo\r\ncharlie\r\n");
        let anchor = (2, 2);
        let cursor = (0, 1);

        let (start, end) = ordered_selection(anchor, cursor);
        let upward = screen.selection(start.0, start.1, end.0, end.1);
        let (start, end) = ordered_selection(cursor, anchor);
        let downward = screen.selection(start.0, start.1, end.0, end.1);

        assert!(
            screen
                .selection(anchor.0, anchor.1, cursor.0, cursor.1)
                .is_empty(),
            "the core rejects a range whose start is after its end"
        );
        assert_eq!(
            upward, downward,
            "the drag direction must not change the copied text"
        );
        assert!(!upward.is_empty(), "the selection has text: {upward:?}");
    }

    /// The terminal the test drives.
    struct TerminalRoot {
        view: Entity<TerminalPane>,
    }

    impl Render for TerminalRoot {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            self.view.clone()
        }
    }

    #[gpui::test]
    fn the_grid_the_key_bar_and_the_find_bar_are_on_screen(cx: &mut gpui::TestAppContext) {
        let (root, cx) = cx.add_window_view(|_, cx| TerminalRoot {
            view: cx.new(TerminalPane::new),
        });
        // The key bar is a touch affordance, and the find bar is open.
        cx.update(|_window, app| {
            let view = root.read(app).view.clone();
            view.update(app, |view, _cx| {
                view.touch = true;
                view.find_open = true;
            });
        });
        cx.update(|window, cx| window.draw(cx).clear());

        assert!(cx.debug_bounds("grid").is_some(), "the grid is on screen");
        assert!(
            cx.debug_bounds("key-bar").is_some(),
            "the key bar is on screen"
        );
        assert!(
            cx.debug_bounds("find-bar").is_some(),
            "the find bar is on screen"
        );
    }

    #[gpui::test]
    fn the_grid_learns_its_own_box_from_the_layout(cx: &mut gpui::TestAppContext) {
        let (root, cx) = cx.add_window_view(|_, cx| TerminalRoot {
            view: cx.new(TerminalPane::new),
        });
        cx.update(|window, cx| window.draw(cx).clear());

        cx.update(|_window, app| {
            let view = root.read(app).view.clone();
            let bounds = view.read(app).grid_bounds.get().expect("the grid's box");
            assert!(
                f32::from(bounds.size.width) > 0.0,
                "the grid has a width of its own: {bounds:?}"
            );
            assert!(
                f32::from(bounds.size.height) > 0.0,
                "the grid has a height of its own: {bounds:?}"
            );
        });
    }

    #[gpui::test]
    fn the_terminal_font_measures_a_monospace_cell(cx: &mut gpui::TestAppContext) {
        let cx = cx.add_empty_window();
        let families = terminal_font_families("");
        let Some(font) = measure_terminal_font(cx.text_system(), &families, px(DEFAULT_FONT_PX))
        else {
            // A machine with no monospace font at all: the grid falls back to
            // the system font. Nothing to measure here.
            return;
        };
        assert!(font.cell.width > 0.0, "the cell has a width");
        assert!(font.cell.height > 0.0, "the cell has a height");

        let text_system = cx.text_system();
        let id = text_system.resolve_font(&gpui::font(font.family.clone()));
        let narrow = text_system
            .advance(id, px(DEFAULT_FONT_PX), 'i')
            .expect("the advance of a narrow glyph");
        let wide = text_system
            .advance(id, px(DEFAULT_FONT_PX), 'W')
            .expect("the advance of a wide glyph");
        assert_eq!(
            narrow.width, wide.width,
            "every glyph sits on the same advance: the columns line up"
        );
    }

    #[gpui::test]
    fn the_terminal_reads_the_clipboard_when_it_pastes(cx: &mut gpui::TestAppContext) {
        let view = cx.update(|app| app.new(TerminalPane::new));
        cx.write_to_clipboard(gpui::ClipboardItem::new_string("pasted words".to_string()));

        let text = cx.update(|app| view.update(app, |view, cx| view.clipboard_text(cx)));

        assert_eq!(
            text.as_deref(),
            Some("pasted words"),
            "the paste path reads what the reader copied"
        );
    }

    #[gpui::test]
    fn new_output_keeps_the_reader_where_they_scrolled(cx: &mut gpui::TestAppContext) {
        let view = cx.update(|app| app.new(TerminalPane::new));
        cx.update(|app| {
            view.update(app, |view, _cx| {
                for line in 0..80 {
                    view.feed(&format!("line {line}\r\n"));
                }
                assert!(
                    view.screen.scrollback_len() > 10,
                    "the feed built scrollback: {}",
                    view.screen.scrollback_len()
                );
                view.screen.scroll_to(10);

                view.feed("late output\r\n");

                assert_eq!(
                    view.screen.offset_from_bottom(),
                    10,
                    "the viewport stayed where the reader put it"
                );
            });
        });
    }

    #[gpui::test]
    fn new_output_follows_when_the_reader_is_at_the_bottom(cx: &mut gpui::TestAppContext) {
        let view = cx.update(|app| app.new(TerminalPane::new));
        cx.update(|app| {
            view.update(app, |view, _cx| {
                for line in 0..80 {
                    view.feed(&format!("line {line}\r\n"));
                }
                assert_eq!(view.screen.offset_from_bottom(), 0);

                view.feed("more output\r\n");

                assert_eq!(
                    view.screen.offset_from_bottom(),
                    0,
                    "a reader at the live bottom keeps following the output"
                );
            });
        });
    }
}
