//! Settings modal (P4): Appearance (dark/light), Density (comfortable/
//! compact), and Key bar extras computed from a saved `Preferences` shape.
//!
//! Persisted via `PUT /api/preferences` with the web client's wire names
//! (`theme`, `density`, `keyBarExtras` — see `src/lib/settings.ts`
//! `DEFAULT_SETTINGS` and `src/lib/keys.ts` `KeyBarExtra`/`KEY_BAR_EXTRAS`)
//! through the in-process `Runtime::http` seam. Escape closes the modal.
//!
//! Deliberately NOT ported yet (follow-up): palettes beyond Quiet Slate,
//! terminal/chat fonts, alerts, quick replies, language, etc. Those live in
//! the web client's full `Settings` interface; this modal covers the four
//! groups P4 scoped, with matching defaults (dark, comfortable, `["alt"]`).

use crate::theme::{Density, Theme, ThemeMode};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::group_box::GroupBox;
use gpui_kit::component::radio::{Radio, RadioGroup};
use gpui_kit::component::switch::Switch;
use gpui_kit::*;
use gpui_kit::{
    div, px, Context, FocusHandle, KeyDownEvent, MouseButton, MouseDownEvent, Render, Styled,
    Window,
};
use orion_core::runtime::Runtime;
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

/// The saved preferences shape: theme, density, key-bar extras, alerts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preferences {
    pub theme: ThemeMode,
    pub density: Density,
    pub key_bar_extras: Vec<String>,
    /// The terminal's font family: a comma-separated list in fallback order.
    /// Empty means the built-in stack (see `crate::terminal_font`).
    pub terminal_font: String,
    /// The terminal's font size in pixels. 13 is the default (Orbit's default).
    pub terminal_font_size: u32,
    pub alerts_on: bool,
    pub alert_input: bool,
    pub alert_done: String,
}

impl Default for Preferences {
    /// `DEFAULT_SETTINGS` in Orbit `src/lib/settings.ts`: dark, comfortable,
    /// `["alt"]`, alerts on, input alerts on, done alerts long.
    fn default() -> Self {
        Self {
            theme: ThemeMode::Dark,
            density: Density::Comfortable,
            key_bar_extras: vec!["alt".to_string()],
            terminal_font: String::new(),
            terminal_font_size: 13,
            alerts_on: true,
            alert_input: true,
            alert_done: "long".to_string(),
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
            "terminalFont": self.terminal_font,
            "terminalFontSize": self.terminal_font_size,
            "alertsOn": self.alerts_on,
            "alertInput": self.alert_input,
            "alertDone": self.alert_done,
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
        let alerts_on = settings
            .get("alertsOn")
            .and_then(Value::as_bool)
            .unwrap_or(defaults.alerts_on);
        let alert_input = settings
            .get("alertInput")
            .and_then(Value::as_bool)
            .unwrap_or(defaults.alert_input);
        let alert_done = match get_str("alertDone") {
            "off" | "long" | "always" => get_str("alertDone").to_string(),
            _ => defaults.alert_done.clone(),
        };
        let terminal_font = settings
            .get("terminalFont")
            .and_then(Value::as_str)
            .map(crate::terminal_font::sanitize_font_family)
            .unwrap_or_default();
        let terminal_font_size = settings
            .get("terminalFontSize")
            .and_then(Value::as_u64)
            .map(|value| value.clamp(10, 22) as u32)
            .unwrap_or(13);
        Self {
            theme,
            density,
            key_bar_extras: extras,
            terminal_font,
            terminal_font_size,
            alerts_on,
            alert_input,
            alert_done,
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
    engine: Option<Arc<dyn Runtime>>,
    prefs: Preferences,
    last_body: Option<String>,
    focus_handle: FocusHandle,
    open: bool,
}

impl SettingsModal {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            engine: None,
            prefs: Preferences::default(),
            last_body: None,
            focus_handle: cx.focus_handle(),
            open: false,
        }
    }

    pub fn attach(&mut self, engine: Arc<dyn Runtime>) {
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

        // Appearance and density are single-choice: the kit's radio group owns
        // the selection, and the chosen index maps back to the persisted value.
        let theme_group = RadioGroup::horizontal("settings-theme")
            .children([
                Radio::new("settings-theme-dark")
                    .label("Dark")
                    .debug_selector(|| "settings-theme-dark".to_string()),
                Radio::new("settings-theme-light")
                    .label("Light")
                    .debug_selector(|| "settings-theme-light".to_string()),
            ])
            .selected_index(if dark { Some(0) } else { Some(1) })
            .on_change(cx.listener(|this, index: &usize, _, cx| {
                this.prefs.theme = if *index == 0 {
                    ThemeMode::Dark
                } else {
                    ThemeMode::Light
                };
                this.save();
                cx.notify();
            }));

        let density_group = RadioGroup::horizontal("settings-density")
            .children([
                Radio::new("settings-density-comfortable")
                    .label("Comfortable")
                    .debug_selector(|| "settings-density-comfortable".to_string()),
                Radio::new("settings-density-compact")
                    .label("Compact")
                    .debug_selector(|| "settings-density-compact".to_string()),
            ])
            .selected_index(if compact { Some(1) } else { Some(0) })
            .on_change(cx.listener(|this, index: &usize, _, cx| {
                this.prefs.density = if *index == 1 {
                    Density::Compact
                } else {
                    Density::Comfortable
                };
                this.save();
                cx.notify();
            }));

        // Key-bar extras are independent toggles: the kit's switch reports the
        // requested state, so the saved set follows the reader's click.
        let extra_switches = KEY_BAR_EXTRAS.iter().map(|extra| {
            let extra = *extra;
            Switch::new(format!("settings-extra-{extra}"))
                .label(extra_label(extra))
                .checked(prefs.has_extra(extra))
                .on_change(cx.listener(move |this, _next: &bool, _, cx| {
                    this.prefs.toggle_extra(extra);
                    this.save();
                    cx.notify();
                }))
        });

        // Alerts: the two booleans are switches (a kit radio cannot be
        // unchecked by the reader), and Done is one choice across
        // long/always/off.
        let alert_done_group = RadioGroup::horizontal("settings-alert-done")
            .children([
                Radio::new("settings-alert-done-long")
                    .label("Done: long")
                    .debug_selector(|| "settings-alert-done-long".to_string()),
                Radio::new("settings-alert-done-always")
                    .label("Done: always")
                    .debug_selector(|| "settings-alert-done-always".to_string()),
                Radio::new("settings-alert-done-off")
                    .label("Done: off")
                    .debug_selector(|| "settings-alert-done-off".to_string()),
            ])
            .selected_index(match prefs.alert_done.as_str() {
                "always" => Some(1),
                "off" => Some(2),
                _ => Some(0),
            })
            .on_change(cx.listener(|this, index: &usize, _, cx| {
                this.prefs.alert_done = match *index {
                    1 => "always",
                    2 => "off",
                    _ => "long",
                }
                .to_string();
                this.save();
                cx.notify();
            }));

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
                        Button::new("settings-close")
                            .label("✕")
                            .text()
                            .on_click(cx.listener(|this, _, _, _cx| this.close())),
                    ),
            )
            .child(GroupBox::new().title("APPEARANCE").child(theme_group))
            .child(GroupBox::new().title("DENSITY").child(density_group))
            .child(
                GroupBox::new()
                    .title("KEY BAR EXTRAS")
                    .child(div().flex().flex_col().gap_1().children(extra_switches)),
            )
            .child(
                GroupBox::new().title("ALERTS").child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(8.0 * scale))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap(px(16.0 * scale))
                                .child(
                                    Switch::new("settings-alerts-on")
                                        .label("Alerts on")
                                        .checked(prefs.alerts_on)
                                        .on_change(cx.listener(|this, next: &bool, _, cx| {
                                            this.prefs.alerts_on = *next;
                                            this.save();
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Switch::new("settings-alert-input")
                                        .label("Input")
                                        .checked(prefs.alert_input)
                                        .on_change(cx.listener(|this, next: &bool, _, cx| {
                                            this.prefs.alert_input = *next;
                                            this.save();
                                            cx.notify();
                                        })),
                                ),
                        )
                        .child(alert_done_group),
                ),
            )
            .child(
                GroupBox::new().title("ABOUT").child(
                    div()
                        .text_size(px(12.0 * scale))
                        .text_color(theme.text_dim)
                        .child("Orbit · native GPUI shell · Engine: live"),
                ),
            )
            .child(
                div().flex_row().justify_end().child(
                    Button::new("settings-done")
                        .label("Done")
                        .primary()
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

    #[::core::prelude::v1::test]
    fn defaults_match_web_default_settings() {
        let prefs = Preferences::default();
        assert_eq!(prefs.theme, ThemeMode::Dark);
        assert_eq!(prefs.density, Density::Comfortable);
        assert_eq!(prefs.key_bar_extras, vec!["alt".to_string()]);
    }

    #[::core::prelude::v1::test]
    fn settings_defaults_match_orbit_defaults() {
        let prefs = Preferences::default();
        assert_eq!(prefs.theme.as_wire(), "dark");
        assert_eq!(prefs.density.as_wire(), "comfortable");
        assert_eq!(prefs.key_bar_extras, vec!["alt".to_string()]);
        assert!(prefs.alerts_on);
        assert!(prefs.alert_input);
        assert_eq!(prefs.alert_done, "long");
    }

    #[::core::prelude::v1::test]
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

    #[::core::prelude::v1::test]
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
