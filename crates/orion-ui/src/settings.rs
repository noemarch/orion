//! Settings modal (P4): Appearance (dark/light), Density (comfortable/
//! compact), and Key bar extras computed from a saved `Preferences` shape.
//!
//! Persisted via `PUT /api/preferences` with the web client's wire names
//! (`theme`, `density`, `keyBarExtras` — see `src/lib/settings.ts`
//! `DEFAULT_SETTINGS` and `src/lib/keys.ts` `KeyBarExtra`/`KEY_BAR_EXTRAS`)
//! through the in-process `Engine::http` seam. Escape closes the modal.
//!
//! Deliberately NOT ported yet (follow-up): palettes beyond Quiet Slate,
//! terminal/chat fonts, alerts, quick replies, language, etc. Those live in
//! the web client's full `Settings` interface; this modal covers the four
//! groups P4 scoped, with matching defaults (dark, comfortable, `["alt"]`).

use crate::theme::{Density, Theme, ThemeMode};
use gpui::prelude::*;
use gpui::{
    div, px, Context, FocusHandle, KeyDownEvent, MouseButton, MouseDownEvent, Render, Styled,
    Window,
};
use orion_core::Engine;
use serde_json::{json, Value};
use std::sync::Arc;

/// Key-bar extras in `KEY_BAR_EXTRAS` order (`src/lib/keys.ts`); each pair is
/// one choice on the web side. `alt` is the sticky modifier button,
/// `shift-tab` is BackTab, `home-end`/`page-up-down` are pairs, the rest are
/// single chords/characters.
pub const KEY_BAR_EXTRAS: &[&str] = &[
    "alt",
    "shift-tab",
    "home-end",
    "page-up-down",
    "ctrl-d",
    "ctrl-z",
    "pipe",
    "tilde",
    "slash",
];

/// Human labels for the extras checkboxes (keyboard notation, as on the web).
fn extra_label(extra: &str) -> &str {
    match extra {
        "alt" => "Alt modifier",
        "shift-tab" => "Shift+Tab (BackTab)",
        "home-end" => "Home / End",
        "page-up-down" => "PageUp / PageDown",
        "ctrl-d" => "Ctrl+D (^D)",
        "ctrl-z" => "Ctrl+Z (^Z)",
        "pipe" => "Pipe (|)",
        "tilde" => "Tilde (~)",
        "slash" => "Slash (/)",
        other => other,
    }
}

/// Extras expressed as key-bar items. Each entry is `(extra, label, chord)`;
/// chord names are the `engine::pane::encode_key` vocabulary (`ctrl+X`,
/// arrows, `home`/`end`/`pageup`/`pagedown`/`delete`, lowercase letters) —
/// verified in `crates/orion-core/src/engine/pane.rs` (`shift+tab` is its
/// own CSI Z sequence, `ctrl+d`/`ctrl+z` are control characters, and `|`,
/// `~`, `/` are single literal characters).
fn extra_keys(extra: &str) -> &'static [(&'static str, &'static str)] {
    match extra {
        "shift-tab" => &[("⇧Tab", "shift+tab")],
        "home-end" => &[("Home", "home"), ("End", "end")],
        "page-up-down" => &[("PgUp", "pageup"), ("PgDn", "pagedown")],
        "ctrl-d" => &[("^D", "ctrl+d")],
        "ctrl-z" => &[("^Z", "ctrl+z")],
        "pipe" => &[("|", "|")],
        "tilde" => &[("~", "~")],
        "slash" => &[("/", "/")],
        _ => &[],
    }
}

/// The saved preferences shape (P4 scope): theme, density, key-bar extras.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preferences {
    pub theme: ThemeMode,
    pub density: Density,
    pub key_bar_extras: Vec<String>,
}

impl Default for Preferences {
    /// `DEFAULT_SETTINGS` in `src/lib/settings.ts`: dark, comfortable, `["alt"]`.
    fn default() -> Self {
        Self {
            theme: ThemeMode::Dark,
            density: Density::Comfortable,
            key_bar_extras: vec!["alt".to_string()],
        }
    }
}

impl Preferences {
    pub fn has_extra(&self, extra: &str) -> bool {
        self.key_bar_extras.iter().any(|e| e == extra)
    }

    pub fn toggle_extra(&mut self, extra: &str) {
        if let Some(pos) = self.key_bar_extras.iter().position(|e| e == extra) {
            self.key_bar_extras.remove(pos);
        } else if KEY_BAR_EXTRAS.contains(&extra) {
            self.key_bar_extras.push(extra.to_string());
        }
    }

    /// The wire shape `PUT /api/preferences` expects under `"settings"`.
    pub fn settings_value(&self) -> Value {
        json!({
            "theme": self.theme.as_wire(),
            "density": self.density.as_wire(),
            "keyBarExtras": self.key_bar_extras,
        })
    }

    pub fn from_settings(settings: &Value) -> Self {
        let defaults = Preferences::default();
        let get_str = |key: &str| settings.get(key).and_then(Value::as_str).unwrap_or("");
        let extras = settings
            .get("keyBarExtras")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .filter(|e| KEY_BAR_EXTRAS.contains(e))
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or(defaults.key_bar_extras.clone());
        let theme = match get_str("theme") {
            "light" => ThemeMode::Light,
            "dark" => ThemeMode::Dark,
            _ => defaults.theme,
        };
        let density = match get_str("density") {
            "compact" => Density::Compact,
            "comfortable" => Density::Comfortable,
            _ => defaults.density,
        };
        Self {
            theme,
            density,
            key_bar_extras: extras,
        }
    }
}

/// What the terminal key bar should show (the settings-driven part).
/// Base keys (Esc/Tab/^C/Enter/arrows/Ctrl/Shift) are always present —
/// see `DEFAULT_KEY_BAR_ITEMS` in `src/lib/keyBar.ts`.
#[derive(Debug, Clone, Default)]
pub struct KeyBarExtras {
    pub alt: bool,
    pub shift_tab: bool,
    pub home_end: bool,
    pub page_up_down: bool,
    pub ctrl_d: bool,
    pub ctrl_z: bool,
    pub pipe: bool,
    pub tilde: bool,
    pub slash: bool,
}

impl KeyBarExtras {
    pub fn from_preferences(prefs: &Preferences) -> Self {
        Self {
            alt: prefs.has_extra("alt"),
            shift_tab: prefs.has_extra("shift-tab"),
            home_end: prefs.has_extra("home-end"),
            page_up_down: prefs.has_extra("page-up-down"),
            ctrl_d: prefs.has_extra("ctrl-d"),
            ctrl_z: prefs.has_extra("ctrl-z"),
            pipe: prefs.has_extra("pipe"),
            tilde: prefs.has_extra("tilde"),
            slash: prefs.has_extra("slash"),
        }
    }

    pub fn any(&self) -> bool {
        self.alt
            || self.shift_tab
            || self.home_end
            || self.page_up_down
            || self.ctrl_d
            || self.ctrl_z
            || self.pipe
            || self.tilde
            || self.slash
    }

    /// Extra `(label, chord)` items in `KEY_BAR_EXTRAS` order, for the bar.
    pub fn keys(&self) -> Vec<(&'static str, &'static str)> {
        let mut out = Vec::new();
        if self.shift_tab {
            out.extend_from_slice(extra_keys("shift-tab"));
        }
        if self.home_end {
            out.extend_from_slice(extra_keys("home-end"));
        }
        if self.page_up_down {
            out.extend_from_slice(extra_keys("page-up-down"));
        }
        if self.ctrl_d {
            out.extend_from_slice(extra_keys("ctrl-d"));
        }
        if self.ctrl_z {
            out.extend_from_slice(extra_keys("ctrl-z"));
        }
        if self.pipe {
            out.extend_from_slice(extra_keys("pipe"));
        }
        if self.tilde {
            out.extend_from_slice(extra_keys("tilde"));
        }
        if self.slash {
            out.extend_from_slice(extra_keys("slash"));
        }
        out
    }
}

pub struct SettingsModal {
    engine: Option<Arc<Engine>>,
    prefs: Preferences,
    last_body: Option<String>,
    focus_handle: FocusHandle,
    row_seq: usize,
    open: bool,
}

impl SettingsModal {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            engine: None,
            prefs: Preferences::default(),
            last_body: None,
            focus_handle: cx.focus_handle(),
            row_seq: 0,
            open: false,
        }
    }

    pub fn attach(&mut self, engine: Arc<Engine>) {
        self.engine = Some(engine);
        self.refresh();
    }

    pub fn prefs(&self) -> &Preferences {
        &self.prefs
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn set_open(&mut self, open: bool) {
        self.open = open;
    }

    fn close(&mut self) {
        self.open = false;
    }

    fn next_row_id(&mut self) -> usize {
        let id = self.row_seq;
        self.row_seq += 1;
        id
    }

    /// Load saved preferences through the wire seam. A 404 (nothing written
    /// yet) keeps the defaults.
    pub fn refresh(&mut self) {
        let Some(engine) = self.engine.clone() else {
            return;
        };
        let (status, body) = engine.http("GET", "/api/preferences", &Value::Null);
        if status != 200 {
            return;
        }
        let encoded = body.to_string();
        if self.last_body.as_deref() == Some(encoded.as_str()) {
            return;
        }
        self.last_body = Some(encoded);
        if let Some(settings) = body.get("settings") {
            self.prefs = Preferences::from_settings(settings);
            self.apply_theme();
        }
    }

    fn apply_theme(&self) {
        Theme::set_current(Theme::for_mode(self.prefs.theme, self.prefs.density));
    }

    fn save(&mut self) {
        self.apply_theme();
        if let Some(engine) = self.engine.clone() {
            let body = json!({ "settings": self.prefs.settings_value() });
            let _ = engine.http("PUT", "/api/preferences", &body);
        }
    }
}

impl Render for SettingsModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current();
        let scale = theme.density_scale();
        let dark = self.prefs.theme == ThemeMode::Dark;
        let compact = self.prefs.density == Density::Compact;
        let prefs = self.prefs.clone();
        let row_h = theme.row_height() * scale;

        let radio = |id: &'static str, label: &'static str, active: bool| {
            div()
                .id(id)
                .flex_row()
                .items_center()
                .gap(px(8.0 * scale))
                .h(px(row_h))
                .px_2()
                .rounded_md()
                .bg(if active {
                    theme.accent_tint
                } else {
                    theme.surface_elevated
                })
                .text_color(if active {
                    theme.text_strong
                } else {
                    theme.text_dim
                })
                .child(if active { "●" } else { "○" })
                .child(label)
        };

        let body = div()
            .id("settings-panel")
            .flex_col()
            .gap(px(12.0 * scale))
            .w(px(360.0))
            .p_4()
            .rounded_lg()
            .bg(theme.surface_panel)
            .border_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(14.0 * scale))
                            .text_color(theme.text_strong)
                            .child("Settings"),
                    )
                    .child(
                        div()
                            .id("settings-close")
                            .flex()
                            .size(px(32.0 * scale))
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .bg(theme.surface_hover)
                            .text_color(theme.text)
                            .child("✕")
                            .on_click(cx.listener(|this, _, _, _cx| this.close())),
                    ),
            )
            .child(
                div()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(11.0 * scale))
                            .text_color(theme.text_dim)
                            .child("APPEARANCE"),
                    )
                    .child(
                        div()
                            .flex_row()
                            .gap_2()
                            .child(radio("settings-theme-dark", "Dark", dark).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.prefs.theme = ThemeMode::Dark;
                                    this.save();
                                    cx.notify();
                                }),
                            ))
                            .child(radio("settings-theme-light", "Light", !dark).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.prefs.theme = ThemeMode::Light;
                                    this.save();
                                    cx.notify();
                                }),
                            )),
                    ),
            )
            .child(
                div()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(11.0 * scale))
                            .text_color(theme.text_dim)
                            .child("DENSITY"),
                    )
                    .child(
                        div()
                            .flex_row()
                            .gap_2()
                            .child(
                                radio("settings-density-comfortable", "Comfortable", !compact)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.prefs.density = Density::Comfortable;
                                        this.save();
                                        cx.notify();
                                    })),
                            )
                            .child(
                                radio("settings-density-compact", "Compact", compact).on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.prefs.density = Density::Compact;
                                        this.save();
                                        cx.notify();
                                    }),
                                ),
                            ),
                    ),
            )
            .child(
                div()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(11.0 * scale))
                            .text_color(theme.text_dim)
                            .child("KEY BAR EXTRAS"),
                    )
                    .children(KEY_BAR_EXTRAS.iter().map(|extra| {
                        let extra = *extra;
                        let on = prefs.has_extra(extra);
                        div()
                            .id(self.next_row_id())
                            .flex_row()
                            .items_center()
                            .gap(px(8.0 * scale))
                            .h(px(row_h))
                            .px_2()
                            .rounded_md()
                            .bg(if on {
                                theme.accent_tint
                            } else {
                                theme.surface_elevated
                            })
                            .text_color(if on {
                                theme.text_strong
                            } else {
                                theme.text_dim
                            })
                            .child(if on { "✓" } else { "○" })
                            .child(extra_label(extra))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.prefs.toggle_extra(extra);
                                this.save();
                                cx.notify();
                            }))
                    })),
            )
            .child(
                div().flex_row().justify_end().child(
                    div()
                        .id("settings-done")
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .bg(theme.primary)
                        .text_color(theme.text)
                        .child("Done")
                        .on_click(cx.listener(|this, _, _, _cx| this.close())),
                ),
            );

        div()
            .id("settings-overlay")
            .track_focus(&self.focus_handle)
            .absolute()
            .inset_0()
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _window, cx| {
                if ev.keystroke.key == "escape" {
                    this.close();
                    cx.notify();
                }
            }))
            .child(
                div()
                    .id("settings-scrim")
                    .absolute()
                    .inset_0()
                    .bg(theme.scrim)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, ev: &MouseDownEvent, _, _cx| {
                            if ev.button == MouseButton::Left {
                                this.close();
                            }
                        }),
                    ),
            )
            .child(
                div()
                    .id("settings-centered")
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(body),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_web_default_settings() {
        let prefs = Preferences::default();
        assert_eq!(prefs.theme, ThemeMode::Dark);
        assert_eq!(prefs.density, Density::Comfortable);
        assert_eq!(prefs.key_bar_extras, vec!["alt".to_string()]);
    }

    #[test]
    fn extras_sanitize_against_known_list() {
        let settings = json!({
            "theme": "light", "density": "compact",
            "keyBarExtras": ["bogus", "pipe", "ctrl-d", "pipe"],
        });
        let prefs = Preferences::from_settings(&settings);
        assert_eq!(prefs.theme, ThemeMode::Light);
        assert_eq!(prefs.density, Density::Compact);
        // Unknown entries are dropped, order follows the wire (dedup is the
        // web's `sanitizeKeyBarExtras` filter-by-list behavior).
        assert!(prefs.key_bar_extras.contains(&"pipe".to_string()));
        assert!(prefs.key_bar_extras.contains(&"ctrl-d".to_string()));
        assert!(!prefs.key_bar_extras.contains(&"bogus".to_string()));
    }

    #[test]
    fn extras_keys_use_encoder_vocabulary() {
        // shift+tab is accepted by encode_key (CSI Z), ctrl chords lowercase,
        // pipe/tilde/slash are single literals.
        let keys = KeyBarExtras {
            shift_tab: true,
            ctrl_d: true,
            pipe: true,
            ..Default::default()
        }
        .keys();
        assert!(keys.contains(&("⇧Tab", "shift+tab")));
        assert!(keys.contains(&("^D", "ctrl+d")));
        assert!(keys.contains(&("|", "|")));
    }
}
