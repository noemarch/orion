//! Pure shell layout model for the Orbit port.

use crate::app::Lens;

pub struct ShellLayout {
    pub sidebar_width_px: f32,
    pub rail: bool,
    pub touch_target_px: f32,
}

impl ShellLayout {
    pub fn for_width(width: f32) -> Self {
        if width < 900.0 {
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

pub struct HeaderModel {
    pub title: String,
    pub workspace: String,
    pub connection: String,
}

impl HeaderModel {
    pub fn empty() -> Self {
        Self {
            title: "Orbit".to_string(),
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
        assert_eq!(model.title, "Orbit");
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
}
