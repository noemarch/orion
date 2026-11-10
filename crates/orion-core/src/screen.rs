//! The engine's terminal grid.
//!
//! PTY bytes are text, not a picture: the same bytes that feed a client's xterm.js feed this
//! grid, so the server can read a pane (chat lens, prompt detection, assistant status) without a
//! second process. `vt100` owns the escape-sequence state machine and the scrollback ring; this
//! module adds the revision counter, the plain-text views and the small selection/find surface the
//! HTTP routes expose.

use std::num::NonZeroU16;

use serde_json::{json, Value};

/// History rows kept per pane. A busy agent can emit hundreds of thousands of rows; the ring
/// keeps the recent ones and drops the oldest without touching the visible screen.
pub const SCROLLBACK_ROWS: usize = 10_000;

/// One terminal's parsed state. Every mutation bumps `revision`, so a client can tell a still
/// screen from a redrawn one without diffing text.
pub struct ScreenState {
    parser: vt100::Parser,
    revision: u64,
    /// Where the last scroll request put the shared viewport, in rows above the live bottom. The
    /// parser itself is never left offset: reads, status detection and prompt detection always see
    /// the live screen, and a scroll produces one repaint for the client that asked.
    scroll_offset: usize,
}

impl ScreenState {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            parser: vt100::Parser::new(nonzero(rows), nonzero(cols), SCROLLBACK_ROWS),
            revision: 0,
            scroll_offset: 0,
        }
    }

    /// Feeds decoded text to the grid. Returns the new revision.
    pub fn process(&mut self, text: &str) -> u64 {
        self.parser.process(text.as_bytes());
        self.revision += 1;
        self.revision
    }

    /// Resizes the grid; the parser moves rows that fall off the top into scrollback.
    pub fn resize(&mut self, rows: u16, cols: u16) -> u64 {
        self.parser.set_size(nonzero(rows), nonzero(cols));
        self.revision += 1;
        self.revision
    }

    /// Blanks the grid and drops its history: the pane keeps its size, the agent keeps running.
    pub fn clear(&mut self) {
        let (rows, cols) = self.size();
        self.parser = vt100::Parser::new(nonzero(rows), nonzero(cols), SCROLLBACK_ROWS);
        self.scroll_offset = 0;
        self.revision += 1;
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn size(&self) -> (u16, u16) {
        let (rows, cols) = self.parser.screen().size();
        (rows.get(), cols.get())
    }

    pub fn scrollback_len(&self) -> usize {
        self.parser.screen().scrollback_len()
    }

    pub fn offset_from_bottom(&self) -> usize {
        self.scroll_offset.min(self.scrollback_len())
    }

    /// Moves the shared viewport and returns the repaint for it. The parser goes back to the live
    /// bottom at once, so nothing reads a scrolled screen by accident.
    pub fn scroll_to(&mut self, offset: usize) -> String {
        let max = self.scrollback_len();
        self.scroll_offset = offset.min(max);
        self.parser.screen_mut().set_scrollback(self.scroll_offset);
        let replay = self.replay();
        self.parser.screen_mut().set_scrollback(0);
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
    /// bottom: the scrollback just above the live screen, then the live screen itself. Cheap
    /// enough for a per-frame draw: it offsets the parser for the read, then restores the live
    /// bottom, so no screen is cloned and the ring is not walked.
    pub fn viewport_rows(&mut self, offset_from_bottom: usize) -> Vec<String> {
        let (_, cols) = self.size();
        let offset = offset_from_bottom.min(self.scrollback_len());
        self.parser.screen_mut().set_scrollback(offset);
        let rows = self
            .parser
            .screen()
            .rows(0, cols)
            .map(|row| row.trim_end().to_string())
            .collect();
        self.parser.screen_mut().set_scrollback(0);
        rows
    }

    /// The live screen's cursor cell as `(row, col)`, for the terminal lens's cursor.
    pub fn cursor_position(&self) -> (u16, u16) {
        self.parser.screen().cursor_position()
    }

    /// The whole visible screen as plain text, one trimmed line per row. Prompt detection reads
    /// this: it is what the agent's TUI draws, not what the transcript says.
    pub fn visible_text(&self) -> String {
        let (_, cols) = self.size();
        self.parser
            .screen()
            .rows(0, cols)
            .map(|row| row.trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Every row the pane can show, oldest first: the scrollback ring and then the live screen.
    /// Blank rows under the cursor are dropped, as a reader expects.
    pub fn history(&self) -> Vec<String> {
        self.rows_with_wrap()
            .into_iter()
            .map(|(text, _)| text)
            .collect()
    }

    /// History rows with their soft-wrap flag, so the chat can rejoin a line the PTY wrapped.
    pub fn rows_with_wrap(&self) -> Vec<(String, bool)> {
        let (rows, cols) = self.size();
        let rows = rows as usize;
        let total_history = self.scrollback_len();
        let mut out: Vec<(String, bool)> = Vec::with_capacity(total_history + rows);

        let mut screen = self.parser.screen().clone();
        let mut taken = 0usize;
        while taken < total_history {
            let take = rows.min(total_history - taken);
            screen.set_scrollback(total_history - taken);
            for (index, text) in screen.rows(0, cols).take(take).enumerate() {
                out.push((
                    text.trim_end().to_string(),
                    screen.row_wrapped(index as u16),
                ));
            }
            taken += take;
        }
        screen.set_scrollback(0);
        for (index, text) in screen.rows(0, cols).enumerate() {
            out.push((
                text.trim_end().to_string(),
                screen.row_wrapped(index as u16),
            ));
        }

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
    pub fn read_recent(&self, lines: usize, unwrapped: bool) -> (String, bool) {
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

    /// A complete repaint for a client that just attached. xterm.js starts blank, so it needs the
    /// cursor, every visible cell and the terminal modes, not a diff against a screen it lacks.
    pub fn replay(&self) -> String {
        let screen = self.parser.screen();
        format!(
            "{}{}",
            screen.contents_formatted(),
            screen.input_mode_formatted()
        )
    }

    /// Whether the live screen or history contains `needle` (case-insensitive).
    pub fn contains(&self, needle: &str) -> bool {
        let needle = needle.to_lowercase();
        self.history()
            .iter()
            .any(|line| line.to_lowercase().contains(&needle))
    }

    /// Matches in content coordinates (rows counted from the oldest history row, columns from 0).
    pub fn find(&self, query: &str, limit: usize) -> Vec<Value> {
        if query.is_empty() {
            return Vec::new();
        }
        let query = query.to_lowercase();
        let mut matches = Vec::new();
        for (row, line) in self.history().iter().enumerate() {
            let haystack = line.to_lowercase();
            let mut from = 0usize;
            while let Some(found) = haystack[from..].find(&query) {
                let start = from + found;
                let end = start + query.len();
                matches.push(json!({
                    "start": { "row": row, "col": start },
                    "end": { "row": row, "col": end.saturating_sub(1) },
                }));
                if matches.len() >= limit {
                    return matches;
                }
                from = end.max(start + 1);
            }
        }
        matches
    }

    /// The text between two content cells, inclusive, with soft-wrapped rows joined.
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
}
