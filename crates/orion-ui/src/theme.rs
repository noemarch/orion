//! Theme tokens (DESIGN.md §2, the generated table — the source of truth;
//! values mirrored value-for-value). The default dark palette is the blue-gray
//! "Quiet Canvas" generation; the earlier plum values were replaced as the
//! default while the additive `surface_canvas`/`diff_*` tokens keep their own
//! names for callers that want the reference palette explicitly.
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

use gpui_kit::{px, rgb, rgba, App, Hsla, Rgba};
use std::sync::atomic::{AtomicU8, Ordering};
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

    // The Herd-reference palette (DESIGN.md §2, generated table). Additive:
    // every token above keeps its Quiet Slate value, so existing views are
    // untouched.
    /// App canvas `--surface-canvas`: the deepest ground, behind every panel.
    pub surface_canvas: Rgba,
    /// Bare surface `--surface-subtle`: gutters and inset fills one step off the canvas.
    pub surface_subtle: Rgba,
    /// Raised card `--surface-raised`: a panel lifted off the canvas.
    pub surface_raised: Rgba,
    /// Raised card, level 2 `--surface-raised-2`: a panel nested in a raised card.
    pub surface_raised_2: Rgba,
    /// Raised card, level 3 `--surface-raised-3`: the deepest nesting before an edge is needed.
    pub surface_raised_3: Rgba,
    /// Hairline divider `--border-soft`: the quiet edge between adjacent rows.
    pub border_soft: Rgba,
    /// Secondary text `--text-muted`: labels and metadata beside primary text.
    pub text_muted: Rgba,
    /// Tertiary text `--text-faint`: placeholders, counts and disabled labels.
    pub text_faint: Rgba,
    /// Accent emphasis `--accent-emphasis`: the loud end of the accent, for selected marks.
    pub accent_emphasis: Rgba,
    /// Informational blue `--info-blue`: hints and neutral notices.
    pub info_blue: Rgba,
    /// Diff add `--diff-add`: the added-line marker and its `+` glyph.
    pub diff_add: Rgba,
    /// Diff del `--diff-del`: the removed-line marker and its `-` glyph.
    pub diff_del: Rgba,
    /// Warning amber `--warning-amber`: cautions that are not errors.
    pub warning_amber: Rgba,
    /// Review purple `--review-purple`: review and comment affordances in a diff.
    pub review_purple: Rgba,
    /// Diff add fill `--diff-add-fill`: the added-line background wash.
    pub diff_add_fill: Rgba,
    /// Diff del fill `--diff-del-fill`: the removed-line background wash.
    pub diff_del_fill: Rgba,
    /// Diff gutter `--diff-gutter`: the line-number gutter background.
    pub diff_gutter: Rgba,
    /// Diff hunk background `--diff-hunk-bg`: the `@@` hunk-header strip.
    pub diff_hunk_bg: Rgba,
    /// Diff hunk text `--diff-hunk-text`: the `@@` hunk-header label.
    pub diff_hunk_text: Rgba,
    /// Diff code `--diff-code`: unchanged code text.
    pub diff_code: Rgba,
    /// Diff add text `--diff-add-text`: added code text on [`Theme::diff_add_fill`].
    pub diff_add_text: Rgba,
    /// Diff del text `--diff-del-text`: removed code text on [`Theme::diff_del_fill`].
    pub diff_del_text: Rgba,
    /// Composer background `--composer-bg`: the prompt input's ground.
    pub composer_bg: Rgba,
    /// Composer border `--composer-border`: the prompt input's edge.
    pub composer_border: Rgba,
    /// Status link `--status-link`: connection and session text in the status bar.
    pub status_link: Rgba,
    /// Tab active background `--tab-active-bg`: the selected tab's ground.
    pub tab_active_bg: Rgba,
    /// Tab active marker `--tab-active-marker`: the selected tab's top marker.
    pub tab_active_marker: Rgba,
    /// Inline code background `--inline-code-bg`: a code span in prose.
    pub inline_code_bg: Rgba,
    /// Inline code text `--inline-code-text`: a code span's text in prose.
    pub inline_code_text: Rgba,
    /// Link blue `--link-blue`: links inside prose and the review pane.
    pub link_blue: Rgba,
    /// Scrollbar thumb `--scrollbar-thumb`: the draggable scrollbar thumb.
    pub scrollbar_thumb: Rgba,
    /// Chip border `--chip-border`: the edge of a filter or label chip.
    pub chip_border: Rgba,
    /// Badge border `--badge-border`: the edge of a count badge.
    pub badge_border: Rgba,
    /// Badge background `--badge-bg`: the ground of a count badge.
    pub badge_bg: Rgba,
    /// Code comment `--code-comment`: comments and doc lines in highlighted code.
    pub code_comment: Rgba,
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
                surface_base: rgb(0x0d1117),
                surface_panel: rgb(0x10151d),
                surface_elevated: rgb(0x151b24),
                surface_hover: rgb(0x1b2431),
                surface_input: rgb(0x0e131a),
                border: rgb(0x222b38),
                border_strong: rgb(0x39434f),
                text: rgb(0xc9d1d9),
                text_dim: rgb(0x8b949e),
                text_strong: rgb(0xe6edf3),
                accent: rgb(0xf778ba),
                accent_tint: tint(0xf778ba, 12),
                primary: rgb(0xf778ba),
                primary_hover: rgb(0xff90c8),
                primary_text: rgb(0x0d1117),
                primary_tint: tint(0xf778ba, 16),
                syntax_keyword: rgb(0xbb9af7),
                syntax_string: rgb(0x9ece6a),
                syntax_number: rgb(0xff9e64),
                syntax_type: rgb(0x7dcfff),
                syntax_variable: rgb(0xc0caf5),
                syntax_meta: rgb(0xb49dd6),
                status_idle: rgb(0x8b949e),
                status_working: rgb(0x58a6ff),
                status_blocked: rgb(0xd29922),
                status_done: rgb(0x56d364),
                status_working_tint: tint(0x58a6ff, 14),
                status_blocked_tint: tint(0xd29922, 14),
                status_done_tint: tint(0x56d364, 14),
                indicator_working: rgb(0x58a6ff),
                indicator_blocked: rgb(0xf85149),
                indicator_done: rgb(0x3fb950),
                indicator_idle: rgb(0x8b949e),
                indicator_unknown: rgb(0x6e7681),
                danger_text: rgb(0xffd0cf),
                danger_tint: tint(0xf85149, 12),
                notice: rgb(0xd29922),
                scrim: rgba(0x010409cc),
                term_bg: rgb(0x0d1117),
                term_fg: rgb(0xc9d1d9),
                term_cursor: rgb(0xf778ba),
                term_selection: rgb(0x1b2431),
                surface_canvas: rgb(0x0d1117),
                surface_subtle: rgb(0x10151d),
                surface_raised: rgb(0x151b24),
                surface_raised_2: rgb(0x1b2431),
                surface_raised_3: rgb(0x202a38),
                border_soft: rgb(0x1a222d),
                text_muted: rgb(0x8b949e),
                text_faint: rgb(0x5e6671),
                accent_emphasis: rgb(0xf778ba),
                info_blue: rgb(0x6cb6ff),
                diff_add: rgb(0x56d364),
                diff_del: rgb(0xf85149),
                warning_amber: rgb(0xd29922),
                review_purple: rgb(0xbc8cff),
                diff_add_fill: tint(0x54a04a, 16),
                diff_del_fill: tint(0xf85149, 13),
                diff_gutter: rgb(0x10151d),
                diff_hunk_bg: rgb(0x182031),
                diff_hunk_text: rgb(0x8ea7c9),
                diff_code: rgb(0xc0caf5),
                diff_add_text: rgb(0xc6f0c8),
                diff_del_text: rgb(0xffd0cf),
                composer_bg: rgb(0x0e131a),
                composer_border: rgb(0x39434f),
                status_link: rgb(0x7d9fc4),
                tab_active_bg: rgb(0x111720),
                tab_active_marker: rgb(0xa6d189),
                inline_code_bg: rgb(0x263044),
                inline_code_text: rgb(0xffb4d8),
                link_blue: rgb(0xa5c9ff),
                scrollbar_thumb: rgb(0x2a3441),
                chip_border: rgb(0x6b7480),
                badge_border: rgb(0x5c4a1f),
                badge_bg: rgb(0x2a2313),
                code_comment: rgb(0x6a7385),
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
                surface_canvas: rgb(0xfaf5fa),
                surface_subtle: rgb(0xffffff),
                surface_raised: rgb(0xf3ebf1),
                surface_raised_2: rgb(0xece1e9),
                surface_raised_3: rgb(0xe4d8e0),
                border_soft: rgb(0xe4d8e0),
                text_muted: rgb(0x7a5570),
                text_faint: rgb(0x9ca0b0),
                accent_emphasis: rgb(0xa84370),
                info_blue: rgb(0x155a72),
                diff_add: rgb(0x2f6317),
                diff_del: rgb(0xa82323),
                warning_amber: rgb(0x8a5300),
                review_purple: rgb(0x8a3f7a),
                diff_add_fill: tint(0x2f6317, 12),
                diff_del_fill: tint(0xa82323, 10),
                diff_gutter: rgb(0xf6f8fa),
                diff_hunk_bg: rgb(0xeef2f8),
                diff_hunk_text: rgb(0x57606a),
                diff_code: rgb(0x24292f),
                diff_add_text: rgb(0x1a7f37),
                diff_del_text: rgb(0xa82323),
                composer_bg: rgb(0xffffff),
                composer_border: rgb(0xd0d7de),
                status_link: rgb(0x0969da),
                tab_active_bg: rgb(0xf6f8fa),
                tab_active_marker: rgb(0x2f6317),
                inline_code_bg: rgb(0xeff1f3),
                inline_code_text: rgb(0xa84370),
                link_blue: rgb(0x0969da),
                scrollbar_thumb: rgb(0xd0d7de),
                chip_border: rgb(0x8c959f),
                badge_border: rgb(0xd4a72c),
                badge_bg: rgb(0xfff8c5),
                code_comment: rgb(0x6e7781),
            },
        }
    }

    /// The live theme. Falls back to Quiet Slate dark with comfortable
    /// density before the app installs saved preferences.
    ///
    /// A read is one relaxed atomic load, not a `Mutex` lock: the palette is a
    /// pure function of (mode, density), and every `set_current` caller passes
    /// a `for_mode` value (or its `dark()`/`light()` wrappers), so the two
    /// packed bits reconstruct the exact Theme (see `CURRENT_KEY`).
    pub fn current() -> Theme {
        let key = CURRENT_KEY.load(Ordering::Relaxed);
        let mode = if key & MODE_BIT != 0 {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        };
        let density = if key & DENSITY_BIT != 0 {
            Density::Compact
        } else {
            Density::Comfortable
        };
        Theme::for_mode(mode, density)
    }

    /// Install the theme every view reads via [`Theme::current()`].
    ///
    /// Only the (mode, density) key is stored; see [`Theme::current`] for the
    /// caller invariant that makes this lossless.
    pub fn set_current(theme: Theme) {
        let mut key = 0u8;
        if theme.mode == ThemeMode::Light {
            key |= MODE_BIT;
        }
        if theme.density == Density::Compact {
            key |= DENSITY_BIT;
        }
        CURRENT_KEY.store(key, Ordering::Relaxed);
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

    /// Chat prose size in px for the density (comfortable 15, compact 14 —
    /// `--fs-chat`, DESIGN.md §Scale).
    pub fn chat_prose_size(self) -> f32 {
        match self.density {
            Density::Comfortable => 15.0,
            Density::Compact => 14.0,
        }
    }

    /// Chat code size in px for the density (comfortable 13, compact 12 —
    /// `--fs-sm`).
    pub fn chat_code_size(self) -> f32 {
        match self.density {
            Density::Comfortable => 13.0,
            Density::Compact => 12.0,
        }
    }

    /// A markdown heading's size in px: h1 is `--fs-xl` (18/17), h2 `--fs-lg`
    /// (16/15), h3 and below the prose size (`--fs-chat`).
    pub fn chat_heading_size(self, level: u8) -> f32 {
        match (level, self.density) {
            (1, Density::Comfortable) => 18.0,
            (1, Density::Compact) => 17.0,
            (2, Density::Comfortable) => 16.0,
            (2, Density::Compact) => 15.0,
            _ => self.chat_prose_size(),
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

/// The chat prose's line height (`--lh-prose`, DESIGN.md §Scale).
pub const CHAT_LINE_HEIGHT: f32 = 1.65;
/// Inline code is `0.87em` of its line (DESIGN.md §Faces).
pub const CHAT_INLINE_CODE_RATIO: f32 = 0.87;

// Packed key for the live theme: bit 0 = Light, bit 1 = Compact. Every
// `set_current` caller passes `Theme::for_mode(mode, density)` (settings.rs,
// app.rs) or its `dark()`/`light()` wrappers, so the full palette is a pure
// function of these two bits and the key reconstructs it losslessly.
const MODE_BIT: u8 = 1;
const DENSITY_BIT: u8 = 2;
static CURRENT_KEY: AtomicU8 = AtomicU8::new(0);

/// Convert a theme color to the `Hsla` `TextStyle` expects.
pub fn hsla(color: Rgba) -> Hsla {
    color.into()
}

// The last palette pushed into the gpui-kit global theme, as
// (is dark, density scale). The kit theme is global mutable state; the app
// re-syncs it only when this changes, so a per-frame call stays cheap.
static KIT_SYNCED: LazyLock<Mutex<(Option<bool>, f32)>> = LazyLock::new(|| Mutex::new((None, 0.0)));

// Push the current Quiet Slate palette into gpui-kit's global theme.
//
// gpui-kit components (Button, Input, Sidebar, ...) paint from
// `Theme::global(cx)`; this is the seam that makes them read Orion's
// colors instead of the kit defaults. Call it after [`Theme::set_current`]
// — the kit `init(cx)` installs the defaults, and every Orion theme
// change (mode or density) rewrites them from this module's table.
pub(crate) fn sync_gpui_kit(cx: &mut App) {
    use gpui_kit::component::theme::{Theme as KitTheme, ThemeMode as KitThemeMode};

    let t = Theme::current();
    let mode = match t.mode {
        ThemeMode::Dark => KitThemeMode::Dark,
        ThemeMode::Light => KitThemeMode::Light,
    };
    let is_dark = matches!(mode, KitThemeMode::Dark);
    let scale = t.density_scale();

    if let Ok(guard) = KIT_SYNCED.lock() {
        if guard.0 == Some(is_dark) && guard.1 == scale {
            return;
        }
    }

    // `change` loads the kit's registered palette for the mode; the edit
    // that follows replaces the colors Quiet Slate owns. A second edit is
    // required only because `change` reloads after a mode switch.
    KitTheme::change(mode, None, cx);
    KitTheme::update(cx, |theme| {
        let c = &mut theme.colors;
        c.background = t.surface_base.into();
        c.foreground = t.text.into();
        c.border = t.border.into();
        c.input = t.surface_input.into();
        c.accent = t.accent.into();
        c.accent_foreground = t.text_strong.into();
        c.primary = t.primary.into();
        c.primary_foreground = t.primary_text.into();
        c.primary_hover = t.primary_hover.into();
        c.primary_active = t.primary.into();
        c.secondary = t.surface_elevated.into();
        c.secondary_foreground = t.text.into();
        c.secondary_hover = t.surface_hover.into();
        c.secondary_active = t.surface_hover.into();
        c.muted = t.surface_panel.into();
        c.muted_foreground = t.text_dim.into();
        c.danger = t.danger_tint.into();
        c.danger_foreground = t.danger_text.into();
        c.danger_active = t.danger_tint.into();
        c.danger_hover = t.danger_tint.into();
        c.success = t.status_done.into();
        c.success_foreground = t.primary_text.into();
        c.success_active = t.status_done.into();
        c.success_hover = t.status_done.into();
        c.warning = t.status_blocked.into();
        c.warning_foreground = t.text_strong.into();
        c.warning_active = t.status_blocked.into();
        c.warning_hover = t.status_blocked.into();
        c.info = t.status_working.into();
        c.info_foreground = t.text_strong.into();
        c.info_active = t.status_working.into();
        c.info_hover = t.status_working.into();
        // Rose is the focus ring and nothing else (DESIGN.md §1).
        c.ring = t.accent.into();
        c.caret = t.accent.into();
        c.popover = t.surface_elevated.into();
        c.popover_foreground = t.text.into();
        c.tab = t.surface_panel.into();
        c.tab_foreground = t.text_dim.into();
        c.tab_active = t.accent_tint.into();
        c.tab_active_foreground = t.text_strong.into();
        c.tab_bar = t.surface_panel.into();
        c.sidebar = t.surface_base.into();
        c.sidebar_foreground = t.text.into();
        c.sidebar_accent = t.surface_hover.into();
        c.sidebar_accent_foreground = t.text_strong.into();
        c.sidebar_border = t.border.into();
        c.sidebar_primary = t.primary.into();
        c.sidebar_primary_foreground = t.primary_text.into();
        c.list = t.surface_panel.into();
        c.list_hover = t.surface_hover.into();
        c.list_active = t.accent_tint.into();
        c.list_active_border = t.accent.into();
        theme.font_size = px(t.text_size());
        theme.mono_font_size = px(t.chat_code_size());
    });

    if let Ok(mut guard) = KIT_SYNCED.lock() {
        guard.0 = Some(is_dark);
        guard.1 = scale;
    }
}

/// The standard xterm 256-colour palette, as red, green, blue triples.
///
/// Index 0-15 are the sixteen ANSI colours, 16-231 are the 6x6x6 colour cube and
/// 232-255 are the greyscale ramp.
pub const XTERM_256: [(u8, u8, u8); 256] = build_xterm_256();

/// The sixteen ANSI colours, ordered as terminals number them (xterm defaults).
const ANSI_16: [(u8, u8, u8); 16] = [
    (0, 0, 0),
    (205, 0, 0),
    (0, 205, 0),
    (205, 205, 0),
    (0, 0, 238),
    (205, 0, 205),
    (0, 205, 205),
    (229, 229, 229),
    (127, 127, 127),
    (255, 0, 0),
    (0, 255, 0),
    (255, 255, 0),
    (92, 92, 255),
    (255, 0, 255),
    (0, 255, 255),
    (255, 255, 255),
];

/// One axis of the 6x6x6 cube: level 0 is off, levels 1-5 are 95, 135, ... 255.
const fn cube_level(level: usize) -> u8 {
    if level == 0 {
        0
    } else {
        55 + 40 * (level as u8)
    }
}

/// Fill the palette at compile time: fixed ANSI block, computed cube, computed
/// greyscale ramp. A computed table cannot drift from the documented formula.
const fn build_xterm_256() -> [(u8, u8, u8); 256] {
    let mut table = [(0, 0, 0); 256];
    let mut i = 0;
    while i < 16 {
        table[i] = ANSI_16[i];
        i += 1;
    }
    while i < 232 {
        let n = i - 16;
        table[i] = (
            cube_level(n / 36),
            cube_level((n % 36) / 6),
            cube_level(n % 6),
        );
        i += 1;
    }
    while i < 256 {
        let grey = 8 + 10 * (i - 232) as u8;
        table[i] = (grey, grey, grey);
        i += 1;
    }
    table
}

/// The colour at `index` in the xterm palette.
///
/// Every `u8` is in the palette. A caller that must tell "the terminal's default
/// colour" apart from a palette colour matches its own colour type first and
/// calls this only for palette indices.
pub fn xterm_256(index: u8) -> Rgba {
    let (r, g, b) = XTERM_256[usize::from(index)];
    rgb_color(r, g, b)
}

/// A 24-bit colour, as a program asked for it.
pub fn rgb_color(r: u8, g: u8, b: u8) -> Rgba {
    rgb(u32::from_be_bytes([0, r, g, b]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn dark_defaults_match_design_table() {
        let theme = Theme::dark();
        assert_eq!(theme.surface_base, rgb(0x0d1117));
        assert_eq!(theme.status_working, rgb(0x58a6ff));
        assert_eq!(theme.accent, rgb(0xf778ba));
        assert_eq!(theme.density, Density::Comfortable);
        assert_eq!(theme.density_scale(), 1.0);
    }

    #[::core::prelude::v1::test]
    fn light_matches_design_table() {
        let theme = Theme::light();
        assert_eq!(theme.surface_base, rgb(0xfaf5fa));
        assert_eq!(theme.text, rgb(0x501854));
        assert_eq!(theme.border, rgb(0xe4d8e0));
    }

    #[::core::prelude::v1::test]
    fn orbit_token_table_matches_design_doc() {
        let theme = Theme::dark();
        assert_eq!(theme.primary_hover, gpui_kit::rgb(0xff90c8));
        assert_eq!(theme.primary_text, gpui_kit::rgb(0x0d1117));
        assert_eq!(theme.syntax_keyword, gpui_kit::rgb(0xbb9af7));
    }

    #[::core::prelude::v1::test]
    fn the_chat_reading_scale_follows_the_design_table() {
        let comfortable = Theme::dark();
        assert_eq!(comfortable.chat_prose_size(), 15.0, "--fs-chat");
        assert_eq!(comfortable.chat_code_size(), 13.0, "--fs-sm");
        assert_eq!(comfortable.chat_heading_size(1), 18.0, "--fs-xl");
        assert_eq!(comfortable.chat_heading_size(2), 16.0, "--fs-lg");
        assert_eq!(comfortable.chat_heading_size(3), 15.0, "--fs-chat");
        assert_eq!(
            comfortable.chat_heading_size(6),
            comfortable.chat_prose_size(),
            "h3 and below read as prose"
        );

        let compact = Theme {
            density: Density::Compact,
            ..Theme::dark()
        };
        assert_eq!(compact.chat_prose_size(), 14.0);
        assert_eq!(compact.chat_code_size(), 12.0);
        assert_eq!(compact.chat_heading_size(1), 17.0);
        assert_eq!(compact.chat_heading_size(2), 15.0);
        assert_eq!(compact.chat_heading_size(3), 14.0);
    }

    #[::core::prelude::v1::test]
    fn compact_scales_without_changing_architecture() {
        let theme = Theme::for_mode(ThemeMode::Dark, Density::Compact);
        assert!(theme.density_scale() < 1.0);
        assert_eq!(theme.text_size(), 13.0);
        assert_eq!(theme.row_height(), 28.0);
        // Colors are density-independent.
        assert_eq!(theme.accent, Theme::dark().accent);
    }

    #[::core::prelude::v1::test]
    fn current_falls_back_to_dark_and_can_be_set() {
        let theme = Theme::current();
        assert_eq!(theme.mode, ThemeMode::Dark);
        Theme::set_current(Theme::light());
        assert_eq!(Theme::current().mode, ThemeMode::Light);
        Theme::set_current(Theme::dark());
    }

    #[::core::prelude::v1::test]
    fn the_palette_starts_with_the_ansi_colours() {
        assert_eq!(xterm_256(0), rgb(0x000000), "index 0 is black");
        assert_eq!(xterm_256(1), rgb(0xcd0000), "index 1 is maroon");
        assert_eq!(xterm_256(4), rgb(0x0000ee), "index 4 is navy");
        assert_eq!(xterm_256(7), rgb(0xe5e5e5), "index 7 is silver");
        assert_eq!(xterm_256(15), rgb(0xffffff), "index 15 is bright white");
    }

    #[::core::prelude::v1::test]
    fn the_cube_starts_at_sixteen() {
        assert_eq!(xterm_256(16), rgb(0x000000), "cube origin is off-black");
        // index 21 is n=5: r=0, g=0, b=5 -> level 5 -> 55 + 40*5 = 255.
        assert_eq!(xterm_256(21), rgb(0x0000ff));
        assert_eq!(xterm_256(196), rgb(0xff0000), "cube pure red");
        assert_eq!(xterm_256(231), rgb(0xffffff), "cube far corner is white");
    }

    /// xterm's ramp is `8 + 10 * (i - 232)`, so the lightest step is 238, not
    /// pure white -- index 15 is the white ANSI colour.
    #[::core::prelude::v1::test]
    fn the_greyscale_ramp_ends_at_white() {
        assert_eq!(xterm_256(232), rgb(0x080808));
        assert_eq!(xterm_256(243), rgb(0x767676));
        assert_eq!(xterm_256(255), rgb(0xeeeeee), "ramp ends at 238");
    }

    #[::core::prelude::v1::test]
    fn a_truecolor_value_is_the_colour_it_names() {
        assert_eq!(rgb_color(10, 20, 30), rgb(0x0a141e));
        assert_eq!(rgb_color(0, 0, 0), rgb(0x000000));
        assert_eq!(rgb_color(255, 255, 255), rgb(0xffffff));
    }

    #[::core::prelude::v1::test]
    fn herd_reference_dark_tokens_match_design_table() {
        let theme = Theme::dark();
        assert_eq!(theme.surface_canvas, rgb(0x0d1117));
        assert_eq!(theme.surface_subtle, rgb(0x10151d));
        assert_eq!(theme.surface_raised, rgb(0x151b24));
        assert_eq!(theme.surface_raised_2, rgb(0x1b2431));
        assert_eq!(theme.surface_raised_3, rgb(0x202a38));
        assert_eq!(theme.border_soft, rgb(0x1a222d));
        assert_eq!(theme.text_muted, rgb(0x8b949e));
        assert_eq!(theme.text_faint, rgb(0x5e6671));
        assert_eq!(theme.accent_emphasis, rgb(0xf778ba));
        assert_eq!(theme.info_blue, rgb(0x6cb6ff));
        assert_eq!(theme.diff_add, rgb(0x56d364));
        assert_eq!(theme.diff_del, rgb(0xf85149));
        assert_eq!(theme.warning_amber, rgb(0xd29922));
        assert_eq!(theme.review_purple, rgb(0xbc8cff));
        // 16% of #54a04a is alpha 41, 13% of #f85149 is alpha 33.
        assert_eq!(theme.diff_add_fill, rgba(0x54a04a29));
        assert_eq!(theme.diff_del_fill, rgba(0xf8514921));
        assert_eq!(theme.diff_gutter, rgb(0x10151d));
        assert_eq!(theme.diff_hunk_bg, rgb(0x182031));
        assert_eq!(theme.diff_hunk_text, rgb(0x8ea7c9));
        assert_eq!(theme.diff_code, rgb(0xc0caf5));
        assert_eq!(theme.diff_add_text, rgb(0xc6f0c8));
        assert_eq!(theme.diff_del_text, rgb(0xffd0cf));
        assert_eq!(theme.composer_bg, rgb(0x0e131a));
        assert_eq!(theme.composer_border, rgb(0x39434f));
        assert_eq!(theme.status_link, rgb(0x7d9fc4));
        assert_eq!(theme.tab_active_bg, rgb(0x111720));
        assert_eq!(theme.tab_active_marker, rgb(0xa6d189));
        assert_eq!(theme.inline_code_bg, rgb(0x263044));
        assert_eq!(theme.inline_code_text, rgb(0xffb4d8));
        assert_eq!(theme.link_blue, rgb(0xa5c9ff));
        assert_eq!(theme.scrollbar_thumb, rgb(0x2a3441));
        assert_eq!(theme.chip_border, rgb(0x6b7480));
        assert_eq!(theme.badge_border, rgb(0x5c4a1f));
        assert_eq!(theme.badge_bg, rgb(0x2a2313));
        assert_eq!(theme.code_comment, rgb(0x6a7385));
    }

    #[::core::prelude::v1::test]
    fn herd_reference_light_tokens_match_design_table() {
        let theme = Theme::light();
        assert_eq!(theme.surface_canvas, rgb(0xfaf5fa));
        assert_eq!(theme.surface_subtle, rgb(0xffffff));
        assert_eq!(theme.surface_raised, rgb(0xf3ebf1));
        assert_eq!(theme.surface_raised_2, rgb(0xece1e9));
        assert_eq!(theme.surface_raised_3, rgb(0xe4d8e0));
        assert_eq!(theme.border_soft, rgb(0xe4d8e0));
        assert_eq!(theme.text_muted, rgb(0x7a5570));
        assert_eq!(theme.text_faint, rgb(0x9ca0b0));
        assert_eq!(theme.accent_emphasis, rgb(0xa84370));
        assert_eq!(theme.info_blue, rgb(0x155a72));
        assert_eq!(theme.diff_add, rgb(0x2f6317));
        assert_eq!(theme.diff_del, rgb(0xa82323));
        assert_eq!(theme.warning_amber, rgb(0x8a5300));
        assert_eq!(theme.review_purple, rgb(0x8a3f7a));
        // 12% of #2f6317 is alpha 31, 10% of #a82323 is alpha 26.
        assert_eq!(theme.diff_add_fill, rgba(0x2f63171f));
        assert_eq!(theme.diff_del_fill, rgba(0xa823231a));
        assert_eq!(theme.diff_gutter, rgb(0xf6f8fa));
        assert_eq!(theme.diff_hunk_bg, rgb(0xeef2f8));
        assert_eq!(theme.diff_hunk_text, rgb(0x57606a));
        assert_eq!(theme.diff_code, rgb(0x24292f));
        assert_eq!(theme.diff_add_text, rgb(0x1a7f37));
        assert_eq!(theme.diff_del_text, rgb(0xa82323));
        assert_eq!(theme.composer_bg, rgb(0xffffff));
        assert_eq!(theme.composer_border, rgb(0xd0d7de));
        assert_eq!(theme.status_link, rgb(0x0969da));
        assert_eq!(theme.tab_active_bg, rgb(0xf6f8fa));
        assert_eq!(theme.tab_active_marker, rgb(0x2f6317));
        assert_eq!(theme.inline_code_bg, rgb(0xeff1f3));
        assert_eq!(theme.inline_code_text, rgb(0xa84370));
        assert_eq!(theme.link_blue, rgb(0x0969da));
        assert_eq!(theme.scrollbar_thumb, rgb(0xd0d7de));
        assert_eq!(theme.chip_border, rgb(0x8c959f));
        assert_eq!(theme.badge_border, rgb(0xd4a72c));
        assert_eq!(theme.badge_bg, rgb(0xfff8c5));
        assert_eq!(theme.code_comment, rgb(0x6e7781));
    }
}
