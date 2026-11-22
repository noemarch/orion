//! Terminal pane view — GPUI rendering of the PTY grid with input.
//!
//! PTY output arrives from the engine via `feed()`; each feed mutates the grid
//! and the caller notifies the view so GPUI repaints. Keyboard input flows back
//! to the pane through `PaneHandle::write` (literal text) and
//! `PaneHandle::keys` (logical chords — the same names `encode_key` in
//! `engine::pane` accepts).
//!
//! Surfaces ported from the web client (P1 — terminal truth):
//! - keyboard input on the focused grid (the soft-keyboard-free desktop path)
//! - a touch-capable key bar (Esc/Tab/Ctrl/Alt/Shift/arrows/Enter, 44px targets)
//! - scrollback navigation (wheel + scrollbar thumb; `ScreenState::scroll_to`)
//! - a find bar (`ScreenState::find`, jump-to-match, match count)
//! - mouse selection (`ScreenState::selection`, Ctrl/Cmd+C copies)

use crate::settings::KeyBarExtras;
use crate::shell_model::ShellLayout;
use crate::terminal_font::{is_symbol_family, terminal_font_families};
use crate::terminal_grid::{
    build_row, terminal_debug, CellScratch, CellSize, GridRow, GridSnapshot, RowCacheKey,
    RowOverlays, TerminalGrid,
};
use crate::theme::Theme;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::status_bar::StatusBar;
use gpui_kit::component::{Selectable, ThemeStyled};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use gpui_kit::{
    div, px, rems, white, Bounds, ClipboardItem, Context, FocusHandle, Font, FontFallbacks,
    IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Render, ScrollEvent, SharedString, StyledText, TextRun, TextStyle, Window, WindowTextSystem,
};
use orion_core::runtime::PaneHandle;
use orion_core::terminal_engine::{
    encode_sgr_mouse, new_engine, sgr_mods, ActiveScreen, Key, TermCell, TermColor, TerminalEngine,
};
use std::cell::Cell;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The default terminal font size, from Orbit's `DEFAULT_SETTINGS`.
const DEFAULT_FONT_PX: f32 = 13.0;
/// Touch target height for key-bar and find-bar controls (DESIGN.md minimum).
const TARGET_PX: f32 = 44.0;
/// Resize requests are debounced this long before reaching the PTY.
pub const RESIZE_DEBOUNCE_MS: u64 = 50;

/// The terminal's font: the exact font the grid paints with, and the cell
/// measured from that same font's real, shaped metrics.
#[derive(Debug, Clone)]
struct TerminalFont {
    font: Font,
    cell: CellSize,
}

/// Whether a face can carry a terminal grid.
///
/// Its letters must share one advance, and the box-drawing and block glyphs an
/// agent draws its frames with must sit on that same advance. A CJK face
/// satisfies the first rule — its ASCII is half width — but draws those glyphs
/// FULL width, so the agent's art would come out twice as wide as the grid and
/// every border would look doubled.
pub fn face_carries_a_grid(
    ascii_advance: f32,
    wide_advance: f32,
    glyph_advances: &[Option<f32>],
) -> bool {
    if (ascii_advance - wide_advance).abs() > 0.01 {
        return false;
    }
    glyph_advances.iter().all(|advance| match advance {
        Some(advance) => (advance - ascii_advance).abs() <= 0.01,
        // A glyph the face cannot even be measured for is drawn by a
        // fallback, which is exactly what this rule exists to refuse.
        None => false,
    })
}

/// The glyphs an agent draws its frames with: a box edge, a filled block and a
/// half block.
const GRID_GLYPHS: [char; 3] = ['\u{2500}', '\u{2588}', '\u{2580}'];

/// Fallback faces used to paint terminal text. A face whose grid glyphs do not
/// share the primary face's advance would shift later glyphs off their cells;
/// only symbol faces are kept unconditionally, for icons `force_width` can snap.
fn grid_fallback_families(
    families: &[String],
    installed: &[String],
    cell_width: f32,
    advance: impl Fn(&str, char) -> Option<f32>,
) -> Vec<String> {
    let mut out = Vec::new();
    for family in families {
        if !installed
            .iter()
            .any(|name| name.eq_ignore_ascii_case(family))
        {
            continue;
        }
        if is_symbol_family(family) {
            out.push(family.clone());
            continue;
        }
        let Some(wide) = advance(family, 'W') else {
            continue;
        };
        let glyphs: Vec<Option<f32>> = GRID_GLYPHS
            .iter()
            .map(|glyph| advance(family, *glyph))
            .collect();
        if face_carries_a_grid(cell_width, wide, &glyphs) {
            out.push(family.clone());
        }
    }
    out
}

/// A line that carries a plain letter, the grid's box glyphs and a Nerd icon,
/// so shaping it exercises every face the painted font can fall back to.
const CELL_PROBE_TEXT: &str = "i\u{2500}\u{2588}\u{2580}\u{E0B0}";

/// A cell must be at least as tall as the line box it paints, or GPUI gives
/// that line negative top padding and its glyphs spill into the row above.
fn cell_height_from_metrics(ascent: f32, descent: f32) -> f32 {
    (ascent + descent).ceil().max(1.0)
}

/// Whole cells that fit in an extent, shared by the grid's geometry and the
/// size reported to the agent.
fn rows_from_height(height: f32, cell_extent: f32) -> u16 {
    (height / cell_extent.max(1.0)).floor().clamp(1.0, 10_000.0) as u16
}

/// Measure the terminal's font: the family the reader configured, or the first
/// one that is installed AND can carry a grid.
///
/// gpui resolves a family that is not installed to the system UI font, which is
/// proportional: a grid drawn with it goes ragged, and the agent's menu stops
/// lining up.
fn measure_terminal_font(
    text_system: &WindowTextSystem,
    families: &[String],
    font_size: Pixels,
) -> Option<TerminalFont> {
    let installed = text_system.all_font_names();
    for family in families {
        // A symbols-only face stays in the stack for glyph fallback, but its
        // own advance is meaningless for a text cell: never size the grid by it.
        if is_symbol_family(family) {
            continue;
        }
        if !installed
            .iter()
            .any(|name| name.eq_ignore_ascii_case(family))
        {
            continue;
        }
        let id = text_system.resolve_font(&gpui_kit::font(family.clone()));
        let Ok(narrow) = text_system.advance(id, font_size, 'i') else {
            continue;
        };
        let Ok(wide) = text_system.advance(id, font_size, 'W') else {
            continue;
        };
        // A cell is one ASCII character wide. A wide character takes two of
        // them, which is the grid's business, not the font's.
        let cell_width = f32::from(narrow.width);
        let glyph_advances: Vec<Option<f32>> = GRID_GLYPHS
            .iter()
            .map(|glyph| {
                text_system
                    .advance(id, font_size, *glyph)
                    .ok()
                    .map(|advance| f32::from(advance.width))
            })
            .collect();
        if !face_carries_a_grid(cell_width, f32::from(wide.width), &glyph_advances) {
            continue;
        }
        // The same font, with the whole fallback stack, is what paint shapes
        // each row with. Measuring a plain id would miss the taller line box
        // of a fallback face used for a box glyph or a Nerd icon.
        let fallback_families =
            grid_fallback_families(families, &installed, cell_width, |name, glyph| {
                let id = text_system.resolve_font(&gpui_kit::font(name.to_string()));
                text_system
                    .advance(id, font_size, glyph)
                    .ok()
                    .map(|advance| f32::from(advance.width))
            });
        let shared_font = Font {
            family: family.into(),
            features: FontFeatures::default(),
            fallbacks: Some(FontFallbacks::from_fonts(fallback_families.clone())),
            weight: FontWeight::NORMAL,
            style: FontStyle::Normal,
        };
        let probe = text_system.shape_line(
            SharedString::new(CELL_PROBE_TEXT),
            font_size,
            &[TextRun {
                len: CELL_PROBE_TEXT.len(),
                font: shared_font.clone(),
                color: white(),
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        );
        return Some(TerminalFont {
            font: shared_font,
            cell: CellSize {
                width: cell_width,
                height: cell_height_from_metrics(f32::from(probe.ascent), f32::from(probe.descent)),
            },
        });
    }
    None
}

/// Debounce state for the find bar: a search fires only after `delay` has elapsed since the
/// last keystroke, so a burst of typing costs at most one grid search per quiet window.
struct FindDebouncer {
    delay: Duration,
    last_keystroke: Option<Instant>,
    fired_for: Option<Instant>,
}

impl FindDebouncer {
    fn new(delay: Duration) -> Self {
        Self {
            delay,
            last_keystroke: None,
            fired_for: None,
        }
    }

    fn keystroke(&mut self, now: Instant) {
        self.last_keystroke = Some(now);
    }

    fn due(&self, now: Instant) -> bool {
        match self.last_keystroke {
            Some(t) => now.duration_since(t) >= self.delay && self.fired_for != Some(t),
            None => false,
        }
    }

    fn mark_fired(&mut self) {
        self.fired_for = self.last_keystroke;
    }
}

const FIND_DEBOUNCE_MS: u64 = 150;

/// How often the "Starting …" placeholder redraws its elapsed seconds.
const PLACEHOLDER_TICK_MS: u64 = 100;

/// Map a y coordinate inside the scrollbar track to a scroll offset. The thumb
/// travels `track_h - thumb_h` pixels: the top of that travel is offset 0 and
/// the bottom is `max_offset`. A track with no travel maps to 0.
fn scroll_offset_for_thumb(y: f32, track_h: f32, thumb_h: f32, max_offset: usize) -> usize {
    let travel = track_h - thumb_h;
    if track_h <= 0.0 || thumb_h <= 0.0 || travel <= 0.0 || max_offset == 0 {
        return 0;
    }
    let fraction = (y / travel).clamp(0.0, 1.0);
    (fraction * max_offset as f32).round() as usize
}

/// Whether the pane's process is running or has ended.
///
/// The exit code rides with the state, so "not exited" can never be mistaken
/// for "exited with an unknown code": the engine's `pty-exit` frame carries a
/// code that is sometimes absent, and the marker says so in words instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessExit {
    Running,
    Exited(Option<i32>),
}

/// The status bar's left text: the exit marker once the process ended,
/// otherwise the polled agent status. The scroll suffix rides on either, so
/// reading history never hides why the pane stopped.
fn status_line_text(status: &str, exit: ProcessExit, offset: usize, max_offset: usize) -> String {
    let mut line = match exit {
        ProcessExit::Running => status.to_string(),
        ProcessExit::Exited(Some(code)) => format!("[process exited with code {code}]"),
        ProcessExit::Exited(None) => "[process exited]".to_string(),
    };
    if offset > 0 {
        line.push_str(&format!("  scroll {offset}/{max_offset}"));
    }
    line
}

/// Whether the pane offers a Restart control: only a process that has ended
/// can be brought back. A live pane has nothing to restart.
fn restart_available(exit: ProcessExit) -> bool {
    matches!(exit, ProcessExit::Exited(_))
}

/// Whether a click on Restart starts a restart. One request at a time: a
/// second click while the first is in flight is ignored, not queued.
fn restart_should_send(exit: ProcessExit, pending: bool) -> bool {
    restart_available(exit) && !pending
}

/// What the terminal asks the app to do. The engine lives on the app, so a
/// Restart click leaves the view as an event instead of a blocking call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalPaneEvent {
    /// The reader clicked Restart on an exited pane.
    RestartRequested { pane_id: String },
}

/// Map a GPUI scroll event to signed rows scrolled UP into history: positive
/// means wheel up (toward older output), negative means wheel down (toward the
/// live prompt). Both `Lines` and `Pixels` deltas preserve the platform sign,
/// so one formula serves both. On Windows, Shift redirects the vertical wheel
/// into `delta.x`, so read that axis when Shift is held.
fn wheel_rows_f64(ev: &ScrollEvent, row_height: f32) -> f64 {
    if row_height <= 0.0 {
        return 0.0;
    }
    let delta = ev.delta.pixel_delta(px(row_height));
    let value = f32::from(if ev.modifiers.shift { delta.x } else { delta.y });
    f64::from(value / row_height)
}

fn wheel_rows_up(ev: &ScrollEvent, row_height: f32) -> i64 {
    wheel_rows_f64(ev, row_height).round() as i64
}

fn accumulate_wheel_rows(delta: f64, fraction: f64) -> (i64, f64) {
    let total = delta + fraction;
    let rows = total.trunc() as i64;
    (rows, total - rows as f64)
}

fn hash_color(color: TermColor, hasher: &mut DefaultHasher) {
    match color {
        TermColor::Default => 0u8.hash(hasher),
        TermColor::Indexed(index) => {
            1u8.hash(hasher);
            index.hash(hasher);
        }
        TermColor::Rgb(red, green, blue) => {
            2u8.hash(hasher);
            red.hash(hasher);
            green.hash(hasher);
            blue.hash(hasher);
        }
    }
}

fn row_content_hash(cells: &[TermCell]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for cell in cells {
        cell.text.hash(&mut hasher);
        hash_color(cell.fg, &mut hasher);
        hash_color(cell.bg, &mut hasher);
        cell.bold.hash(&mut hasher);
        cell.dim.hash(&mut hasher);
        cell.italic.hash(&mut hasher);
        cell.underline.hash(&mut hasher);
        cell.inverse.hash(&mut hasher);
        cell.wide.hash(&mut hasher);
        cell.wide_continuation.hash(&mut hasher);
    }
    hasher.finish()
}

/// What one wheel event does, decided purely from the emulator's live mode
/// state so the priority is testable without a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WheelAction {
    Scrollback,
    Sgr,
    Arrow,
    Ignore,
}

fn wheel_action(
    shift: bool,
    screen: ActiveScreen,
    tracking: bool,
    alternate_scroll: bool,
) -> WheelAction {
    if shift {
        return WheelAction::Scrollback;
    }
    if tracking {
        return WheelAction::Sgr;
    }
    if screen == ActiveScreen::Alternate {
        return if alternate_scroll {
            WheelAction::Arrow
        } else {
            WheelAction::Ignore
        };
    }
    WheelAction::Scrollback
}

fn arrow_bytes(up: bool, application_cursor: bool) -> &'static str {
    match (up, application_cursor) {
        (true, true) => "\x1bOA",
        (true, false) => "\x1b[A",
        (false, true) => "\x1bOB",
        (false, false) => "\x1b[B",
    }
}

fn sgr_modifier_bits(mods: &gpui_kit::Modifiers) -> u8 {
    let mut bits = 0;
    if mods.shift {
        bits |= sgr_mods::SHIFT;
    }
    if mods.alt {
        bits |= sgr_mods::ALT;
    }
    if mods.control {
        bits |= sgr_mods::CTRL;
    }
    bits
}

pub struct TerminalPane {
    screen: Box<dyn TerminalEngine>,
    focus_handle: FocusHandle,
    has_focus: bool,
    pane: Option<Arc<dyn PaneHandle>>,
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
    find_debouncer: FindDebouncer,
    // Selection state in content coordinates (rows from oldest history row).
    sel_anchor: Option<(usize, usize)>,
    sel_cursor: Option<(usize, usize)>,
    selecting: bool,
    /// Whether a mouse-tracking press was forwarded to the PTY, so a release is
    /// only sent when there was a matching press.
    mouse_forwarding_press: bool,
    /// Whether the attached TUI has ever enabled mouse reporting.
    mouse_reporting_seen: bool,
    /// Fractional rows left over from a pixel wheel delta, carried into the next
    /// event so a slow touchpad gesture still moves the viewport.
    wheel_fraction: f64,
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
    /// Whether the scrollbar thumb is being dragged.
    scrollbar_dragging: bool,
    /// The scroll offset when the current thumb drag began.
    drag_scroll_base: usize,
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
    /// Bumped on every held-back resize so only the newest one-shot flush
    /// timer sends the final size.
    resize_flush_generation: u64,
    /// Whether this view only watches the pane. An observing view follows its
    /// own box but never resizes the PTY: the pane's size belongs to the view
    /// that owns it, and the split canvas shares that PTY with the main one.
    observe: bool,
    /// Whether the device has shown a touch: the key bar is a touch
    /// affordance.
    touch: bool,
    /// The grid takes the focus on the next draw.
    focus_wanted: bool,
    /// Cached viewport cells, keyed by (revision, scroll offset) so a frame
    /// that paints the same content reuses the allocation instead of walking
    /// the grid again. Shared by reference: the grid element holds the same
    /// allocation, so a frame clones no cells.
    cached_viewport_revision: u64,
    cached_viewport_offset: usize,
    cached_viewport_cells: Rc<[(usize, Vec<TermCell>)]>,
    /// Bumped on every find-match refresh (and on the empty-query clear), so
    /// the row cache can tell "same match count, different spans" apart.
    find_revision: u64,
    /// Per-row render cache, addressed by content rather than visible-row slot:
    /// output streaming shifts every visible row down by one, and slot indexing
    /// would rebuild all of them. A row with a matching content and overlay key
    /// reuses its text, styled spans and background spans.
    row_cache: Vec<Rc<GridRow>>,
    /// How many rows were rebuilt during renders. Read by tests to prove the
    /// cache-hit path does not rebuild rows.
    rows_built: u64,
    /// The status line the render path draws. `pane.status()` is a
    /// synchronous socket round-trip on the daemon path (30s timeout), so
    /// render never calls it: the status poller (app.rs) refreshes this
    /// cache off the UI thread and render reads only the cache.
    status_line: String,
    /// The pane process's exit state, cached from the engine's `pty-exit`
    /// push. Render draws the marker and the Restart control from this cache
    /// and never asks the pane.
    process_exit: ProcessExit,
    /// A Restart request is in flight: the control waits instead of sending
    /// a second request.
    restart_pending: bool,
    /// The last Restart failure, in plain words. Cleared by a fresh exit or a
    /// successful restart.
    restart_error: Option<String>,
    /// Per-row scratch for `build_row`, reused across rows and frames so a
    /// steady-state frame does not rebuild the column-span or run buffers.
    cell_scratch: CellScratch,
    /// Whether the grid showed readable content on the last frame. The
    /// placeholder uses it, and the startup timeline marks the first frame
    /// that had something on it.
    grid_has_content: bool,
    /// The agent the grid belongs to, in the reader's words ("Claude Code").
    /// The placeholder names it, so a slow agent reads as starting rather
    /// than as a frozen window.
    agent_label: String,
    /// When the pane was attached: the placeholder counts from here.
    started_at: Instant,
    /// Whether the placeholder's ticker is already running.
    placeholder_ticking: bool,
    /// Whether the debug repaint ticker is already running.
    repaint_ticking: bool,
    /// Whether the startup timeline already marked the first frame with
    /// content.
    nonempty_traced: bool,
    /// Whether the startup timeline already marked the first resize.
    resize_traced: bool,
}

impl gpui_kit::EventEmitter<TerminalPaneEvent> for TerminalPane {}

impl TerminalPane {
    pub fn new(cx: &mut Context<Self>) -> Self {
        orion_core::trace::record(orion_core::trace::Event::TerminalViewCreated);
        Self {
            screen: new_engine(120, 32),
            focus_handle: cx.focus_handle(),
            has_focus: false,
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
            find_debouncer: FindDebouncer::new(Duration::from_millis(FIND_DEBOUNCE_MS)),
            sel_anchor: None,
            sel_cursor: None,
            selecting: false,
            mouse_forwarding_press: false,
            mouse_reporting_seen: false,
            wheel_fraction: 0.0,
            char_w: 7.81, // refined from the measured font on the first frame.
            last_cols: 120,
            last_rows: 32,
            last_resize: Instant::now() - Duration::from_millis(RESIZE_DEBOUNCE_MS),
            font_size: DEFAULT_FONT_PX,
            grid_bounds: Rc::new(Cell::new(None)),
            scrollbar_dragging: false,
            drag_scroll_base: 0,
            drawn_bounds: None,
            font: None,
            font_setting: String::new(),
            pending_resize: None,
            resize_flush_generation: 0,
            observe: false,
            touch: false,
            focus_wanted: false,
            cached_viewport_revision: 0,
            cached_viewport_offset: 0,
            cached_viewport_cells: Rc::from(Vec::new()),
            find_revision: 0,
            row_cache: Vec::new(),
            rows_built: 0,
            status_line: "starting agent…".to_string(),
            process_exit: ProcessExit::Running,
            restart_pending: false,
            restart_error: None,
            cell_scratch: CellScratch::default(),
            grid_has_content: false,
            nonempty_traced: false,
            agent_label: "the agent".to_string(),
            started_at: Instant::now(),
            placeholder_ticking: false,
            repaint_ticking: false,
            resize_traced: false,
        }
    }

    /// Attach the live pane this view drives (set once the engine spawns it).
    /// Switching panes clears the grid so the replay of the new pane is not
    /// drawn over the previous pane's scrollback.
    pub fn set_pane(&mut self, pane: Arc<dyn PaneHandle>) {
        let changed = self
            .pane
            .as_ref()
            .is_none_or(|current| current.id() != pane.id());
        if changed {
            self.reset_for_new_pane();
        }
        self.pane = Some(Arc::clone(&pane));
        // The pane's TUI may have enabled mouse reporting before we attached,
        // so its DECSET never reached this grid. A focus-in makes TUIs that
        // track focus re-emit every active mode, including mouse reporting.
        let _ = pane.write("\x1b[I");
    }

    /// Attach the pane a Restart produced. The id is the same, but the
    /// process is not: the old grid, status line and exit marker start over.
    pub fn set_restarted_pane(&mut self, pane: Arc<dyn PaneHandle>) {
        self.reset_for_new_pane();
        self.pane = Some(pane);
    }

    fn reset_for_new_pane(&mut self) {
        self.mouse_reporting_seen = false;
        self.screen = new_engine(self.last_cols, self.last_rows);
        self.cached_viewport_revision = 0;
        self.cached_viewport_offset = 0;
        self.cached_viewport_cells = Rc::from(Vec::new());
        self.row_cache.clear();
        self.find_matches.clear();
        self.find_current = None;
        self.sel_anchor = None;
        self.sel_cursor = None;
        self.status_line = "starting agent…".to_string();
        self.process_exit = ProcessExit::Running;
        self.restart_pending = false;
        self.restart_error = None;
        // A new pane is a new agent: the placeholder counts from here.
        self.started_at = Instant::now();
        self.grid_has_content = false;
    }

    pub fn is_running(&self) -> bool {
        matches!(self.process_exit, ProcessExit::Running)
    }

    /// Cache the exit state the engine pushed for `pane_id`. Only the pane on
    /// screen takes a marker; a push for a pane this view left is ignored.
    pub fn set_exit_code(&mut self, pane_id: &str, code: Option<i32>) -> bool {
        if self.pane.as_ref().map(|pane| pane.id()) != Some(pane_id) {
            return false;
        }
        let next = ProcessExit::Exited(code);
        if self.process_exit == next && self.restart_error.is_none() {
            return false;
        }
        self.process_exit = next;
        self.restart_pending = false;
        self.restart_error = None;
        true
    }

    /// Show a Restart failure as one plain sentence and let the control try
    /// again. There is no automatic retry.
    pub fn set_restart_error(&mut self, message: String) -> bool {
        self.restart_pending = false;
        if self.restart_error.as_deref() == Some(message.as_str()) {
            return false;
        }
        self.restart_error = Some(message);
        true
    }

    /// The request that was in flight has been answered: the control is
    /// clickable again even if the pane could not be re-attached.
    pub fn clear_restart_pending(&mut self) {
        self.restart_pending = false;
    }

    /// The agent's name, in the reader's words. The placeholder names it.
    pub fn set_agent_label(&mut self, label: &str) {
        if self.agent_label != label {
            self.agent_label = label.to_string();
        }
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

    /// Watch the pane without owning its size.
    ///
    /// The split canvas shares a pane's PTY with the main canvas: its grid
    /// follows its own box, but the agent keeps the size the view that owns
    /// the pane gave it.
    pub fn set_observe(&mut self, observe: bool) {
        if self.observe == observe {
            return;
        }
        self.observe = observe;
        if observe {
            // A size held back for the drag must not go out once the view
            // stops owning the PTY.
            self.pending_resize = None;
        }
    }

    /// Tell the agent about a new size, or hold it back until the drag has
    /// quieted down.
    ///
    /// The render is the only caller: the observing rule lives there, where
    /// the view decides whether the agent's size is its business. A held-back
    /// size also arms a one-shot timer so the final size reaches the agent
    /// even when nothing else repaints.
    fn request_resize(&mut self, cols: u16, rows: u16, cx: &mut Context<Self>) {
        if self.last_resize.elapsed() >= Duration::from_millis(RESIZE_DEBOUNCE_MS) {
            self.last_resize = Instant::now();
            self.pending_resize = None;
            self.send_resize(cols, rows);
            return;
        }
        self.pending_resize = Some((cols, rows));
        self.resize_flush_generation = self.resize_flush_generation.wrapping_add(1);
        let generation = self.resize_flush_generation;
        cx.spawn(async move |view, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(RESIZE_DEBOUNCE_MS))
                .await;
            let _ = view.update(cx, |view, _cx| {
                if view.resize_flush_generation != generation {
                    return;
                }
                view.flush_pending_resize();
            });
        })
        .detach();
    }

    /// Send a held-back resize, if any. Both the render path and the one-shot
    /// timer call this; taking the pending size first means only one sends it.
    fn flush_pending_resize(&mut self) {
        if self.observe {
            return;
        }
        if let Some((cols, rows)) = self.pending_resize.take() {
            self.last_resize = Instant::now();
            self.send_resize(cols, rows);
        }
    }

    /// Send a size to the agent's PTY.
    fn send_resize(&mut self, cols: u16, rows: u16) {
        if let Some(pane) = &self.pane {
            let _ = pane.resize(cols, rows);
        }
        if !self.resize_traced {
            self.resize_traced = true;
            let (grid_rows, grid_cols) = self.screen.size();
            orion_core::trace::record_with(
                orion_core::trace::Event::FirstResizeApplied,
                Some(format!("pty {cols}x{rows}, grid {grid_cols}x{grid_rows}")),
            );
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

    /// Debug-only repaint clock: `ORION_TERMINAL_DEBUG=repaint` asks for a frame
    /// every 60ms so the bypassed row cache is exercised without starving the
    /// machine.
    fn keep_repainting(&mut self, cx: &mut Context<Self>) {
        if !terminal_debug().full_repaint || self.repaint_ticking {
            return;
        }
        self.repaint_ticking = true;
        cx.spawn(async move |weak, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(60))
                .await;
            let alive = cx.update(|app| {
                weak.update(app, |view, cx| {
                    cx.notify();
                    view.repaint_ticking
                })
                .unwrap_or(false)
            });
            if !alive {
                break;
            }
        })
        .detach();
    }

    /// Keep the placeholder's clock moving.
    ///
    /// The elapsed seconds are drawn from the clock, so the frame has to come
    /// again while the placeholder is up. One ticker runs at a time and stops
    /// as soon as the grid has something on it.
    fn keep_placeholder_ticking(&mut self, cx: &mut Context<Self>) {
        if self.placeholder_ticking {
            return;
        }
        self.placeholder_ticking = true;
        cx.spawn(async move |weak, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(PLACEHOLDER_TICK_MS))
                .await;
            let keep_ticking = cx.update(|app| {
                weak.update(app, |view, cx| {
                    if view.grid_has_content {
                        view.placeholder_ticking = false;
                        return false;
                    }
                    cx.notify();
                    true
                })
                .unwrap_or(false)
            });
            if !keep_ticking {
                break;
            }
        })
        .detach();
    }

    /// Feed a chunk of PTY text into the grid.
    pub fn feed(&mut self, text: &str) {
        #[cfg(debug_assertions)]
        let feed_started = Instant::now();
        self.screen.write_output(text.as_bytes());
        adjust_selection_after_feed(
            &mut self.sel_anchor,
            &mut self.sel_cursor,
            self.screen.ring_wrapped(),
        );
        #[cfg(debug_assertions)]
        crate::perf::record_feed(feed_started.elapsed());
        // The reader's scroll position is theirs: the parser always reads the
        // live screen, and only the live bottom follows the new output.
    }

    /// Whether the grid shows anything the reader can read. Bytes alone do
    /// not count: the attachment replays the screen as escape sequences, so
    /// "something arrived" says nothing about "something is there".
    pub fn has_content(&self) -> bool {
        !self.screen.visible_text().trim().is_empty()
    }

    /// The current screen revision, so callers can tell whether feeding changed anything.
    pub fn revision(&self) -> u64 {
        self.screen.revision()
    }

    /// The text the reader sees: the grid's live rows, one per line.
    pub fn visible_text(&self) -> String {
        self.screen.visible_text()
    }

    /// Store the status line the render path draws. Called by the status
    /// poller (app.rs) off the UI thread; returns whether the value changed
    /// so the poller can notify only then.
    pub fn set_status_line(&mut self, status: String) -> bool {
        if self.status_line == status {
            return false;
        }
        self.status_line = status;
        true
    }

    fn send_chord(&self, chord: &str) {
        // The chord is encoded by the engine, not by the pane: the emulator
        // owns what a key means, and the bytes go to the PTY as they are.
        let bytes = self.screen.encode_key(&Key::Chord(chord.to_string()));
        if bytes.is_empty() {
            return;
        }
        let Ok(text) = std::str::from_utf8(&bytes) else {
            return;
        };
        if let Some(pane) = &self.pane {
            let _ = pane.write(text);
        }
    }

    fn send_text(&self, text: &str) {
        if let Some(pane) = &self.pane {
            let _ = pane.write(text);
        }
    }

    fn mouse_tracking(&mut self) -> bool {
        // TUIs that enable mouse reporting once can lose the DEC modes after a
        // focus change (ConPTY strips them). Keep forwarding once seen.
        if self.screen.mode(1000)
            || self.screen.mode(1002)
            || self.screen.mode(1003)
            || self.screen.mode(1006)
        {
            self.mouse_reporting_seen = true;
        }
        self.mouse_reporting_seen
    }

    fn sgr_report(button: u8, vis_row: usize, col: usize, press: bool, mods: u8) -> Vec<u8> {
        let col = u32::try_from(col).unwrap_or(0).saturating_add(1);
        let row = u32::try_from(vis_row).unwrap_or(0).saturating_add(1);
        let mut out = Vec::with_capacity(24);
        encode_sgr_mouse(&mut out, button, col, row, press, mods);
        out
    }

    /// A wheel event emits exactly one press report and no release: a TUI can
    /// read a release as a button event and select something under the pointer.
    fn wheel_sgr_reports(rows_up: i64) -> [(u8, bool); 1] {
        [(if rows_up > 0 { 64 } else { 65 }, true)]
    }

    fn debug_mouse_bytes(label: &str, button: u8, out: &[u8]) {
        if !terminal_debug().mouse {
            return;
        }
        let hex: String = out.iter().map(|byte| format!("{byte:02x} ")).collect();
        let text = String::from_utf8_lossy(out);
        eprintln!("ORION mouse {label}: button={button} hex=[{hex}] text={text:?}");
    }

    fn forward_sgr(&self, button: u8, vis_row: usize, col: usize, press: bool, mods: u8) {
        let out = Self::sgr_report(button, vis_row, col, press, mods);
        Self::debug_mouse_bytes(if press { "press" } else { "release" }, button, &out);
        let text = String::from_utf8_lossy(&out);
        self.send_text(text.as_ref());
    }

    fn handle_scroll(&mut self, ev: &ScrollEvent, row_height: f32, cx: &mut Context<Self>) {
        let action = wheel_action(
            ev.modifiers.shift,
            self.screen.active_screen(),
            self.mouse_tracking(),
            self.screen.mode(1007),
        );
        match action {
            WheelAction::Scrollback => {
                let (rows_up, fraction) =
                    accumulate_wheel_rows(wheel_rows_f64(ev, row_height), self.wheel_fraction);
                self.wheel_fraction = fraction;
                if rows_up == 0 {
                    return;
                }
                let max = self.screen.scrollback_len();
                let next = (self.screen.offset_from_bottom() as i64 + rows_up).clamp(0, max as i64)
                    as usize;
                if next != self.screen.offset_from_bottom() {
                    self.screen.scroll_to(next);
                    cx.notify();
                }
            }
            WheelAction::Sgr => {
                let rows_up = wheel_rows_up(ev, row_height);
                if rows_up == 0 {
                    return;
                }
                let (vis_row, col) = self.pos_to_cell(ev.position);
                let mods = sgr_modifier_bits(&ev.modifiers);
                for (button, press) in Self::wheel_sgr_reports(rows_up) {
                    self.forward_sgr(button, vis_row, col, press, mods);
                }
            }
            WheelAction::Arrow => {
                let rows_up = wheel_rows_up(ev, row_height);
                if rows_up == 0 {
                    return;
                }
                let bytes = arrow_bytes(rows_up > 0, self.screen.mode(1));
                self.send_text(bytes);
            }
            WheelAction::Ignore => {}
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
            KeyAction::FindKey => self.on_find_key(ev, cx),
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
    fn on_find_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        match ev.keystroke.key.to_ascii_lowercase().as_str() {
            "escape" => self.find_open = false,
            "enter" => self.find_next(),
            "backspace" => {
                self.find_query.pop();
                self.schedule_find_refresh(cx);
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
                    self.schedule_find_refresh(cx);
                }
            }
            _ => {}
        }
        true
    }

    /// Debounce the grid search: an empty query clears matches immediately, anything else
    /// waits `FIND_DEBOUNCE_MS` after the last keystroke. Spawned timers that wake before the
    /// quiet window elapses do nothing, so a burst of typing costs at most one search.
    fn schedule_find_refresh(&mut self, cx: &mut Context<Self>) {
        if self.find_query.is_empty() {
            self.find_matches.clear();
            self.find_current = None;
            self.find_revision += 1;
            cx.notify();
            return;
        }
        self.find_debouncer.keystroke(Instant::now());
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(FIND_DEBOUNCE_MS))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.find_debouncer.due(Instant::now()) {
                    this.find_debouncer.mark_fired();
                    this.refresh_find_matches();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn refresh_find_matches(&mut self) {
        self.find_revision += 1;
        self.find_matches = self
            .screen
            .find(&self.find_query, 1024)
            .iter()
            .map(|found| {
                (
                    found.start_row,
                    found.start_col,
                    found.end_row,
                    found.end_col,
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

    /// Bring the viewport-cell cache up to date with the screen's current
    /// (revision, offset). A frame that paints the same content reuses the
    /// allocation instead of walking the grid again.
    fn refresh_viewport_cache(&mut self) {
        let revision = self.screen.revision();
        let offset = self.screen.offset_from_bottom();
        if revision == self.cached_viewport_revision
            && offset == self.cached_viewport_offset
            && !self.cached_viewport_cells.is_empty()
        {
            return;
        }
        let base = self.screen.scrollback_len().saturating_sub(offset);
        let rows: Vec<(usize, Vec<TermCell>)> = self
            .screen
            .snapshot(offset)
            .rows
            .into_iter()
            .enumerate()
            .map(|(index, cells)| (base + index, cells))
            .collect();
        self.cached_viewport_cells = Rc::from(rows);
        self.cached_viewport_revision = revision;
        self.cached_viewport_offset = offset;
    }

    /// A handle on the cached viewport rows: (content_row, cells) for the
    /// visible window. Cloning it clones no cells.
    ///
    /// The rows come from the core's cheap viewport read: it does not clone the
    /// screen or walk the ten-thousand row ring, so the grid can be drawn every
    /// frame. Same rows the plain read gives, with every cell's styling.
    ///
    /// Call `refresh_viewport_cache` first; the returned allocation is the one
    /// the frame paints, and it is only rebuilt when the content or the scroll
    /// offset changes.
    fn viewport_rows(&self) -> Rc<[(usize, Vec<TermCell>)]> {
        Rc::clone(&self.cached_viewport_cells)
    }

    /// Viewport rows of the current content as cells: (content_row, cells) for
    /// the visible window.
    #[cfg(test)]
    fn viewport_cells(&self) -> &[(usize, Vec<TermCell>)] {
        &self.cached_viewport_cells
    }

    /// The snapshot the grid element paints this frame: the visible rows with
    /// their styled runs and background spans, plus the cursor. Built from the
    /// cached viewport rows; rows whose content, overlays, offset, revision or
    /// theme fingerprint are unchanged reuse their cached text and runs.
    fn grid_snapshot(&mut self, theme: Theme, cell: Option<CellSize>, rows: u16) -> GridSnapshot {
        self.refresh_viewport_cache();
        // The cached rows are shared, not borrowed: the snapshot the grid
        // element draws holds the same allocation, so a frame clones no cells.
        let rows_v = self.viewport_rows();
        // Whether the grid shows anything the reader can read. The rows are
        // already in hand, so this costs nothing.
        let grid_has_content = rows_v
            .iter()
            .any(|(_, cells)| cells.iter().any(|cell| !cell.text.trim().is_empty()));
        // The reader's selection, in content coordinates.
        let sel = match (self.sel_anchor, self.sel_cursor) {
            (Some(a), Some(b)) => Some(ordered_selection(a, b)),
            _ => None,
        };
        let offset = self.screen.offset_from_bottom();
        // Taken out of `self` so the rows can be built while the scratch is
        // reused across rows; put back after the grid is built.
        let mut scratch = std::mem::take(&mut self.cell_scratch);
        let row_cache = std::mem::take(&mut self.row_cache);
        let find_revision = self.find_revision;
        let mut rows_built = 0u64;
        let mut grid_rows: Vec<Rc<GridRow>> = Vec::with_capacity(rows_v.len());
        let mut next_cache: Vec<Rc<GridRow>> = Vec::with_capacity(rows_v.len());
        for (content_row, cells) in rows_v.iter() {
            let mut overlays = RowOverlays::default();
            if let Some((a, b)) = sel {
                if *content_row >= a.0 && *content_row <= b.0 {
                    let first = if *content_row == a.0 { a.1 } else { 0 };
                    let last = if *content_row == b.0 {
                        b.1
                    } else {
                        cells.len().saturating_sub(1)
                    };
                    overlays.selection = Some((first, last));
                }
            }
            for m in &self.find_matches {
                if m.0 == *content_row && m.2 == *content_row {
                    overlays.find.push(m.1..m.3 + 1);
                }
            }
            let key = RowCacheKey {
                content_hash: row_content_hash(cells),
                selection: overlays.selection,
                find: overlays.find.clone(),
                find_revision,
                theme_fg: theme.term_fg,
                theme_bg: theme.term_bg,
                theme_selection: theme.term_selection,
                theme_accent: theme.accent,
            };
            let cached = if terminal_debug().full_repaint {
                None
            } else {
                row_cache.iter().find(|row| row.key == key).cloned()
            };
            let row = match cached {
                Some(row) => row,
                None => {
                    rows_built += 1;
                    Rc::new(build_row(cells, &theme, &overlays, key, &mut scratch))
                }
            };
            grid_rows.push(Rc::clone(&row));
            next_cache.push(row);
        }
        self.cell_scratch = scratch;
        self.row_cache = next_cache;
        self.rows_built += rows_built;
        #[cfg(debug_assertions)]
        crate::perf::record_rows_reshaped(rows_built);
        if terminal_debug().perf {
            eprintln!("ORION frame: {rows_built} rows reshaped");
        }
        self.grid_has_content = grid_has_content;

        let cursor_visible = self.has_focus && self.screen.mode(25);
        let cursor = if cursor_visible {
            match cell {
                Some(_) => cursor_in_view(offset, self.screen.cursor_position(), rows),
                None => None,
            }
        } else {
            None
        };
        // The cursor cell's text and style, for the paint pass: a block
        // cursor is a quad plus the glyph sitting on it, not a bare quad.
        let cursor_glyph = match cursor {
            Some((vis_row, col)) => rows_v
                .get(vis_row)
                .and_then(|(_, cells)| cells.get(col))
                .filter(|cell| !cell.text.trim().is_empty())
                .map(|cell| {
                    (
                        cell.text.clone(),
                        crate::terminal_grid::CellStyle::of_cell(cell, &theme),
                    )
                }),
            None => None,
        };
        GridSnapshot {
            rows: grid_rows,
            cursor,
            cursor_visible,
            cursor_glyph,
            cell: cell.unwrap_or(CellSize {
                width: self.char_w,
                height: self.font_size,
            }),
            font: self
                .font
                .as_ref()
                .map(|font| font.font.clone())
                .unwrap_or_default(),
            font_size: self.font_size,
            theme,
        }
    }

    /// How many rows were rebuilt during renders. Tests use this to prove a
    /// cache-hit frame does not rebuild rows.
    pub fn rows_built(&self) -> u64 {
        self.rows_built
    }

    /// Map a window-relative position onto a visible (row, col) cell.
    ///
    /// The position is relative to the grid's own box, not the window's: the
    /// header, the lens switch, the tab strip, the sidebar and the split pane
    /// all sit above and beside it.
    fn pos_to_cell(&self, pos: gpui_kit::Point<gpui_kit::Pixels>) -> (usize, usize) {
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

/// Keep the selection pointed at the same text after a feed.
///
/// Content coordinates are stable below capacity: when a line enters scrollback,
/// `scrollback_len` grows by one and every live-screen row's `vis_row` shrinks by one for the
/// same text, so `content_row = scrollback_len - offset + vis_row` is unchanged. At capacity
/// the emulator drops the oldest rows without exposing how many, so the shift is unknowable: the
/// selection is cleared instead. A lost selection is visible and safe; a silently mis-pointed
/// one copies the wrong text.
fn adjust_selection_after_feed(
    sel_anchor: &mut Option<(usize, usize)>,
    sel_cursor: &mut Option<(usize, usize)>,
    ring_wrapped: bool,
) {
    if ring_wrapped {
        *sel_anchor = None;
        *sel_cursor = None;
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

/// Where the cursor sits in the viewport, when it is on screen.
///
/// The engine reports the cursor in live-screen coordinates; the reader may be
/// scrolled back, which pushes it down the viewport, and a scroll far enough
/// takes it off the top.
fn cursor_in_view(offset: usize, cursor: (u16, u16), rows: u16) -> Option<(usize, usize)> {
    let visible_row = offset + cursor.0 as usize;
    (visible_row < rows as usize).then_some((visible_row, cursor.1 as usize))
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
        #[cfg(debug_assertions)]
        let frame_started = Instant::now();
        // Keep the engine pane's geometry in sync with the window.
        // The font: the family the reader configured, or the first monospace
        // one that is installed. It is measured, not guessed: the cell is the
        // font's own advance.
        self.ensure_font(window);

        // The grid takes the focus when the lens becomes the one on screen.
        if self.focus_wanted {
            self.focus_wanted = false;
            self.focus_handle.focus(window, cx);
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
                rows_from_height(f32::from(bounds.size.width), cell.width),
                rows_from_height(f32::from(bounds.size.height), cell.height),
            ),
            _ => (self.last_cols, self.last_rows),
        };
        if cols != self.last_cols || rows != self.last_rows {
            self.last_cols = cols;
            self.last_rows = rows;
            // The grid follows the box at once; the agent is told after the
            // debounce, so a drag does not flood it with resizes.
            self.screen.resize(cols, rows);
            // An observing view follows its own box but never resizes the PTY
            // it shares: the agent keeps the size the owning view gave it.
            // An observing view follows its own box but never resizes the PTY
            // it shares: the agent keeps the size the owning view gave it.
            if !self.observe {
                self.request_resize(cols, rows, cx);
            }
        }
        // A resize held back while the drag was moving goes out once the
        // debounce has elapsed. The one-shot timer does the same when no
        // further render happens; taking the pending size makes it a no-op
        // if this render already sent it.
        if !self.observe
            && self.pending_resize.is_some()
            && self.last_resize.elapsed() >= Duration::from_millis(RESIZE_DEBOUNCE_MS)
        {
            self.flush_pending_resize();
        }
        if let Some(cell) = cell {
            self.char_w = cell.width;
        }

        let max_offset = self.screen.scrollback_len();
        let offset = self.screen.offset_from_bottom();
        let (visible_rows, _) = self.screen.size();
        let theme = Theme::current();

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
                // touch controls stay out of the tab order: the grid owns the keyboard.
                .child(
                    Button::new("find-prev")
                        .label("↑")
                        .tooltip("Previous match")
                        .ghost()
                        .h(px(TARGET_PX))
                        .w(px(TARGET_PX))
                        .tab_stop(false)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.find_prev();
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("find-next")
                        .label("↓")
                        .tooltip("Next match")
                        .ghost()
                        .h(px(TARGET_PX))
                        .w(px(TARGET_PX))
                        .tab_stop(false)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.find_next();
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("find-close")
                        .label("✕")
                        .tooltip("Close find")
                        .ghost()
                        .h(px(TARGET_PX))
                        .w(px(TARGET_PX))
                        .tab_stop(false)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.find_open = false;
                            cx.notify();
                        })),
                )
                .into_any_element()
        } else {
            div().h_0().into_any_element()
        };

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
                // A literal key keeps the label exactly as the reader sees it:
                // Kbd formats real keystrokes into platform words ("up" -> "Up").
                let keycap = Kbd::new(Keystroke {
                    key: label.to_string(),
                    ..Default::default()
                });
                bar = bar.child(
                    Button::new(label)
                        .ghost()
                        .h(px(TARGET_PX))
                        .min_w(px(TARGET_PX))
                        .tab_stop(false)
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
                        .child(keycap),
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
                    Button::new(label)
                        .ghost()
                        .selected(pressed)
                        .h(px(TARGET_PX))
                        .min_w(px(TARGET_PX))
                        .tab_stop(false)
                        .on_click(cx.listener(move |this, _, _, _cx| match label {
                            "Ctrl" => this.sticky_ctrl = !this.sticky_ctrl,
                            "Alt" => this.sticky_alt = !this.sticky_alt,
                            _ => this.sticky_shift = !this.sticky_shift,
                        }))
                        .child(Kbd::new(Keystroke {
                            key: label.to_string(),
                            ..Default::default()
                        })),
                );
            }
            bar.into_any_element()
        } else {
            div().h_0().into_any_element()
        };

        // The status line comes only from the cache: `pane.status()` is a
        // synchronous socket round-trip on the daemon path and must never
        // run on the UI thread. The poller refreshes the cache; a stale
        // line is fine, a frozen UI is not.
        let status_line =
            status_line_text(&self.status_line, self.process_exit, offset, max_offset);
        let mut status_bar = StatusBar::new().left(status_line).h(px(TARGET_PX * 0.75));
        if let Some(error) = self.restart_error.clone() {
            status_bar = status_bar.left(div().text_color(theme.danger_text).child(error));
        }
        if restart_available(self.process_exit) {
            let pane_id = self.pane.as_ref().map(|pane| pane.id().to_string());
            let pending = self.restart_pending;
            status_bar = status_bar.right(
                Button::new("terminal-restart")
                    .label("Restart")
                    .tooltip("Restart agent")
                    .ghost()
                    .h(px(TARGET_PX))
                    .min_w(px(TARGET_PX))
                    .tab_stop(false)
                    .loading(pending)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let Some(pane_id) = pane_id.clone() else {
                            return;
                        };
                        if !restart_should_send(this.process_exit, this.restart_pending) {
                            return;
                        }
                        this.restart_pending = true;
                        this.restart_error = None;
                        cx.emit(TerminalPaneEvent::RestartRequested { pane_id });
                        cx.notify();
                    })),
            );
        }

        self.has_focus = self.focus_handle.is_focused(window);
        // The grid: one custom element paints the rows; it takes its own
        // snapshot from the engine and draws it, cell for cell.
        let snapshot = self.grid_snapshot(theme, cell, rows);
        let grid_bounds = Rc::clone(&self.grid_bounds);
        let grid = div()
            .flex()
            .flex_col()
            .flex_1()
            .w_full()
            .overflow_hidden()
            .relative()
            .on_children_prepainted(move |bounds, _window, _cx| {
                if let Some(bounds) = bounds.first() {
                    grid_bounds.set(Some(*bounds));
                }
            })
            .id("grid")
            .debug_selector(|| "grid".to_string())
            .child(TerminalGrid::new(snapshot).flex_1().w_full());
        // The placeholder: an agent that is slow to say anything must not look
        // like a frozen window. It counts the seconds since the pane was
        // attached and goes away with the first real output.
        let starting = self.pane.is_some() && !self.grid_has_content;
        let grid = if starting {
            let elapsed = self.started_at.elapsed().as_secs_f32();
            grid.child(
                div()
                    .id("terminal-starting")
                    .debug_selector(|| "terminal-starting".to_string())
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(theme.text_dim)
                    .text_size(px(13.0))
                    .child(format!("Starting {}… ({elapsed:.1}s)", self.agent_label)),
            )
        } else {
            grid
        };
        if starting {
            self.keep_placeholder_ticking(cx);
        }
        self.keep_repainting(cx);

        // The scrollbar track's box, learned from its layout so a drag can map
        // the pointer's y to a scroll offset. Mirrors `grid_bounds`.
        let scrollbar_bounds: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::new(Cell::new(None));
        let thumb_h_px = thumb_h * grid_height / 100.0;

        let pane = div()
            .id("terminal-pane")
            .when(self.has_focus, |el| el.focus_ring_style(window, cx))
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.term_bg)
            .font_family({
                let family = crate::terminal_font::sanitize_font_family(&self.font_setting);
                if family.is_empty() {
                    crate::terminal_font::TERMINAL_FONT_STACK.to_string()
                } else {
                    family
                }
            })
            .text_size(px(self.font_size))
            .on_key_down(cx.listener(|this, ev, _, cx| {
                if this.on_key(ev, cx) {
                    cx.notify();
                }
            }))
            .on_scroll(cx.listener(move |this, ev: &ScrollEvent, _, cx| {
                if matches!(ev.touch_phase, gpui_kit::TouchPhase::Started) {
                    this.touch = true;
                }
                let row_height = this
                    .cell()
                    .map(|cell| cell.height)
                    .unwrap_or(this.font_size);
                this.handle_scroll(ev, row_height, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    this.focus_handle.focus(window, cx);
                    if ev.button != MouseButton::Left {
                        return;
                    }
                    let (vis_row, col) = this.pos_to_cell(ev.position);
                    if terminal_debug().mouse {
                        eprintln!(
                            "ORION mouse down: tracking={} shift={} mode1000={} mode1002={} mode1003={} mode1006={} cell=({vis_row},{col})",
                            this.mouse_tracking(),
                            ev.modifiers.shift,
                            this.screen.mode(1000),
                            this.screen.mode(1002),
                            this.screen.mode(1003),
                            this.screen.mode(1006),
                        );
                    }
                    if this.mouse_tracking() && !ev.modifiers.shift {
                        let mods = sgr_modifier_bits(&ev.modifiers);
                        this.forward_sgr(0, vis_row, col, true, mods);
                        this.mouse_forwarding_press = true;
                        return;
                    }
                    this.sel_anchor = Some(this.cell_to_content(vis_row, col));
                    this.sel_cursor = this.sel_anchor;
                    this.selecting = true;
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                if this.mouse_forwarding_press && ev.dragging() {
                    let (vis_row, col) = this.pos_to_cell(ev.position);
                    let mods = sgr_modifier_bits(&ev.modifiers) | sgr_mods::MOTION;
                    this.forward_sgr(0, vis_row, col, true, mods);
                    return;
                }
                if this.selecting && ev.dragging() {
                    let (vis_row, col) = this.pos_to_cell(ev.position);
                    this.sel_cursor = Some(this.cell_to_content(vis_row, col));
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseUpEvent, _, cx| {
                    if ev.button != MouseButton::Left {
                        return;
                    }
                    if this.mouse_forwarding_press {
                        this.mouse_forwarding_press = false;
                        let (vis_row, col) = this.pos_to_cell(ev.position);
                        let mods = sgr_modifier_bits(&ev.modifiers);
                        this.forward_sgr(0, vis_row, col, false, mods);
                        return;
                    }
                    if this.selecting {
                        this.selecting = false;
                        cx.notify();
                    }
                }),
            )
            .child(div().flex().flex_row().flex_1().child(grid).child({
                let prepaint_bounds = Rc::clone(&scrollbar_bounds);
                let down_bounds = Rc::clone(&scrollbar_bounds);
                let move_bounds = Rc::clone(&scrollbar_bounds);
                div()
                    .w(px(10.0))
                    .h_full()
                    .relative()
                    .on_children_prepainted(move |bounds, _window, _cx| {
                        if let Some(bounds) = bounds.first() {
                            prepaint_bounds.set(Some(*bounds));
                        }
                    })
                    .id("scrollbar")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                            this.focus_handle.focus(window, cx);
                            this.scrollbar_dragging = true;
                            let max = this.screen.scrollback_len();
                            this.drag_scroll_base = this.screen.offset_from_bottom();
                            let next = match down_bounds.get() {
                                Some(bounds) => scroll_offset_for_thumb(
                                    f32::from(ev.position.y - bounds.origin.y),
                                    f32::from(bounds.size.height),
                                    thumb_h_px,
                                    max,
                                ),
                                None => this.drag_scroll_base,
                            };
                            this.screen.scroll_to(next);
                            cx.notify();
                        }),
                    )
                    .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _, cx| {
                        if !this.scrollbar_dragging {
                            return;
                        }
                        let max = this.screen.scrollback_len();
                        let next = match move_bounds.get() {
                            Some(bounds) => scroll_offset_for_thumb(
                                f32::from(ev.position.y - bounds.origin.y),
                                f32::from(bounds.size.height),
                                thumb_h_px,
                                max,
                            ),
                            None => this.drag_scroll_base,
                        };
                        if next != this.screen.offset_from_bottom() {
                            this.screen.scroll_to(next);
                            cx.notify();
                        }
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseUpEvent, _, cx| {
                            if this.scrollbar_dragging {
                                this.scrollbar_dragging = false;
                                cx.notify();
                            }
                        }),
                    )
                    // Invisible full-size child: its bounds are the track's,
                    // which `on_children_prepainted` reports to the drag handlers.
                    .child(div().absolute().inset_0())
                    .child(
                        div()
                            .id("thumb")
                            .w_full()
                            .h(px(thumb_h * grid_height / 100.0))
                            .rounded_full()
                            .bg(theme.surface_hover)
                            .absolute()
                            .top(px(thumb_top * grid_height / 100.0)),
                    )
            }))
            .child(find_bar)
            .child(key_bar)
            .child(status_bar);
        orion_core::trace::count_repaint();
        if self.grid_has_content && !self.nonempty_traced {
            self.nonempty_traced = true;
            orion_core::trace::record(orion_core::trace::Event::FirstNonemptyFramePainted);
        }
        #[cfg(debug_assertions)]
        crate::perf::record_frame(frame_started.elapsed());
        pane
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn fallback_faces_must_carry_the_same_grid_advance() {
        let families = vec![
            "Mono".to_string(),
            "Bad Mono".to_string(),
            "Symbols Nerd Font Mono".to_string(),
            "Not Installed".to_string(),
        ];
        let installed = vec![
            "Mono".to_string(),
            "Bad Mono".to_string(),
            "Symbols Nerd Font Mono".to_string(),
        ];
        let kept = grid_fallback_families(&families, &installed, 10.0, |name, glyph| {
            if name == "Bad Mono" && glyph == '\u{2500}' {
                Some(20.0)
            } else {
                Some(10.0)
            }
        });
        assert_eq!(
            kept,
            vec!["Mono".to_string(), "Symbols Nerd Font Mono".to_string()],
            "a face with a wrong grid glyph cannot paint terminal text"
        );
    }

    use gpui_kit::test::TestWindowExt;
    use gpui_kit::Entity;
    use gpui_kit::{Keystroke, Modifiers};
    use orion_core::screen::ScreenState;

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
            prefer_character_input: false,
        }
    }

    #[::core::prelude::v1::test]
    fn scroll_offset_for_thumb_maps_top_middle_and_bottom() {
        // Track 100px, thumb 20px -> 80px of travel, 1000 lines of scrollback.
        assert_eq!(scroll_offset_for_thumb(0.0, 100.0, 20.0, 1000), 0);
        assert_eq!(scroll_offset_for_thumb(40.0, 100.0, 20.0, 1000), 500);
        assert_eq!(scroll_offset_for_thumb(80.0, 100.0, 20.0, 1000), 1000);
    }

    #[::core::prelude::v1::test]
    fn scroll_offset_for_thumb_clamps_out_of_range_y() {
        assert_eq!(scroll_offset_for_thumb(-10.0, 100.0, 20.0, 1000), 0);
        assert_eq!(scroll_offset_for_thumb(500.0, 100.0, 20.0, 1000), 1000);
    }

    #[::core::prelude::v1::test]
    fn scroll_offset_for_thumb_handles_degenerate_tracks() {
        assert_eq!(scroll_offset_for_thumb(10.0, 0.0, 0.0, 1000), 0);
        assert_eq!(scroll_offset_for_thumb(10.0, 100.0, 100.0, 1000), 0);
        assert_eq!(scroll_offset_for_thumb(10.0, 100.0, 120.0, 1000), 0);
        assert_eq!(scroll_offset_for_thumb(10.0, 100.0, 20.0, 0), 0);
    }

    #[::core::prelude::v1::test]
    fn status_line_text_shows_the_exit_marker() {
        assert_eq!(
            status_line_text("working", ProcessExit::Running, 0, 0),
            "working",
            "a live pane shows the polled status"
        );
        assert_eq!(
            status_line_text("working", ProcessExit::Exited(Some(0)), 0, 0),
            "[process exited with code 0]"
        );
        assert_eq!(
            status_line_text("error", ProcessExit::Exited(Some(130)), 0, 0),
            "[process exited with code 130]"
        );
        assert_eq!(
            status_line_text("done", ProcessExit::Exited(None), 0, 0),
            "[process exited]",
            "an exit without a reported code still shows the marker"
        );
    }

    #[::core::prelude::v1::test]
    fn status_line_text_keeps_the_scroll_suffix() {
        assert_eq!(
            status_line_text("working", ProcessExit::Running, 3, 10),
            "working  scroll 3/10"
        );
        assert_eq!(
            status_line_text("done", ProcessExit::Exited(Some(1)), 3, 10),
            "[process exited with code 1]  scroll 3/10",
            "reading history does not hide why the pane stopped"
        );
    }

    #[::core::prelude::v1::test]
    fn restart_is_offered_only_for_an_exited_pane() {
        assert!(!restart_available(ProcessExit::Running));
        assert!(restart_available(ProcessExit::Exited(Some(0))));
        assert!(restart_available(ProcessExit::Exited(None)));
        assert!(!restart_should_send(ProcessExit::Running, false));
        assert!(restart_should_send(ProcessExit::Exited(Some(0)), false));
        assert!(
            !restart_should_send(ProcessExit::Exited(Some(0)), true),
            "a second click while the first request is in flight is ignored"
        );
    }

    fn scroll_ev(delta: gpui_kit::ScrollDelta, shift: bool) -> ScrollEvent {
        ScrollEvent {
            position: gpui_kit::point(px(0.), px(0.)),
            delta,
            modifiers: gpui_kit::Modifiers {
                shift,
                ..Default::default()
            },
            touch_phase: gpui_kit::TouchPhase::default(),
            source: gpui_kit::ScrollSource::Pointer,
        }
    }

    #[::core::prelude::v1::test]
    fn wheel_rows_up_lines_delta_up_is_positive() {
        // 3 lines at 16px/line = 48px up => +3 rows into history.
        let ev = scroll_ev(
            gpui_kit::ScrollDelta::Lines(gpui_kit::point(0.0, 3.0)),
            false,
        );
        assert_eq!(wheel_rows_up(&ev, 16.0), 3);
    }

    #[::core::prelude::v1::test]
    fn wheel_rows_up_lines_delta_down_is_negative() {
        // -2 lines at 16px/line = -32px => -2 rows toward the live prompt.
        let ev = scroll_ev(
            gpui_kit::ScrollDelta::Lines(gpui_kit::point(0.0, -2.0)),
            false,
        );
        assert_eq!(wheel_rows_up(&ev, 16.0), -2);
    }

    #[::core::prelude::v1::test]
    fn wheel_rows_up_pixel_delta_up_is_positive() {
        // 48px up at 16px/line => +3 rows into history.
        let ev = scroll_ev(
            gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(48.))),
            false,
        );
        assert_eq!(wheel_rows_up(&ev, 16.0), 3);
    }

    #[::core::prelude::v1::test]
    fn wheel_rows_up_pixel_delta_down_is_negative() {
        // -32px at 16px/line => -2 rows toward the live prompt.
        let ev = scroll_ev(
            gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(-32.))),
            false,
        );
        assert_eq!(wheel_rows_up(&ev, 16.0), -2);
    }

    #[::core::prelude::v1::test]
    fn wheel_rows_up_shift_reads_x_not_y() {
        // Shift redirects the wheel into x. y is a large decoy: using it would
        // give 99 (Lines) / 999px (Pixels) instead of the x-derived value.
        let lines = scroll_ev(
            gpui_kit::ScrollDelta::Lines(gpui_kit::point(4.0, 99.0)),
            true,
        );
        assert_eq!(wheel_rows_up(&lines, 16.0), 4);
        let pixels = scroll_ev(
            gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(32.), px(999.))),
            true,
        );
        assert_eq!(wheel_rows_up(&pixels, 16.0), 2);
    }

    #[::core::prelude::v1::test]
    fn wheel_rows_up_zero_or_negative_row_height_is_zero() {
        let ev = scroll_ev(
            gpui_kit::ScrollDelta::Lines(gpui_kit::point(0.0, 3.0)),
            false,
        );
        assert_eq!(wheel_rows_up(&ev, 0.0), 0);
        assert_eq!(wheel_rows_up(&ev, -1.0), 0);
    }

    #[::core::prelude::v1::test]
    fn arrow_bytes_respect_application_cursor_mode() {
        assert_eq!(arrow_bytes(true, true), "\x1bOA", "up + DECCKM");
        assert_eq!(arrow_bytes(true, false), "\x1b[A", "up + normal");
        assert_eq!(arrow_bytes(false, true), "\x1bOB", "down + DECCKM");
        assert_eq!(arrow_bytes(false, false), "\x1b[B", "down + normal");
    }

    #[::core::prelude::v1::test]
    fn sub_line_pixel_deltas_accumulate_into_whole_rows() {
        let ev = scroll_ev(
            gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(6.))),
            false,
        );
        let mut fraction = 0.0;
        let mut rows = 0;
        for _ in 0..8 {
            let (whole, rest) = accumulate_wheel_rows(wheel_rows_f64(&ev, 16.0), fraction);
            fraction = rest;
            rows += whole;
        }
        assert_eq!(rows, 3, "8 events of 6px at 16px/line span three rows");
        assert!((fraction - 0.0).abs() < 1e-6, "the remainder is 48 - 48px");
    }

    #[::core::prelude::v1::test]
    fn row_content_hash_changes_with_text_and_style() {
        let base = [TermCell {
            text: "x".to_string(),
            fg: TermColor::Default,
            bg: TermColor::Default,
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            inverse: false,
            wide: false,
            wide_continuation: false,
        }];
        let mut styled = base.clone();
        styled[0].bold = true;
        let mut other_text = base.clone();
        other_text[0].text = "y".to_string();
        let unchanged = row_content_hash(&base);
        assert_eq!(unchanged, row_content_hash(&base));
        assert_ne!(unchanged, row_content_hash(&styled), "style is in the key");
        assert_ne!(
            unchanged,
            row_content_hash(&other_text),
            "text is in the key"
        );
    }

    #[::core::prelude::v1::test]
    fn wheel_action_decision_table() {
        use ActiveScreen::{Alternate, Primary};
        assert_eq!(
            wheel_action(false, Primary, false, false),
            WheelAction::Scrollback,
            "primary, no tracking -> scrollback"
        );
        assert_eq!(
            wheel_action(false, Primary, true, false),
            WheelAction::Sgr,
            "primary, tracking -> SGR"
        );
        assert_eq!(
            wheel_action(false, Alternate, false, true),
            WheelAction::Arrow,
            "alt, ?1007, no tracking -> arrows"
        );
        assert_eq!(
            wheel_action(false, Alternate, true, true),
            WheelAction::Sgr,
            "alt, tracking -> SGR"
        );
        assert_eq!(
            wheel_action(false, Alternate, false, false),
            WheelAction::Ignore,
            "alt, neither -> ignore"
        );
        assert_eq!(
            wheel_action(true, Alternate, true, true),
            WheelAction::Scrollback,
            "shift -> local scrollback"
        );
    }

    #[::core::prelude::v1::test]
    fn wheel_sgr_reports_are_press_only_and_never_button_zero() {
        let up = TerminalPane::wheel_sgr_reports(3);
        assert_eq!(up, [(64, true)]);
        let down = TerminalPane::wheel_sgr_reports(-2);
        assert_eq!(down, [(65, true)]);
        assert_eq!(
            TerminalPane::sgr_report(up[0].0, 4, 7, up[0].1, 0),
            b"\x1b[<64;8;5M"
        );
        for (button, press) in up.into_iter().chain(down) {
            assert_ne!(button, 0, "a wheel event must never encode a left click");
            assert!(press, "a wheel event must not send a release");
        }
    }

    #[::core::prelude::v1::test]
    fn cell_height_ceil_covers_ascent_plus_descent() {
        // A fractional line box must be rounded UP: a few tenths short makes
        // GPUI's baseline padding negative and neighbouring rows overlap.
        let height = cell_height_from_metrics(10.2, 3.4);
        assert!(
            height >= 10.2 + 3.4,
            "the cell encloses the painted line box"
        );
        assert_eq!(height, 14.0, "the line box is rounded up to a whole pixel");
    }

    #[::core::prelude::v1::test]
    fn cell_height_never_collapses_to_zero() {
        assert_eq!(cell_height_from_metrics(12.01, 2.99), 15.0);
        assert_eq!(cell_height_from_metrics(11.0001, 2.0), 14.0);
        assert_eq!(cell_height_from_metrics(0.1, 0.2), 1.0);
    }

    #[::core::prelude::v1::test]
    fn rows_from_height_matches_floor_including_fractional_case() {
        assert_eq!(rows_from_height(800.0, 16.0), 50);
        assert_eq!(
            rows_from_height(807.9, 16.0),
            50,
            "a partial row is not drawn"
        );
        assert_eq!(
            rows_from_height(15.9, 16.0),
            1,
            "a short grid still has a row"
        );
        assert_eq!(rows_from_height(100_000.0, 1.0), 10_000, "the clamp holds");
    }

    #[::core::prelude::v1::test]
    fn rows_match_the_painted_cell_grid() {
        // The same ceil'd cell height drives row origins on paint and the
        // row count sent to the PTY: 1000px / 14px = 71 whole cells.
        let cell_h = cell_height_from_metrics(10.2, 3.4);
        assert_eq!(rows_from_height(1000.0, cell_h), 71);
    }

    #[::core::prelude::v1::test]
    fn find_debounce_fires_at_most_twice_for_20_keystrokes_in_1s() {
        let mut debouncer = FindDebouncer::new(Duration::from_millis(FIND_DEBOUNCE_MS));
        let start = Instant::now();
        let mut searches = 0;
        // 20 keystrokes, one every 50ms: the quiet window never elapses mid-burst.
        for i in 0..20 {
            let t = start + Duration::from_millis(i * 50);
            debouncer.keystroke(t);
            if debouncer.due(t) {
                debouncer.mark_fired();
                searches += 1;
            }
        }
        // The quiet window after the last keystroke lets exactly one search through.
        let quiet = start + Duration::from_millis(20 * 50 + FIND_DEBOUNCE_MS);
        if debouncer.due(quiet) {
            debouncer.mark_fired();
            searches += 1;
        }
        assert!(
            searches <= 2,
            "expected at most 2 grid searches, got {searches}"
        );
        assert_eq!(searches, 1);
    }

    #[::core::prelude::v1::test]
    fn resize_debounce_delay_is_50ms() {
        assert_eq!(RESIZE_DEBOUNCE_MS, 50);
    }

    #[::core::prelude::v1::test]
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

    #[::core::prelude::v1::test]
    fn modifier_only_and_platform_keys_are_dropped() {
        assert!(linear_map_keystroke(&ev("control", false, true, false)).is_none());
        let mut platform = ev("c", false, false, false);
        platform.keystroke.modifiers.platform = true;
        assert!(linear_map_keystroke(&platform).is_none());
    }

    #[::core::prelude::v1::test]
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

    #[::core::prelude::v1::test]
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

    #[::core::prelude::v1::test]
    fn the_find_bar_owns_paste_while_it_is_open() {
        assert_eq!(
            key_action(&ev("v", false, true, false), true, false),
            KeyAction::FindKey,
            "the find bar keeps the keyboard, so nothing reaches the agent"
        );
    }

    #[::core::prelude::v1::test]
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

    #[::core::prelude::v1::test]
    fn the_selection_survives_feeds_below_wrap_capacity() {
        let mut screen = ScreenState::new(4, 20);
        let mut anchor = Some((0, 0));
        let mut cursor = Some((0, 3));
        for _ in 0..100 {
            screen.process("line\r\n");
            adjust_selection_after_feed(&mut anchor, &mut cursor, screen.ring_wrapped());
        }
        assert_eq!(
            anchor,
            Some((0, 0)),
            "content coordinates are stable below capacity"
        );
        assert_eq!(
            cursor,
            Some((0, 3)),
            "content coordinates are stable below capacity"
        );
    }

    #[::core::prelude::v1::test]
    fn the_selection_is_cleared_when_the_ring_wraps() {
        let mut screen = ScreenState::new(2, 20);
        let fill = "x\r\n".repeat(orion_core::screen::SCROLLBACK_ROWS + 8);
        screen.process(&fill);
        assert_eq!(screen.scrollback_len(), orion_core::screen::SCROLLBACK_ROWS);
        let mut anchor = Some((3, 0));
        let mut cursor = Some((5, 2));
        screen.process("more\r\n");
        assert!(screen.ring_wrapped(), "feed at capacity reports the wrap");
        adjust_selection_after_feed(&mut anchor, &mut cursor, screen.ring_wrapped());
        assert_eq!(anchor, None, "selection cleared, not mis-pointed");
        assert_eq!(cursor, None, "selection cleared, not mis-pointed");
    }

    #[::core::prelude::v1::test]
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

    crate::orion_test!(
        fn the_grid_the_key_bar_and_the_find_bar_are_on_screen(cx: &mut gpui_kit::TestAppContext) {
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
            cx.update(|window, cx| window.render_frame(cx));

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
    );

    crate::orion_test!(
        fn the_grid_learns_its_own_box_from_the_layout(cx: &mut gpui_kit::TestAppContext) {
            let (root, cx) = cx.add_window_view(|_, cx| TerminalRoot {
                view: cx.new(TerminalPane::new),
            });
            cx.update(|window, cx| window.render_frame(cx));

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
    );

    /// A pane whose `status()` must never run on the render path: it panics.
    /// `pane.status()` is a synchronous socket round-trip on the daemon path
    /// (30s timeout); render reads only the cached `status_line`.
    struct StatusPanicsPane {
        cwd: std::path::PathBuf,
    }

    impl PaneHandle for StatusPanicsPane {
        fn id(&self) -> &str {
            "status-panics"
        }

        fn cwd(&self) -> &std::path::Path {
            &self.cwd
        }

        fn write(&self, _data: &str) -> orion_core::Result<()> {
            Ok(())
        }

        fn keys(&self, _keys: &[String]) -> orion_core::Result<()> {
            Ok(())
        }

        fn interrupt(&self) -> orion_core::Result<()> {
            Ok(())
        }

        fn resize(&self, _cols: u16, _rows: u16) -> orion_core::Result<()> {
            Ok(())
        }

        fn clear(&self) {}

        fn status(&self) -> orion_core::protocol::AgentStatus {
            panic!("render must not call pane.status(); the status line comes from the cache")
        }

        fn prompt(&self) -> Option<orion_core::prompt::DetectedPrompt> {
            None
        }

        fn attach(
            &self,
            _connection: &str,
            _sink: orion_core::FrameSink,
            _role: orion_core::Role,
            _cols: u16,
            _rows: u16,
            _keep_size: bool,
        ) -> orion_core::Result<()> {
            Ok(())
        }

        fn detach(&self, _connection: &str) {}

        fn detach_all(&self, _connection: &str) {}
    }

    crate::orion_test!(
        fn render_reads_the_cached_status_line_never_the_pane(cx: &mut gpui_kit::TestAppContext) {
            let (root, cx) = cx.add_window_view(|_, cx| TerminalRoot {
                view: cx.new(TerminalPane::new),
            });
            cx.update(|_window, app| {
                let view = root.read(app).view.clone();
                view.update(app, |view, _cx| {
                    view.set_pane(Arc::new(StatusPanicsPane {
                        cwd: std::path::PathBuf::from("."),
                    }));
                    assert!(view.set_status_line("working".to_string()));
                });
            });
            // If render called pane.status(), the panic would fail this draw.
            cx.update(|window, cx| window.render_frame(cx));

            cx.update(|_window, app| {
                let view = root.read(app).view.clone();
                let status = view.read(app).status_line.clone();
                assert_eq!(status, "working", "render drew the cached status line");
            });
        }
    );

    crate::orion_test!(
        fn exit_code_is_cached_only_for_the_pane_on_screen(cx: &mut gpui_kit::TestAppContext) {
            let (root, cx) = cx.add_window_view(|_, cx| TerminalRoot {
                view: cx.new(TerminalPane::new),
            });
            cx.update(|_window, app| {
                let view = root.read(app).view.clone();
                view.update(app, |view, _cx| {
                    view.set_pane(Arc::new(StatusPanicsPane {
                        cwd: std::path::PathBuf::from("."),
                    }));
                    assert!(view.set_exit_code("status-panics", Some(7)));
                    assert_eq!(view.process_exit, ProcessExit::Exited(Some(7)));
                    assert!(
                        !view.set_exit_code("another-pane", Some(9)),
                        "a push for a pane this view left is ignored"
                    );
                    assert_eq!(view.process_exit, ProcessExit::Exited(Some(7)));
                });
            });
        }
    );

    crate::orion_test!(
        fn a_restarted_pane_clears_the_exit_marker(cx: &mut gpui_kit::TestAppContext) {
            let (root, cx) = cx.add_window_view(|_, cx| TerminalRoot {
                view: cx.new(TerminalPane::new),
            });
            cx.update(|_window, app| {
                let view = root.read(app).view.clone();
                view.update(app, |view, _cx| {
                    view.set_pane(Arc::new(StatusPanicsPane {
                        cwd: std::path::PathBuf::from("."),
                    }));
                    assert!(view.set_exit_code("status-panics", Some(1)));
                    assert!(view.set_restart_error("no runtime".to_string()));
                    view.set_restarted_pane(Arc::new(StatusPanicsPane {
                        cwd: std::path::PathBuf::from("."),
                    }));
                    assert_eq!(view.process_exit, ProcessExit::Running);
                    assert_eq!(view.status_line, "starting agent…");
                    assert!(view.restart_error.is_none());
                    assert!(!view.restart_pending);
                });
            });
        }
    );

    crate::orion_test!(
        fn render_draws_the_exit_marker_from_the_cache(cx: &mut gpui_kit::TestAppContext) {
            let (root, cx) = cx.add_window_view(|_, cx| TerminalRoot {
                view: cx.new(TerminalPane::new),
            });
            cx.update(|_window, app| {
                let view = root.read(app).view.clone();
                view.update(app, |view, _cx| {
                    view.set_pane(Arc::new(StatusPanicsPane {
                        cwd: std::path::PathBuf::from("."),
                    }));
                    view.set_exit_code("status-panics", Some(3));
                });
            });
            // If render called pane.status(), the panic would fail this draw.
            cx.update(|window, cx| window.render_frame(cx));

            cx.update(|_window, app| {
                let view = root.read(app).view.clone();
                let view = view.read(app);
                assert_eq!(
                    status_line_text(&view.status_line, view.process_exit, 0, 0),
                    "[process exited with code 3]"
                );
                assert!(restart_available(view.process_exit));
            });
        }
    );

    /// A pane that records every size it was asked to take.
    struct ResizeRecordingPane {
        cwd: std::path::PathBuf,
        resizes: Arc<std::sync::Mutex<Vec<(u16, u16)>>>,
    }

    impl PaneHandle for ResizeRecordingPane {
        fn id(&self) -> &str {
            "resize-recorder"
        }

        fn cwd(&self) -> &std::path::Path {
            &self.cwd
        }

        fn write(&self, _data: &str) -> orion_core::Result<()> {
            Ok(())
        }

        fn keys(&self, _keys: &[String]) -> orion_core::Result<()> {
            Ok(())
        }

        fn interrupt(&self) -> orion_core::Result<()> {
            Ok(())
        }

        fn resize(&self, cols: u16, rows: u16) -> orion_core::Result<()> {
            self.resizes
                .lock()
                .expect("the recorder's lock")
                .push((cols, rows));
            Ok(())
        }

        fn clear(&self) {}

        fn status(&self) -> orion_core::protocol::AgentStatus {
            orion_core::protocol::AgentStatus::Idle
        }

        fn prompt(&self) -> Option<orion_core::prompt::DetectedPrompt> {
            None
        }

        fn attach(
            &self,
            _connection: &str,
            _sink: orion_core::FrameSink,
            _role: orion_core::Role,
            _cols: u16,
            _rows: u16,
            _keep_size: bool,
        ) -> orion_core::Result<()> {
            Ok(())
        }

        fn detach(&self, _connection: &str) {}

        fn detach_all(&self, _connection: &str) {}
    }

    // A view that watches the pane never asks the agent for a new size.
    //
    // The split canvas shares the pane's PTY with the main canvas: the size
    // belongs to the view that owns the pane.
    crate::orion_test!(
        fn an_observing_view_never_resizes_the_pane(cx: &mut gpui_kit::TestAppContext) {
            let resizes = Arc::new(std::sync::Mutex::new(Vec::new()));
            let (root, cx) = cx.add_window_view(|_, cx| TerminalRoot {
                view: cx.new(TerminalPane::new),
            });
            cx.update(|_window, app| {
                let view = root.read(app).view.clone();
                view.update(app, |view, _cx| {
                    view.set_pane(Arc::new(ResizeRecordingPane {
                        cwd: std::path::PathBuf::from("."),
                        resizes: Arc::clone(&resizes),
                    }));
                    // A cell the test controls: the grid's size is then the box's
                    // size divided by this cell, whatever fonts the machine has.
                    view.font = Some(TerminalFont {
                        font: Font::default(),
                        cell: CellSize {
                            width: 8.0,
                            height: 16.0,
                        },
                    });
                });
            });
            // The first draw learns the grid's box, the second one resizes to it.
            cx.update(|window, cx| window.render_frame(cx));
            cx.update(|window, cx| window.render_frame(cx));
            let owner = resizes.lock().expect("the recorder's lock").len();
            assert!(owner > 0, "the owning view told the agent about its box");

            // The observing view takes the same box and must leave the PTY alone.
            cx.update(|_window, app| {
                let view = root.read(app).view.clone();
                view.update(app, |view, _cx| {
                    view.set_observe(true);
                    // A size the grid has never seen, so the render takes the
                    // resize branch, and no debounce left to wait for.
                    view.last_cols = 0;
                    view.last_rows = 0;
                    view.last_resize = Instant::now() - Duration::from_millis(RESIZE_DEBOUNCE_MS);
                });
            });
            cx.update(|window, cx| window.render_frame(cx));
            let observed = resizes.lock().expect("the recorder's lock").len();
            assert_eq!(observed, owner, "an observing view never resizes the pane");
            // The render did take the resize branch: the grid followed its box.
            // Only the agent's PTY was left alone.
            let mut grid_cols = 0;
            cx.update(|_window, app| {
                let view = root.read(app).view.clone();
                grid_cols = view.read(app).last_cols;
            });
            assert!(
                grid_cols > 0,
                "the observing view still followed its own box"
            );
        }
    );

    #[::core::prelude::v1::test]
    fn a_face_that_draws_its_box_glyphs_full_width_cannot_carry_a_grid() {
        // A Latin monospace face: every glyph on the same advance.
        assert!(face_carries_a_grid(
            7.2,
            7.2,
            &[Some(7.2), Some(7.2), Some(7.2)]
        ));
        // A proportional face: the letters do not share an advance.
        assert!(!face_carries_a_grid(
            2.9,
            11.2,
            &[Some(2.9), Some(11.2), Some(11.2)]
        ));
        // A CJK monospace face: its ASCII is half width, but the box-drawing
        // and block glyphs it draws are FULL width, so the agent's art would
        // come out twice as wide as the grid.
        assert!(!face_carries_a_grid(
            7.2,
            7.2,
            &[Some(7.2), Some(14.4), Some(14.4)]
        ));
        // A face that cannot even be measured for those glyphs carries no
        // grid either: the agent's frames would be drawn by a fallback.
        assert!(!face_carries_a_grid(7.2, 7.2, &[None]));
    }

    crate::orion_test!(
        fn mouse_reporting_stays_enabled_after_modes_are_cleared(
            cx: &mut gpui_kit::TestAppContext,
        ) {
            let view = cx.update(|app| app.new(TerminalPane::new));
            cx.update(|app| {
                view.update(app, |pane, _cx| {
                    assert!(!pane.mouse_tracking(), "a fresh pane forwards no clicks");
                    pane.feed("\x1b[?1000h\x1b[?1002h\x1b[?1006h");
                    assert!(
                        pane.mouse_tracking(),
                        "DECSET 1000/1002/1006 enables forwarding"
                    );
                    pane.feed("\x1b[?1000l\x1b[?1002l\x1b[?1006l");
                    assert!(
                        pane.mouse_tracking(),
                        "a focus change that strips DEC modes must not silence clicks"
                    );
                });
            });
        }
    );

    crate::orion_test!(
        fn the_terminal_font_measures_a_monospace_cell(cx: &mut gpui_kit::TestAppContext) {
            let cx = cx.add_empty_window();
            let families = terminal_font_families("");
            let window_text_system = WindowTextSystem::new(cx.text_system().clone());
            let Some(font) =
                measure_terminal_font(&window_text_system, &families, px(DEFAULT_FONT_PX))
            else {
                // A machine with no monospace font at all: the grid falls back to
                // the system font. Nothing to measure here.
                return;
            };
            assert!(font.cell.width > 0.0, "the cell has a width");
            assert!(font.cell.height > 0.0, "the cell has a height");

            let text_system = cx.text_system();
            let id = text_system.resolve_font(&font.font);
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

            let painted = window_text_system.shape_line(
                SharedString::new(CELL_PROBE_TEXT),
                px(DEFAULT_FONT_PX),
                &[TextRun {
                    len: CELL_PROBE_TEXT.len(),
                    font: font.font.clone(),
                    color: white(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }],
                None,
            );
            assert!(
                font.cell.height >= f32::from(painted.ascent) + f32::from(painted.descent),
                "the cell encloses the line the grid actually paints"
            );
        }
    );

    crate::orion_test!(
        fn the_terminal_reads_the_clipboard_when_it_pastes(cx: &mut gpui_kit::TestAppContext) {
            let view = cx.update(|app| app.new(TerminalPane::new));
            cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
                "pasted words".to_string(),
            ));

            let text = cx.update(|app| view.update(app, |view, cx| view.clipboard_text(cx)));

            assert_eq!(
                text.as_deref(),
                Some("pasted words"),
                "the paste path reads what the reader copied"
            );
        }
    );

    crate::orion_test!(
        fn new_output_keeps_the_reader_where_they_scrolled(cx: &mut gpui_kit::TestAppContext) {
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
    );

    crate::orion_test!(
        fn new_output_follows_when_the_reader_is_at_the_bottom(cx: &mut gpui_kit::TestAppContext) {
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
    );

    crate::orion_test!(
        fn the_cache_hit_path_borrows_the_cached_rows(cx: &mut gpui_kit::TestAppContext) {
            let view = cx.update(|app| app.new(TerminalPane::new));
            cx.update(|app| {
                view.update(app, |view, _cx| {
                    for line in 0..80 {
                        view.feed(&format!("line {line}\r\n"));
                    }
                    view.refresh_viewport_cache();
                    let cached = view.viewport_cells().as_ptr();
                    let cached_revision = view.cached_viewport_revision;

                    view.refresh_viewport_cache();

                    assert_eq!(
                        cached,
                        view.viewport_cells().as_ptr(),
                        "an unchanged (revision, offset) reuses the cached allocation"
                    );
                    assert_eq!(view.cached_viewport_revision, cached_revision);

                    // A feed after a scroll changes the revision: the cache is
                    // rebuilt and the new output is on screen.
                    view.screen.scroll_to(5);
                    view.feed("late output\r\n");
                    view.refresh_viewport_cache();

                    assert_ne!(
                        view.cached_viewport_revision, cached_revision,
                        "a feed after a scroll invalidates the cache"
                    );
                    assert_eq!(
                        view.screen.offset_from_bottom(),
                        5,
                        "the reader stays where they scrolled"
                    );
                    let text: String = view
                        .viewport_cells()
                        .iter()
                        .flat_map(|(_, cells)| cells.iter().map(|cell| cell.text.as_str()))
                        .collect();
                    assert!(
                        !text.contains("late output"),
                        "the rebuilt cache shows the scrolled viewport, not the live bottom"
                    );
                });
            });
        }
    );

    crate::orion_test!(
        fn a_cache_hit_frame_does_not_rebuild_rows(cx: &mut gpui_kit::TestAppContext) {
            let (root, cx) = cx.add_window_view(|_, cx| TerminalRoot {
                view: cx.new(TerminalPane::new),
            });
            cx.update(|_window, app| {
                let view = root.read(app).view.clone();
                view.update(app, |view, _cx| {
                    for line in 0..40 {
                        view.feed(&format!("line {line}\r\n"));
                    }
                });
            });
            // Warm up: the first draws build the cache and learn the grid's box.
            cx.update(|window, cx| window.render_frame(cx));
            cx.update(|window, cx| window.render_frame(cx));
            let mut calls = 0;
            cx.update(|_window, app| {
                calls = root.read(app).view.read(app).rows_built();
            });
            assert!(calls > 0, "the warm-up draws built the rows");

            // Force a re-render with no content change: the cache must serve it.
            cx.update(|_window, app| {
                let view = root.read(app).view.clone();
                view.update(app, |_view, cx| cx.notify());
            });
            cx.update(|window, cx| window.render_frame(cx));

            let mut after = 0;
            cx.update(|_window, app| {
                after = root.read(app).view.read(app).rows_built();
            });
            assert_eq!(
                calls, after,
                "an unchanged frame reuses the cached rows instead of re-building them"
            );
            // The cached frame still paints the fed content.
            let mut text = String::new();
            cx.update(|_window, app| {
                text = root
                    .read(app)
                    .view
                    .read(app)
                    .viewport_cells()
                    .iter()
                    .flat_map(|(_, cells)| cells.iter().map(|cell| cell.text.as_str()))
                    .collect();
            });
            assert!(
                text.contains("line 39"),
                "the cached frame paints the content"
            );
        }
    );

    crate::orion_test!(
        fn a_one_line_feed_reuses_the_rows_that_merely_shifted(cx: &mut gpui_kit::TestAppContext) {
            let (root, cx) = cx.add_window_view(|_, cx| TerminalRoot {
                view: cx.new(TerminalPane::new),
            });
            cx.update(|_window, app| {
                let view = root.read(app).view.clone();
                view.update(app, |view, _cx| {
                    for line in 0..80 {
                        view.feed(&format!("line {line}\r\n"));
                    }
                });
            });
            cx.update(|window, cx| window.render_frame(cx));
            cx.update(|window, cx| window.render_frame(cx));

            let mut before = 0;
            let mut visible = 0;
            cx.update(|_window, app| {
                let view = root.read(app).view.clone();
                view.update(app, |view, _cx| {
                    before = view.rows_built();
                    visible = view.viewport_cells().len();
                });
            });
            assert!(before > 0, "the warm-up frames built the rows");

            cx.update(|_window, app| {
                let view = root.read(app).view.clone();
                view.update(app, |view, cx| {
                    view.feed("one more line\r\n");
                    cx.notify();
                });
            });
            cx.update(|window, cx| window.render_frame(cx));

            let mut after = 0;
            cx.update(|_window, app| {
                after = root.read(app).view.read(app).rows_built();
            });
            let rebuilt = after - before;
            assert!(
                rebuilt <= 2,
                "a one-line shift rebuilds at most the new bottom row, got {rebuilt} of {visible}"
            );
        }
    );

    crate::orion_test!(
        fn the_row_cache_invalidates_on_feed_scroll_selection_and_find(
            cx: &mut gpui_kit::TestAppContext,
        ) {
            let (root, cx) = cx.add_window_view(|_, cx| TerminalRoot {
                view: cx.new(TerminalPane::new),
            });
            cx.update(|_window, app| {
                let view = root.read(app).view.clone();
                view.update(app, |view, _cx| {
                    for line in 0..40 {
                        view.feed(&format!("line {line}\r\n"));
                    }
                });
            });
            cx.update(|window, cx| window.render_frame(cx));
            cx.update(|window, cx| window.render_frame(cx));

            // A feed bumps the content revision: every visible row rebuilds.
            let mut before = 0;
            cx.update(|_window, app| {
                before = root.read(app).view.read(app).rows_built();
            });
            cx.update(|_window, app| {
                let view = root.read(app).view.clone();
                view.update(app, |view, cx| {
                    view.feed("fresh output\r\n");
                    cx.notify();
                });
            });
            cx.update(|window, cx| window.render_frame(cx));
            let mut after_feed = 0;
            cx.update(|_window, app| {
                after_feed = root.read(app).view.read(app).rows_built();
            });
            assert!(after_feed > before, "a feed invalidates the cached rows");

            // A scroll changes the offset: the cached rows rebuild.
            cx.update(|_window, app| {
                let view = root.read(app).view.clone();
                view.update(app, |view, cx| {
                    view.screen.scroll_to(3);
                    cx.notify();
                });
            });
            cx.update(|window, cx| window.render_frame(cx));
            let mut after_scroll = 0;
            cx.update(|_window, app| {
                after_scroll = root.read(app).view.read(app).rows_built();
            });
            assert!(
                after_scroll > after_feed,
                "a scroll invalidates the cached rows"
            );

            // A selection on one visible row rebuilds exactly that row.
            let mut before_selection = 0;
            cx.update(|_window, app| {
                let view = root.read(app).view.clone();
                view.update(app, |view, cx| {
                    let row = view.viewport_cells()[0].0;
                    view.sel_anchor = Some((row, 0));
                    view.sel_cursor = Some((row, 4));
                    cx.notify();
                    before_selection = view.rows_built();
                });
            });
            cx.update(|window, cx| window.render_frame(cx));
            let mut after_selection = 0;
            cx.update(|_window, app| {
                after_selection = root.read(app).view.read(app).rows_built();
            });
            assert_eq!(
                after_selection - before_selection,
                1,
                "a one-row selection rebuilds exactly the selected row"
            );

            // A find refresh bumps the find revision even when the matches are
            // identical - the count is not the fingerprint.
            let mut rev = 0;
            let mut before_find = 0;
            cx.update(|_window, app| {
                let view = root.read(app).view.clone();
                view.update(app, |view, cx| {
                    view.find_query = "line".to_string();
                    view.refresh_find_matches();
                    cx.notify();
                    rev = view.find_revision;
                    before_find = view.rows_built();
                });
            });
            cx.update(|window, cx| window.render_frame(cx));
            let mut after_find = 0;
            cx.update(|_window, app| {
                after_find = root.read(app).view.read(app).rows_built();
            });
            assert!(
                after_find > before_find,
                "a find refresh invalidates the cached rows"
            );
            cx.update(|_window, app| {
                let view = root.read(app).view.clone();
                view.update(app, |view, _cx| view.refresh_find_matches());
            });
            let mut rev_after = 0;
            cx.update(|_window, app| {
                rev_after = root.read(app).view.read(app).find_revision;
            });
            assert_eq!(
                rev_after,
                rev + 1,
                "every refresh bumps the find revision, even with identical matches"
            );

            // Clearing the query bumps the revision too.
            cx.update(|_window, app| {
                let view = root.read(app).view.clone();
                view.update(app, |view, cx| {
                    view.find_query.clear();
                    view.schedule_find_refresh(cx);
                });
            });
            let mut rev_cleared = 0;
            cx.update(|_window, app| {
                rev_cleared = root.read(app).view.read(app).find_revision;
            });
            assert_eq!(
                rev_cleared,
                rev + 2,
                "the empty-query clear bumps the find revision"
            );
        }
    );
    // The snapshot the element paints for a fed grid contains the same
    // cells, cursor and selection the row-based path produced: the rows the
    // grid paints are the rows the engine puts on the screen, with the
    // selection painted as background spans across those cells.
    crate::orion_test!(
        fn the_grid_snapshot_contains_the_content_the_engine_shows(
            cx: &mut gpui_kit::TestAppContext,
        ) {
            let (root, cx) = cx.add_window_view(|_, cx| TerminalRoot {
                view: cx.new(TerminalPane::new),
            });
            cx.update(|_window, app| {
                let view = root.read(app).view.clone();
                view.update(app, |view, _cx| {
                    view.has_focus = true;
                    view.feed("alpha\x1b[31m red\x1b[0m beta");
                    view.sel_anchor = Some((0, 0));
                    view.sel_cursor = Some((0, 5));
                });
            });
            cx.update(|window, cx| window.render_frame(cx));

            let (lines, cursor, selection_row) = cx.update(|_window, app| {
                let view = root.read(app).view.clone();
                view.update(app, |view, _cx| {
                    view.has_focus = true;
                    let snapshot = view.grid_snapshot(
                        Theme::dark(),
                        Some(crate::terminal_grid::CellSize {
                            width: 8.0,
                            height: 16.0,
                        }),
                        32,
                    );
                    let lines: Vec<String> = snapshot
                        .rows
                        .iter()
                        .take(1)
                        .map(|row| row.text.trim_end().to_string())
                        .collect();
                    let selection: Vec<crate::terminal_grid::BgSpan> =
                        snapshot.rows[0].bgs.to_vec();
                    (lines, snapshot.cursor, selection)
                })
            });

            assert_eq!(lines.len(), 1, "the snapshot holds the fed viewport rows");
            assert!(
                lines[0].contains("red"),
                "the fed colour reaches the snapshot: {lines:?}"
            );
            assert_eq!(
                cursor,
                Some((0, 14)),
                "the cursor sits after the fed text, on the live screen"
            );
            assert!(
                selection_row.iter().any(|span| span.first_col == 0
                    && span.end_col == 6
                    && span.color == Theme::dark().term_selection),
                "the fed selection lands on the snapshot row: {selection_row:?}"
            );
        }
    );

    crate::orion_test!(
        fn cursor_visibility_hidden_mode_yields_no_cursor(cx: &mut gpui_kit::TestAppContext) {
            let (root, cx) = cx.add_window_view(|_, cx| TerminalRoot {
                view: cx.new(TerminalPane::new),
            });
            cx.update(|_window, app| {
                let view = root.read(app).view.clone();
                view.update(app, |view, _cx| {
                    view.has_focus = true;
                    view.feed("abc\x1b[?25l");
                });
            });
            let (cursor, visible) = cx.update(|_window, app| {
                let view = root.read(app).view.clone();
                view.update(app, |view, _cx| {
                    let snap = view.grid_snapshot(
                        Theme::dark(),
                        Some(crate::terminal_grid::CellSize {
                            width: 8.0,
                            height: 16.0,
                        }),
                        32,
                    );
                    (snap.cursor, snap.cursor_visible)
                })
            });
            assert_eq!(cursor, None, "DEC mode 25 hidden means no cursor");
            assert!(!visible, "the snapshot marks the cursor hidden");
        }
    );

    crate::orion_test!(
        fn cursor_visibility_unfocused_pane_yields_no_cursor(cx: &mut gpui_kit::TestAppContext) {
            let (root, cx) = cx.add_window_view(|_, cx| TerminalRoot {
                view: cx.new(TerminalPane::new),
            });
            cx.update(|_window, app| {
                let view = root.read(app).view.clone();
                view.update(app, |view, _cx| {
                    view.has_focus = false;
                    view.feed("abc");
                });
            });
            let (cursor, visible) = cx.update(|_window, app| {
                let view = root.read(app).view.clone();
                view.update(app, |view, _cx| {
                    let snap = view.grid_snapshot(
                        Theme::dark(),
                        Some(crate::terminal_grid::CellSize {
                            width: 8.0,
                            height: 16.0,
                        }),
                        32,
                    );
                    (snap.cursor, snap.cursor_visible)
                })
            });
            assert_eq!(cursor, None, "an unfocused pane never paints a cursor");
            assert!(!visible, "the snapshot marks the cursor hidden");
        }
    );

    crate::orion_test!(
        fn cursor_visibility_focused_visible_pane_yields_cursor(cx: &mut gpui_kit::TestAppContext) {
            let (root, cx) = cx.add_window_view(|_, cx| TerminalRoot {
                view: cx.new(TerminalPane::new),
            });
            cx.update(|_window, app| {
                let view = root.read(app).view.clone();
                view.update(app, |view, _cx| {
                    view.has_focus = true;
                    view.feed("abc");
                });
            });
            let (cursor, visible) = cx.update(|_window, app| {
                let view = root.read(app).view.clone();
                view.update(app, |view, _cx| {
                    let snap = view.grid_snapshot(
                        Theme::dark(),
                        Some(crate::terminal_grid::CellSize {
                            width: 8.0,
                            height: 16.0,
                        }),
                        32,
                    );
                    (snap.cursor, snap.cursor_visible)
                })
            });
            assert_eq!(cursor, Some((0, 3)), "the cursor sits after the fed text");
            assert!(visible, "the snapshot marks the cursor visible");
        }
    );
}
