//! New Workspace flow: the folder browser and the agent picker.
//!
//! The modal posts `POST /api/workspace/create` with `{cwd, label?, agent:
//! {kind}}`; the Engine starts the agent, records the Workspace/Tab/Pane, and
//! returns the new ids. Folder browsing uses
//! `GET /api/workspace/directories?path=…` and the agent picker uses
//! `GET /api/agents` (`{agents: [{kind, label, installed}]}`).

use serde_json::Value;

use crate::theme::Theme;
use gpui_kit::base::component_traits::Disableable;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::radio::{Radio, RadioGroup};
use gpui_kit::*;
use gpui_kit::{div, px, Context, FocusHandle, Render, Styled, Window};
use orion_core::runtime::Runtime;
use serde_json::json;
use std::sync::Arc;

/// One agent the New Workspace picker can start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentChoice {
    pub kind: String,
    pub label: String,
    pub installed: bool,
}

impl AgentChoice {
    /// A short written hint for the picker row.
    pub fn hint(&self) -> &'static str {
        if self.installed {
            "Installed"
        } else {
            "Not on PATH"
        }
    }
}

/// Projects `GET /api/agents` into picker rows, in the route's order.
pub fn agent_choices(agents_body: &Value) -> Vec<AgentChoice> {
    let Some(agents) = agents_body.get("agents").and_then(Value::as_array) else {
        return Vec::new();
    };
    agents
        .iter()
        .filter_map(|agent| {
            let kind = agent.get("kind").and_then(Value::as_str)?.to_string();
            let label = agent
                .get("label")
                .and_then(Value::as_str)
                .filter(|label| !label.trim().is_empty())
                .unwrap_or(&kind)
                .to_string();
            let installed = agent
                .get("installed")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Some(AgentChoice {
                kind,
                label,
                installed,
            })
        })
        .collect()
}

/// Where the folder browser starts: the explicit recent directory, else the
/// focused workspace's cwd, else any open workspace's cwd, else home.
///
/// Blank entries are ignored so an empty saved setting does not strand the
/// picker at a path it cannot read.
pub fn recent_directory(
    explicit: Option<&str>,
    focused_workspace_cwd: Option<&str>,
    any_workspace_cwd: Option<&str>,
    home: &str,
) -> String {
    [
        explicit,
        focused_workspace_cwd,
        any_workspace_cwd,
        Some(home),
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .find(|path| !path.is_empty())
    .unwrap_or_default()
    .to_string()
}

/// What the picker reports after `POST /api/workspace/create`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewWorkspaceEvent {
    Created(String),
}

impl gpui_kit::EventEmitter<NewWorkspaceEvent> for NewWorkspaceModal {}

/// The folder-browser + agent-picker modal behind `POST /api/workspace/create`.
///
/// The app opens it with [`NewWorkspaceModal::open_at`] and listens for
/// [`NewWorkspaceEvent::Created`] to focus the returned pane. The modal never
/// touches the sidebar directly; the Engine snapshot is the only source of
/// truth.
pub struct NewWorkspaceModal {
    engine: Option<Arc<dyn Runtime>>,
    open: bool,
    cwd: String,
    parent: Option<String>,
    directories: Vec<String>,
    agents: Vec<AgentChoice>,
    selected_agent: Option<String>,
    error: Option<String>,
    focus_handle: FocusHandle,
    row_seq: usize,
}

impl NewWorkspaceModal {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            engine: None,
            open: false,
            cwd: String::new(),
            parent: None,
            directories: Vec::new(),
            agents: Vec::new(),
            selected_agent: None,
            error: None,
            focus_handle: cx.focus_handle(),
            row_seq: 0,
        }
    }

    pub fn attach(&mut self, engine: Arc<dyn Runtime>) {
        self.engine = Some(engine);
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open the picker at `start` (or home when blank) and load agents.
    pub fn open_at(&mut self, start: Option<String>) {
        self.open = true;
        self.error = None;
        // A stale listing from the previous open must not mask a missing recent
        // folder: clear it so `load` decides whether `cwd` is usable.
        self.cwd.clear();
        self.parent = None;
        self.directories.clear();
        self.load_agents();
        let start = start
            .filter(|path| !path.trim().is_empty())
            .unwrap_or_else(|| "~".to_string());
        self.load(&start);
        // A moved or deleted recent folder should not strand the picker.
        if self.cwd.is_empty() {
            self.load("~");
        }
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    fn next_row_id(&mut self) -> usize {
        let id = self.row_seq;
        self.row_seq += 1;
        id
    }

    fn load(&mut self, path: &str) {
        let Some(engine) = self.engine.clone() else {
            return;
        };
        let route = format!(
            "/api/workspace/directories?path={}",
            crate::files_entities::url_encode(path)
        );
        let (status, body) = engine.http("GET", &route, &Value::Null);
        if status != 200 {
            self.error = Some(error_message(&body, "That folder could not be read."));
            return;
        }
        match crate::files_entities::DirectoryListing::parse(&body) {
            Ok(listing) => {
                self.cwd = listing.path;
                self.parent = listing.parent;
                self.directories = listing.directories;
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }

    fn load_agents(&mut self) {
        let Some(engine) = self.engine.clone() else {
            return;
        };
        let (status, body) = engine.http("GET", "/api/agents", &Value::Null);
        if status != 200 {
            return;
        }
        self.agents = agent_choices(&body);
        if self.selected_agent.as_ref().is_none_or(|kind| {
            !self
                .agents
                .iter()
                .any(|agent| &agent.kind == kind && agent.installed)
        }) {
            self.selected_agent = self
                .agents
                .iter()
                .find(|agent| agent.installed)
                .map(|agent| agent.kind.clone());
        }
    }

    fn navigate(&mut self, name: &str) {
        let next = crate::files_entities::join_path(&self.cwd, name);
        self.load(&next);
    }

    fn go_up(&mut self) {
        if let Some(parent) = self.parent.clone() {
            self.load(&parent);
        }
    }

    fn go_home(&mut self) {
        self.load("~");
    }

    fn create(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.engine.clone() else {
            return;
        };
        let Some(kind) = self.selected_agent.clone() else {
            self.error = Some("Choose an installed agent first.".to_string());
            cx.notify();
            return;
        };
        if self.cwd.trim().is_empty() {
            self.error = Some("Choose a folder first.".to_string());
            cx.notify();
            return;
        }
        let body = json!({ "cwd": self.cwd, "agent": { "kind": kind } });
        orion_core::trace::begin();
        orion_core::trace::record_with(
            orion_core::trace::Event::WorkspaceOpenClicked,
            Some(format!("{kind} in {}", self.cwd)),
        );
        let (status, response) = engine.http("POST", "/api/workspace/create", &body);
        if status != 200 {
            self.error = Some(error_message(
                &response,
                "The workspace could not be opened.",
            ));
            cx.notify();
            return;
        }
        let _ = engine.http(
            "PUT",
            "/api/preferences",
            &json!({ "settings": { "recentDirectory": self.cwd } }),
        );
        let pane_id = response
            .get("pane_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        self.open = false;
        if let Some(pane_id) = pane_id {
            cx.emit(NewWorkspaceEvent::Created(pane_id));
        }
        cx.notify();
    }
}

/// The Engine's plain-language message from an error body, or `fallback`.
pub(crate) fn error_message(body: &Value, fallback: &str) -> String {
    body.get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .unwrap_or(fallback)
        .to_string()
}

impl Render for NewWorkspaceModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The modal owns the keyboard while it is open, so Escape closes it
        // regardless of which pane had focus.
        if !self.focus_handle.is_focused(window) {
            self.focus_handle.focus(window, cx);
        }
        let theme = Theme::current();
        let scale = theme.density_scale();
        let _row_h = theme.row_height() * scale;
        let can_create = self.selected_agent.is_some() && !self.cwd.trim().is_empty();
        let cwd = self.cwd.clone();
        let parent = self.parent.clone();
        let error = self.error.clone();
        let directories = self.directories.clone();
        let agents = self.agents.clone();
        let selected = self.selected_agent.clone();

        // A folder is a button: the row opens the folder, and the kit's
        // text button carries the chevron and the name in one label.
        let directory_rows = directories
            .iter()
            .map(|name| {
                let name_for_click = name.clone();
                let row_id = self.next_row_id();
                Button::new(format!("new-workspace-dir-{row_id}"))
                    .label(format!("▸ {name}"))
                    .text()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.navigate(&name_for_click);
                        cx.notify();
                    }))
            })
            .collect::<Vec<_>>();

        // The agent list is one choice out of many: the kit's radio group
        // owns the selection, and an agent that is not installed stays
        // unselectable — the handler ignores the change, so the group's
        // selected index keeps pointing at the agent that can be used.
        let selected_index = agents
            .iter()
            .position(|agent| selected.as_deref() == Some(agent.kind.as_str()));
        let agent_choices = agents.iter().map(|agent| {
            let kind = agent.kind.clone();
            let label = if agent.installed {
                agent.label.clone()
            } else {
                format!("{} — {}", agent.label, agent.hint())
            };
            Radio::new(format!("new-workspace-agent-{kind}"))
                .label(label)
                .checked(selected.as_deref() == Some(kind.as_str()))
                .disabled(!agent.installed)
        });

        div()
            .id("new-workspace-modal")
            .flex()
            .items_center()
            .justify_center()
            .size_full()
            .bg(theme.scrim)
            .child(
                div()
                    .id("new-workspace-panel")
                    .flex()
                    .flex_col()
                    .gap(px(10.0 * scale))
                    .w(px(520.0))
                    .max_h(px(560.0))
                    .p_4()
                    .rounded_lg()
                    .bg(theme.surface_panel)
                    .border_1()
                    .border_color(theme.border)
                    .track_focus(&self.focus_handle)
                    .on_key_down(cx.listener(|this, ev: &gpui_kit::KeyDownEvent, _, cx| {
                        if ev.keystroke.key == "escape" {
                            this.close();
                            cx.notify();
                        }
                    }))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(px(14.0 * scale))
                                    .text_color(theme.text_strong)
                                    .child("New Workspace"),
                            )
                            .child(
                                Button::new("new-workspace-close")
                                    .label("✕")
                                    .tooltip("Close")
                                    .min_h(px(44.0))
                                    .min_w(px(44.0))
                                    .text()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.close();
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.0 * scale))
                            .text_size(px(11.0 * scale))
                            .text_color(theme.text_dim)
                            .child("FOLDER")
                            .child(
                                div()
                                    .flex_1()
                                    .flex()
                                    .flex_row()
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
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(11.0 * scale))
                            .text_color(theme.text_dim)
                            .child(cwd.clone()),
                    )
                    .child(
                        div()
                            .id("new-workspace-directories")
                            .flex()
                            .flex_none()
                            .flex_col()
                            .gap_1()
                            .h(px(180.0))
                            .overflow_y_scroll()
                            .children(directory_rows),
                    )
                    .child(
                        div()
                            .text_size(px(11.0 * scale))
                            .text_color(theme.text_dim)
                            .child("AGENT"),
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
                    )
                    .child(if let Some(error) = error {
                        Alert::error("new-workspace-error", error).into_any_element()
                    } else {
                        div().into_any_element()
                    })
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .justify_end()
                            .gap(px(8.0 * scale))
                            .child(
                                Button::new("new-workspace-cancel")
                                    .label("Cancel")
                                    .secondary()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.close();
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("new-workspace-create")
                                    .label("Open Workspace")
                                    .primary()
                                    .disabled(!can_create)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.create(cx);
                                    })),
                            ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orion_core::Engine;
    use serde_json::json;

    #[::core::prelude::v1::test]
    fn agent_choices_read_the_agents_route() {
        let body = json!({
            "agents": [
                { "kind": "claude", "label": "Claude Code", "installed": true },
                { "kind": "codex", "label": "Codex", "installed": false },
            ],
        });
        let choices = agent_choices(&body);
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0].kind, "claude");
        assert_eq!(choices[0].label, "Claude Code");
        assert!(choices[0].installed);
        assert_eq!(choices[0].hint(), "Installed");
        assert!(!choices[1].installed);
        assert_eq!(choices[1].hint(), "Not on PATH");
    }

    #[::core::prelude::v1::test]
    fn agent_choices_ignore_malformed_rows() {
        let body = json!({
            "agents": [
                { "label": "No kind", "installed": true },
                { "kind": "codex" },
            ],
        });
        let choices = agent_choices(&body);
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0].label, "codex");
        assert!(!choices[0].installed);
    }

    #[::core::prelude::v1::test]
    fn agent_choices_are_empty_without_the_key() {
        assert!(agent_choices(&json!({})).is_empty());
    }

    #[::core::prelude::v1::test]
    fn recent_directory_prefers_the_explicit_value() {
        let start = recent_directory(
            Some("C:/work/explicit"),
            Some("C:/work/focused"),
            Some("C:/work/any"),
            "C:/home",
        );
        assert_eq!(start, "C:/work/explicit");
    }

    #[::core::prelude::v1::test]
    fn recent_directory_prefers_the_focused_workspace() {
        let start = recent_directory(
            None,
            Some("C:/work/focused"),
            Some("C:/work/any"),
            "C:/home",
        );
        assert_eq!(start, "C:/work/focused");
    }

    #[::core::prelude::v1::test]
    fn recent_directory_falls_back_to_any_workspace_then_home() {
        assert_eq!(
            recent_directory(None, None, Some("C:/work/any"), "C:/home"),
            "C:/work/any"
        );
        assert_eq!(recent_directory(None, None, None, "C:/home"), "C:/home");
    }

    #[::core::prelude::v1::test]
    fn recent_directory_ignores_blank_entries() {
        assert_eq!(
            recent_directory(Some("   "), Some(""), Some("C:/work/any"), "C:/home"),
            "C:/work/any"
        );
    }

    crate::orion_test!(
        fn modal_draws_the_folder_browser_at_the_recent_directory(
            cx: &mut gpui_kit::TestAppContext,
        ) {
            use gpui_kit::{point, size};
            let unique = format!(
                "orion-modal-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|elapsed| elapsed.as_nanos())
                    .unwrap_or_default()
            );
            let dir = std::env::temp_dir().join(unique);
            std::fs::create_dir_all(dir.join("project")).expect("project directory");
            let engine = Arc::new(Engine::with_store(orion_core::engine::store::Store::at(
                dir.join("state"),
            )));
            let start = dir.to_string_lossy().to_string();
            let cx = cx.add_empty_window();
            cx.draw(point(px(0.), px(0.)), size(px(640.), px(560.)), |_, cx| {
                let modal = cx.new(NewWorkspaceModal::new);
                modal.update(cx, |view, _cx| {
                    view.attach(engine);
                    view.open_at(Some(start));
                });
                modal.into_any_element()
            });
        }
    );

    crate::orion_test!(
        fn reopening_the_picker_recovers_from_a_missing_recent_folder(
            cx: &mut gpui_kit::TestAppContext,
        ) {
            use gpui_kit::{point, size};
            let unique = format!(
                "orion-modal-recover-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|elapsed| elapsed.as_nanos())
                    .unwrap_or_default()
            );
            let dir = std::env::temp_dir().join(unique);
            std::fs::create_dir_all(dir.join("project")).expect("project directory");
            let engine = Arc::new(Engine::with_store(orion_core::engine::store::Store::at(
                dir.join("state"),
            )));
            let valid = dir.to_string_lossy().to_string();
            let missing = dir.join("gone").to_string_lossy().to_string();
            let cx = cx.add_empty_window();
            cx.draw(
                point(px(0.), px(0.)),
                size(px(640.), px(560.)),
                move |_, cx| {
                    let modal = cx.new(NewWorkspaceModal::new);
                    modal.update(cx, |view, _cx| {
                        view.attach(engine);
                        view.open_at(Some(valid.clone()));
                        let first = view.cwd.clone();
                        assert!(!first.is_empty(), "the first folder loads");
                        assert!(view.error.is_none());

                        view.open_at(Some(missing.clone()));
                        assert!(
                            view.error.is_none(),
                            "a deleted recent folder falls back home: {:?}",
                            view.error
                        );
                        assert!(!view.cwd.is_empty(), "the picker still points somewhere");
                        assert_ne!(view.cwd, first, "it does not keep the stale listing");
                    });
                    modal.into_any_element()
                },
            );
        }
    );
}
