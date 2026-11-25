//! Settings modal (P4): Appearance (dark/light), Density (comfortable/
//! compact), and Key bar extras computed from a saved `Preferences` shape.
//!
//! Persisted via `PUT /api/preferences` with the web client's wire names
//! (`theme`, `density`, `keyBarExtras` — see `src/lib/settings.ts`
//! `DEFAULT_SETTINGS` and `src/lib/keys.ts` `KeyBarExtra`/`KEY_BAR_EXTRAS`)
//! through the in-process `Runtime::http` seam. Escape closes the modal.
//!
//! Palettes beyond Quiet Slate, quick replies, and language remain out of
//! scope. The covered groups match the web defaults (dark, comfortable,
//! `["alt"]`, alerts on, input alerts on, done alerts long).

use crate::shell_model::{
    WindowGeometry, SIDEBAR_WIDTH_MAX_PX, SIDEBAR_WIDTH_MIN_PX, SIDEBAR_WIDTH_WIDE_PX,
};
use crate::theme::{Density, Theme, ThemeMode, TOUCH_TARGET_PX};
use gpui_kit::assets::IconName;
use gpui_kit::base::{FocusTrapElement, StyledExt};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::group_box::GroupBoxVariant;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::setting::{
    SettingField, SettingGroup, SettingItem, SettingPage, Settings,
};
use gpui_kit::component::Sizable;
use gpui_kit::*;
use gpui_kit::{div, px, Context, FocusHandle, KeyDownEvent, Render, Styled, Window};
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

const UI_SIZE_CHOICES: [(Option<u8>, &str); 6] = [
    (None, "Auto"),
    (Some(12), "12"),
    (Some(14), "14"),
    (Some(16), "16"),
    (Some(18), "18"),
    (Some(20), "20"),
];

fn ui_size_key(size: Option<u8>) -> SharedString {
    match size {
        None => "auto".into(),
        Some(value) => value.to_string().into(),
    }
}

fn ui_size_from_key(key: &str) -> Option<u8> {
    if key == "auto" {
        None
    } else {
        key.parse().ok()
    }
}

// The wire accepts 11..=20, so a stored value outside the standard choices
// needs its own option or the dropdown would render with nothing selected.
fn ui_size_options(current: Option<u8>) -> Vec<(SharedString, SharedString)> {
    let mut options: Vec<(SharedString, SharedString)> = UI_SIZE_CHOICES
        .iter()
        .map(|(size, label)| {
            let text = match size {
                None => (*label).to_string(),
                Some(value) => format!("{value} px"),
            };
            (ui_size_key(*size), text.into())
        })
        .collect();
    if let Some(value) = current {
        if (11..=20).contains(&value)
            && !UI_SIZE_CHOICES.iter().any(|(size, _)| *size == Some(value))
        {
            options.push((value.to_string().into(), format!("{value} px").into()));
        }
    }
    options
}

pub const SHELL_WIRE_KINDS: &[&str] = &["powershell", "pwsh", "bash", "cmd", "zsh"];

#[cfg(windows)]
const DEFAULT_SHELL: &str = "powershell";
#[cfg(target_os = "macos")]
const DEFAULT_SHELL: &str = "zsh";
#[cfg(all(not(windows), not(target_os = "macos")))]
const DEFAULT_SHELL: &str = "bash";

fn about_text() -> String {
    format!(
        "Orion v{} · wire protocol {}",
        env!("CARGO_PKG_VERSION"),
        orion_core::runtime::PROTOCOL_VERSION
    )
}

/// A user-supplied shell path: trimmed, control characters dropped, capped at
/// 1024 chars. Existence is checked by the engine at launch time, not here.
fn sanitize_shell_path(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(1024)
        .collect()
}

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

fn extra_description(extra: &str) -> String {
    if extra == "alt" {
        return "Sticky Alt modifier".to_string();
    }
    let keys = extra_keys(extra);
    if keys.len() == 1 && keys[0].0 != keys[0].1 {
        format!("Sends {} ({})", keys[0].0, keys[0].1)
    } else {
        let labels = keys
            .iter()
            .map(|(label, _)| *label)
            .collect::<Vec<_>>()
            .join(", ");
        format!("Sends {labels}")
    }
}

fn alerts_dependents_disabled(prefs: &Preferences) -> bool {
    !prefs.alerts_on
}

/// The saved preferences shape: theme, density, key-bar extras, alerts.
#[derive(Debug, Clone, PartialEq)]
pub struct Preferences {
    pub theme: ThemeMode,
    pub density: Density,
    /// Body size in px (11–20); `None` follows the density's body size.
    pub ui_font_size: Option<u8>,
    pub key_bar_extras: Vec<String>,
    /// The terminal's font family: a comma-separated list in fallback order.
    /// Defaults to the explicit monospace stack (see `crate::terminal_font`).
    pub terminal_font: String,
    /// The terminal's font size in pixels. 13 is the default (Orbit's default).
    pub terminal_font_size: u32,
    pub alerts_on: bool,
    pub alert_input: bool,
    pub alert_done: String,
    pub default_shell: String,
    /// An optional absolute path to a shell executable, sent to the engine.
    pub shell_path: String,
    pub window_bounds: Option<WindowGeometry>,
    pub sidebar_width: f32,
}

impl Default for Preferences {
    /// `DEFAULT_SETTINGS` in Orbit `src/lib/settings.ts`: dark, comfortable,
    /// `["alt"]`, alerts on, input alerts on, done alerts long.
    fn default() -> Self {
        Self {
            theme: ThemeMode::Dark,
            density: Density::Comfortable,
            ui_font_size: None,
            key_bar_extras: vec!["alt".to_string()],
            terminal_font: crate::terminal_font::TERMINAL_FONT_STACK.to_string(),
            terminal_font_size: 13,
            alerts_on: true,
            alert_input: true,
            alert_done: "long".to_string(),
            default_shell: DEFAULT_SHELL.to_string(),
            shell_path: String::new(),
            window_bounds: None,
            sidebar_width: SIDEBAR_WIDTH_WIDE_PX,
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
            "uiFontSize": self.ui_font_size,
            "keyBarExtras": self.key_bar_extras,
            "terminalFont": self.terminal_font,
            "terminalFontSize": self.terminal_font_size,
            "alertsOn": self.alerts_on,
            "alertInput": self.alert_input,
            "alertDone": self.alert_done,
            "defaultShell": self.default_shell,
            "shellPath": self.shell_path,
            "windowBounds": self.window_bounds.map(|bounds| {
                json!({
                    "x": bounds.x,
                    "y": bounds.y,
                    "width": bounds.width,
                    "height": bounds.height,
                })
            }),
            "sidebarWidth": self.sidebar_width,
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
        let ui_font_size = settings
            .get("uiFontSize")
            .and_then(Value::as_u64)
            .and_then(|value| u8::try_from(value).ok())
            .filter(|value| (11..=20).contains(value));
        let terminal_font = settings
            .get("terminalFont")
            .and_then(Value::as_str)
            .map(crate::terminal_font::sanitize_font_family)
            .unwrap_or_else(|| defaults.terminal_font.clone());
        let terminal_font_size = settings
            .get("terminalFontSize")
            .and_then(Value::as_u64)
            .map(|value| value.clamp(10, 22) as u32)
            .unwrap_or(13);
        let default_shell = match get_str("defaultShell") {
            value if SHELL_WIRE_KINDS.contains(&value) => value.to_string(),
            _ => defaults.default_shell.clone(),
        };
        let shell_path = settings
            .get("shellPath")
            .and_then(Value::as_str)
            .map(sanitize_shell_path)
            .unwrap_or_default();
        let finite_number = |value: Option<&Value>| {
            value
                .and_then(Value::as_f64)
                .map(|value| value as f32)
                .filter(|value| value.is_finite())
        };
        let window_bounds = settings.get("windowBounds").and_then(|bounds| {
            Some(WindowGeometry {
                x: finite_number(bounds.get("x"))?,
                y: finite_number(bounds.get("y"))?,
                width: finite_number(bounds.get("width"))?,
                height: finite_number(bounds.get("height"))?,
            })
        });
        let sidebar_width = finite_number(settings.get("sidebarWidth"))
            .map(|width| width.clamp(SIDEBAR_WIDTH_MIN_PX, SIDEBAR_WIDTH_MAX_PX))
            .unwrap_or(defaults.sidebar_width);
        Self {
            theme,
            density,
            ui_font_size,
            key_bar_extras: extras,
            terminal_font,
            terminal_font_size,
            alerts_on,
            alert_input,
            alert_done,
            default_shell,
            shell_path,
            window_bounds,
            sidebar_width,
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

// SettingField callbacks only receive `&App`/`&mut App`, so the preferences
// they read and write live in a global bridge rather than in the view entity.
#[derive(Default)]
struct SettingsStore {
    prefs: Preferences,
    engine: Option<Arc<dyn Runtime>>,
    last_body: Option<String>,
    view_id: Option<EntityId>,
}

impl Global for SettingsStore {}

fn apply_theme(cx: &App) {
    let prefs = &cx.global::<SettingsStore>().prefs;
    Theme::set_current(Theme::for_preferences(
        prefs.theme,
        prefs.density,
        prefs.ui_font_size,
    ));
}

// A 404 (nothing written yet) keeps the defaults.
fn refresh_prefs(cx: &mut App) {
    let engine = cx.global::<SettingsStore>().engine.clone();
    let Some(engine) = engine else {
        return;
    };
    let (status, body) = engine.http("GET", "/api/preferences", &Value::Null);
    if status != 200 {
        return;
    }
    let encoded = body.to_string();
    let settings = body.get("settings").cloned();
    if cx.global::<SettingsStore>().last_body.as_deref() == Some(encoded.as_str()) {
        return;
    }
    {
        let store = cx.global_mut::<SettingsStore>();
        store.last_body = Some(encoded);
        if let Some(settings) = &settings {
            store.prefs = Preferences::from_settings(settings);
        }
    }
    if settings.is_some() {
        apply_theme(cx);
    }
}

fn save_prefs(cx: &mut App) {
    apply_theme(cx);
    let (body, engine, view_id) = {
        let store = cx.global::<SettingsStore>();
        (
            json!({ "settings": store.prefs.settings_value() }),
            store.engine.clone(),
            store.view_id,
        )
    };
    if let Some(engine) = engine {
        let _ = engine.http("PUT", "/api/preferences", &body);
    }
    if let Some(view_id) = view_id {
        cx.notify(view_id);
    }
}

pub struct SettingsModal {
    focus_handle: FocusHandle,
    opener_focus: Option<FocusHandle>,
    open: bool,
}

impl SettingsModal {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let id = cx.entity_id();
        cx.default_global::<SettingsStore>().view_id = Some(id);
        Self {
            focus_handle: cx.focus_handle(),
            opener_focus: None,
            open: false,
        }
    }

    pub fn attach(&mut self, engine: Arc<dyn Runtime>, cx: &mut App) {
        cx.global_mut::<SettingsStore>().engine = Some(engine);
        refresh_prefs(cx);
    }

    pub fn prefs(&self, cx: &App) -> Preferences {
        cx.global::<SettingsStore>().prefs.clone()
    }

    pub fn set_window_bounds(&mut self, bounds: WindowGeometry, cx: &mut App) {
        if cx.global::<SettingsStore>().prefs.window_bounds == Some(bounds) {
            return;
        }
        cx.global_mut::<SettingsStore>().prefs.window_bounds = Some(bounds);
        save_prefs(cx);
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn open_with_focus(&mut self, opener: Option<FocusHandle>) {
        self.opener_focus = opener;
        self.open = true;
    }

    fn close(&mut self, window: &mut Window, cx: &mut App) {
        self.open = false;
        if let Some(handle) = self.opener_focus.take() {
            handle.focus(window, cx);
        }
    }
}

impl Render for SettingsModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current();
        let scale = theme.density_scale();
        let available = f32::from(window.bounds().size.width);
        let panel_w = (760.0 * scale)
            .min(available - 32.0 * scale)
            .max(320.0 * scale);

        let appearance_page = SettingPage::new("Appearance")
            .icon(IconName::Palette)
            .default_open(true)
            .resettable(false)
            .groups([SettingGroup::new().title("Display").items([
                SettingItem::new(
                    "Theme",
                    SettingField::<SharedString>::dropdown(
                        vec![
                            ("dark".into(), "Dark".into()),
                            ("light".into(), "Light".into()),
                        ],
                        |cx: &App| cx.global::<SettingsStore>().prefs.theme.as_wire().into(),
                        |value: SharedString, cx: &mut App| {
                            cx.global_mut::<SettingsStore>().prefs.theme =
                                ThemeMode::from_wire(&value);
                            save_prefs(cx);
                        },
                    ),
                ),
                SettingItem::new(
                    "Interface size",
                    SettingField::<SharedString>::dropdown(
                        ui_size_options(cx.global::<SettingsStore>().prefs.ui_font_size),
                        |cx: &App| ui_size_key(cx.global::<SettingsStore>().prefs.ui_font_size),
                        |value: SharedString, cx: &mut App| {
                            cx.global_mut::<SettingsStore>().prefs.ui_font_size =
                                ui_size_from_key(&value);
                            save_prefs(cx);
                        },
                    ),
                ),
                SettingItem::new(
                    "Sidebar width",
                    SettingField::<SharedString>::dropdown(
                        vec![
                            ("264".into(), "Default (264 px)".into()),
                            ("300".into(), "Wide (300 px)".into()),
                            ("360".into(), "Extra wide (360 px)".into()),
                            ("420".into(), "Maximum (420 px)".into()),
                        ],
                        |cx: &App| {
                            SharedString::from(
                                (cx.global::<SettingsStore>().prefs.sidebar_width.round() as u32)
                                    .to_string(),
                            )
                        },
                        |value: SharedString, cx: &mut App| {
                            cx.global_mut::<SettingsStore>().prefs.sidebar_width = value
                                .parse::<f32>()
                                .unwrap_or(SIDEBAR_WIDTH_WIDE_PX)
                                .clamp(SIDEBAR_WIDTH_MIN_PX, SIDEBAR_WIDTH_MAX_PX);
                            save_prefs(cx);
                        },
                    ),
                )
                .description("Width of the pane tree on wide windows"),
                SettingItem::new(
                    "Density",
                    SettingField::<SharedString>::dropdown(
                        vec![
                            ("comfortable".into(), "Comfortable".into()),
                            ("compact".into(), "Compact".into()),
                        ],
                        |cx: &App| cx.global::<SettingsStore>().prefs.density.as_wire().into(),
                        |value: SharedString, cx: &mut App| {
                            cx.global_mut::<SettingsStore>().prefs.density =
                                Density::from_wire(&value);
                            save_prefs(cx);
                        },
                    ),
                ),
            ])]);

        let key_bar_page = SettingPage::new("Key bar")
            .icon(IconName::Keyboard)
            .default_open(true)
            .resettable(false)
            .groups([
                SettingGroup::new()
                    .title("Modifiers")
                    .items([SettingItem::new(
                        extra_label("alt"),
                        SettingField::<bool>::switch(
                            |cx: &App| cx.global::<SettingsStore>().prefs.has_extra("alt"),
                            |value: bool, cx: &mut App| {
                                {
                                    let store = cx.global_mut::<SettingsStore>();
                                    if store.prefs.has_extra("alt") != value {
                                        store.prefs.toggle_extra("alt");
                                    }
                                }
                                save_prefs(cx);
                            },
                        ),
                    )
                    .description(extra_description("alt"))]),
                SettingGroup::new()
                    .title("Shortcuts")
                    .items(KEY_BAR_EXTRAS.iter().skip(1).map(|extra| {
                        let extra = *extra;
                        SettingItem::new(
                            extra_label(extra),
                            SettingField::<bool>::switch(
                                move |cx: &App| cx.global::<SettingsStore>().prefs.has_extra(extra),
                                move |value: bool, cx: &mut App| {
                                    {
                                        let store = cx.global_mut::<SettingsStore>();
                                        if store.prefs.has_extra(extra) != value {
                                            store.prefs.toggle_extra(extra);
                                        }
                                    }
                                    save_prefs(cx);
                                },
                            ),
                        )
                        .description(extra_description(extra))
                    })),
            ]);

        let dependents_disabled = alerts_dependents_disabled(&cx.global::<SettingsStore>().prefs);
        let alerts_page = SettingPage::new("Alerts")
            .icon(IconName::Bell)
            .default_open(true)
            .resettable(false)
            .groups([SettingGroup::new().title("Notifications").items([
                SettingItem::new(
                    "Alerts on",
                    SettingField::<bool>::switch(
                        |cx: &App| cx.global::<SettingsStore>().prefs.alerts_on,
                        |value: bool, cx: &mut App| {
                            cx.global_mut::<SettingsStore>().prefs.alerts_on = value;
                            save_prefs(cx);
                        },
                    ),
                )
                .description("Master switch for terminal alerts"),
                SettingItem::new(
                    "Input alerts",
                    SettingField::<bool>::switch(
                        |cx: &App| cx.global::<SettingsStore>().prefs.alert_input,
                        |value: bool, cx: &mut App| {
                            cx.global_mut::<SettingsStore>().prefs.alert_input = value;
                            save_prefs(cx);
                        },
                    ),
                )
                .description(if dependents_disabled {
                    "Requires alerts on"
                } else {
                    "Notify when the agent needs input"
                })
                .disabled(dependents_disabled),
                SettingItem::new(
                    "Done alerts",
                    SettingField::<SharedString>::dropdown(
                        vec![
                            ("long".into(), "Long".into()),
                            ("always".into(), "Always".into()),
                            ("off".into(), "Off".into()),
                        ],
                        |cx: &App| cx.global::<SettingsStore>().prefs.alert_done.clone().into(),
                        |value: SharedString, cx: &mut App| {
                            cx.global_mut::<SettingsStore>().prefs.alert_done = value.to_string();
                            save_prefs(cx);
                        },
                    ),
                )
                .description(if dependents_disabled {
                    "Requires alerts on"
                } else {
                    "Notify when a turn completes"
                })
                .disabled(dependents_disabled),
            ])]);

        let shell_options: Vec<(SharedString, SharedString)> = orion_core::ShellKind::ALL
            .iter()
            .map(|kind| (kind.wire_kind().into(), kind.display_name().into()))
            .collect();
        let terminal_page = SettingPage::new("Terminal")
            .icon(IconName::Terminal)
            .default_open(true)
            .resettable(false)
            .groups([SettingGroup::new().title("Default shell").items([
                SettingItem::new(
                    "Shell",
                    SettingField::<SharedString>::dropdown(
                        shell_options,
                        |cx: &App| {
                            SharedString::from(
                                cx.global::<SettingsStore>().prefs.default_shell.clone(),
                            )
                        },
                        |value: SharedString, cx: &mut App| {
                            cx.global_mut::<SettingsStore>().prefs.default_shell =
                                value.to_string();
                            save_prefs(cx);
                        },
                    ),
                ),
                SettingItem::new(
                    "Shell path (optional)",
                    SettingField::<SharedString>::render(|_, window, cx| {
                        let current = cx.global::<SettingsStore>().prefs.shell_path.clone();
                        let state = window.use_keyed_state(
                            SharedString::from("settings-shell-path-input"),
                            cx,
                            |window, cx| {
                                let input = cx.new(|cx| {
                                    InputState::new(window, cx)
                                        .default_value(&current)
                                        .placeholder("C:\\path\\to\\shell.exe")
                                });
                                let subscription = cx.subscribe(
                                    &input,
                                    |_, input: Entity<InputState>, event: &InputEvent, cx| {
                                        if matches!(event, InputEvent::Change) {
                                            cx.global_mut::<SettingsStore>().prefs.shell_path =
                                                input.read(cx).value().to_string();
                                            save_prefs(cx);
                                        }
                                    },
                                );
                                (input, subscription)
                            },
                        );
                        let (input, _) = state.read(cx);
                        Input::new(input)
                    }),
                )
                .layout(Axis::Vertical),
            ])]);

        let about_page = SettingPage::new("About")
            .icon(IconName::Info)
            .default_open(true)
            .resettable(false)
            .groups([SettingGroup::new()
                .title("Version")
                .items([SettingItem::render(|_, _, _| {
                    let theme = Theme::current();
                    div()
                        .text_size(px(12.0 * theme.density_scale()))
                        .text_color(theme.text_dim)
                        .child(about_text())
                })])]);

        let body = div()
            .id("settings-panel")
            .debug_selector(|| "settings-panel".to_string())
            .v_flex()
            .gap(px(12.0 * scale))
            .w(px(panel_w))
            .h(px(600.0 * scale))
            .p_4()
            .rounded_lg()
            .bg(theme.surface_panel)
            .border_1()
            .border_color(theme.border)
            .child(
                div()
                    .h_flex()
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
                            .icon(IconName::X)
                            .tooltip("Close")
                            .accessibility_label("Close settings")
                            .debug_selector(|| "settings-close".to_string())
                            .w(px(TOUCH_TARGET_PX))
                            .h(px(TOUCH_TARGET_PX))
                            .min_h(px(TOUCH_TARGET_PX))
                            .min_w(px(TOUCH_TARGET_PX))
                            .text()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.close(window, cx);
                            })),
                    ),
            )
            .child(
                div().size_full().flex_1().child(
                    Settings::new("settings-panel-content")
                        .pages([
                            appearance_page,
                            key_bar_page,
                            alerts_page,
                            terminal_page,
                            about_page,
                        ])
                        .with_group_variant(GroupBoxVariant::Outline)
                        .with_size(gpui_kit::component::Size::default())
                        .sidebar_width(px(if panel_w < 560.0 * scale {
                            120.0
                        } else {
                            200.0
                        } * scale))
                        .sidebar_size_range(px(100.0 * scale)..px(360.0 * scale)),
                ),
            );

        if self.open && !self.focus_handle.is_focused(window) {
            self.focus_handle.focus(window, cx);
        }

        div()
            .id("settings-overlay")
            .track_focus(&self.focus_handle)
            .focus_trap("settings-modal", &self.focus_handle)
            .absolute()
            .inset_0()
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                if ev.keystroke.key == "escape" {
                    this.close(window, cx);
                    cx.notify();
                }
            }))
            .child(
                div()
                    .id("settings-scrim")
                    .absolute()
                    .inset_0()
                    .bg(theme.scrim),
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
    use gpui_kit::test::TestWindowExt;
    use std::ops::Deref;

    struct SettingsRoot {
        view: Entity<SettingsModal>,
        opener: FocusHandle,
    }

    impl Render for SettingsRoot {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().track_focus(&self.opener).child(self.view.clone())
        }
    }

    crate::orion_test!(
        fn the_modal_traps_focus_and_restores_the_opener(cx: &mut gpui_kit::TestAppContext) {
            let opener = cx.update(|app| app.focus_handle().tab_stop(true));
            let (root, cx) = cx.add_window_view(|_, cx| SettingsRoot {
                view: cx.new(SettingsModal::new),
                opener: opener.clone(),
            });
            cx.update(|window, cx| opener.focus(window, cx));

            cx.update(|_window, app| {
                let view = root.read(app).view.clone();
                view.update(app, |view, cx| {
                    view.open_with_focus(Some(opener.clone()));
                    cx.notify();
                });
            });
            cx.update(|window, cx| window.render_frame(cx));

            cx.update(|window, cx| {
                let modal = root.read(cx).view.clone();
                assert!(
                    modal.read(cx).focus_handle.is_focused(window),
                    "the modal owns the keyboard on open"
                );
            });

            cx.simulate_keystrokes("tab");
            cx.update(|window, cx| {
                let modal = root.read(cx).view.clone();
                assert!(
                    modal.read(cx).focus_handle.contains_focused(window, cx),
                    "Tab kept focus inside the modal"
                );
            });

            cx.simulate_keystrokes("shift-tab");
            cx.update(|window, cx| {
                let modal = root.read(cx).view.clone();
                assert!(
                    modal.read(cx).focus_handle.contains_focused(window, cx),
                    "Shift+Tab kept focus inside the modal"
                );
            });

            cx.simulate_keystrokes("escape");
            cx.update(|window, cx| {
                let modal = root.read(cx).view.clone();
                assert!(!modal.read(cx).is_open(), "Escape closed the modal");
                assert!(opener.is_focused(window), "focus returned to the opener");
            });
        }
    );

    fn assert_panel_fits(cx: &mut gpui_kit::TestAppContext, width: f32) {
        let window = cx.open_window(
            Size {
                width: px(width),
                height: px(800.0),
            },
            |_window, cx| SettingsRoot {
                view: cx.new(SettingsModal::new),
                opener: cx.focus_handle(),
            },
        );
        let root = window.root(cx).expect("the settings root");
        let mut cx = gpui_kit::VisualTestContext::from_window(*window.deref(), cx);
        cx.update(|_window, app| {
            let view = root.read(app).view.clone();
            view.update(app, |view, cx| {
                view.open_with_focus(None);
                cx.notify();
            });
        });
        cx.update(|window, cx| window.render_frame(cx));

        let panel = cx
            .debug_bounds("settings-panel")
            .unwrap_or_else(|| panic!("the settings panel is drawn at {width}px"));
        let window = cx.update(|window, _cx| window.bounds());
        assert!(
            f32::from(panel.size.width) > 0.0,
            "the panel has width at {width}px"
        );
        assert!(
            f32::from(panel.size.height) > 0.0,
            "the panel has height at {width}px"
        );
        assert!(
            f32::from(panel.origin.x) >= f32::from(window.origin.x),
            "the panel is inside the left edge at {width}px: {panel:?}"
        );
        assert!(
            f32::from(panel.origin.x) + f32::from(panel.size.width)
                <= f32::from(window.origin.x) + f32::from(window.size.width),
            "the panel is inside the right edge at {width}px: {panel:?}"
        );
    }

    crate::orion_test!(
        fn settings_panel_fits_at_desktop_width(cx: &mut gpui_kit::TestAppContext) {
            assert_panel_fits(cx, 1280.0);
        }
    );

    crate::orion_test!(
        fn settings_panel_fits_at_narrow_width(cx: &mut gpui_kit::TestAppContext) {
            assert_panel_fits(cx, 480.0);
        }
    );

    crate::orion_test!(
        fn the_panel_has_one_close_control(cx: &mut gpui_kit::TestAppContext) {
            let window = cx.open_window(
                Size {
                    width: px(1280.0),
                    height: px(800.0),
                },
                |_window, cx| SettingsRoot {
                    view: cx.new(SettingsModal::new),
                    opener: cx.focus_handle(),
                },
            );
            let root = window.root(cx).expect("the settings root");
            let mut cx = gpui_kit::VisualTestContext::from_window(*window.deref(), cx);
            cx.update(|_window, app| {
                let view = root.read(app).view.clone();
                view.update(app, |view, cx| {
                    view.open_with_focus(None);
                    cx.notify();
                });
            });
            cx.update(|window, cx| window.render_frame(cx));

            let close = cx
                .debug_bounds("settings-close")
                .expect("the header close control is drawn");
            let panel = cx
                .debug_bounds("settings-panel")
                .expect("the settings panel is drawn");
            assert!(
                f32::from(close.size.width) <= 60.0,
                "the close control is a single 44px button: {close:?}"
            );
            assert!(
                f32::from(panel.origin.x + panel.size.width - close.origin.x - close.size.width)
                    <= 40.0,
                "the close control is right-aligned in the header: {close:?} {panel:?}"
            );
            assert!(
                cx.debug_bounds("settings-done").is_none(),
                "there is no second close control"
            );
        }
    );

    #[::core::prelude::v1::test]
    fn wire_key_set_is_unchanged() {
        let value = Preferences::default().settings_value();
        let mut keys = value
            .as_object()
            .expect("the wire body is an object")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "alertDone",
                "alertInput",
                "alertsOn",
                "defaultShell",
                "density",
                "keyBarExtras",
                "shellPath",
                "sidebarWidth",
                "terminalFont",
                "terminalFontSize",
                "theme",
                "uiFontSize",
                "windowBounds",
            ],
            "the thirteen persisted keys are unchanged"
        );
    }

    #[::core::prelude::v1::test]
    fn layout_round_trips_through_settings() {
        let prefs = Preferences {
            window_bounds: Some(WindowGeometry {
                x: 40.0,
                y: 60.0,
                width: 1440.0,
                height: 900.0,
            }),
            sidebar_width: 312.0,
            ..Preferences::default()
        };
        let restored = Preferences::from_settings(&prefs.settings_value());
        assert_eq!(restored.window_bounds, prefs.window_bounds);
        assert_eq!(restored.sidebar_width, 312.0);
    }

    #[::core::prelude::v1::test]
    fn layout_survives_a_settings_restart() {
        use orion_core::engine::store::Store;
        use orion_core::Engine;

        let dir = std::env::temp_dir().join(format!("orion-layout-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let engine = Engine::with_store(Store::at(dir.clone()));
        let body = json!({ "settings": {
            "windowBounds": { "x": 40.0, "y": 60.0, "width": 1440.0, "height": 900.0 },
            "sidebarWidth": 312.0,
        }});
        let (status, _) = engine.http("PUT", "/api/preferences", &body);
        assert_eq!(status, 200);

        let restarted = Engine::with_store(Store::at(dir));
        let (status, saved) = restarted.http("GET", "/api/preferences", &Value::Null);
        assert_eq!(status, 200);
        let prefs = Preferences::from_settings(saved.get("settings").expect("saved settings"));
        assert_eq!(
            prefs.window_bounds,
            Some(WindowGeometry {
                x: 40.0,
                y: 60.0,
                width: 1440.0,
                height: 900.0,
            })
        );
        assert_eq!(prefs.sidebar_width, 312.0);
    }

    #[::core::prelude::v1::test]
    fn alert_dependents_disable_without_changing_stored_values() {
        let mut prefs = Preferences::default();
        assert!(!alerts_dependents_disabled(&prefs));
        prefs.alerts_on = false;
        prefs.alert_input = false;
        prefs.alert_done = "off".into();
        let stored = prefs.clone();
        assert!(alerts_dependents_disabled(&prefs));
        assert_eq!(prefs, stored, "disabling is derived, not persisted");
    }

    #[::core::prelude::v1::test]
    fn single_choice_options_cover_every_wire_value() {
        let sizes = ui_size_options(Some(11));
        for key in ["auto", "11", "12", "14", "16", "18", "20"] {
            assert!(
                sizes.iter().any(|(value, _)| value.as_str() == key),
                "the interface size dropdown offers {key}"
            );
        }
        assert_eq!(ui_size_key(None), "auto");
        assert_eq!(ui_size_from_key("auto"), None);
        assert_eq!(ui_size_from_key("11"), Some(11));

        let shell_keys = orion_core::ShellKind::ALL
            .iter()
            .map(|kind| kind.wire_kind())
            .collect::<Vec<_>>();
        assert_eq!(shell_keys.len(), 5);
        assert!(shell_keys.contains(&"powershell"));
        assert!(shell_keys.contains(&"pwsh"));

        for extra in KEY_BAR_EXTRAS {
            assert!(
                !extra_description(extra).is_empty(),
                "every key bar toggle carries a description"
            );
        }
    }

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

    #[::core::prelude::v1::test]
    fn default_shell_round_trips_through_settings() {
        let prefs = Preferences::default();
        let value = prefs.settings_value();
        assert_eq!(
            value.get("defaultShell").and_then(Value::as_str),
            Some(prefs.default_shell.as_str())
        );
        let restored = Preferences::from_settings(&value);
        assert_eq!(restored.default_shell, prefs.default_shell);
    }

    #[::core::prelude::v1::test]
    fn invalid_default_shell_falls_back_to_the_platform_default() {
        let settings = json!({ "defaultShell": "fish" });
        let prefs = Preferences::from_settings(&settings);
        assert_eq!(prefs.default_shell, Preferences::default().default_shell);
    }

    #[::core::prelude::v1::test]
    fn settings_value_carries_the_chosen_default_shell() {
        let prefs = Preferences {
            default_shell: "cmd".to_string(),
            ..Preferences::default()
        };
        assert_eq!(prefs.settings_value()["defaultShell"], "cmd");
    }

    #[::core::prelude::v1::test]
    fn default_shell_matches_the_current_os() {
        #[cfg(windows)]
        let expected = "powershell";
        #[cfg(target_os = "macos")]
        let expected = "zsh";
        #[cfg(all(not(windows), not(target_os = "macos")))]
        let expected = "bash";
        assert_eq!(Preferences::default().default_shell, expected);
    }

    #[::core::prelude::v1::test]
    fn shell_path_round_trips_through_settings() {
        let mut prefs = Preferences::default();
        assert_eq!(prefs.shell_path, "");
        prefs.shell_path = "C:\\tools\\nu.exe".to_string();
        let value = prefs.settings_value();
        assert_eq!(
            value.get("shellPath").and_then(Value::as_str),
            Some("C:\\tools\\nu.exe")
        );
        let restored = Preferences::from_settings(&value);
        assert_eq!(restored.shell_path, prefs.shell_path);
    }

    #[::core::prelude::v1::test]
    fn invalid_shell_path_is_sanitized() {
        let settings = json!({ "shellPath": "  C:\\Program Files\\pwsh.exe\n  " });
        let prefs = Preferences::from_settings(&settings);
        assert_eq!(prefs.shell_path, "C:\\Program Files\\pwsh.exe");

        let settings = json!({ "shellPath": "C:\\a\u{0}b\u{1f}c.exe" });
        let prefs = Preferences::from_settings(&settings);
        assert_eq!(prefs.shell_path, "C:\\abc.exe");

        let long = "a".repeat(2000);
        let settings = json!({ "shellPath": long });
        let prefs = Preferences::from_settings(&settings);
        assert_eq!(prefs.shell_path.chars().count(), 1024);
    }

    #[::core::prelude::v1::test]
    fn terminal_font_defaults_are_non_empty_and_monospace() {
        let prefs = Preferences::default();
        assert_eq!(
            prefs.terminal_font,
            crate::terminal_font::TERMINAL_FONT_STACK,
            "the default family is the explicit monospace stack"
        );
        assert!(!prefs.terminal_font.is_empty());
        let families = crate::terminal_font::terminal_font_families(&prefs.terminal_font);
        assert!(
            families
                .iter()
                .any(|name| !crate::terminal_font::is_symbol_family(name)),
            "a real monospace face is available for cell sizing: {families:?}"
        );
        assert!(
            families
                .iter()
                .any(|name| crate::terminal_font::is_symbol_family(name)),
            "the symbols-only face stays in the stack for glyph fallback"
        );
    }

    #[::core::prelude::v1::test]
    fn about_reports_the_running_version_and_protocol() {
        let text = about_text();
        assert!(
            text.contains(env!("CARGO_PKG_VERSION")),
            "About shows the binary version: {text}"
        );
        assert!(
            text.contains(&orion_core::runtime::PROTOCOL_VERSION.to_string()),
            "About shows the wire protocol: {text}"
        );
    }

    #[::core::prelude::v1::test]
    fn interface_font_size_defaults_to_following_density() {
        assert_eq!(Preferences::default().ui_font_size, None);
        assert_eq!(
            Preferences::from_settings(&json!({})).ui_font_size,
            None,
            "a record with no uiFontSize follows the density"
        );
    }

    #[::core::prelude::v1::test]
    fn interface_font_size_round_trips_through_settings() {
        let prefs = Preferences {
            ui_font_size: Some(18),
            ..Preferences::default()
        };
        let value = prefs.settings_value();
        assert_eq!(value["uiFontSize"], 18);
        assert_eq!(Preferences::from_settings(&value).ui_font_size, Some(18));
    }

    #[::core::prelude::v1::test]
    fn interface_font_size_is_clamped_to_the_supported_range() {
        let too_small = Preferences::from_settings(&json!({ "uiFontSize": 5 }));
        let too_large = Preferences::from_settings(&json!({ "uiFontSize": 99 }));
        let non_numeric = Preferences::from_settings(&json!({ "uiFontSize": "big" }));
        assert_eq!(too_small.ui_font_size, None);
        assert_eq!(too_large.ui_font_size, None);
        assert_eq!(non_numeric.ui_font_size, None);
        assert_eq!(
            Preferences::from_settings(&json!({ "uiFontSize": 11 })).ui_font_size,
            Some(11)
        );
        assert_eq!(
            Preferences::from_settings(&json!({ "uiFontSize": 20 })).ui_font_size,
            Some(20)
        );
    }

    #[::core::prelude::v1::test]
    fn terminal_font_defaults_keep_an_explicit_size() {
        let prefs = Preferences::default();
        assert_eq!(
            prefs.terminal_font_size, 13,
            "an explicit pixel size is carried"
        );
        let restored = Preferences::from_settings(&json!({}));
        assert_eq!(restored.terminal_font, prefs.terminal_font);
        assert_eq!(restored.terminal_font_size, prefs.terminal_font_size);
    }
}
