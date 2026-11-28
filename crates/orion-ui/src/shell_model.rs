//! Pure shell layout model for the Orbit port.

use crate::app::Lens;
use crate::theme::TOUCH_TARGET_PX;

pub struct ShellLayout {
    pub sidebar_width_px: f32,
    pub rail: bool,
    pub touch_target_px: f32,
}

/// The window width below which the shell switches to its narrow (rail)
/// layout. One definition, so every view reads the same breakpoint.
pub const NARROW_BREAKPOINT_PX: f32 = 900.0;

/// App header height at wide widths (`--header-h-wide`, DESIGN.md §tokens).
pub const HEADER_HEIGHT_WIDE_PX: f32 = 46.0;
/// Default wide sidebar width (`--sidebar-w`, DESIGN.md §tokens).
pub const SIDEBAR_WIDTH_WIDE_PX: f32 = 264.0;
pub const SIDEBAR_WIDTH_MIN_PX: f32 = 220.0;
pub const SIDEBAR_WIDTH_MAX_PX: f32 = 420.0;
pub const MIN_WINDOW_WIDTH_PX: f32 = 900.0;
pub const MIN_WINDOW_HEIGHT_PX: f32 = 600.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowGeometry {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl ShellLayout {
    pub fn for_width(width: f32) -> Self {
        if width < NARROW_BREAKPOINT_PX {
            Self {
                sidebar_width_px: 64.0,
                rail: true,
                touch_target_px: TOUCH_TARGET_PX,
            }
        } else {
            Self {
                sidebar_width_px: SIDEBAR_WIDTH_WIDE_PX,
                rail: false,
                touch_target_px: TOUCH_TARGET_PX,
            }
        }
    }

    pub fn for_width_with_preference(width: f32, preferred_sidebar_width: f32) -> Self {
        let mut layout = Self::for_width(width);
        if !layout.rail {
            layout.sidebar_width_px =
                preferred_sidebar_width.clamp(SIDEBAR_WIDTH_MIN_PX, SIDEBAR_WIDTH_MAX_PX);
        }
        layout
    }
}

pub fn clamp_window_bounds(
    geometry: WindowGeometry,
    display_x: f32,
    display_y: f32,
    display_width: f32,
    display_height: f32,
) -> WindowGeometry {
    let min_width = MIN_WINDOW_WIDTH_PX.min(display_width);
    let min_height = MIN_WINDOW_HEIGHT_PX.min(display_height);
    let width = geometry
        .width
        .clamp(min_width, display_width.max(min_width));
    let height = geometry
        .height
        .clamp(min_height, display_height.max(min_height));
    WindowGeometry {
        x: geometry
            .x
            .clamp(display_x, display_x + display_width - width),
        y: geometry
            .y
            .clamp(display_y, display_y + display_height - height),
        width,
        height,
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

/// The three lenses in switcher order.
pub const LENS_ORDER: [Lens; 3] = [Lens::Chat, Lens::Terminal, Lens::Changes];

/// Position of a lens in [`LENS_ORDER`].
pub fn lens_position(lens: Lens) -> usize {
    match lens {
        Lens::Chat => 0,
        Lens::Terminal => 1,
        Lens::Changes => 2,
    }
}

/// Move through the three lenses with a roving arrow key, wrapping at the ends.
pub fn cycle_lens(current: Lens, forward: bool) -> Lens {
    let step = if forward {
        1
    } else {
        LENS_ORDER.len() as i32 - 1
    };
    let next = (lens_position(current) as i32 + step).rem_euclid(LENS_ORDER.len() as i32) as usize;
    LENS_ORDER[next]
}

/// The compact switcher has only Chat and Terminal; Changes keeps Chat selected.
pub fn cycle_middle_lens(current: Lens) -> Lens {
    if current == Lens::Terminal {
        Lens::Chat
    } else {
        Lens::Terminal
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

/// Session list width in the reference 3-panel layout (px).
pub const SIDEBAR_WIDTH_REFERENCE_PX: f32 = 300.0;
/// Conversation column width in the reference 3-panel layout (px).
pub const CONVERSATION_WIDTH_PX: f32 = 670.0;
/// Narrowest the diff review pane may get before it is unreadable (px).
pub const REVIEW_MIN_WIDTH_PX: f32 = 320.0;
/// Window width below which the review pane collapses away (px).
pub const REVIEW_COLLAPSE_BREAKPOINT_PX: f32 = 1200.0;

/// What the right-hand column shows. Review keeps the diff review; Transcript
/// shows the same agent's live transcript. Pure state, so the shell can switch
/// modes without a window and the two bodies share one column slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RightColumnMode {
    #[default]
    Review,
    Transcript,
}

/// The three columns of the reference layout: session list, conversation, and
/// diff review. When the window is too narrow for all three, the review pane
/// collapses and its room goes to the conversation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReferenceLayout {
    pub sidebar_w: f32,
    pub conversation_w: f32,
    pub review_w: f32,
    pub collapse_review: bool,
}

/// Split one window width into the reference layout's three columns.
pub fn reference_layout(width: f32) -> ReferenceLayout {
    if width < REVIEW_COLLAPSE_BREAKPOINT_PX {
        ReferenceLayout {
            sidebar_w: SIDEBAR_WIDTH_REFERENCE_PX,
            conversation_w: (width - SIDEBAR_WIDTH_REFERENCE_PX).max(0.0),
            review_w: 0.0,
            collapse_review: true,
        }
    } else {
        // The three columns must fit the window. The review keeps its readable
        // minimum, and the conversation gives up room only in the band where
        // 300 + 670 + 320 would overflow (1200..1290px).
        let conversation_w =
            CONVERSATION_WIDTH_PX.min(width - SIDEBAR_WIDTH_REFERENCE_PX - REVIEW_MIN_WIDTH_PX);
        ReferenceLayout {
            sidebar_w: SIDEBAR_WIDTH_REFERENCE_PX,
            conversation_w,
            review_w: (width - SIDEBAR_WIDTH_REFERENCE_PX - conversation_w)
                .max(REVIEW_MIN_WIDTH_PX),
            collapse_review: false,
        }
    }
}

/// Whether the left sidebar div is mounted. A hidden sidebar is not mounted at
/// all (zero width), which is distinct from the narrow-width rail decided by
/// [`ShellLayout::for_width`]. The sidebar entity stays alive; only its mount
/// is conditional, so session selection and scroll survive a hide/show cycle.
pub fn sidebar_visible(sidebar_open: bool) -> bool {
    sidebar_open
}

/// Whether the right-hand review column is shown. It needs the user's opt-in,
/// enough window width, at least one workspace, and no split second pane.
/// `split_second` is true when a split second pane is present.
pub fn review_column_visible(
    review_open: bool,
    collapse_review: bool,
    has_workspaces: bool,
    split_second: bool,
) -> bool {
    review_open && !collapse_review && has_workspaces && !split_second
}

/// Whether the right-hand review column is shown while a shell terminal is
/// active. A shell terminal owns the full window width, so the review column
/// is always hidden in that case; otherwise the ordinary rule applies.
pub fn review_column_visible_for_terminal(
    review_open: bool,
    collapse_review: bool,
    has_workspaces: bool,
    split_second: bool,
    shell_terminal_active: bool,
) -> bool {
    if shell_terminal_active {
        false
    } else {
        review_column_visible(review_open, collapse_review, has_workspaces, split_second)
    }
}

/// The conversation column's effective width. A shell terminal takes the
/// review column's room as well, so it spans both columns; otherwise it keeps
/// the reference conversation width.
pub fn effective_conversation_width(reference: &ReferenceLayout, full_width: bool) -> f32 {
    if full_width {
        reference.conversation_w + reference.review_w
    } else {
        reference.conversation_w
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn narrow_window_uses_rail_with_touch_targets() {
        let layout = ShellLayout::for_width(800.0);
        assert!(layout.rail);
        assert_eq!(layout.sidebar_width_px, 64.0);
        assert!(layout.touch_target_px >= 44.0);
    }

    #[::core::prelude::v1::test]
    fn remembered_sidebar_width_is_used_only_when_not_rail() {
        let wide = ShellLayout::for_width_with_preference(1280.0, 320.0);
        assert!(!wide.rail);
        assert_eq!(wide.sidebar_width_px, 320.0);

        let clamped = ShellLayout::for_width_with_preference(1280.0, 900.0);
        assert_eq!(clamped.sidebar_width_px, SIDEBAR_WIDTH_MAX_PX);

        let rail = ShellLayout::for_width_with_preference(800.0, 320.0);
        assert!(rail.rail);
        assert_eq!(rail.sidebar_width_px, 64.0);
    }

    #[::core::prelude::v1::test]
    fn restored_bounds_are_clamped_into_the_display() {
        let geometry = WindowGeometry {
            x: 5000.0,
            y: -200.0,
            width: 2400.0,
            height: 1500.0,
        };
        let clamped = clamp_window_bounds(geometry, 100.0, 50.0, 1920.0, 1080.0);
        assert_eq!(clamped.x, 100.0);
        assert_eq!(clamped.y, 50.0);
        assert_eq!(clamped.width, 1920.0);
        assert_eq!(clamped.height, 1080.0);

        let small = clamp_window_bounds(
            WindowGeometry {
                x: 100.0,
                y: 100.0,
                width: 200.0,
                height: 200.0,
            },
            0.0,
            0.0,
            1920.0,
            1080.0,
        );
        assert_eq!(small.width, MIN_WINDOW_WIDTH_PX);
        assert_eq!(small.height, MIN_WINDOW_HEIGHT_PX);
    }

    #[::core::prelude::v1::test]
    fn desktop_window_uses_orbit_sidebar_width() {
        let layout = ShellLayout::for_width(1280.0);
        assert!(!layout.rail);
        assert_eq!(layout.sidebar_width_px, 264.0);
    }

    #[::core::prelude::v1::test]
    fn empty_session_renders_inviting_empty_state() {
        let model = HeaderModel::empty();
        assert_eq!(model.title, "Orion");
        assert_eq!(model.workspace, "No workspace");
        assert_eq!(model.connection, "Connecting…");
    }

    #[::core::prelude::v1::test]
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

    #[::core::prelude::v1::test]
    fn cycle_lens_wraps_in_both_directions() {
        assert_eq!(cycle_lens(Lens::Chat, true), Lens::Terminal);
        assert_eq!(cycle_lens(Lens::Terminal, true), Lens::Changes);
        assert_eq!(cycle_lens(Lens::Changes, true), Lens::Chat);
        assert_eq!(cycle_lens(Lens::Chat, false), Lens::Changes);
        assert_eq!(cycle_lens(Lens::Changes, false), Lens::Terminal);
        assert_eq!(cycle_lens(Lens::Terminal, false), Lens::Chat);
    }

    #[::core::prelude::v1::test]
    fn cycle_middle_lens_toggles_chat_and_terminal() {
        assert_eq!(cycle_middle_lens(Lens::Chat), Lens::Terminal);
        assert_eq!(cycle_middle_lens(Lens::Terminal), Lens::Chat);
        assert_eq!(cycle_middle_lens(Lens::Changes), Lens::Terminal);
    }

    #[::core::prelude::v1::test]
    fn lens_book_defaults_new_panes_to_chat() {
        let book = LensBook::new();
        assert_eq!(book.lens_for(Some("p1")), crate::app::Lens::Chat);
        assert_eq!(book.lens_for(None), crate::app::Lens::Chat);
    }

    #[::core::prelude::v1::test]
    fn lens_book_remembers_each_pane_separately() {
        let mut book = LensBook::new();
        book.set(Some("p1"), crate::app::Lens::Terminal);
        book.set(Some("p2"), crate::app::Lens::Changes);
        assert_eq!(book.lens_for(Some("p1")), crate::app::Lens::Terminal);
        assert_eq!(book.lens_for(Some("p2")), crate::app::Lens::Changes);
        assert_eq!(book.lens_for(Some("p3")), crate::app::Lens::Chat);
    }

    #[::core::prelude::v1::test]
    fn lens_book_forgets_closed_panes() {
        let mut book = LensBook::new();
        book.set(Some("p1"), crate::app::Lens::Terminal);
        book.forget("p1");
        assert_eq!(book.lens_for(Some("p1")), crate::app::Lens::Chat);
    }

    #[::core::prelude::v1::test]
    fn lens_book_ignores_a_missing_pane() {
        let mut book = LensBook::new();
        book.set(None, crate::app::Lens::Changes);
        assert_eq!(book.lens_for(None), crate::app::Lens::Chat);
    }

    #[::core::prelude::v1::test]
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

    #[::core::prelude::v1::test]
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

    #[::core::prelude::v1::test]
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

    #[::core::prelude::v1::test]
    fn lens_book_drops_panes_that_left_the_session() {
        let mut book = LensBook::new();
        book.set(Some("p1"), crate::app::Lens::Terminal);
        book.set(Some("p2"), crate::app::Lens::Changes);
        book.retain_panes(&["p2".to_string()]);
        assert_eq!(book.lens_for(Some("p1")), crate::app::Lens::Chat);
        assert_eq!(book.lens_for(Some("p2")), crate::app::Lens::Changes);
    }

    #[::core::prelude::v1::test]
    fn reference_layout_at_1600_splits_three_panels() {
        let layout = reference_layout(1600.0);
        assert_eq!(layout.sidebar_w, 300.0);
        assert_eq!(layout.conversation_w, 670.0);
        assert_eq!(layout.review_w, 630.0);
        assert!(!layout.collapse_review);
    }

    #[::core::prelude::v1::test]
    fn right_column_mode_defaults_to_review() {
        assert_eq!(RightColumnMode::default(), RightColumnMode::Review);
        assert_ne!(RightColumnMode::Review, RightColumnMode::Transcript);
    }

    #[::core::prelude::v1::test]
    fn reference_layout_at_1280_fits_within_the_window() {
        let layout = reference_layout(1280.0);
        assert_eq!(layout.sidebar_w, 300.0);
        assert_eq!(layout.conversation_w, 660.0);
        assert_eq!(layout.review_w, REVIEW_MIN_WIDTH_PX);
        assert!(!layout.collapse_review);
        assert!(layout.sidebar_w + layout.conversation_w + layout.review_w <= 1280.0);
    }

    #[::core::prelude::v1::test]
    fn reference_layout_at_1199_collapses_review() {
        let layout = reference_layout(1199.0);
        assert_eq!(layout.sidebar_w, 300.0);
        assert_eq!(layout.review_w, 0.0);
        assert!(layout.collapse_review);
        assert_eq!(
            layout.sidebar_w + layout.conversation_w + layout.review_w,
            1199.0
        );
    }

    #[::core::prelude::v1::test]
    fn reference_layout_shrinks_conversation_only_in_the_clamp_band() {
        // Below the collapse breakpoint the review is gone and the conversation
        // owns the rest of the window.
        assert!(reference_layout(1199.0).collapse_review);
        assert_eq!(reference_layout(1199.0).conversation_w, 899.0);
        // Inside 1200..1290 the conversation gives up exactly what the review
        // needs to keep its 320px floor.
        assert_eq!(reference_layout(1200.0).conversation_w, 580.0);
        assert_eq!(reference_layout(1280.0).conversation_w, 660.0);
        assert_eq!(reference_layout(1289.0).conversation_w, 669.0);
        // At and above 1290 the conversation keeps its full reference width and
        // the review takes the surplus.
        assert_eq!(reference_layout(1290.0).conversation_w, 670.0);
        assert_eq!(reference_layout(1290.0).review_w, 320.0);
        assert_eq!(reference_layout(1600.0).conversation_w, 670.0);
        assert_eq!(reference_layout(1600.0).review_w, 630.0);
    }

    #[::core::prelude::v1::test]
    fn collapsed_review_column_is_never_mounted() {
        assert!(!review_column_visible(true, true, true, false));
        assert!(review_column_visible(true, false, true, false));
    }

    #[::core::prelude::v1::test]
    fn reference_layout_at_1100_collapses_review_into_conversation() {
        let layout = reference_layout(1100.0);
        assert_eq!(layout.sidebar_w, 300.0);
        assert_eq!(layout.conversation_w, 800.0);
        assert_eq!(layout.review_w, 0.0);
        assert!(layout.collapse_review);
    }

    #[::core::prelude::v1::test]
    fn reference_layout_keeps_the_classic_wide_sidebar_width() {
        assert_eq!(SIDEBAR_WIDTH_WIDE_PX, 264.0);
    }

    #[::core::prelude::v1::test]
    fn sidebar_visible_tracks_the_toggle() {
        assert!(sidebar_visible(true));
        assert!(!sidebar_visible(false));
    }

    #[::core::prelude::v1::test]
    fn review_column_visible_covers_every_boolean_combination() {
        for review_open in [false, true] {
            for collapse_review in [false, true] {
                for has_workspaces in [false, true] {
                    for split_second in [false, true] {
                        let expected =
                            review_open && !collapse_review && has_workspaces && !split_second;
                        assert_eq!(
                            review_column_visible(
                                review_open,
                                collapse_review,
                                has_workspaces,
                                split_second
                            ),
                            expected,
                            "review_open={review_open} collapse_review={collapse_review} \
                             has_workspaces={has_workspaces} split_second={split_second}"
                        );
                    }
                }
            }
        }
    }

    #[::core::prelude::v1::test]
    fn shell_terminal_hides_the_review_column() {
        // Every ordinary condition is met, yet an active shell terminal wins.
        assert!(!review_column_visible_for_terminal(
            true, false, true, false, true
        ));
        // Without a shell terminal the ordinary rule still applies.
        assert!(review_column_visible_for_terminal(
            true, false, true, false, false
        ));
        assert!(!review_column_visible_for_terminal(
            false, false, true, false, false
        ));
    }

    #[::core::prelude::v1::test]
    fn shell_terminal_expands_the_conversation_width() {
        let reference = reference_layout(1600.0);
        assert_eq!(effective_conversation_width(&reference, false), 670.0);
        assert_eq!(
            effective_conversation_width(&reference, true),
            670.0 + 630.0
        );
        // A collapsed review contributes no extra width.
        let narrow = reference_layout(1100.0);
        assert_eq!(
            effective_conversation_width(&narrow, true),
            narrow.conversation_w
        );
    }
}
