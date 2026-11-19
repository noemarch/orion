//! The terminal emulator behind one trait.
//!
//! The shell draws a grid. The grid comes from an emulator, and today that
//! emulator is libghostty-vt. Nothing outside this module should have to say so:
//! the shell depends on [`TerminalEngine`], so another emulator can be
//! measured against this one without the shell changing.
//!
//! The trait is the seam: the implementation here is backed by the headless
//! libghostty-vt terminal-state library.

use std::ops::Range;

use crate::screen::ScreenState;

pub use crate::screen::{TermCell, TermColor};

/// A key press the terminal understands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    /// A literal character, typed by the reader.
    Char(char),
    /// A logical chord, named the way the shell names them: "ctrl+c", "up",
    /// "enter".
    Chord(String),
}

/// One row of the terminal's history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// The row's text, without its trailing padding.
    pub text: String,
    /// Whether the program soft-wrapped the row.
    pub wrapped: bool,
}

/// The visible grid, with the cursor and the styles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridSnapshot {
    /// The visible rows, each a row of cells.
    pub rows: Vec<Vec<TermCell>>,
    /// The live cursor's `(row, col)`.
    pub cursor: (u16, u16),
    /// The grid's `(rows, cols)`.
    pub size: (u16, u16),
}

/// A search match, in content coordinates: rows counted from the oldest
/// history row, columns from zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FindMatch {
    pub start_row: usize,
    pub start_col: usize,
    pub end_row: usize,
    pub end_col: usize,
}

/// The terminal emulator the shell draws.
///
/// The methods are the surface the terminal lens uses, plus the five that
/// name the seam itself: the bytes arrive, the grid resizes, the grid is
/// read, the history is read, and a key becomes bytes.
pub trait TerminalEngine {
    /// Feeds bytes from the PTY into the emulator. Returns the new revision.
    fn write_output(&mut self, bytes: &[u8]) -> u64;

    /// Resizes the emulator's grid. Returns the new revision.
    ///
    /// The order is columns first, rows second: that is the order a terminal
    /// speaks in.
    fn resize(&mut self, cols: u16, rows: u16) -> u64;

    /// The visible grid, with the cursor and the styles.
    ///
    /// `offset_from_bottom` is how far the viewport sits above the live
    /// bottom: zero is the live screen, one is the row above it.
    fn snapshot(&mut self, offset_from_bottom: usize) -> GridSnapshot;

    /// The rows in a content range, oldest first.
    ///
    /// Content rows are counted from the oldest history row; the live screen
    /// is the tail of the range.
    fn scrollback(&self, range: Range<usize>) -> Vec<Line>;

    /// The bytes a key press sends to the PTY.
    fn encode_key(&self, key: &Key) -> Vec<u8>;

    /// The grid's revision. It advances on every change.
    fn revision(&self) -> u64;

    /// The grid's `(rows, cols)`.
    fn size(&self) -> (u16, u16);

    /// Blanks the grid and drops its history. The program keeps running.
    fn clear(&mut self);

    /// Whether the last write ran while the history ring was full, so content
    /// coordinates may have shifted.
    fn ring_wrapped(&self) -> bool;

    /// How many rows the history holds.
    fn scrollback_len(&self) -> usize;

    /// How far the viewport sits above the live bottom.
    fn offset_from_bottom(&self) -> usize;

    /// Moves the viewport.
    fn scroll_to(&mut self, offset: usize);

    /// The live screen as plain text, one trimmed row per line.
    fn visible_text(&self) -> String;

    /// The live cursor's `(row, col)`.
    fn cursor_position(&self) -> (u16, u16);

    /// Searches the newest rows for `query`.
    fn find(&mut self, query: &str, limit: usize) -> Vec<FindMatch>;

    /// The text between two content cells, inclusive.
    fn selection(
        &self,
        start_row: usize,
        start_col: usize,
        end_row: usize,
        end_col: usize,
    ) -> String;
}

/// A new engine with an empty grid.
///
/// The order is columns first, rows second: the same order
/// [`TerminalEngine::resize`] speaks in.
pub fn new_engine(cols: u16, rows: u16) -> Box<dyn TerminalEngine> {
    Box::new(ScreenState::new(rows, cols))
}

/// The emulator Orion has today, behind the trait.
impl TerminalEngine for ScreenState {
    fn write_output(&mut self, bytes: &[u8]) -> u64 {
        self.process_bytes(bytes)
    }

    fn resize(&mut self, cols: u16, rows: u16) -> u64 {
        ScreenState::resize(self, rows, cols)
    }

    fn snapshot(&mut self, offset_from_bottom: usize) -> GridSnapshot {
        let cursor = self.cursor_position();
        let size = self.size();
        GridSnapshot {
            rows: self.viewport_cells(offset_from_bottom),
            cursor,
            size,
        }
    }

    fn scrollback(&self, range: Range<usize>) -> Vec<Line> {
        self.rows_with_wrap()
            .into_iter()
            .skip(range.start)
            .take(range.end.saturating_sub(range.start))
            .map(|(text, wrapped)| Line { text, wrapped })
            .collect()
    }

    fn encode_key(&self, key: &Key) -> Vec<u8> {
        match key {
            Key::Char(character) => character.to_string().into_bytes(),
            Key::Chord(chord) => encode_chord(chord).unwrap_or_default().into_bytes(),
        }
    }

    fn revision(&self) -> u64 {
        ScreenState::revision(self)
    }

    fn size(&self) -> (u16, u16) {
        ScreenState::size(self)
    }

    fn clear(&mut self) {
        ScreenState::clear(self);
    }

    fn ring_wrapped(&self) -> bool {
        ScreenState::ring_wrapped(self)
    }

    fn scrollback_len(&self) -> usize {
        ScreenState::scrollback_len(self)
    }

    fn offset_from_bottom(&self) -> usize {
        ScreenState::offset_from_bottom(self)
    }

    fn scroll_to(&mut self, offset: usize) {
        ScreenState::scroll_to(self, offset);
    }

    fn visible_text(&self) -> String {
        ScreenState::visible_text(self)
    }

    fn cursor_position(&self) -> (u16, u16) {
        ScreenState::cursor_position(self)
    }

    fn find(&mut self, query: &str, limit: usize) -> Vec<FindMatch> {
        self.find_matches(query, limit)
    }

    fn selection(
        &self,
        start_row: usize,
        start_col: usize,
        end_row: usize,
        end_col: usize,
    ) -> String {
        ScreenState::selection(self, start_row, start_col, end_row, end_col)
    }
}

/// Encodes one logical key chord into the bytes a terminal receives.
///
/// The names are the shell's: "ctrl+c", "up", "enter", a literal character.
/// An unknown name encodes to nothing.
pub fn encode_chord(key: &str) -> Option<String> {
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut name = key;
    loop {
        let lowered = name.to_ascii_lowercase();
        if let Some(rest) = lowered.strip_prefix("ctrl+") {
            ctrl = true;
            name = &name[name.len() - rest.len()..];
        } else if let Some(rest) = lowered.strip_prefix("alt+") {
            alt = true;
            name = &name[name.len() - rest.len()..];
        } else if let Some(rest) = lowered.strip_prefix("shift+") {
            shift = true;
            name = &name[name.len() - rest.len()..];
        } else {
            break;
        }
    }

    let modifier = 1 + i32::from(shift) + 2 * i32::from(alt) + 4 * i32::from(ctrl);
    let lowered = name.to_ascii_lowercase();
    // Shift+Tab is its own sequence (CSI Z), not a modified Tab.
    if lowered == "tab" && shift && !ctrl && !alt {
        return Some("\x1b[Z".to_string());
    }
    let special = match lowered.as_str() {
        "up" => Some("\x1b[A".to_string()),
        "down" => Some("\x1b[B".to_string()),
        "right" => Some("\x1b[C".to_string()),
        "left" => Some("\x1b[D".to_string()),
        "home" => Some("\x1b[H".to_string()),
        "end" => Some("\x1b[F".to_string()),
        "pageup" => Some("\x1b[5~".to_string()),
        "pagedown" => Some("\x1b[6~".to_string()),
        "insert" => Some("\x1b[2~".to_string()),
        "delete" => Some("\x1b[3~".to_string()),
        "backtab" => Some("\x1b[Z".to_string()),
        "backspace" => Some("\x7f".to_string()),
        "enter" | "return" => Some("\r".to_string()),
        "tab" => Some("\t".to_string()),
        "esc" | "escape" => Some("\x1b".to_string()),
        "space" => Some(" ".to_string()),
        "plus" => Some("+".to_string()),
        _ => {
            if let Some(digit) = lowered.strip_prefix('f') {
                if let Ok(number) = digit.parse::<u8>() {
                    if (1..=12).contains(&number) {
                        let code = match number {
                            1..=4 => format!("\x1b[{}~", 10 + number),
                            5 => "\x1b[15~".to_string(),
                            6..=10 => format!("\x1b[{}~", 16 + number),
                            _ => format!("\x1b[{}~", 22 + number),
                        };
                        Some(code)
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        }
    };

    if let Some(sequence) = special {
        if modifier == 1 {
            return Some(sequence);
        }
        if sequence.starts_with("\x1b[") && sequence.ends_with('~') {
            let inner = &sequence[2..sequence.len() - 1];
            return Some(format!("\x1b[{inner};{modifier}~"));
        }
        if sequence.len() == 3 && sequence.starts_with("\x1b[") {
            let final_byte = &sequence[2..];
            return Some(format!("\x1b[1;{modifier}{final_byte}"));
        }
        if alt && sequence.len() == 1 {
            return Some(format!("\x1b{sequence}"));
        }
        return Some(sequence);
    }

    // A literal character.
    if name.chars().count() == 1 {
        let ch = name.chars().next().unwrap_or(' ');
        if ctrl {
            let upper = ch.to_ascii_uppercase();
            if upper.is_ascii_alphabetic() || "@[\\]^_".contains(upper) {
                let code = (upper as u8) & 0x1f;
                let text = char::from(code).to_string();
                return Some(if alt { format!("\x1b{text}") } else { text });
            }
        }
        return Some(if alt {
            format!("\x1b{name}")
        } else {
            name.to_string()
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_emulator_is_reachable_through_the_trait() {
        let mut engine: Box<dyn TerminalEngine> = Box::new(ScreenState::new(6, 40));
        let revision = engine.write_output(b"hello");
        assert_eq!(revision, 1);
        assert_eq!(engine.size(), (6, 40));
        assert!(engine.visible_text().starts_with("hello"));

        let snapshot = engine.snapshot(0);
        assert_eq!(snapshot.size, (6, 40));
        assert_eq!(snapshot.cursor, (0, 5));
        assert_eq!(snapshot.rows[0][0].text, "h");
    }

    #[test]
    fn the_trait_resizes_in_columns_then_rows() {
        let mut engine: Box<dyn TerminalEngine> = Box::new(ScreenState::new(6, 40));
        // `resize` takes columns first; `size` reports rows first.
        engine.resize(20, 4);
        assert_eq!(engine.size(), (4, 20), "the grid is 20 columns by 4 rows");
    }

    #[test]
    fn the_trait_reads_the_history_in_content_coordinates() {
        let mut engine: Box<dyn TerminalEngine> = Box::new(ScreenState::new(3, 20));
        engine.write_output(b"one\r\ntwo\r\nthree\r\n");
        let lines = engine.scrollback(0..2);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "one");
        assert_eq!(lines[1].text, "two");
    }

    #[test]
    fn the_trait_encodes_keys() {
        let engine = ScreenState::new(6, 40);
        assert_eq!(engine.encode_key(&Key::Chord("enter".into())), b"\r");
        assert_eq!(engine.encode_key(&Key::Chord("ctrl+c".into())), b"\x03");
        assert_eq!(engine.encode_key(&Key::Chord("up".into())), b"\x1b[A");
        assert_eq!(engine.encode_key(&Key::Char('a')), b"a");
        assert!(engine.encode_key(&Key::Chord("nonsense".into())).is_empty());
    }

    #[test]
    fn the_trait_finds_and_selects() {
        let mut engine: Box<dyn TerminalEngine> = Box::new(ScreenState::new(5, 30));
        engine.write_output(b"alpha beta\r\ngamma beta");
        let matches = engine.find("beta", 10);
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].start_row, 0);
        assert_eq!(matches[0].start_col, 6);
        assert_eq!(engine.selection(0, 6, 0, 9), "beta");
    }
}
