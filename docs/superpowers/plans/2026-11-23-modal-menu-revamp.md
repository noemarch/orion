# Modal Menu Revamp Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Apply the shipped Settings modal's visual language to Add Workspace, Shell Picker, and the two confirmation dialogs without changing non-visual behavior.

**Architecture:** Add one stateless `ModalShell` render helper and use it in the two custom overlays. Keep each modal view as the owner of `open`, form state, and its focus handle; add `opener_focus` for restoration. Leave gpui-kit alert dialogs and Settings unchanged except for verified token/copy evidence.

**Tech Stack:** Rust, GPUI, gpui-kit 0.9.1, serde_json, existing `orion_test!` harness.

## Global Constraints

- No changes to `orion-core`, wire frames, HTTP routes, storage keys, or event payloads.
- No migration of custom overlays to gpui-kit `Settings`; that component is a page/sidebar browser and is wrong for short forms and pickers.
- No new feature, no validation-rule change, no change to which agent or shell can be selected.
- No changes to in-layout controls that are not modals: tab rename, terminal find bar, terminal touch key bar, tooltips, scrollbars, or the empty-state button that opens Add Workspace.
- Colors come from `cx.theme()` / Orion theme tokens only; no raw hex or `rgb`.
- Spacing, type, and panel dimensions use `theme.density_scale()`; `TOUCH_TARGET_PX` remains a fixed 44px physical minimum.
- One visible close control per custom overlay: header X only. Escape also closes. No scrim click-to-close.
- Focus is trapped while open and restored to the opener on close.
- All HTTP calls, validation rules, emitted events, selection rules, recent-directory fallback, and error meaning stay unchanged.
- Confirmation dialogs keep gpui-kit alert-dialog primitives and callbacks; only sentence-case titles, outcome-verb buttons, and verified theme tokens may change.
- Commit messages are English semantic commits matching the repo style, with the Sisyphus trailer applied by `--trailer`.

---

### Task 1: Add the shared `ModalShell` helper

**Files:**
- Create: `crates/orion-ui/src/modal_shell.rs`
- Modify: `crates/orion-ui/src/lib.rs:3-33` (module declaration)
- Test: `crates/orion-ui/src/modal_shell.rs` (`#[cfg(test)]` module in same file)

**Interfaces:**
- Consumes: `Theme::current()`, `Theme::density_scale()`, `TOUCH_TARGET_PX`, gpui-kit `FocusTrapElement`, `Button`, `IconName`.
- Produces: `pub struct ModalShell` and `ModalShell::new(id, title, focus_handle, opener_focus, preferred_width, max_height, content, on_close)` used by Tasks 2 and 3.

- [ ] **Step 1: Add the module and a failing test**

Add `pub mod modal_shell;` to `crates/orion-ui/src/lib.rs` beside the other module declarations. Create `crates/orion-ui/src/modal_shell.rs` with this test scaffold and a minimal type that renders only content so the close assertion fails:

```rust
use gpui_kit::*;

pub struct ModalShell;

impl ModalShell {
    #[allow(clippy::too_many_arguments)]
    pub fn new<E: IntoElement>(
        _id: impl Into<SharedString>,
        _title: impl Into<SharedString>,
        _focus_handle: &FocusHandle,
        _opener_focus: Option<FocusHandle>,
        _preferred_width: f32,
        _max_height: f32,
        content: E,
        _on_close: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        let _ = content.into_any_element();
        Self
    }
}

impl RenderOnce for ModalShell {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orion_test;
    use gpui_kit::test::TestWindowExt;
    use std::rc::Rc;

    struct ShellHost {
        closed: bool,
        focus_handle: FocusHandle,
        opener: FocusHandle,
    }

    impl Render for ShellHost {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let view = cx.entity();
            let close = Rc::new(move |_: &mut Window, app: &mut App| {
                view.update(app, |this, cx| {
                    this.closed = true;
                    cx.notify();
                });
            });
            div().track_focus(&self.opener).child(ModalShell::new(
                "shell-test",
                "Test shell",
                &self.focus_handle,
                Some(self.opener.clone()),
                420.0,
                500.0,
                div().id("shell-test-content").child("body"),
                close,
            ))
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
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p orion-ui --lib modal_shell::tests::modal_shell_has_one_close_control -- --nocapture`
Expected: FAIL with `debug_bounds("shell-test-close")` returning `None`.

- [ ] **Step 3: Implement `ModalShell`**

Replace the scaffold with the real helper:

```rust
use crate::theme::{Theme, TOUCH_TARGET_PX};
use gpui_kit::assets::IconName;
use gpui_kit::base::{FocusTrapElement, StyledExt};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::*;
use std::rc::Rc;

/// Stateless shell for the two custom modal overlays.
pub struct ModalShell {
    id: SharedString,
    title: SharedString,
    focus_handle: FocusHandle,
    opener_focus: Option<FocusHandle>,
    preferred_width: f32,
    max_height: f32,
    content: AnyElement,
    on_close: Rc<dyn Fn(&mut Window, &mut App)>,
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
        let width = (self.preferred_width * scale)
            .min(available - 32.0 * scale)
            .max(300.0 * scale);

        let close_click = self.on_close.clone();
        let click_opener = self.opener_focus.clone();
        let close_escape = self.on_close.clone();
        let escape_opener = self.opener_focus.clone();
        let overlay_id = format!("{}-overlay", self.id);
        let panel_id = format!("{}-panel", self.id);
        let close_id = format!("{}-close", self.id);

        div()
            .id(overlay_id)
            .flex()
            .items_center()
            .justify_center()
            .size_full()
            .bg(theme.scrim)
            .track_focus(&self.focus_handle)
            .focus_trap(self.id.clone(), &self.focus_handle)
            .on_key_down(move |ev: &KeyDownEvent, window, cx| {
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
                            .h_flex()
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
                            .overflow_y_scroll()
                            .child(self.content),
                    ),
            )
    }
}
```

Keep the test module from Step 1; add a second test before Task 2:

```rust
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
        cx.update(|_, app| {
            assert!(root.read(app).closed, "Escape closed the shell");
            assert!(opener.is_focused(app), "focus returned to the opener");
        });
    }
);
```

- [ ] **Step 4: Run the tests and check**

Run: `cargo test -p orion-ui --lib modal_shell::tests -- --nocapture`
Expected: both tests PASS.
Run: `cargo check -p orion-ui`
Expected: exit 0. If `debug_selector` or `focus_trap` fails to compile, verify the local gpui 0.2.2 / gpui-base 0.9.1 source before changing the call; do not substitute an invented API.

- [ ] **Step 5: Commit**

```bash
$env:GIT_MASTER='1'; git add crates/orion-ui/src/lib.rs crates/orion-ui/src/modal_shell.rs
$env:GIT_MASTER='1'; git -c trailer.ifexists=addIfDifferent commit -m "feat(ui): add shared modal shell" --trailer "Co-authored-by: sisyphus-dev-ai <sisyphus-dev-ai@users.noreply.github.com>"
```

---

### Task 2: Apply `ModalShell` to Add Workspace

**Files:**
- Modify: `crates/orion-ui/src/new_workspace.rs:100-168` (struct and open/close), `293-514` (render), `516-676` (tests)
- Modify: `crates/orion-ui/src/app.rs:568-570, 1060-1098, 2080-2086` (trigger opener)

**Interfaces:**
- Consumes: `ModalShell::new(...)` from Task 1.
- Produces: `NewWorkspaceModal::open_with_focus(opener: Option<FocusHandle>, start: Option<String>, cx: &mut App)`; existing `open_at` remains for tests and internal use.

- [ ] **Step 1: Write the failing tests**

Add to `new_workspace.rs` test module:

```rust
crate::orion_test!(
    fn modal_has_one_close_control(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::test::TestWindowExt;
        let (_, cx) = cx.add_window_view(|_, cx| NewWorkspaceModal::new(cx));
        cx.update(|window, cx| {
            window.render_frame(cx);
        });
        assert!(cx.debug_bounds("new-workspace-close").is_some());
        assert!(cx.debug_bounds("new-workspace-cancel").is_none());
        assert!(cx.debug_bounds("new-workspace-directories").is_some());
    }
);

crate::orion_test!(
    fn modal_restores_opener_focus_on_escape(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::test::TestWindowExt;
        let opener = cx.update(|app| app.focus_handle().tab_stop(true));
        let (root, cx) = cx.add_window_view(|_, cx| NewWorkspaceModal::new(cx));
        cx.update(|window, cx| {
            root.update(cx, |view, cx| {
                view.open_with_focus(Some(opener.clone()), None, cx);
            });
            opener.focus(window, cx);
            window.render_frame(cx);
        });
        cx.simulate_keystrokes("escape");
        cx.update(|_, app| {
            assert!(!root.read(app).is_open());
            assert!(opener.is_focused(app));
        });
    }
);
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p orion-ui --lib new_workspace::tests::modal_has_one_close_control -- --nocapture`
Expected: FAIL because `new-workspace-cancel` still exists and `open_with_focus` is missing for the second test.

- [ ] **Step 3: Add opener state and open method**

In `NewWorkspaceModal`, add field `opener_focus: Option<FocusHandle>` after `focus_handle`, initialize to `None` in `new`, and add:

```rust
pub fn open_with_focus(
    &mut self,
    opener: Option<FocusHandle>,
    start: Option<String>,
    cx: &mut App,
) {
    self.opener_focus = opener;
    self.open_at(start);
    cx.notify();
}
```

Change `close` to `pub fn close(&mut self) { self.open = false; }` (no focus logic; shell restores focus). In `create`, after `self.open = false;` add `self.opener_focus = None;` so successful creation gives focus to the new pane instead of the opener.

- [ ] **Step 4: Replace the overlay/panel/header/footer rendering**

Keep lines 300-346 (derived values, directory rows, agent rows). Replace lines 348-512 with:

```rust
let theme = Theme::current();
let scale = theme.density_scale();
let can_create = self.selected_agent.is_some() && !self.cwd.trim().is_empty();
let shell = ModalShell::new(
    "new-workspace",
    "New workspace",
    &self.focus_handle,
    self.opener_focus.clone(),
    520.0,
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
                        .child("Folder"),
                )
                .child(
                    div()
                        .text_size(px(11.0 * scale))
                        .text_color(theme.text_dim)
                        .child(cwd.clone()),
                )
                .child(
                    div()
                        .h_flex()
                        .items_center()
                        .justify_end()
                        .gap(px(8.0 * scale))
                        .child(
                            Button::new("new-workspace-home")
                                .label("Home")
                                .secondary()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.go_home();
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("new-workspace-up")
                                .label("Up")
                                .secondary()
                                .disabled(parent.is_none())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.go_up();
                                    cx.notify();
                                })),
                        ),
                )
                .child(
                    div()
                        .id("new-workspace-directories")
                        .flex()
                        .flex_col()
                        .gap_1()
                        .h(px(180.0 * scale))
                        .overflow_y_scroll()
                        .children(directory_rows),
                ),
        )
        .child(
            div()
                .v_flex()
                .gap(px(8.0 * scale))
                .child(
                    div()
                        .text_size(px(11.0 * scale))
                        .text_color(theme.text_dim)
                        .child("Agent"),
                )
                .child(
                    div()
                        .id("new-workspace-agent-scroll")
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_h(px(0.0))
                        .overflow_y_scroll()
                        .child(
                            RadioGroup::new("new-workspace-agents")
                                .children(agent_choices)
                                .selected_index(selected_index)
                                .on_change(cx.listener(|this, index: &usize, _, cx| {
                                    if let Some(agent) = this.agents.get(*index) {
                                        if agent.installed {
                                            this.selected_agent = Some(agent.kind.clone());
                                            cx.notify();
                                        }
                                    }
                                })),
                        ),
                ),
        )
        .child(if let Some(error) = error {
            Alert::error("new-workspace-error", error).into_any_element()
        } else {
            div().into_any_element()
        })
        .child(
            div()
                .h_flex()
                .justify_end()
                .gap(px(8.0 * scale))
                .child(
                    Button::new("new-workspace-create")
                        .label("Open workspace")
                        .primary()
                        .disabled(!can_create)
                        .on_click(cx.listener(|this, _, _, cx| this.create(cx))),
                ),
        ),
    {
        let view = cx.entity();
        move |window, cx| {
            view.update(cx, |this, cx| {
                this.close();
                cx.notify();
            });
            let _ = window;
        }
    },
);
shell
```

Remove the direct `if !self.focus_handle.is_focused(window) { ... }` block; `ModalShell` handles focus trap. Add imports `crate::modal_shell::ModalShell`, `gpui_kit::base::StyledExt`, and keep `IconName`, `Button`, `Radio`, `RadioGroup`, `Alert`. Remove unused `gpui_kit::KeyDownEvent` if no longer referenced.

- [ ] **Step 5: Update app.rs triggers**

Change `open_new_workspace` signature to `fn open_new_workspace(&mut self, cx: &mut App, opener: Option<FocusHandle>)`. At line 1095 replace `view.open_at(start);` with `view.open_with_focus(opener, start, cx);`. At line 570 pass `None`. At lines 2080-2086 change listener to `|this, _, window, cx| { this.open_new_workspace(cx, window.focused(cx)); cx.notify(); }`.

- [ ] **Step 6: Run targeted tests and check**

Run: `cargo test -p orion-ui --lib new_workspace::tests -- --nocapture`
Expected: all existing tests plus two new tests PASS.
Run: `cargo check -p orion-ui`
Expected: exit 0.

- [ ] **Step 7: Commit**

```bash
$env:GIT_MASTER='1'; git add crates/orion-ui/src/new_workspace.rs crates/orion-ui/src/app.rs
$env:GIT_MASTER='1'; git -c trailer.ifexists=addIfDifferent commit -m "refactor(ui): revamp add workspace modal" --trailer "Co-authored-by: sisyphus-dev-ai <sisyphus-dev-ai@users.noreply.github.com>"
```

---

### Task 3: Apply `ModalShell` to Shell Picker

**Files:**
- Modify: `crates/orion-ui/src/shell_picker.rs:73-193` (struct and open/close), `218-391` (render), `393-456` (tests)
- Modify: `crates/orion-ui/src/app.rs:1269-1291` (trigger opener)

**Interfaces:**
- Consumes: `ModalShell::new(...)` from Task 1.
- Produces: `ShellPickerModal::open_with_focus(opener: Option<FocusHandle>, default: &str, cwd: Option<String>, cx: &mut Context<Self>)`.

- [ ] **Step 1: Write the failing tests**

Add to `shell_picker.rs` tests:

```rust
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
        cx.update(|_, app| {
            assert!(!root.read(app).is_open());
            assert!(opener.is_focused(app));
        });
    }
);
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p orion-ui --lib shell_picker::tests::modal_has_one_close_control -- --nocapture`
Expected: FAIL because the cancel button still exists and `open_with_focus` is missing.

- [ ] **Step 3: Add opener state and open method**

Add field `opener_focus: Option<FocusHandle>` after `focus_handle`, initialize to `None`, and add:

```rust
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
```

Change `close` to `pub fn close(&mut self) { self.open = false; }`. In `create`, after `self.open = false;` set `self.opener_focus = None;`.

- [ ] **Step 4: Replace the overlay/panel/header/footer rendering**

Keep lines 223-270 (derived values, input creation, rows). Replace lines 272-389 with `ModalShell::new(...)` containing: a "Shell" section with the radio list, the optional path field, empty/error state, and one primary button:

```rust
let shell = ModalShell::new(
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
            div()
                .h_flex()
                .justify_end()
                .gap(px(8.0 * scale))
                .child(
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
        move |window, cx| {
            view.update(cx, |this, cx| {
                this.close();
                cx.notify();
            });
            let _ = window;
        }
    },
);
shell
```

Add imports `crate::modal_shell::ModalShell`, `gpui_kit::base::StyledExt`; keep `IconName` if still used by the shell helper? This file no longer needs `IconName` after removing the header; remove it if clippy flags unused.

- [ ] **Step 5: Update app.rs header trigger**

In the `header-new-terminal` listener (lines 1277-1291), change `|this, _, _, cx|` to `|this, _, window, cx|` and replace `view.open_with_default(&default, cwd, cx);` with `view.open_with_focus(window.focused(cx), &default, cwd, cx);`.

- [ ] **Step 6: Run targeted tests and check**

Run: `cargo test -p orion-ui --lib shell_picker::tests -- --nocapture`
Expected: all existing tests plus two new tests PASS.
Run: `cargo check -p orion-ui`
Expected: exit 0.

- [ ] **Step 7: Commit**

```bash
$env:GIT_MASTER='1'; git add crates/orion-ui/src/shell_picker.rs crates/orion-ui/src/app.rs
$env:GIT_MASTER='1'; git -c trailer.ifexists=addIfDifferent commit -m "refactor(ui): revamp shell picker modal" --trailer "Co-authored-by: sisyphus-dev-ai <sisyphus-dev-ai@users.noreply.github.com>"
```

---

### Task 4: Verify confirmation dialogs

**Files:**
- Read-only verification: `crates/orion-ui/src/app.rs:1435-1455`, `crates/orion-ui/src/sidebar.rs:690-710`
- Evidence: screenshots under `docs/visual/screenshots/`

**Interfaces:**
- Consumes: gpui-kit 0.9.1 `open_alert_dialog` APIs already used in source.
- Produces: verification evidence only; no source changes expected.

- [ ] **Step 1: Verify the APIs from local gpui-kit 0.9.1 source**

Run: `rg -n "fn open_alert_dialog|fn title|fn description|fn confirm|fn ok_text" G:\Temp\rust\home\registry\src\index.crates.io-1949cf8c6b5b557f\gpui-component-0.9.1\src`
Expected: all builder methods used by the two dialogs exist. If any do not, record the gap and leave that dialog unchanged.

- [ ] **Step 2: Audit copy and tokens**

Run: `rg -n "title\(|description\(|ok_text\(|#[0-9a-fA-F]{3,8}|rgb\(" crates/orion-ui/src/app.rs crates/orion-ui/src/sidebar.rs`
Expected: titles are sentence case, buttons are outcome verbs, and no raw colors appear in the dialog code. The current strings `Close running agent?`, `The agent is still running. Closing it will terminate it.`, `Close running session?`, and `ok_text("Close")` already satisfy the rule; do not rewrite them.

- [ ] **Step 3: Capture evidence**

Use OS-level automation to open each confirmation and capture light/dark screenshots into `docs/visual/screenshots/`. If a dialog cannot be reached in the current environment, record the exact blocker and keep the code trace as evidence. Do not introduce a new test-only env flag.

- [ ] **Step 4: Run checks**

Run: `cargo test -p orion-ui --lib`
Expected: PASS. If no source changed, commit nothing for this task.

---

### Task 5: Final verification, evidence, and handoff

**Files:**
- Evidence: `docs/visual/screenshots/` fresh PNGs for Add Workspace and Shell Picker in light/dark at wide and narrow widths.
- No source files expected.

**Interfaces:**
- Consumes: all prior tasks.
- Produces: final verification report and screenshot evidence.

- [ ] **Step 1: Run the full gate set**

Run:
```bash
cargo fmt --check
cargo check -p orion-ui
cargo test -p orion-ui --lib
cargo test -p orion-ui --test pane_seam_smoke
cargo test -p orion-ui --test terminal_startup
cargo clippy -p orion-ui -- -D warnings
cargo check -p orion-core
git diff --stat -- crates/orion-core
```
Expected: all commands exit 0 and the core diff is empty. If the all-target `cargo test -p orion-ui` is attempted and hits the known rustc OOM or hanging `repaint_storm::live_cat_through_a_pty`, record it as an environment note; do not delete or skip the test.

- [ ] **Step 2: Capture fresh modal screenshots**

For each of Add Workspace and Shell Picker, capture:
- 1280x800 dark and light, modal open.
- 480x800 dark and light, modal open.

Use the repo's screenshot naming convention and verify each PNG opens with the expected dimensions. Confirm visually: one close control, captioned sections, disabled rows legible, no clipped content, focus/selected states visible.

- [ ] **Step 3: Visual review**

Dispatch two read-only QA reviewers with the fresh captures. Fix any product defect, re-capture only the touched states, and re-review. Record taste findings without blocking.

- [ ] **Step 4: Commit evidence**

```bash
$env:GIT_MASTER='1'; git add docs/visual/screenshots/
$env:GIT_MASTER='1'; git -c trailer.ifexists=addIfDifferent commit -m "docs(visual): record modal revamp evidence" --trailer "Co-authored-by: sisyphus-dev-ai <sisyphus-dev-ai@users.noreply.github.com>"
```
If no new screenshots were added, skip this commit.

## Self-review checklist

- [ ] Spec scope: all five surfaces covered (Tasks 1-4 plus evidence in Task 5); Settings intentionally unchanged.
- [ ] Spec architecture: shared helper, no Settings component import, state stays in view entities.
- [ ] Spec visual language: header/close/sections/tokens/disabled states covered in Tasks 1-3.
- [ ] Spec behavior: one close control, Escape, opener restoration, no scrim close, all routes/events preserved.
- [ ] Placeholder scan: no TBD/TODO/implement-later remains in this plan.
- [ ] Type consistency: `ModalShell::new`, `open_with_focus`, `close()`, and element IDs use the same names across tasks.
