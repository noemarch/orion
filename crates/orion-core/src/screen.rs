//! The engine's terminal grid.
//!
//! PTY bytes are text, not a picture: the same bytes the shell draws feed this grid, so the
//! server can read a pane (chat lens, prompt detection, assistant status) without a second
//! process. `libghostty-vt` owns the escape-sequence state machine and the scrollback ring; this
//! module adds the revision counter, the plain-text views and the small selection/find surface the
//! HTTP routes expose.

use std::num::NonZeroU16;

use libghostty_vt::render::{Color, RenderState, RenderedCell, Terminal};
use serde_json::{json, Value};

use crate::ghostty_extra;
use crate::terminal_engine::FindMatch;

/// History rows kept per pane. A busy agent can emit hundreds of thousands of rows; the ring
/// keeps the recent ones and drops the oldest without touching the visible screen.
pub const SCROLLBACK_ROWS: usize = 10_000;

/// Newest rows `find` examines per execution: the scrollback tail plus the live screen. Older
/// rows are not searched, so a per-keystroke find never walks the whole ring.
pub const FIND_MAX_ROWS: usize = 2000;

/// A terminal cell's colour, as the program asked for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermColor {
    /// The terminal's own default foreground or background.
    Default,
    /// One of the 256 indexed colours.
    Indexed(u8),
    /// A 24-bit colour.
    Rgb(u8, u8, u8),
}

impl From<Color> for TermColor {
    fn from(color: Color) -> Self {
        match color {
            Color::Default => Self::Default,
            Color::Indexed(index) => Self::Indexed(index),
            Color::Rgb(red, green, blue) => Self::Rgb(red, green, blue),
        }
    }
}

/// One cell of the grid, with the styling the program asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermCell {
    /// The text in the cell. Empty for the second half of a wide character.
    pub text: String,
    /// The foreground colour.
    pub fg: TermColor,
    /// The background colour.
    pub bg: TermColor,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    /// The program asked for the foreground and background to be swapped.
    pub inverse: bool,
    /// The cell holds the first half of a double-width character.
    pub wide: bool,
    /// The cell is the empty second half of a double-width character.
    pub wide_continuation: bool,
}

impl TermCell {
    fn from_rendered(cell: &RenderedCell) -> Self {
        Self {
            text: cell.graphemes.clone(),
            fg: cell.fg.into(),
            bg: cell.bg.into(),
            bold: cell.bold,
            dim: cell.dim,
            italic: cell.italic,
            underline: cell.underline,
            inverse: cell.inverse,
            wide: cell.wide,
            wide_continuation: cell.wide_continuation,
        }
    }
}

/// One row held by the Rust-side mirror of the grid: its plain text, its soft-wrap flag and its
/// styled cells. The mirror is fed from ghostty's render state on every mutation so reads never
/// have to walk the scrollback ring one `grid_ref` at a time.
#[derive(Clone)]
struct CachedRow {
    text: String,
    wrapped: bool,
    cells: Vec<TermCell>,
}

impl CachedRow {
    fn from_rendered(cells: Vec<TermCell>) -> Self {
        let wrapped = !cells.is_empty()
            && cells
                .iter()
                .all(|cell| cell.wide_continuation || !cell.text.trim().is_empty());
        let mut text = String::new();
        for cell in &cells {
            if !cell.wide_continuation {
                text.push_str(&cell.text);
            }
        }
        Self {
            text: text.trim_end().to_string(),
            wrapped,
            cells,
        }
    }
}

/// One terminal's parsed state. Every mutation bumps `revision`, so a client can tell a still
/// screen from a redrawn one without diffing text.
pub struct ScreenState {
    terminal: Terminal,
    render: RenderState,
    revision: u64,
    /// Where the last scroll request put the shared viewport, in rows above the live bottom. The
    /// terminal itself is never left offset: reads, status detection and prompt detection always see
    /// the live screen, and a scroll produces one repaint for the client that asked.
    scroll_offset: usize,
    /// Set by `process` when the ring was already at capacity before the feed: rows may have
    /// scrolled off the top, shifting every content coordinate by an amount ghostty does not
    /// expose. Callers holding content-coordinate state (selection) must treat it as stale.
    ring_wrapped: bool,
    /// The full logical row list, oldest first: scrollback tail followed by the live screen.
    rows: Vec<CachedRow>,
}

impl ScreenState {
    pub fn new(rows: u16, cols: u16) -> Self {
        let (rows, cols) = (nonzero(rows).get(), nonzero(cols).get());
        let mut terminal = Terminal::new(cols, rows).expect("ghostty terminal creation failed");
        ghostty_extra::configure(
            terminal.as_raw() as ghostty_extra::RawTerminal,
            SCROLLBACK_ROWS * 2,
            64 * 1024 * 1024,
            64 * 1024,
            true,
        );
        let mut render = RenderState::new().expect("ghostty render state creation failed");
        render.update(&mut terminal);
        let mut state = Self {
            terminal,
            render,
            revision: 0,
            scroll_offset: 0,
            ring_wrapped: false,
            rows: Vec::new(),
        };
        state.rows = state.all_rows();
        state
    }

    /// Feeds decoded text to the grid. Returns the new revision.
    pub fn process(&mut self, text: &str) -> u64 {
        self.process_bytes(text.as_bytes())
    }

    /// Feeds raw bytes from the PTY to the grid. Returns the new revision.
    ///
    /// The bytes go to the parser as they are: a multi-byte character split
    /// across two reads is the parser's business, not the caller's.
    pub fn process_bytes(&mut self, bytes: &[u8]) -> u64 {
        let before = scrollback_len(&self.render);
        let at_capacity = before >= SCROLLBACK_ROWS;
        let screen_rows = usize::from(self.render.frame().size().0).max(1);
        self.terminal.write(bytes);
        self.render.update(&mut self.terminal);
        let after = scrollback_len(&self.render);
        self.revision += 1;
        // Conservative: any mutation while the ring is full may have dropped rows. ghostty keeps
        // no public dropped-row counter through this surface, so callers must assume content
        // coordinates shifted.
        self.ring_wrapped = at_capacity;
        let live = read_live(&self.render);
        let keep = self
            .rows
            .len()
            .saturating_sub(screen_rows)
            .min(after)
            .min(SCROLLBACK_ROWS);
        self.rows.truncate(keep);
        if after > before {
            let grown = self.read_scrolled(after - before, after);
            let grown_len = grown.len();
            let room = SCROLLBACK_ROWS.saturating_sub(keep);
            self.rows
                .extend(grown.into_iter().skip(grown_len.saturating_sub(room)));
        }
        self.rows.extend(live);
        self.revision
    }

    /// Resizes the grid; rows that fall off the top move into scrollback.
    pub fn resize(&mut self, rows: u16, cols: u16) -> u64 {
        let (rows, cols) = (nonzero(rows).get(), nonzero(cols).get());
        let status = self.terminal.resize(cols, rows, 1, 1);
        assert!(status.is_ok(), "ghostty resize failed: {status:?}");
        self.render.update(&mut self.terminal);
        self.rows = self.all_rows();
        self.revision += 1;
        self.ring_wrapped = false;
        self.revision
    }

    /// Blanks the grid and drops its history: the pane keeps its size, the agent keeps running.
    pub fn clear(&mut self) {
        self.terminal.reset();
        self.render.update(&mut self.terminal);
        self.rows = self.all_rows();
        self.scroll_offset = 0;
        self.revision += 1;
        self.ring_wrapped = false;
    }

    /// Whether the last `process` ran while the scrollback ring was at capacity, meaning rows
    /// may have been dropped and content coordinates shifted by an unknowable amount.
    pub fn ring_wrapped(&self) -> bool {
        self.ring_wrapped
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn size(&self) -> (u16, u16) {
        self.render.frame().size()
    }

    pub fn scrollback_len(&self) -> usize {
        let screen_rows = usize::from(self.render.frame().size().0).max(1);
        self.rows.len().saturating_sub(screen_rows)
    }

    pub fn offset_from_bottom(&self) -> usize {
        self.scroll_offset.min(self.scrollback_len())
    }

    /// Moves the shared viewport and returns the repaint for it. The terminal goes back to the live
    /// bottom at once, so nothing reads a scrolled screen by accident.
    pub fn scroll_to(&mut self, offset: usize) -> String {
        let max = self.scrollback_len();
        self.scroll_offset = offset.min(max);
        let window = self.viewport(self.scroll_offset);
        let replay = repaint(&window, self.cursor_position(), self.scroll_offset);
        self.revision += 1;
        replay
    }

    pub fn scroll_info(&self) -> Value {
        let (rows, _) = self.size();
        let max = self.scrollback_len();
        json!({
            "max_offset_from_bottom": max,
            "offset_from_bottom": self.offset_from_bottom(),
            "viewport_rows": rows,
        })
    }

    /// The rows the pane shows when its viewport sits `offset_from_bottom` rows above the live
    /// bottom: the scrollback just above the live screen, then the live screen itself. The terminal
    /// viewport is moved for the read and restored to the live bottom immediately afterwards.
    pub fn viewport_rows(&mut self, offset_from_bottom: usize) -> Vec<String> {
        self.viewport(offset_from_bottom)
            .iter()
            .map(|row| row.text.clone())
            .collect()
    }

    /// The rows the pane shows when its viewport sits `offset_from_bottom` rows above the live
    /// bottom, with every cell's styling. Same rows as `viewport_rows`, so the terminal lens can
    /// paint colour without a second source of truth for what is on screen.
    pub fn viewport_cells(&mut self, offset_from_bottom: usize) -> Vec<Vec<TermCell>> {
        self.viewport(offset_from_bottom)
            .iter()
            .map(|row| row.cells.clone())
            .collect()
    }

    /// The live screen's cursor cell as `(row, col)`, for the terminal lens's cursor.
    pub fn cursor_position(&self) -> (u16, u16) {
        let cursor = self.render.frame().cursor();
        (cursor.y, cursor.x)
    }

    /// The whole visible screen as plain text, one trimmed line per row. Prompt detection reads
    /// this: it is what the agent's TUI draws, not what the transcript says.
    pub fn visible_text(&self) -> String {
        let screen_rows = usize::from(self.render.frame().size().0);
        self.rows
            .iter()
            .skip(self.rows.len().saturating_sub(screen_rows))
            .map(|row| row.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Every row the pane can show, oldest first: the scrollback ring and then the live screen.
    /// Blank rows under the cursor are dropped, as a reader expects.
    // PERF: full-ring clone, O(scrollback)
    pub fn history(&self) -> Vec<String> {
        self.rows_with_wrap()
            .into_iter()
            .map(|(text, _)| text)
            .collect()
    }

    /// History rows with their soft-wrap flag, so the chat can rejoin a line the PTY wrapped.
    // PERF: full-ring clone, O(scrollback)
    pub fn rows_with_wrap(&self) -> Vec<(String, bool)> {
        let mut out: Vec<(String, bool)> = self
            .rows
            .iter()
            .map(|row| (row.text.clone(), row.wrapped))
            .collect();
        while out
            .last()
            .is_some_and(|(text, wrapped)| text.is_empty() && !*wrapped)
        {
            out.pop();
        }
        out
    }

    /// The recent rows a chat lens reads. `unwrapped` joins rows the PTY soft-wrapped at its own
    /// width, so the chat's narrower width can reflow them. Returns `(text, truncated)`.
    // PERF: full-ring clone, O(scrollback)
    pub fn read_recent(&mut self, lines: usize, unwrapped: bool) -> (String, bool) {
        let rows = self.rows_with_wrap();
        let start = rows.len().saturating_sub(lines);
        let truncated = start > 0;
        let slice = &rows[start..];
        let text = if unwrapped {
            let mut text = String::new();
            for (line, wrapped) in slice {
                text.push_str(line);
                if !wrapped {
                    text.push('\n');
                }
            }
            text.trim_end_matches('\n').to_string()
        } else {
            slice
                .iter()
                .map(|(line, _)| line.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        };
        (text, truncated)
    }

    /// A complete repaint for a client that just attached. A fresh view starts blank, so it needs the
    /// cursor, every visible cell and the terminal modes, not a diff against a screen it lacks.
    pub fn replay(&self) -> String {
        let offset = self.offset_from_bottom();
        let window = self.window(offset);
        if window
            .iter()
            .all(|row| row.cells.iter().all(|cell| cell.text.trim().is_empty()))
        {
            return String::new();
        }
        repaint(&window, self.cursor_position(), offset)
    }

    /// Whether the live screen or history contains `needle` (case-insensitive).
    // PERF: full-ring clone, O(scrollback)
    pub fn contains(&self, needle: &str) -> bool {
        let needle = needle.to_lowercase();
        self.history()
            .iter()
            .any(|line| line.to_lowercase().contains(&needle))
    }

    /// Matches in content coordinates (rows counted from the oldest history row, columns from 0).
    /// Cap: only the newest `FIND_MAX_ROWS` rows (scrollback tail plus the live screen) are
    /// examined, so find shows recent matches first and never walks the whole ring per keystroke.
    /// Matches are reported oldest-first within that window; rows older than the cap are not
    /// searched.
    // PERF: full-ring clone, O(scrollback); the cap truncates to the newest FIND_MAX_ROWS rows
    pub fn find(&mut self, query: &str, limit: usize) -> Vec<Value> {
        self.find_matches(query, limit)
            .into_iter()
            .map(|found| {
                json!({
                    "start": { "row": found.start_row, "col": found.start_col },
                    "end": { "row": found.end_row, "col": found.end_col },
                })
            })
            .collect()
    }

    /// The same matches as [`ScreenState::find`], in typed content coordinates.
    // PERF: full-ring clone, O(scrollback); the cap truncates to the newest FIND_MAX_ROWS rows
    pub fn find_matches(&mut self, query: &str, limit: usize) -> Vec<FindMatch> {
        if query.is_empty() {
            return Vec::new();
        }
        let query = query.to_lowercase();
        let rows = self.rows_with_wrap();
        let total = rows.len();
        let window = total.min(FIND_MAX_ROWS);
        let start_abs = total - window;
        let mut matches = Vec::new();
        for (offset, (line, _)) in rows[start_abs..].iter().enumerate() {
            collect_matches(
                &line.to_lowercase(),
                &query,
                start_abs + offset,
                limit,
                &mut matches,
            );
            if matches.len() >= limit {
                break;
            }
        }
        matches
    }

    /// The text between two content cells, inclusive, with soft-wrapped rows joined.
    // PERF: full-ring clone, O(scrollback)
    pub fn selection(
        &self,
        start_row: usize,
        start_col: usize,
        end_row: usize,
        end_col: usize,
    ) -> String {
        if start_row > end_row || (start_row == end_row && start_col > end_col) {
            return String::new();
        }
        let rows = self.rows_with_wrap();
        let mut text = String::new();
        for (row, (line, wrapped)) in rows
            .iter()
            .enumerate()
            .take(end_row.min(rows.len().saturating_sub(1)) + 1)
            .skip(start_row)
        {
            let chars: Vec<char> = line.chars().collect();
            let from = if row == start_row {
                start_col.min(chars.len())
            } else {
                0
            };
            let to = if row == end_row {
                (end_col + 1).min(chars.len())
            } else {
                chars.len()
            };
            if from < to {
                text.extend(&chars[from..to]);
            }
            if row != end_row && !wrapped {
                text.push('\n');
            }
        }
        text
    }

    /// The screen-sized window `offset_from_bottom` rows above the live bottom.
    fn window(&self, offset_from_bottom: usize) -> Vec<CachedRow> {
        let screen_rows = usize::from(self.render.frame().size().0).max(1);
        let sbl = self.scrollback_len();
        let offset = offset_from_bottom.min(sbl);
        let end = self.rows.len().saturating_sub(offset);
        let start = end.saturating_sub(screen_rows);
        self.rows[start..end].to_vec()
    }

    /// Rebuild the mirror from ghostty's own scrollback, reading it in screen-sized viewport chunks.
    fn all_rows(&mut self) -> Vec<CachedRow> {
        let sbl = scrollback_len(&self.render);
        let screen_rows = usize::from(self.render.frame().size().0).max(1);
        let total = sbl + screen_rows;
        let mut all = Vec::with_capacity(total);
        let mut start = 0usize;
        while start < total {
            let offset = sbl.saturating_sub(start);
            let chunk = self.viewport(offset);
            let len = chunk.len();
            let base = sbl - offset.min(sbl);
            for (index, row) in chunk.into_iter().enumerate() {
                let absolute = base + index;
                if absolute >= start && absolute < total {
                    all.push(row);
                }
            }
            let next = base + len;
            if next <= start {
                break;
            }
            start = next;
        }
        let excess = all
            .len()
            .saturating_sub(SCROLLBACK_ROWS.saturating_add(screen_rows));
        if excess > 0 {
            all.drain(..excess);
        }
        all
    }

    /// Move the ghostty viewport for one read, then put it back at the live bottom. The absolute
    /// row is `scrollback_rows - offset`, so the requested row becomes the top visible row.
    fn viewport(&mut self, offset_from_bottom: usize) -> Vec<CachedRow> {
        let scrollback = scrollback_len(&self.render);
        let offset = offset_from_bottom.min(scrollback);
        let raw = self.terminal.as_raw() as ghostty_extra::RawTerminal;
        let status = ghostty_extra::scroll_to_row(raw, scrollback - offset);
        assert_eq!(
            status,
            ghostty_extra::GHOSTTY_SUCCESS,
            "ghostty viewport scroll failed"
        );
        self.render.update(&mut self.terminal);
        let rows = read_live(&self.render);
        let status = ghostty_extra::scroll_bottom(raw);
        assert_eq!(
            status,
            ghostty_extra::GHOSTTY_SUCCESS,
            "ghostty viewport restore failed"
        );
        self.render.update(&mut self.terminal);
        rows
    }

    /// Read the newest `count` scrollback rows, oldest of them first.
    fn read_scrolled(&mut self, count: usize, scrollback: usize) -> Vec<CachedRow> {
        let mut rows = Vec::with_capacity(count);
        let mut remaining = count.min(scrollback);
        while remaining > 0 {
            let chunk = self.viewport(remaining);
            let take = remaining.min(chunk.len());
            rows.extend(chunk.into_iter().take(take));
            remaining -= take;
        }
        rows
    }
}

/// Plain rows of the live screen, read from the render state in one pass.
fn read_live(render: &RenderState) -> Vec<CachedRow> {
    let frame = render.frame();
    let (rows, _) = frame.size();
    (0..rows)
        .map(|y| {
            let row = frame
                .row(y)
                .expect("row index comes from the frame's own size");
            let cells = row.cells().map(TermCell::from_rendered).collect();
            CachedRow::from_rendered(cells)
        })
        .collect()
}

fn repaint(rows: &[CachedRow], cursor: (u16, u16), offset: usize) -> String {
    let mut out = String::from("\x1b[?25l\x1b[H\x1b[2J");
    let mut current_fg: Option<TermColor> = None;
    let mut current_bg: Option<TermColor> = None;
    let mut flags = (false, false, false, false, false);
    for (y, row) in rows.iter().enumerate() {
        out.push_str(&format!("\x1b[{};1H", y + 1));
        for cell in &row.cells {
            if cell.wide_continuation {
                continue;
            }
            let next = (
                cell.bold,
                cell.dim,
                cell.italic,
                cell.underline,
                cell.inverse,
            );
            if next != flags {
                out.push_str("\x1b[0m");
                if cell.bold {
                    out.push_str("\x1b[1m");
                }
                if cell.dim {
                    out.push_str("\x1b[2m");
                }
                if cell.italic {
                    out.push_str("\x1b[3m");
                }
                if cell.underline {
                    out.push_str("\x1b[4m");
                }
                if cell.inverse {
                    out.push_str("\x1b[7m");
                }
                flags = next;
            }
            if cell.fg != TermColor::Default && Some(cell.fg) != current_fg {
                out.push_str(&sgr_color(38, cell.fg));
                current_fg = Some(cell.fg);
            } else if cell.fg == TermColor::Default && current_fg.is_some() {
                out.push_str("\x1b[39m");
                current_fg = None;
            }
            if cell.bg != TermColor::Default && Some(cell.bg) != current_bg {
                out.push_str(&sgr_color(48, cell.bg));
                current_bg = Some(cell.bg);
            } else if cell.bg == TermColor::Default && current_bg.is_some() {
                out.push_str("\x1b[49m");
                current_bg = None;
            }
            out.push_str(&cell.text);
        }
    }
    out.push_str(&format!(
        "\x1b[{};{}H\x1b[?25h\x1b[?1l\x1b>",
        cursor.0.saturating_sub(offset as u16) + 1,
        cursor.1 + 1,
    ));
    out
}

/// Scrollback row count reported by the render state.
fn scrollback_len(render: &RenderState) -> usize {
    render.frame().scrollback_rows()
}

fn sgr_color(kind: u8, color: TermColor) -> String {
    match color {
        TermColor::Default => String::new(),
        TermColor::Indexed(index) => format!("\x1b[{kind};5;{index}m"),
        TermColor::Rgb(red, green, blue) => format!("\x1b[{kind};2;{red};{green};{blue}m"),
    }
}

fn collect_matches(
    haystack: &str,
    query: &str,
    row: usize,
    limit: usize,
    matches: &mut Vec<FindMatch>,
) {
    let mut from = 0usize;
    while let Some(found) = haystack[from..].find(query) {
        let start = from + found;
        let end = start + query.len();
        matches.push(FindMatch {
            start_row: row,
            start_col: start,
            end_row: row,
            end_col: end.saturating_sub(1),
        });
        if matches.len() >= limit {
            return;
        }
        from = end.max(start + 1);
    }
}

fn nonzero(value: u16) -> NonZeroU16 {
    NonZeroU16::new(value.max(1)).expect("clamped above zero")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_sequences_become_a_grid_and_bump_the_revision() {
        let mut screen = ScreenState::new(6, 40);
        let before = screen.revision();
        screen.process("hello");
        screen.process("\r\nworld");
        assert!(screen.revision() > before);
        let text = screen.visible_text();
        assert!(text.starts_with("hello\nworld"), "{text:?}");
        assert_eq!(screen.size(), (6, 40));
    }

    #[test]
    fn scrolling_keeps_history_and_reads_it_oldest_first() {
        let mut screen = ScreenState::new(3, 20);
        for line in ["one", "two", "three", "four", "five"] {
            screen.process(&format!("{line}\r\n"));
        }
        let history = screen.history();
        assert!(history.len() >= 5, "history: {history:?}");
        assert_eq!(
            &history[history.len() - 5..],
            &["one", "two", "three", "four", "five"]
        );
        let (recent, truncated) = screen.read_recent(2, false);
        assert!(truncated);
        assert!(recent.ends_with("four\nfive"), "{recent:?}");
        assert_eq!(
            screen.scroll_info()["max_offset_from_bottom"],
            screen.scrollback_len()
        );
    }

    #[test]
    fn a_soft_wrapped_row_rejoins_only_when_asked() {
        let mut screen = ScreenState::new(4, 10);
        screen.process("abcdefghijkl");
        let (wrapped, _) = screen.read_recent(10, false);
        assert!(
            wrapped.contains('\n'),
            "physical rows stay apart: {wrapped:?}"
        );
        let (unwrapped, _) = screen.read_recent(10, true);
        assert_eq!(unwrapped, "abcdefghijkl");
    }

    #[test]
    fn find_only_searches_the_newest_rows() {
        let mut screen = ScreenState::new(2, 20);
        screen.process("needle-old\r\n");
        for i in 0..FIND_MAX_ROWS + 10 {
            screen.process(&format!("filler-{i}\r\n"));
        }
        let matches = screen.find("needle-old", 10);
        assert!(
            matches.is_empty(),
            "a match older than the {FIND_MAX_ROWS}-row cap must not be found"
        );
        screen.process("needle-new\r\n");
        let matches = screen.find("needle-new", 10);
        assert_eq!(matches.len(), 1);
    }

    #[test]
    fn finds_every_occurrence_with_content_coordinates() {
        let mut screen = ScreenState::new(5, 30);
        screen.process("alpha beta\r\ngamma beta");
        let matches = screen.find("beta", 10);
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0]["start"]["row"], 0);
        assert_eq!(matches[1]["start"]["row"], 1);
    }

    #[test]
    fn selection_joins_a_soft_wrapped_line() {
        let mut screen = ScreenState::new(4, 5);
        screen.process("abcdefgh");
        let text = screen.selection(0, 2, 1, 2);
        assert_eq!(text, "cdefgh");
    }

    #[test]
    fn the_viewport_reads_the_history_above_the_live_screen() {
        let mut screen = ScreenState::new(3, 20);
        screen.process("one\r\ntwo\r\nthree\r\nfour\r\n");
        assert_eq!(screen.viewport_rows(0), vec!["three", "four", ""]);
        assert_eq!(screen.viewport_rows(1), vec!["two", "three", "four"]);
        assert_eq!(screen.viewport_rows(2), vec!["one", "two", "three"]);
    }

    #[test]
    fn the_cursor_sits_after_the_last_written_cell() {
        let mut screen = ScreenState::new(3, 20);
        screen.process("hello");
        assert_eq!(screen.cursor_position(), (0, 5));
    }

    #[test]
    fn the_viewport_offset_is_clamped_to_the_history() {
        let mut screen = ScreenState::new(3, 20);
        screen.process("one\r\ntwo\r\nthree\r\nfour\r\n");
        let clamped = screen.viewport_rows(usize::MAX);
        assert_eq!(clamped, screen.viewport_rows(screen.scrollback_len()));
        assert_eq!(clamped.first().map(String::as_str), Some("one"));
    }

    #[test]
    fn a_plain_cell_has_the_default_colours() {
        let mut screen = ScreenState::new(6, 40);
        screen.process("hi");
        let cells = screen.viewport_cells(0);
        let cell = &cells[0][0];
        assert_eq!(cell.text, "h");
        assert_eq!(cell.fg, TermColor::Default);
        assert_eq!(cell.bg, TermColor::Default);
        assert!(!cell.bold);
        assert!(!cell.dim);
        assert!(!cell.italic);
        assert!(!cell.underline);
        assert!(!cell.inverse);
        assert!(!cell.wide);
        assert!(!cell.wide_continuation);
        assert_eq!(cells[0][1].text, "i");
    }

    #[test]
    fn a_cell_carries_the_colour_the_program_asked_for() {
        let mut screen = ScreenState::new(6, 40);
        screen.process("\x1b[31mred\x1b[0m");
        screen.process("\x1b[38;2;10;20;30mtrue\x1b[0m");
        let cells = screen.viewport_cells(0);
        assert_eq!(cells[0][0].text, "r");
        assert_eq!(cells[0][0].fg, TermColor::Indexed(1));
        assert_eq!(cells[0][3].text, "t");
        assert_eq!(cells[0][3].fg, TermColor::Rgb(10, 20, 30));
    }

    #[test]
    fn a_bold_cell_says_so() {
        let mut screen = ScreenState::new(6, 40);
        screen.process("\x1b[1mbold\x1b[0m");
        let cells = screen.viewport_cells(0);
        assert!(cells[0][0].bold);
        assert_eq!(cells[0][0].text, "b");
    }

    #[test]
    fn an_inverse_cell_says_so() {
        let mut screen = ScreenState::new(6, 40);
        screen.process("\x1b[7mrev\x1b[0m");
        let cells = screen.viewport_cells(0);
        assert!(cells[0][0].inverse);
    }

    #[test]
    fn a_wide_character_occupies_two_cells() {
        let mut screen = ScreenState::new(6, 40);
        screen.process("漢");
        let cells = screen.viewport_cells(0);
        assert_eq!(cells[0][0].text, "漢");
        assert!(cells[0][0].wide);
        assert_eq!(cells[0][1].text, "");
        assert!(cells[0][1].wide_continuation);
    }

    #[test]
    fn a_cell_carries_the_background_the_program_asked_for() {
        let mut screen = ScreenState::new(6, 40);
        screen.process("\x1b[44mbg\x1b[0m");
        let cells = screen.viewport_cells(0);
        assert_eq!(cells[0][0].text, "b");
        assert_eq!(cells[0][0].bg, TermColor::Indexed(4));
    }

    #[test]
    fn the_cells_agree_with_the_plain_rows() {
        let mut screen = ScreenState::new(4, 20);
        screen.process("hello\r\nworld\r\nthird\r\nfourth\r\n");
        for offset in 0..=2 {
            let rows = screen.viewport_rows(offset);
            let cells = screen.viewport_cells(offset);
            assert_eq!(cells.len(), rows.len(), "offset {offset}");
            for (cells_row, plain) in cells.iter().zip(&rows) {
                let text: String = cells_row
                    .iter()
                    .filter(|cell| !cell.wide_continuation)
                    .map(|cell| cell.text.as_str())
                    .collect();
                assert_eq!(text.trim_end(), plain, "offset {offset}");
            }
        }
    }
}
