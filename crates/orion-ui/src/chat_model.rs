//! Pure model for the Chat Lens: what the transcript column shows, without a
//! window.
//!
//! The Chat view reads the Engine's Transcript through
//! `GET /api/pane/conversation`; this module holds what that answer means for
//! the surface: which phase the column is in (loading, turns, empty, failed),
//! which thinking rows are open, and whether new turns still follow the
//! bottom. Keeping it here means the rules are testable without a window and
//! the view only draws.

use crate::chat_entities::{ChatTurn, ConversationResponse};

/// What the chat column shows right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatPhase {
    /// The first fetch has not answered yet.
    Loading,
    /// Turns are on screen.
    Ready,
    /// The transcript was read and holds no turns.
    Empty,
    /// The last fetch failed; the message is plain language, never a status code.
    Failed(String),
}

impl ChatPhase {
    /// Whether the column is showing the failed state.
    pub fn is_failed(&self) -> bool {
        matches!(self, ChatPhase::Failed(_))
    }
}

/// The chat column's state: the phase, the turns, and the two rules that make
/// the surface behave (thinking rows, follow-the-bottom).
#[derive(Debug, Clone)]
pub struct ChatModel {
    phase: ChatPhase,
    source: String,
    turns: Vec<ChatTurn>,
    /// The reasoning rows the reader opened (they start folded away).
    thinking_rows: OpenRows,
    /// The tool rows the reader folded away (they start open).
    tool_rows: OpenRows,
    follow: FollowBottom,
}

impl Default for ChatModel {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatModel {
    /// A model that has not fetched anything yet.
    pub fn new() -> Self {
        Self {
            phase: ChatPhase::Loading,
            source: String::new(),
            turns: Vec::new(),
            thinking_rows: OpenRows::new(false),
            tool_rows: OpenRows::new(true),
            follow: FollowBottom::default(),
        }
    }

    /// What the column shows right now.
    pub fn phase(&self) -> &ChatPhase {
        &self.phase
    }

    /// The transcript's source (`claude-transcript`, `scrollback`, …).
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The turns on screen, oldest first.
    pub fn turns(&self) -> &[ChatTurn] {
        &self.turns
    }

    /// A new pane: the column starts empty, with nothing open and following
    /// the bottom again.
    pub fn reset(&mut self) {
        *self = ChatModel::new();
    }

    /// The transcript was read: turns, or the empty state when it holds none.
    pub fn apply(&mut self, response: ConversationResponse) {
        self.source = response.source;
        self.turns = response.turns;
        self.phase = if self.turns.is_empty() {
            ChatPhase::Empty
        } else {
            ChatPhase::Ready
        };
        // Turns only ever grow, so the rows the reader opened stay open.
        self.thinking_rows.retain(self.turns.len());
        self.tool_rows.retain(self.turns.len());
    }

    /// The fetch failed: the message is plain language and a Retry is offered.
    pub fn fail(&mut self, message: String) {
        self.phase = ChatPhase::Failed(message);
    }

    /// The reader asked for the transcript again.
    pub fn retry(&mut self) {
        self.phase = ChatPhase::Loading;
    }

    /// Whether the column shows the loading state: a read is in flight and
    /// there is nothing to show yet.
    pub fn is_loading(&self) -> bool {
        matches!(self.phase, ChatPhase::Loading) && self.turns.is_empty()
    }

    /// Whether the column shows the empty state.
    pub fn is_empty(&self) -> bool {
        matches!(self.phase, ChatPhase::Empty) && self.turns.is_empty()
    }

    /// The failure message, when the last fetch failed.
    pub fn failure(&self) -> Option<&str> {
        match &self.phase {
            ChatPhase::Failed(message) => Some(message),
            _ => None,
        }
    }

    /// Whether a thinking row is open. Reasoning starts folded away.
    pub fn is_thinking_open(&self, turn: usize, part: usize) -> bool {
        self.thinking_rows.is_open(turn, part)
    }

    /// Open or close one thinking row.
    pub fn toggle_thinking(&mut self, turn: usize, part: usize) {
        self.thinking_rows.toggle(turn, part);
    }

    /// Whether a tool row is open. A tool's work starts open: the reader
    /// came for it.
    pub fn is_tool_open(&self, turn: usize, part: usize) -> bool {
        self.tool_rows.is_open(turn, part)
    }

    /// Open or close one tool row.
    pub fn toggle_tool(&mut self, turn: usize, part: usize) {
        self.tool_rows.toggle(turn, part);
    }

    /// Whether new turns follow the bottom.
    pub fn follows_bottom(&self) -> bool {
        self.follow.at_bottom()
    }

    /// The reader scrolled: up pins the view, scrolling while at the bottom
    /// keeps following.
    pub fn on_scroll(&mut self, delta_y: f32, at_bottom_now: bool) {
        self.follow.on_wheel(delta_y, at_bottom_now);
    }

    /// The scroll position says where the reader is.
    pub fn sync_scroll(&mut self, offset_y: f32, max_offset_y: f32) {
        self.follow.sync(offset_y, max_offset_y);
    }
}

/// Which rows of a turn the reader opened, per turn and part. A row the
/// reader never touched keeps the kind's own default.
#[derive(Debug, Clone)]
pub struct OpenRows {
    /// The state of a row the reader touched.
    states: Vec<((usize, usize), bool)>,
    /// Whether a row of this kind starts open.
    default: bool,
}

impl OpenRows {
    /// Rows of a kind that starts `default`.
    pub fn new(default: bool) -> Self {
        Self {
            states: Vec::new(),
            default,
        }
    }

    /// Whether one row is open.
    pub fn is_open(&self, turn: usize, part: usize) -> bool {
        self.states
            .iter()
            .find(|((row_turn, row_part), _)| *row_turn == turn && *row_part == part)
            .map(|(_, open)| *open)
            .unwrap_or(self.default)
    }

    /// Open a closed row, close an open one.
    pub fn toggle(&mut self, turn: usize, part: usize) {
        let open = !self.is_open(turn, part);
        match self
            .states
            .iter_mut()
            .find(|((row_turn, row_part), _)| *row_turn == turn && *row_part == part)
        {
            Some(entry) => entry.1 = open,
            None => self.states.push(((turn, part), open)),
        }
    }

    /// Drop rows of turns that are gone.
    pub fn retain(&mut self, turns: usize) {
        self.states.retain(|((turn, _), _)| *turn < turns);
    }
}

/// Whether the chat follows the bottom of the transcript.
#[derive(Debug, Clone, Copy)]
pub struct FollowBottom {
    at_bottom: bool,
}

/// How close to the bottom still counts as the bottom, in pixels.
pub const BOTTOM_EPSILON: f32 = 2.0;

/// Whether a scroll position is at the bottom of the column. The scroll offset
/// counts *down* as the reader goes down, so the bottom is the most negative
/// offset; one convention, so the view and the model cannot disagree.
pub fn at_bottom(offset_y: f32, max_offset_y: f32) -> bool {
    if max_offset_y <= 0.0 {
        // Nothing to scroll: the whole transcript fits.
        return true;
    }
    -offset_y >= max_offset_y - BOTTOM_EPSILON
}

impl Default for FollowBottom {
    fn default() -> Self {
        Self { at_bottom: true }
    }
}

impl FollowBottom {
    /// Whether new turns follow the bottom.
    pub fn at_bottom(&self) -> bool {
        self.at_bottom
    }

    /// The reader scrolled by `delta_y` and is now at `at_bottom_now`.
    ///
    /// A wheel delta is positive when the reader scrolls *up* (GPUI counts the
    /// wheel the way the platform reports it), so a positive delta pins the
    /// view: the transcript must not jump away.
    pub fn on_wheel(&mut self, delta_y: f32, at_bottom_now: bool) {
        if delta_y > 0.0 {
            // Scrolling up pins the view.
            self.at_bottom = false;
        } else if at_bottom_now {
            // Scrolling down at the bottom keeps following.
            self.at_bottom = true;
        }
    }

    /// The scroll position says where the reader is. A reader who scrolled
    /// away from the bottom (by wheel, by scrollbar or by keyboard) pins the
    /// view; only the bottom itself follows the new turns.
    pub fn sync(&mut self, offset_y: f32, max_offset_y: f32) {
        self.at_bottom = at_bottom(offset_y, max_offset_y);
    }
}

/// The wire's RFC 3339 stamp as a short local time (`10 Nov 03:25`). A stamp
/// the shell cannot read is shown as it came; a missing stamp reads as no stamp.
pub fn format_timestamp(raw: Option<&str>) -> String {
    let Some(raw) = raw.map(str::trim).filter(|stamp| !stamp.is_empty()) else {
        return String::new();
    };
    let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(raw) else {
        return raw.to_string();
    };
    parsed
        .with_timezone(&chrono::Local)
        .format("%d %b %H:%M")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_entities::ChatRole;
    use serde_json::json;

    fn turn(role: &str, parts: serde_json::Value) -> serde_json::Value {
        json!({ "role": role, "ts": "2026-01-01T00:00:00Z", "parts": parts })
    }

    fn response(turns: Vec<serde_json::Value>) -> ConversationResponse {
        ConversationResponse::parse(&json!({
            "source": "claude-transcript",
            "turns": turns,
        }))
        .expect("the wire shape parses")
    }

    #[test]
    fn a_chat_that_has_not_fetched_yet_is_loading() {
        let model = ChatModel::new();
        assert_eq!(model.phase(), &ChatPhase::Loading);
        assert!(model.is_loading());
        assert!(!model.is_empty());
    }

    #[test]
    fn a_transcript_with_turns_is_ready() {
        let mut model = ChatModel::new();
        model.apply(response(vec![turn(
            "user",
            json!([{ "kind": "text", "text": "hello" }]),
        )]));
        assert_eq!(model.phase(), &ChatPhase::Ready);
        assert_eq!(model.turns().len(), 1);
        assert_eq!(model.source(), "claude-transcript");
        assert!(!model.is_loading());
        assert!(!model.is_empty());
    }

    #[test]
    fn a_transcript_without_turns_is_empty() {
        let mut model = ChatModel::new();
        model.apply(response(Vec::new()));
        assert_eq!(model.phase(), &ChatPhase::Empty);
        assert!(model.is_empty());
        assert!(!model.is_loading());
    }

    #[test]
    fn a_failed_fetch_says_so_in_plain_language() {
        let mut model = ChatModel::new();
        model.fail("Couldn't load the conversation. Try again.".to_string());
        assert_eq!(
            model.failure(),
            Some("Couldn't load the conversation. Try again.")
        );
        assert!(model.phase().is_failed());
        assert!(!model.is_empty(), "a failure is not an empty transcript");
    }

    #[test]
    fn a_failed_read_keeps_the_last_turns() {
        let mut model = ChatModel::new();
        model.apply(response(vec![turn(
            "user",
            json!([{ "kind": "text", "text": "hello" }]),
        )]));
        model.fail("Couldn't load the conversation.".to_string());
        assert_eq!(
            model.turns().len(),
            1,
            "the reader keeps the transcript they were reading"
        );
        assert!(model.phase().is_failed());
    }

    #[test]
    fn a_retry_goes_back_to_loading_and_recovers() {
        let mut model = ChatModel::new();
        model.fail("Couldn't load the conversation. Try again.".to_string());
        model.retry();
        assert_eq!(model.phase(), &ChatPhase::Loading);
        assert_eq!(model.failure(), None);

        model.apply(response(vec![turn(
            "assistant",
            json!([{ "kind": "text", "text": "hi" }]),
        )]));
        assert_eq!(model.phase(), &ChatPhase::Ready);
        assert_eq!(model.failure(), None);
    }

    #[test]
    fn thinking_rows_open_and_close_on_their_own() {
        let mut model = ChatModel::new();
        assert!(!model.is_thinking_open(0, 0));
        model.toggle_thinking(0, 0);
        assert!(model.is_thinking_open(0, 0));
        assert!(
            !model.is_thinking_open(0, 1),
            "another part of the same turn keeps its own state"
        );
        model.toggle_thinking(0, 0);
        assert!(!model.is_thinking_open(0, 0));
    }

    #[test]
    fn a_tool_row_starts_open_and_folds_away() {
        let mut model = ChatModel::new();
        assert!(
            model.is_tool_open(0, 0),
            "a tool's work starts open: the reader came for it"
        );
        model.toggle_tool(0, 0);
        assert!(!model.is_tool_open(0, 0), "the row folds away");
        model.toggle_tool(0, 0);
        assert!(model.is_tool_open(0, 0), "and opens again");
        assert!(
            !model.is_thinking_open(0, 0),
            "reasoning starts folded away"
        );
    }

    #[test]
    fn an_open_thinking_row_stays_open_when_the_transcript_grows() {
        let mut model = ChatModel::new();
        model.apply(response(vec![turn(
            "assistant",
            json!([{ "kind": "thinking", "text": "hmm" }]),
        )]));
        model.toggle_thinking(0, 0);
        model.apply(response(vec![
            turn("assistant", json!([{ "kind": "thinking", "text": "hmm" }])),
            turn("user", json!([{ "kind": "text", "text": "more" }])),
        ]));
        assert!(
            model.is_thinking_open(0, 0),
            "the reader's open row survives the next poll"
        );
    }

    #[test]
    fn scrolling_up_pins_the_view_and_scrolling_back_resumes_following() {
        let mut model = ChatModel::new();
        assert!(model.follows_bottom());

        // A positive wheel delta scrolls up.
        model.on_scroll(120.0, false);
        assert!(!model.follows_bottom(), "scrolling up pins the view");

        model.on_scroll(-120.0, true);
        assert!(
            model.follows_bottom(),
            "scrolling back to the bottom resumes following"
        );
    }

    #[test]
    fn the_scroll_position_repins_at_the_bottom() {
        let mut model = ChatModel::new();
        model.on_scroll(120.0, false);
        assert!(!model.follows_bottom());

        // The offset counts down as the reader goes down: 20px short of the
        // bottom still pins the view.
        model.sync_scroll(-980.0, 1000.0);
        assert!(
            !model.follows_bottom(),
            "still 20px above the bottom: the view stays pinned"
        );

        model.sync_scroll(-1000.0, 1000.0);
        assert!(model.follows_bottom());
    }

    #[test]
    fn a_scroll_away_from_the_bottom_pins_the_view() {
        let mut model = ChatModel::new();
        // A scrollbar drag or a keyboard scroll: the position says the reader
        // is not at the bottom, so the transcript must not jump.
        model.sync_scroll(-400.0, 1000.0);
        assert!(
            !model.follows_bottom(),
            "a reader away from the bottom pins the view"
        );
    }

    #[test]
    fn a_transcript_that_fits_does_not_pin_the_view() {
        let mut model = ChatModel::new();
        model.on_scroll(120.0, false);
        model.sync_scroll(0.0, 0.0);
        assert!(model.follows_bottom());
    }

    #[test]
    fn a_new_pane_starts_with_nothing_open() {
        let mut model = ChatModel::new();
        model.apply(response(vec![turn(
            "assistant",
            json!([{ "kind": "thinking", "text": "hmm" }]),
        )]));
        model.toggle_thinking(0, 0);
        model.reset();
        assert!(
            !model.is_thinking_open(0, 0),
            "the old pane's rows are gone"
        );
        assert_eq!(model.turns().len(), 0);
        assert!(model.follows_bottom());
    }

    #[test]
    fn timestamps_read_as_a_short_local_time() {
        assert_eq!(
            format_timestamp(Some("2026-01-01T00:01:00Z")),
            chrono::DateTime::parse_from_rfc3339("2026-01-01T00:01:00Z")
                .unwrap()
                .with_timezone(&chrono::Local)
                .format("%d %b %H:%M")
                .to_string()
        );
        assert!(format_timestamp(Some("2026-01-01T00:01:00Z")).contains("Jan"));
    }

    #[test]
    fn a_missing_or_unreadable_stamp_says_nothing_or_shows_itself() {
        assert_eq!(format_timestamp(None), "");
        assert_eq!(format_timestamp(Some("   ")), "");
        assert_eq!(
            format_timestamp(Some("yesterday")),
            "yesterday",
            "a stamp the shell cannot read is shown as it came"
        );
    }

    #[test]
    fn a_role_is_read_from_the_wire() {
        let parsed = response(vec![turn("user", json!([]))]);
        assert_eq!(parsed.turns[0].role, ChatRole::User);
    }
}
