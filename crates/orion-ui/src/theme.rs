//! Quiet Slate theme tokens (DESIGN.md §2, the generated table — the source of
//! truth; values mirrored value-for-value).
//!
//! Usage: views call [`Theme::current()`] in `render` and bind its tokens to
//! backgrounds/text. The app sets the current theme from saved preferences.
//! A module-level accessor is used instead of threading `Theme` through every
//! constructor; the accessor is the single seam, so swapping it for DI later
//! does not touch call sites.
//!
//! Density (DESIGN.md §1): comfortable/compact scales spacing and text
//! without changing information architecture. [`Theme::density_scale`]
//! multiplies spacing tokens; text sizes use [`Theme::text_size`].

use gpui::{rgb, rgba, Hsla, Rgba};
use std::sync::{LazyLock, Mutex};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeMode {
    #[default]
    Dark,
    Light,
}

impl ThemeMode {
    pub fn as_wire(self) -> &'static str {
        match self {
            ThemeMode::Dark => "dark",
            ThemeMode::Light => "light",
        }
    }

    pub fn from_wire(s: &str) -> Self {
        match s {
            "light" => ThemeMode::Light,
            _ => ThemeMode::Dark,
        }
    }
}

/// UI density, persisted as `settings.density` ("comfortable"/"compact").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Density {
    #[default]
    Comfortable,
    Compact,
}

impl Density {
    pub fn as_wire(self) -> &'static str {
        match self {
            Density::Comfortable => "comfortable",
            Density::Compact => "compact",
        }
    }

    pub fn from_wire(s: &str) -> Self {
        match s {
            "compact" => Density::Compact,
            _ => Density::Comfortable,
        }
    }
}

/// One palette generation: Quiet Slate dark is the default (DESIGN.md §1).
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub mode: ThemeMode,
    pub density: Density,
    /// Surface/base `--bg`.
    pub surface_base: Rgba,
    /// Surface/panel `--bg-panel`.
    pub surface_panel: Rgba,
    /// Surface/elevated `--bg-elevated`.
    pub surface_elevated: Rgba,
    /// Surface/hover `--bg-hover`.
    pub surface_hover: Rgba,
    /// Surface/input `--bg-input`.
    pub surface_input: Rgba,
    /// `--border`.
    pub border: Rgba,
    /// `--border-strong`.
    pub border_strong: Rgba,
    /// Text/primary `--text`.
    pub text: Rgba,
    /// Text/dim `--text-dim`.
    pub text_dim: Rgba,
    /// Text/strong `--text-strong`.
    pub text_strong: Rgba,
    /// Accent `--accent` — rose marks the live edge alone (DESIGN.md §1).
    pub accent: Rgba,
    /// Accent/tint `--accent-tint`.
    pub accent_tint: Rgba,
    /// Primary `--primary`.
    pub primary: Rgba,
    /// Primary/hover `--primary-hover`.
    pub primary_hover: Rgba,
    /// Primary/text `--primary-text`.
    pub primary_text: Rgba,
    /// Primary/tint `--primary-tint`.
    pub primary_tint: Rgba,
    /// Syntax/keyword `--syntax-keyword`.
    pub syntax_keyword: Rgba,
    /// Syntax/string `--syntax-string`.
    pub syntax_string: Rgba,
    /// Syntax/number `--syntax-number`.
    pub syntax_number: Rgba,
    /// Syntax/type `--syntax-type`.
    pub syntax_type: Rgba,
    /// Syntax/variable `--syntax-variable`.
    pub syntax_variable: Rgba,
    /// Syntax/meta `--syntax-meta`.
    pub syntax_meta: Rgba,
    /// Status/idle `--status-idle`.
    pub status_idle: Rgba,
    /// Status/working `--status-working` (#7fc4de dark).
    pub status_working: Rgba,
    /// Status/blocked `--status-blocked`.
    pub status_blocked: Rgba,
    /// Status/done `--status-done`.
    pub status_done: Rgba,
    /// Working/tint `--status-working-tint`.
    pub status_working_tint: Rgba,
    /// Blocked/tint `--status-blocked-tint`.
    pub status_blocked_tint: Rgba,
    /// Done/tint `--status-done-tint`.
    pub status_done_tint: Rgba,
    /// Indicator/working `--indicator-working` (sidebar RUN pulse).
    pub indicator_working: Rgba,
    /// Indicator/blocked `--indicator-blocked` (sidebar ERROR).
    pub indicator_blocked: Rgba,
    /// Indicator/done `--indicator-done` (sidebar DONE).
    pub indicator_done: Rgba,
    /// Indicator/idle `--indicator-idle`.
    pub indicator_idle: Rgba,
    /// Indicator/unknown `--indicator-unknown`.
    pub indicator_unknown: Rgba,
    /// Danger/text `--danger-text`.
    pub danger_text: Rgba,
    /// Danger/tint `--danger-tint`.
    pub danger_tint: Rgba,
    /// Notice `--notice`.
    pub notice: Rgba,
    /// Overlay/scrim `--scrim`.
    pub scrim: Rgba,
    /// Terminal/background `--term-bg`.
    pub term_bg: Rgba,
    /// Terminal/foreground `--term-fg`.
    pub term_fg: Rgba,
    /// Terminal/cursor `--term-cursor`.
    pub term_cursor: Rgba,
    /// Terminal/selection `--term-selection`.
    pub term_selection: Rgba,
}

fn tint(hex: u32, alpha_pct: u8) -> Rgba {
    let a = ((alpha_pct as u32 * 255) + 50) / 100;
    rgba((hex << 8) | a)
}

impl Theme {
    /// Quiet Slate dark (DESIGN.md §2, dark column). The default before
    /// settings load (DESIGN.md §1).
    pub fn dark() -> Self {
        Self::for_mode(ThemeMode::Dark, Density::Comfortable)
    }

    /// Quiet Slate light (DESIGN.md §2, light column).
    pub fn light() -> Self {
        Self::for_mode(ThemeMode::Light, Density::Comfortable)
    }

    pub fn for_mode(mode: ThemeMode, density: Density) -> Self {
        match mode {
            ThemeMode::Dark => Theme {
                mode,
                density,
                surface_base: rgb(0x221d27),
                surface_panel: rgb(0x221d27),
                surface_elevated: rgb(0x2a2430),
                surface_hover: rgb(0x2e2a33),
                surface_input: rgb(0x28232e),
                border: tint(0xffffff, 6),
                border_strong: tint(0xffffff, 12),
                text: rgb(0xf2eef5),
                text_dim: rgb(0x9a90a3),
                text_strong: rgb(0xf2eef5),
                accent: rgb(0xa3185a),
                accent_tint: tint(0xa3185a, 12),
                primary: rgb(0x8f1a52),
                primary_hover: rgb(0xa3185a),
                primary_text: rgb(0xf2eef5),
                primary_tint: tint(0x8f1a52, 28),
                syntax_keyword: rgb(0xff8fa8),
                syntax_string: rgb(0xa8d48a),
                syntax_number: rgb(0xd2a8ff),
                syntax_type: rgb(0x79c0ff),
                syntax_variable: rgb(0xffb86b),
                syntax_meta: rgb(0xb49dd6),
                status_idle: rgb(0x9a93a6),
                status_working: rgb(0x7fc4de),
                status_blocked: rgb(0xff9a92),
                status_done: rgb(0x8fd07a),
                status_working_tint: tint(0x7fc4de, 14),
                status_blocked_tint: tint(0xff9a92, 10),
                status_done_tint: tint(0x8fd07a, 14),
                indicator_working: rgb(0xa3185a),
                indicator_blocked: rgb(0xf85149),
                indicator_done: rgb(0x3fb950),
                indicator_idle: rgb(0x9a90a3),
                indicator_unknown: rgb(0x6c7086),
                danger_text: rgb(0xffd9d4),
                danger_tint: tint(0xf85149, 12),
                notice: rgb(0xd9a7ba),
                scrim: rgba(0x060408a3),
                term_bg: rgb(0x121016),
                term_fg: rgb(0xecebf1),
                term_cursor: rgb(0xa3185a),
                term_selection: rgb(0x3a2433),
            },
            ThemeMode::Light => Theme {
                mode,
                density,
                surface_base: rgb(0xfaf5fa),
                surface_panel: rgb(0xffffff),
                surface_elevated: rgb(0xf3ebf1),
                surface_hover: rgb(0xece1e9),
                surface_input: rgb(0xffffff),
                border: rgb(0xe4d8e0),
                border_strong: rgb(0xcbbcc7),
                text: rgb(0x501854),
                text_dim: rgb(0x7a5570),
                text_strong: rgb(0x2e0d31),
                accent: rgb(0xa84370),
                accent_tint: tint(0xa84370, 10),
                primary: rgb(0xa84370),
                primary_hover: rgb(0xc25081),
                primary_text: rgb(0xfff7fa),
                primary_tint: tint(0xa84370, 14),
                syntax_keyword: rgb(0xa33a17),
                syntax_string: rgb(0x4b6b18),
                syntax_number: rgb(0x8a3f7a),
                syntax_type: rgb(0x155a72),
                syntax_variable: rgb(0x7a5418),
                syntax_meta: rgb(0x5d4791),
                status_idle: rgb(0x7a5570),
                status_working: rgb(0x155a72),
                status_blocked: rgb(0xa82323),
                status_done: rgb(0x2f6317),
                status_working_tint: tint(0x155a72, 12),
                status_blocked_tint: tint(0xa82323, 12),
                status_done_tint: tint(0x2f6317, 12),
                indicator_working: rgb(0xa84370),
                indicator_blocked: rgb(0xcf222e),
                indicator_done: rgb(0x1a7f37),
                indicator_idle: rgb(0x7a5570),
                indicator_unknown: rgb(0x9ca0b0),
                danger_text: rgb(0x8f1d1d),
                danger_tint: tint(0xa82323, 10),
                notice: rgb(0xa84370),
                scrim: rgba(0x3c1e3252),
                term_bg: rgb(0xffffff),
                term_fg: rgb(0x501854),
                term_cursor: rgb(0xa84370),
                term_selection: rgb(0xf2dbe6),
            },
        }
    }

    /// The live theme. Falls back to Quiet Slate dark with comfortable
    /// density before the app installs saved preferences.
    pub fn current() -> Theme {
        CURRENT
            .lock()
            .map(|guard| *guard)
            .unwrap_or_else(|_| Theme::dark())
    }

    /// Install the theme every view reads via [`Theme::current()`].
    pub fn set_current(theme: Theme) {
        if let Ok(mut guard) = CURRENT.lock() {
            *guard = theme;
        }
    }

    /// Spacing/text scale for the density (DESIGN.md §1: scales without
    /// changing information architecture).
    pub fn density_scale(self) -> f32 {
        match self.density {
            Density::Comfortable => 1.0,
            Density::Compact => 0.85,
        }
    }

    /// Body text size in px for the density (comfortable 14, compact 13 —
    /// `BODY_FONT` in `src/lib/settings.ts`).
    pub fn text_size(self) -> f32 {
        match self.density {
            Density::Comfortable => 14.0,
            Density::Compact => 13.0,
        }
    }

    /// Row height in px for the density (32 comfortable, 28 compact).
    pub fn row_height(self) -> f32 {
        match self.density {
            Density::Comfortable => 32.0,
            Density::Compact => 28.0,
        }
    }

    /// Silence for `.bg(..)`: a fully transparent color from this palette.
    pub fn transparent() -> Rgba {
        rgba(0)
    }
}

static CURRENT: LazyLock<Mutex<Theme>> = LazyLock::new(|| Mutex::new(Theme::dark()));

/// Convert a theme color to the `Hsla` `TextStyle` expects.
pub fn hsla(color: Rgba) -> Hsla {
    color.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_defaults_match_design_table() {
        let theme = Theme::dark();
        assert_eq!(theme.surface_base, rgb(0x221d27));
        assert_eq!(theme.status_working, rgb(0x7fc4de));
        assert_eq!(theme.accent, rgb(0xa3185a));
        assert_eq!(theme.density, Density::Comfortable);
        assert_eq!(theme.density_scale(), 1.0);
    }

    #[test]
    fn light_matches_design_table() {
        let theme = Theme::light();
        assert_eq!(theme.surface_base, rgb(0xfaf5fa));
        assert_eq!(theme.text, rgb(0x501854));
        assert_eq!(theme.border, rgb(0xe4d8e0));
    }

    #[test]
    fn orbit_token_table_matches_design_doc() {
        let theme = Theme::dark();
        assert_eq!(theme.primary_hover, gpui::rgb(0xa3185a));
        assert_eq!(theme.primary_text, gpui::rgb(0xf2eef5));
        assert_eq!(theme.syntax_keyword, gpui::rgb(0xff8fa8));
    }

    #[test]
    fn compact_scales_without_changing_architecture() {
        let theme = Theme::for_mode(ThemeMode::Dark, Density::Compact);
        assert!(theme.density_scale() < 1.0);
        assert_eq!(theme.text_size(), 13.0);
        assert_eq!(theme.row_height(), 28.0);
        // Colors are density-independent.
        assert_eq!(theme.accent, Theme::dark().accent);
    }

    #[test]
    fn current_falls_back_to_dark_and_can_be_set() {
        let theme = Theme::current();
        assert_eq!(theme.mode, ThemeMode::Dark);
        Theme::set_current(Theme::light());
        assert_eq!(Theme::current().mode, ThemeMode::Light);
        Theme::set_current(Theme::dark());
    }
}
