//! Stateless shell for the two custom modal overlays (Add Workspace and Shell
//! Picker). It renders the scrim, centered panel, header title, one close
//! control, hairline separator, and the single vertical scrolling content
//! region. All application state stays in the modal view entities.

use crate::theme::{Theme, TOUCH_TARGET_PX};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::*;
use gpui_kit::{div, px, FocusHandle, Styled, Window};
use std::rc::Rc;

type CloseCallback = Rc<dyn Fn(&mut Window, &mut App)>;

/// Stateless presentation frame for a custom overlay.
#[derive(IntoElement)]
pub struct ModalShell {
    id: SharedString,
    title: SharedString,
    focus_handle: FocusHandle,
    opener_focus: Option<FocusHandle>,
    preferred_width: f32,
    max_height: f32,
    content: AnyElement,
    on_close: CloseCallback,
}

impl ModalShell {
    #[allow(clippy::too_many_arguments)]
    pub fn new<E: IntoElement>(
        id: impl Into<SharedString>,
        title: impl Into<SharedString>,
        focus_handle: &FocusHandle,
        opener_focus: Option<FocusHandle>,
        preferred_width: f32,
        max_height: f32,
        content: E,
        on_close: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            focus_handle: focus_handle.clone(),
            opener_focus,
            preferred_width,
            max_height,
            content: content.into_any_element(),
            on_close: Rc::new(on_close),
        }
    }
}

impl RenderOnce for ModalShell {
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let theme = Theme::current();
        let scale = theme.density_scale();
        let available = f32::from(window.bounds().size.width);
        let width = (self.preferred_width * scale).min(available - 32.0 * scale);

        let close_click = self.on_close.clone();
        let click_opener = self.opener_focus.clone();
        let close_escape = self.on_close.clone();
        let escape_opener = self.opener_focus.clone();
        let overlay_id = format!("{}-overlay", self.id);
        let panel_id = format!("{}-panel", self.id);
        let close_id = format!("{}-close", self.id);
        let overlay_selector = overlay_id.clone();
        let panel_selector = panel_id.clone();

        let overlay = div()
            .id(overlay_id)
            .debug_selector(move || overlay_selector.clone())
            .flex()
            .items_center()
            .justify_center()
            .size_full()
            .bg(theme.scrim)
            .track_focus(&self.focus_handle);
        let overlay = gpui_kit::base::FocusTrapElement::focus_trap(
            overlay,
            self.id.clone(),
            &self.focus_handle,
        );
        overlay
            .on_key_down(move |ev: &gpui_kit::KeyDownEvent, window, cx| {
                if ev.keystroke.key == "escape" {
                    close_escape(window, cx);
                    if let Some(handle) = &escape_opener {
                        handle.focus(window, cx);
                    }
                }
            })
            .child(
                div()
                    .id(panel_id)
                    .debug_selector(move || panel_selector.clone())
                    .flex()
                    .flex_col()
                    .gap(px(10.0 * scale))
                    .w(px(width))
                    .max_h(px(self.max_height * scale))
                    .p_4()
                    .rounded_lg()
                    .bg(theme.surface_panel)
                    .border_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .border_b_1()
                            .border_color(theme.border)
                            .pb(px(8.0 * scale))
                            .child(
                                div()
                                    .text_size(px(14.0 * scale))
                                    .text_color(theme.text_strong)
                                    .child(self.title.clone()),
                            )
                            .child(
                                Button::new(close_id.clone())
                                    .debug_selector(move || close_id.clone())
                                    .icon(IconName::X)
                                    .tooltip("Close")
                                    .accessibility_label("Close")
                                    .w(px(TOUCH_TARGET_PX))
                                    .h(px(TOUCH_TARGET_PX))
                                    .min_h(px(TOUCH_TARGET_PX))
                                    .min_w(px(TOUCH_TARGET_PX))
                                    .text()
                                    .on_click(move |_, window, cx| {
                                        close_click(window, cx);
                                        if let Some(handle) = &click_opener {
                                            handle.focus(window, cx);
                                        }
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_h(px(0.0))
                            .overflow_y_scrollbar()
                            .child(self.content),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orion_test;
    use gpui_kit::test::TestWindowExt;

    struct ShellHost {
        closed: bool,
        focus_handle: FocusHandle,
        opener: FocusHandle,
    }

    impl Render for ShellHost {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            if !self.closed && !self.focus_handle.is_focused(window) {
                self.focus_handle.focus(window, cx);
            }
            let view = cx.entity();
            let close = move |_: &mut Window, app: &mut App| {
                view.update(app, |this, cx| {
                    this.closed = true;
                    cx.notify();
                });
            };
            if self.closed {
                return div().id("shell-test-closed").into_any_element();
            }
            div()
                .track_focus(&self.opener)
                .child(ModalShell::new(
                    "shell-test",
                    "Test shell",
                    &self.focus_handle,
                    Some(self.opener.clone()),
                    420.0,
                    500.0,
                    div()
                        .id("shell-test-content")
                        .debug_selector(|| "shell-test-content".to_string())
                        .child("body"),
                    close,
                ))
                .into_any_element()
        }
    }

    orion_test!(
        fn modal_shell_has_one_close_control(cx: &mut gpui_kit::TestAppContext) {
            let opener = cx.update(|app| app.focus_handle().tab_stop(true));
            let (_, cx) = cx.add_window_view(|_, cx| ShellHost {
                closed: false,
                focus_handle: cx.focus_handle(),
                opener: opener.clone(),
            });
            cx.update(|window, cx| window.render_frame(cx));
            assert!(cx.debug_bounds("shell-test-close").is_some());
            assert!(cx.debug_bounds("shell-test-cancel").is_none());
            assert!(cx.debug_bounds("shell-test-content").is_some());
        }
    );

    crate::orion_test!(
        fn modal_shell_escape_closes_and_restores_opener(cx: &mut gpui_kit::TestAppContext) {
            let opener = cx.update(|app| app.focus_handle().tab_stop(true));
            let (root, cx) = cx.add_window_view(|_, cx| ShellHost {
                closed: false,
                focus_handle: cx.focus_handle(),
                opener: opener.clone(),
            });
            cx.update(|window, cx| {
                opener.focus(window, cx);
                window.render_frame(cx);
            });
            cx.simulate_keystrokes("escape");
            cx.update(|window, app| {
                assert!(root.read(app).closed, "Escape closed the shell");
                assert!(opener.is_focused(window), "focus returned to the opener");
            });
        }
    );

    crate::orion_test!(
        fn close_control_restores_opener(cx: &mut gpui_kit::TestAppContext) {
            let opener = cx.update(|app| app.focus_handle().tab_stop(true));
            let (root, cx) = cx.add_window_view(|_, cx| ShellHost {
                closed: false,
                focus_handle: cx.focus_handle(),
                opener: opener.clone(),
            });
            cx.update(|window, cx| {
                opener.focus(window, cx);
                window.render_frame(cx);
                window.click("shell-test-close", cx);
            });
            cx.update(|window, app| {
                assert!(root.read(app).closed, "the header X closed the shell");
                assert!(opener.is_focused(window), "focus returned to the opener");
            });
        }
    );
}
