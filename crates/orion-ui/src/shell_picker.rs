//! New Terminal flow: the shell picker behind `POST /api/pane/shell`.
//!
//! The modal loads `GET /api/shells` (`{shells: [{kind, label, command,
//! installed}]}`), keeps only installed shells, and posts the chosen kind to
//! `POST /api/pane/shell`. The app listens for [`ShellPickerEvent::Created`] to
//! focus the returned pane and force its Terminal lens.

use crate::theme::Theme;
use gpui_kit::base::component_traits::Disableable;
use gpui_kit::base::StyledExt;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::radio::{Radio, RadioGroup};
use gpui_kit::*;
use gpui_kit::{div, px, Context, FocusHandle, Render, Styled, Window};
use orion_core::runtime::Runtime;
use serde_json::{json, Value};
use std::sync::Arc;

/// One shell the New Terminal picker can start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellChoice {
    pub kind: String,
    pub label: String,
    pub command: String,
    pub installed: bool,
}

/// Projects `GET /api/shells` into picker rows, in the route's order.
pub fn shell_choices(body: &Value) -> Vec<ShellChoice> {
    let Some(shells) = body.get("shells").and_then(Value::as_array) else {
        return Vec::new();
    };
    shells
        .iter()
        .filter_map(|shell| {
            let kind = shell.get("kind").and_then(Value::as_str)?.to_string();
            let label = shell
                .get("label")
                .and_then(Value::as_str)
                .filter(|label| !label.trim().is_empty())
                .unwrap_or(&kind)
                .to_string();
            let command = shell
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let installed = shell
                .get("installed")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Some(ShellChoice {
                kind,
                label,
                command,
                installed,
            })
        })
        .collect()
}

/// What the picker reports after `POST /api/pane/shell`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellPickerEvent {
    Created(String),
}

impl gpui_kit::EventEmitter<ShellPickerEvent> for ShellPickerModal {}

/// The shell picker behind `POST /api/pane/shell`.
pub struct ShellPickerModal {
    engine: Option<Arc<dyn Runtime>>,
    open: bool,
    shells: Vec<ShellChoice>,
    selected: Option<String>,
    cwd: Option<String>,
    error: Option<String>,
    focus_handle: FocusHandle,
    opener_focus: Option<FocusHandle>,
    shell_path_input: Option<Entity<InputState>>,
    _shell_path_subscription: Option<Subscription>,
}

impl ShellPickerModal {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            engine: None,
            open: false,
            shells: Vec::new(),
            selected: None,
            cwd: None,
            error: None,
            focus_handle: cx.focus_handle(),
            opener_focus: None,
            shell_path_input: None,
            _shell_path_subscription: None,
        }
    }

    pub fn attach(&mut self, engine: Arc<dyn Runtime>) {
        self.engine = Some(engine);
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn set_open(&mut self, open: bool) {
        self.open = open;
    }

    pub fn open_with_focus(
        &mut self,
        opener: Option<FocusHandle>,
        default: &str,
        cwd: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.opener_focus = opener;
        self.open_with_default(default, cwd, cx);
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// Open the picker with the saved default selected when it is installed,
    /// otherwise with the first installed shell.
    pub fn open_with_default(
        &mut self,
        default: &str,
        cwd: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.open = true;
        self.error = None;
        self.cwd = cwd;
        self.shells.clear();
        self.selected = None;
        if let Some(engine) = self.engine.clone() {
            let (status, body) = engine.http("GET", "/api/shells", &Value::Null);
            if status == 200 {
                self.shells = shell_choices(&body)
                    .into_iter()
                    .filter(|shell| shell.installed)
                    .collect();
            } else {
                self.error = Some(if status == 404 || status == 501 {
                    "The Orion runtime is outdated. Restart the Orion daemon, then try again."
                        .to_string()
                } else {
                    error_message(&body, "The shell list could not be loaded.")
                });
            }
        }
        self.selected = self
            .shells
            .iter()
            .find(|shell| shell.kind == default)
            .or_else(|| self.shells.first())
            .map(|shell| shell.kind.clone());
        cx.notify();
    }

    pub fn create(&mut self, cwd: Option<String>, cx: &mut Context<Self>) {
        let Some(engine) = self.engine.clone() else {
            return;
        };
        let path = self
            .shell_path_input
            .as_ref()
            .map(|state| state.read(cx).value().to_string())
            .unwrap_or_default()
            .trim()
            .to_string();
        let kind = self.selected.clone();
        if kind.is_none() && path.is_empty() {
            self.error = Some("Choose an installed shell or type a shell path.".to_string());
            cx.notify();
            return;
        }
        let mut body = json!({ "shell": shell_body(kind.as_deref(), &path) });
        if let Some(cwd) = cwd.filter(|path| !path.trim().is_empty()) {
            body["cwd"] = json!(cwd);
        }
        let (status, response) = engine.http("POST", "/api/pane/shell", &body);
        if status != 200 {
            self.error = Some(error_message(
                &response,
                "The terminal could not be opened.",
            ));
            cx.notify();
            return;
        }
        let pane_id = response
            .get("pane_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        self.open = false;
        self.opener_focus = None;
        if let Some(pane_id) = pane_id {
            cx.emit(ShellPickerEvent::Created(pane_id));
        }
        cx.notify();
    }
}

/// The `shell` object for `POST /api/pane/shell`: a kind when one is selected,
/// and a path when the reader typed one. Either alone is enough to launch.
fn shell_body(kind: Option<&str>, path: &str) -> Value {
    let mut shell = json!({});
    if let Some(kind) = kind {
        shell["kind"] = json!(kind);
    }
    if !path.is_empty() {
        shell["path"] = json!(path);
    }
    shell
}

/// The Engine's plain-language message from an error body, or `fallback`.
fn error_message(body: &Value, fallback: &str) -> String {
    body.get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .unwrap_or(fallback)
        .to_string()
}

impl Render for ShellPickerModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.open && !self.focus_handle.is_focused(window) {
            self.focus_handle.focus(window, cx);
        }
        let theme = Theme::current();
        let scale = theme.density_scale();
        let shells = self.shells.clone();
        let selected = self.selected.clone();
        let error = self.error.clone();
        let cwd = self.cwd.clone();

        if self.shell_path_input.is_none() {
            let input =
                cx.new(|cx| InputState::new(window, cx).placeholder("C:\\path\\to\\shell.exe"));
            let subscription = cx.subscribe(
                &input,
                |_this: &mut Self, _input, _event: &InputEvent, cx| {
                    cx.notify();
                },
            );
            self.shell_path_input = Some(input);
            self._shell_path_subscription = Some(subscription);
        }
        let has_path = self
            .shell_path_input
            .as_ref()
            .map(|state| !state.read(cx).value().trim().is_empty())
            .unwrap_or(false);
        let shell_path_field = match &self.shell_path_input {
            Some(state) => div()
                .flex()
                .flex_col()
                .gap(px(4.0 * scale))
                .child(
                    div()
                        .text_size(px(11.0 * scale))
                        .text_color(theme.text_dim)
                        .child("Or type a shell path (optional)"),
                )
                .child(Input::new(state))
                .into_any_element(),
            None => div().into_any_element(),
        };

        let selected_index = shells
            .iter()
            .position(|shell| selected.as_deref() == Some(shell.kind.as_str()));
        let shell_rows = shells.iter().map(|shell| {
            Radio::new(format!("shell-picker-{}", shell.kind))
                .label(shell.label.clone())
                .checked(selected.as_deref() == Some(shell.kind.as_str()))
        });

        crate::modal_shell::ModalShell::new(
            "shell-picker",
            "New terminal",
            &self.focus_handle,
            self.opener_focus.clone(),
            420.0,
            560.0,
            div()
                .v_flex()
                .gap(px(10.0 * scale))
                .child(
                    div()
                        .v_flex()
                        .gap(px(8.0 * scale))
                        .border_b_1()
                        .border_color(theme.border)
                        .pb(px(8.0 * scale))
                        .child(
                            div()
                                .text_size(px(11.0 * scale))
                                .text_color(theme.text_dim)
                                .child("Shell"),
                        )
                        .child(
                            div()
                                .id("shell-picker-shells")
                                .debug_selector(|| "shell-picker-shells".to_string())
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    RadioGroup::new("shell-picker-choices")
                                        .children(shell_rows)
                                        .selected_index(selected_index)
                                        .on_change(cx.listener(|this, index: &usize, _, cx| {
                                            if let Some(shell) = this.shells.get(*index) {
                                                if shell.installed {
                                                    this.selected = Some(shell.kind.clone());
                                                    cx.notify();
                                                }
                                            }
                                        })),
                                ),
                        )
                        .child(shell_path_field)
                        .child(if shells.is_empty() && error.is_none() {
                            div()
                                .text_size(px(12.0 * scale))
                                .text_color(theme.text_dim)
                                .child("No shells are installed on this machine.")
                                .into_any_element()
                        } else {
                            div().into_any_element()
                        }),
                )
                .child(if let Some(error) = error {
                    Alert::error("shell-picker-error", error).into_any_element()
                } else {
                    div().into_any_element()
                })
                .child(
                    div().h_flex().justify_end().gap(px(8.0 * scale)).child(
                        Button::new("shell-picker-open")
                            .label("Open terminal")
                            .primary()
                            .disabled(selected.is_none() && !has_path)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.create(cwd.clone(), cx);
                            })),
                    ),
                ),
            {
                let view = cx.entity();
                move |_: &mut Window, app: &mut App| {
                    view.update(app, |this, cx| {
                        this.close();
                        cx.notify();
                    });
                }
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    crate::orion_test!(
        fn primary_action_is_disabled_until_a_shell_is_available(
            cx: &mut gpui_kit::TestAppContext,
        ) {
            use gpui_kit::test::TestWindowExt;
            let (root, cx) = cx.add_window_view(|_, cx| ShellPickerModal::new(cx));
            cx.update(|window, cx| {
                window.render_frame(cx);
                assert_eq!(window.find("shell-picker-open").disabled(), Some(true));
            });
            cx.update(|_, cx| {
                root.update(cx, |view, cx| {
                    view.selected = Some("powershell".into());
                    cx.notify();
                });
            });
            cx.update(|window, cx| {
                window.render_frame(cx);
                assert_eq!(window.find("shell-picker-open").disabled(), None);
            });
        }
    );

    crate::orion_test!(
        fn modal_has_one_close_control(cx: &mut gpui_kit::TestAppContext) {
            use gpui_kit::test::TestWindowExt;
            let (_, cx) = cx.add_window_view(|_, cx| ShellPickerModal::new(cx));
            cx.update(|window, cx| window.render_frame(cx));
            assert!(cx.debug_bounds("shell-picker-close").is_some());
            assert!(cx.debug_bounds("shell-picker-cancel").is_none());
            assert!(cx.debug_bounds("shell-picker-shells").is_some());
        }
    );

    fn assert_panel_fits(cx: &mut gpui_kit::TestAppContext, width: f32) {
        use gpui_kit::test::TestWindowExt;
        use gpui_kit::Size;
        use std::ops::Deref;
        let window = cx.open_window(
            Size {
                width: gpui_kit::px(width),
                height: gpui_kit::px(800.0),
            },
            |_, cx| ShellPickerModal::new(cx),
        );
        let root = window.root(cx).expect("the shell picker root");
        let mut cx = gpui_kit::VisualTestContext::from_window(*window.deref(), cx);
        cx.update(|_window, app| {
            root.update(app, |view, cx| {
                view.open_with_focus(None, "bash", None, cx);
            });
        });
        cx.update(|window, cx| window.render_frame(cx));
        let panel = cx
            .debug_bounds("shell-picker-panel")
            .unwrap_or_else(|| panic!("the panel is drawn at {width}px"));
        let bounds = cx.update(|window, _cx| window.bounds());
        assert!(
            f32::from(panel.size.width) > 0.0,
            "panel has width at {width}px"
        );
        assert!(
            f32::from(panel.origin.x) >= f32::from(bounds.origin.x),
            "panel inside the left edge at {width}px"
        );
        assert!(
            f32::from(panel.origin.x + panel.size.width)
                <= f32::from(bounds.origin.x + bounds.size.width),
            "panel inside the right edge at {width}px"
        );
    }

    crate::orion_test!(
        fn modal_panel_fits_at_desktop_width(cx: &mut gpui_kit::TestAppContext) {
            assert_panel_fits(cx, 1280.0);
        }
    );

    crate::orion_test!(
        fn modal_panel_fits_at_narrow_width(cx: &mut gpui_kit::TestAppContext) {
            assert_panel_fits(cx, 480.0);
        }
    );

    crate::orion_test!(
        fn modal_restores_opener_focus_on_escape(cx: &mut gpui_kit::TestAppContext) {
            use gpui_kit::test::TestWindowExt;
            let opener = cx.update(|app| app.focus_handle().tab_stop(true));
            let (root, cx) = cx.add_window_view(|_, cx| ShellPickerModal::new(cx));
            cx.update(|window, cx| {
                root.update(cx, |view, cx| {
                    view.open_with_focus(Some(opener.clone()), "bash", None, cx);
                });
                opener.focus(window, cx);
                window.render_frame(cx);
            });
            cx.simulate_keystrokes("escape");
            cx.update(|window, app| {
                assert!(!root.read(app).is_open());
                assert!(opener.is_focused(window));
            });
        }
    );

    #[::core::prelude::v1::test]
    fn shell_choices_reads_the_shells_route() {
        let body = json!({
            "shells": [
                { "kind": "powershell", "label": "Windows PowerShell", "command": "powershell.exe", "installed": true },
                { "kind": "pwsh", "label": "PowerShell 7", "command": "pwsh.exe", "installed": false },
            ],
        });
        let choices = shell_choices(&body);
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0].kind, "powershell");
        assert_eq!(choices[0].label, "Windows PowerShell");
        assert_eq!(choices[0].command, "powershell.exe");
        assert!(choices[0].installed);
        assert_eq!(choices[1].kind, "pwsh");
        assert!(!choices[1].installed);
    }

    #[::core::prelude::v1::test]
    fn shell_choices_ignore_malformed_rows() {
        let body = json!({
            "shells": [
                { "label": "No kind", "installed": true },
                { "kind": "bash" },
            ],
        });
        let choices = shell_choices(&body);
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0].kind, "bash");
        assert_eq!(choices[0].label, "bash");
        assert_eq!(choices[0].command, "");
        assert!(!choices[0].installed);
    }

    #[::core::prelude::v1::test]
    fn shell_choices_are_empty_without_the_key() {
        assert!(shell_choices(&json!({})).is_empty());
    }

    #[::core::prelude::v1::test]
    fn shell_body_sends_only_the_kind_when_no_path_is_typed() {
        let body = shell_body(Some("bash"), "");
        assert_eq!(body["kind"], "bash");
        assert!(body.get("path").is_none());
    }

    #[::core::prelude::v1::test]
    fn shell_body_sends_only_the_path_when_no_kind_is_selected() {
        let body = shell_body(None, "C:\\tools\\nu.exe");
        assert!(body.get("kind").is_none());
        assert_eq!(body["path"], "C:\\tools\\nu.exe");
    }

    #[::core::prelude::v1::test]
    fn shell_body_sends_both_when_both_are_present() {
        let body = shell_body(Some("pwsh"), "C:\\pwsh.exe");
        assert_eq!(body["kind"], "pwsh");
        assert_eq!(body["path"], "C:\\pwsh.exe");
    }
}
