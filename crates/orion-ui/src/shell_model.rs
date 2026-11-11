//! Pure shell layout model for the Orbit port.

use crate::app::Lens;

pub struct ShellLayout {
    pub sidebar_width_px: f32,
    pub rail: bool,
    pub touch_target_px: f32,
}

/// The window width below which the shell switches to its narrow (rail)
/// layout. One definition, so every view reads the same breakpoint.
pub const NARROW_BREAKPOINT_PX: f32 = 900.0;

impl ShellLayout {
    pub fn for_width(width: f32) -> Self {
        if width < NARROW_BREAKPOINT_PX {
            Self {
                sidebar_width_px: 64.0,
                rail: true,
                touch_target_px: 44.0,
            }
        } else {
            Self {
                sidebar_width_px: 280.0,
                rail: false,
                touch_target_px: 44.0,
            }
        }
    }
}

/// The Files surface's width in the Changes lens (px): the tree and the reader.
pub const FILES_SURFACE_WIDTH_WIDE: f32 = 460.0;
/// Narrowest the tree may get (px).
pub const TREE_WIDTH_MIN: f32 = 140.0;
/// Narrowest the reader may get (px) — below this the code is not readable.
pub const READER_WIDTH_MIN: f32 = 240.0;
/// The tree's width before the reader drags the splitter (px).
pub const TREE_WIDTH_DEFAULT: f32 = 212.0;
/// The splitter's own column width (px). One definition for every split view,
/// so a drag on one lens costs the same as a drag on another.
pub const SPLITTER_WIDTH_PX: f32 = 8.0;

/// Clamp a requested tree width so the reader keeps [`READER_WIDTH_MIN`] of
/// whatever room the view received, with the splitter's column taken off too.
pub fn clamp_tree_width(requested: f32, available: f32) -> f32 {
    let max = (available - READER_WIDTH_MIN - SPLITTER_WIDTH_PX).max(TREE_WIDTH_MIN);
    requested.clamp(TREE_WIDTH_MIN, max)
}

pub struct HeaderModel {
    pub title: String,
    pub workspace: String,
    pub connection: String,
}

impl HeaderModel {
    pub fn empty() -> Self {
        Self {
            title: "Orion".to_string(),
            workspace: "No workspace".to_string(),
            connection: "Connecting…".to_string(),
        }
    }
}

pub fn lens_segment_state(lens: Lens) -> (bool, bool, bool) {
    match lens {
        Lens::Chat => (true, false, false),
        Lens::Terminal => (false, true, false),
        Lens::Changes => (false, false, true),
    }
}

/// Which lens each pane was last showing. Switching workspace or tab changes
/// the active pane without resetting the lens that pane chose.
#[derive(Debug, Default)]
pub struct LensBook {
    by_pane: std::collections::HashMap<String, Lens>,
}

impl LensBook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn lens_for(&self, pane_id: Option<&str>) -> Lens {
        pane_id
            .and_then(|id| self.by_pane.get(id).copied())
            .unwrap_or(Lens::Chat)
    }

    pub fn set(&mut self, pane_id: Option<&str>, lens: Lens) {
        if let Some(id) = pane_id {
            self.by_pane.insert(id.to_string(), lens);
        }
    }

    pub fn forget(&mut self, pane_id: &str) {
        self.by_pane.remove(pane_id);
    }

    /// Drops entries for panes that are no longer in the Session, so closing a
    /// workspace does not leak one lens choice per pane for the whole run.
    pub fn retain_panes(&mut self, live: &[String]) {
        self.by_pane
            .retain(|pane_id, _| live.iter().any(|live| live == pane_id));
    }
}

/// What a key does to the tab label being edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextEdit {
    /// Escape: drop the draft.
    Cancel,
    /// Enter: commit the (trimmed) draft.
    Commit,
    /// The draft changed.
    Changed,
    /// Not an editing key.
    Ignored,
}

/// Applies one keystroke to a tab label draft, so rename is a pure reducer the
/// tests can drive without a window. `command_modifier` covers Ctrl/Alt/Cmd:
/// those chords are shortcuts, never text.
pub fn apply_text_key(
    text: &mut String,
    key: &str,
    key_char: Option<&str>,
    command_modifier: bool,
) -> TextEdit {
    if command_modifier {
        return TextEdit::Ignored;
    }
    if key == "escape" {
        return TextEdit::Cancel;
    }
    if key == "enter" {
        return TextEdit::Commit;
    }
    if key == "backspace" {
        text.pop();
        return TextEdit::Changed;
    }
    if let Some(ch) = key_char {
        text.push_str(ch);
        return TextEdit::Changed;
    }
    TextEdit::Ignored
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrow_window_uses_rail_with_touch_targets() {
        let layout = ShellLayout::for_width(800.0);
        assert!(layout.rail);
        assert_eq!(layout.sidebar_width_px, 64.0);
        assert!(layout.touch_target_px >= 44.0);
    }

    #[test]
    fn desktop_window_uses_orbit_sidebar_width() {
        let layout = ShellLayout::for_width(1280.0);
        assert!(!layout.rail);
        assert_eq!(layout.sidebar_width_px, 280.0);
    }

    #[test]
    fn empty_session_renders_inviting_empty_state() {
        let model = HeaderModel::empty();
        assert_eq!(model.title, "Orion");
        assert_eq!(model.workspace, "No workspace");
        assert_eq!(model.connection, "Connecting…");
    }

    #[test]
    fn lens_segment_marks_only_the_active_lens() {
        assert_eq!(
            lens_segment_state(crate::app::Lens::Chat),
            (true, false, false)
        );
        assert_eq!(
            lens_segment_state(crate::app::Lens::Terminal),
            (false, true, false)
        );
        assert_eq!(
            lens_segment_state(crate::app::Lens::Changes),
            (false, false, true)
        );
    }

    #[test]
    fn lens_book_defaults_new_panes_to_chat() {
        let book = LensBook::new();
        assert_eq!(book.lens_for(Some("p1")), crate::app::Lens::Chat);
        assert_eq!(book.lens_for(None), crate::app::Lens::Chat);
    }

    #[test]
    fn lens_book_remembers_each_pane_separately() {
        let mut book = LensBook::new();
        book.set(Some("p1"), crate::app::Lens::Terminal);
        book.set(Some("p2"), crate::app::Lens::Changes);
        assert_eq!(book.lens_for(Some("p1")), crate::app::Lens::Terminal);
        assert_eq!(book.lens_for(Some("p2")), crate::app::Lens::Changes);
        assert_eq!(book.lens_for(Some("p3")), crate::app::Lens::Chat);
    }

    #[test]
    fn lens_book_forgets_closed_panes() {
        let mut book = LensBook::new();
        book.set(Some("p1"), crate::app::Lens::Terminal);
        book.forget("p1");
        assert_eq!(book.lens_for(Some("p1")), crate::app::Lens::Chat);
    }

    #[test]
    fn lens_book_ignores_a_missing_pane() {
        let mut book = LensBook::new();
        book.set(None, crate::app::Lens::Changes);
        assert_eq!(book.lens_for(None), crate::app::Lens::Chat);
    }

    #[test]
    fn rename_keys_build_and_commit_a_label() {
        let mut text = String::from("ta");
        assert_eq!(
            apply_text_key(&mut text, "b", Some("b"), false),
            TextEdit::Changed
        );
        assert_eq!(text, "tab");
        assert_eq!(
            apply_text_key(&mut text, "backspace", None, false),
            TextEdit::Changed
        );
        assert_eq!(text, "ta");
        assert_eq!(
            apply_text_key(&mut text, "enter", None, false),
            TextEdit::Commit
        );
        assert_eq!(
            apply_text_key(&mut text, "escape", None, false),
            TextEdit::Cancel
        );
        assert_eq!(
            apply_text_key(&mut text, "shift", None, false),
            TextEdit::Ignored
        );
        assert_eq!(text, "ta", "ignored keys leave the draft alone");
    }

    #[test]
    fn rename_ignores_ctrl_alt_and_command_chords() {
        let mut text = String::from("tab");
        for key in ["c", "x", "v", "a"] {
            assert_eq!(
                apply_text_key(&mut text, key, Some(key), true),
                TextEdit::Ignored,
                "ctrl+{key} is a shortcut, not text"
            );
        }
        assert_eq!(text, "tab", "a chord leaves the draft alone");
    }

    #[test]
    fn clamp_tree_width_keeps_the_reader_usable() {
        // Plenty of room: the default passes through.
        assert_eq!(
            clamp_tree_width(TREE_WIDTH_DEFAULT, FILES_SURFACE_WIDTH_WIDE),
            TREE_WIDTH_DEFAULT
        );
        // No room measured yet: the reader's floor never makes the cap
        // negative, so the tree keeps its own minimum.
        assert_eq!(clamp_tree_width(TREE_WIDTH_DEFAULT, 0.0), TREE_WIDTH_MIN);
        // Too small a request: the tree keeps its own minimum.
        assert_eq!(
            clamp_tree_width(10.0, FILES_SURFACE_WIDTH_WIDE),
            TREE_WIDTH_MIN
        );
        // Too large a request: capped at what the reader can spare, with the
        // splitter's own column taken off the top.
        assert_eq!(
            clamp_tree_width(10_000.0, FILES_SURFACE_WIDTH_WIDE),
            FILES_SURFACE_WIDTH_WIDE - READER_WIDTH_MIN - SPLITTER_WIDTH_PX
        );
        // Narrow room still leaves the reader its floor.
        assert_eq!(clamp_tree_width(TREE_WIDTH_DEFAULT, 300.0), TREE_WIDTH_MIN);
    }

    #[test]
    fn lens_book_drops_panes_that_left_the_session() {
        let mut book = LensBook::new();
        book.set(Some("p1"), crate::app::Lens::Terminal);
        book.set(Some("p2"), crate::app::Lens::Changes);
        book.retain_panes(&["p2".to_string()]);
        assert_eq!(book.lens_for(Some("p1")), crate::app::Lens::Chat);
        assert_eq!(book.lens_for(Some("p2")), crate::app::Lens::Changes);
    }
}
