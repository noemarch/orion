//! Changes view (P3): repo diff file list + one file's patch.
//!
//! `GET /api/workspace/diff/files?pane_id=…&scope=…` lists changed files with
//! status/additions/deletions; selecting one fetches
//! `GET /api/workspace/diff/file?pane_id=…&path=…&scope=…` and renders the
//! patch as colored unified lines (additions green, deletions red, hunk
//! headers blue), classified with `similar`'s [`similar::ChangeTag`]
//! semantics over the unified-diff prefixes.
//!
//! Scope semantics mirror `repoDiffQuery` in `src/lib/api.ts`: `workspace`
//! (working tree vs index + untracked), `staged` (index vs HEAD), or
//! `commit` (base..head — base/head only passed when scope is `commit`).
//! The base/head inputs show only in commit scope; they are plain text
//! fields edited by typing while focused (no search, no validation — the
//! route answers with an empty list for unknown refs).
//!
//! Polling: 2s timer like `ChatView` (the engine already owns `notify`
//! watchers; GPUI views poll rather than adding a second watcher to
//! `orion-ui`), and only while visible (the Changes lens is active).

use crate::files_entities::{
    url_encode, RepoDiffFile, RepoDiffFileList, RepoDiffFilePatch, RepoDiffScope, RepoDiffStatus,
};
use crate::theme::Theme;
use gpui::prelude::*;
use gpui::{
    div, px, white, Context, FontWeight, HighlightStyle, KeyDownEvent, Render, ScrollHandle,
    StyledText, TextStyle, Window,
};
use orion_core::engine::pane::Pane;
use orion_core::Engine;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditTarget {
    None,
    Base,
    Head,
}

/// One patch line, classified the way `similar` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchKind {
    Add,
    Delete,
    Hunk,
    Header,
    Context,
}

/// Classify one unified-diff line by its first character (the same
/// Insert/Delete vocabulary `similar::ChangeTag` uses for diffs).
pub fn classify_patch_line(line: &str) -> PatchKind {
    if line.starts_with("@@") {
        PatchKind::Hunk
    } else if line.starts_with("diff --git")
        || line.starts_with("--- ")
        || line.starts_with("+++ ")
        || line.starts_with("index ")
    {
        PatchKind::Header
    } else if line.starts_with('+') {
        PatchKind::Add
    } else if line.starts_with('-') {
        PatchKind::Delete
    } else {
        PatchKind::Context
    }
}

/// Color for a patch line kind.
fn patch_color(kind: PatchKind) -> gpui::Hsla {
    let theme = Theme::current();
    match kind {
        PatchKind::Add => theme.status_done.into(),
        PatchKind::Delete => theme.status_blocked.into(),
        PatchKind::Hunk => theme.status_working.into(),
        PatchKind::Header => theme.text_dim.into(),
        PatchKind::Context => theme.text.into(),
    }
}

fn patch_highlight(kind: PatchKind) -> HighlightStyle {
    match kind {
        PatchKind::Header => HighlightStyle {
            font_weight: Some(FontWeight::BOLD),
            color: Some(patch_color(kind)),
            ..Default::default()
        },
        _ => HighlightStyle {
            font_style: None,
            color: Some(patch_color(kind)),
            ..Default::default()
        },
    }
}

pub struct ChangesView {
    engine: Option<Arc<Engine>>,
    pane_id: String,
    scope: RepoDiffScope,
    base: String,
    head: String,
    editing: EditTarget,
    list: Option<RepoDiffFileList>,
    selected_path: Option<String>,
    selected_old_path: Option<String>,
    patch: Option<RepoDiffFilePatch>,
    error: Option<String>,
    visible: bool,
    focus_handle: gpui::FocusHandle,
    list_scroll: ScrollHandle,
    patch_scroll: ScrollHandle,
    row_seq: usize,
    last_list_body: Option<String>,
    last_patch_body: Option<String>,
}

impl ChangesView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let view = Self {
            engine: None,
            pane_id: "pane-1".to_string(),
            scope: RepoDiffScope::Workspace,
            base: String::new(),
            head: String::new(),
            editing: EditTarget::None,
            list: None,
            selected_path: None,
            selected_old_path: None,
            patch: None,
            error: None,
            visible: false,
            focus_handle: cx.focus_handle(),
            list_scroll: ScrollHandle::new(),
            patch_scroll: ScrollHandle::new(),
            row_seq: 0,
            last_list_body: None,
            last_patch_body: None,
        };
        view.start_poll(cx);
        view
    }

    fn next_row_id(&mut self) -> usize {
        let id = self.row_seq;
        self.row_seq += 1;
        id
    }

    /// Wire the engine + pane once they exist (called from `app.rs` after spawn).
    pub fn attach(&mut self, engine: Arc<Engine>, pane: Arc<Pane>, pane_id: String) {
        self.engine = Some(engine);
        self.pane_id = pane_id;
        let _ = pane;
        self.refresh();
        self.refresh_patch();
    }

    /// Whether the 2s poll runs (the Changes lens is active).
    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
        if visible {
            self.refresh();
            self.refresh_patch();
        }
    }

    pub fn visible(&self) -> bool {
        self.visible
    }

    fn query(&self) -> String {
        let mut query = format!(
            "pane_id={}&scope={}",
            url_encode(&self.pane_id),
            self.scope.as_str(),
        );
        if self.scope == RepoDiffScope::Commit {
            if !self.base.trim().is_empty() {
                query.push_str(&format!("&base={}", url_encode(self.base.trim())));
            }
            if !self.head.trim().is_empty() {
                query.push_str(&format!("&head={}", url_encode(self.head.trim())));
            }
        }
        query
    }

    /// Fetch the file list through the wire seam (`Engine::http`).
    pub fn refresh(&mut self) {
        let Some(engine) = self.engine.clone() else {
            return;
        };
        let path = format!("/api/workspace/diff/files?{}", self.query());
        let (status, body) = engine.http("GET", &path, &Value::Null);
        if status != 200 {
            self.error = Some(format!("diff list fetch failed ({status})"));
            return;
        }
        let encoded = body.to_string();
        if self.last_list_body.as_deref() == Some(encoded.as_str()) {
            return;
        }
        self.last_list_body = Some(encoded);
        match RepoDiffFileList::parse(&body) {
            Ok(list) => {
                self.list = Some(list);
                self.error = None;
            }
            Err(error) => self.error = Some(format!("diff list parse failed: {error}")),
        }
    }

    fn set_scope(&mut self, scope: RepoDiffScope) {
        self.scope = scope;
        self.last_list_body = None;
        self.last_patch_body = None;
        self.refresh();
        self.refresh_patch();
    }

    /// Fetch the selected file's patch through the wire seam.
    pub fn refresh_patch(&mut self) {
        let Some(engine) = self.engine.clone() else {
            return;
        };
        let Some(path) = self.selected_path.clone() else {
            return;
        };
        let mut query = format!("{}&path={}", self.query(), url_encode(&path));
        if let Some(old_path) = &self.selected_old_path {
            query.push_str(&format!("&old_path={}", url_encode(old_path)));
        }
        let full = format!("/api/workspace/diff/file?{query}");
        let (status, body) = engine.http("GET", &full, &Value::Null);
        if status != 200 {
            self.error = Some(format!("diff patch fetch failed ({status})"));
            return;
        }
        let encoded = body.to_string();
        if self.last_patch_body.as_deref() == Some(encoded.as_str()) {
            return;
        }
        self.last_patch_body = Some(encoded);
        match RepoDiffFilePatch::parse(&body) {
            Ok(patch) => {
                self.patch = Some(patch);
                self.error = None;
            }
            Err(error) => self.error = Some(format!("diff patch parse failed: {error}")),
        }
    }

    fn select_file(&mut self, file: &RepoDiffFile) {
        self.selected_path = Some(file.path.clone());
        self.selected_old_path = file.old_path.clone();
        self.patch = None;
        self.last_patch_body = None;
        self.refresh_patch();
    }

    fn start_poll(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |view, cx| loop {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            let _ = view.update(cx, |view, _cx| {
                if view.visible {
                    view.refresh();
                    view.refresh_patch();
                }
            });
            if view.upgrade().is_none() {
                break;
            }
        })
        .detach();
    }

    fn scope_button(
        &mut self,
        label: &'static str,
        scope: RepoDiffScope,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.scope == scope;
        div()
            .id(self.next_row_id())
            .px_3()
            .py_1()
            .rounded_md()
            .bg(if active {
                Theme::current().accent
            } else {
                Theme::current().surface_hover
            })
            .text_size(px(12.0 * Theme::current().density_scale()))
            .text_color(white())
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.set_scope(scope);
                cx.notify();
            }))
    }

    fn render_scope_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut bar = div()
            .id(self.next_row_id())
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .px_3()
            .py_1()
            .child(self.scope_button("Workspace", RepoDiffScope::Workspace, cx))
            .child(self.scope_button("Staged", RepoDiffScope::Staged, cx))
            .child(self.scope_button("Commit", RepoDiffScope::Commit, cx));
        if self.scope == RepoDiffScope::Commit {
            bar = bar
                .child(self.render_field("base", EditTarget::Base, &self.base.clone(), cx))
                .child(self.render_field("head", EditTarget::Head, &self.head.clone(), cx));
        }
        bar
    }

    fn render_field(
        &mut self,
        label: &'static str,
        target: EditTarget,
        value: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let at_target = self.editing == target;
        let shown = if value.is_empty() {
            format!("{label}: (empty)")
        } else {
            format!("{label}: {value}")
        };
        div()
            .id(self.next_row_id())
            .px_2()
            .py_1()
            .rounded_md()
            .bg(if at_target {
                Theme::current().surface_elevated
            } else {
                Theme::current().surface_input
            })
            .text_size(px(11.0 * Theme::current().density_scale()))
            .text_color(Theme::current().text)
            .child(shown)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.editing = if this.editing == target {
                    EditTarget::None
                } else {
                    target
                };
                cx.notify();
            }))
    }

    fn handle_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        if self.editing == EditTarget::None {
            return;
        }
        let target = self.editing;
        let field = if target == EditTarget::Base {
            &mut self.base
        } else {
            &mut self.head
        };
        let key = event.keystroke.key.as_str();
        if key == "backspace" {
            field.pop();
            self.last_list_body = None;
            self.last_patch_body = None;
            self.refresh();
            self.refresh_patch();
        } else if key == "escape" {
            self.editing = EditTarget::None;
        } else if key.len() == 1 {
            let ch = key.chars().next().unwrap();
            if !ch.is_control() {
                field.push(ch);
                self.last_list_body = None;
                self.last_patch_body = None;
                self.refresh();
                self.refresh_patch();
            }
        }
        cx.notify();
    }
}

impl Render for ChangesView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.row_seq = 0;
        let theme = Theme::current();
        let mut root = div()
            .id("changes-view")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.surface_base)
            .text_color(theme.text)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                this.handle_key(event, cx);
            }));

        root = root.child(self.render_scope_bar(cx));

        if let Some(repo) = self.list.clone().map(|list| list.repository) {
            let branch = repo
                .branch
                .clone()
                .unwrap_or_else(|| "(detached)".to_string());
            root = root.child(
                div()
                    .id(self.next_row_id())
                    .px_3()
                    .py_1()
                    .text_size(px(11.0 * Theme::current().density_scale()))
                    .text_color(Theme::current().text_dim)
                    .child(format!(
                        "{} · {} · head {}",
                        repo.root,
                        branch,
                        repo.head.clone().unwrap_or_else(|| "(none)".to_string())
                    )),
            );
        }

        if let Some(error) = self.error.clone() {
            root = root.child(
                div()
                    .id(self.next_row_id())
                    .px_3()
                    .py_1()
                    .text_size(px(12.0 * Theme::current().density_scale()))
                    .text_color(Theme::current().danger_text)
                    .child(error.clone()),
            );
        }

        let mut panes = div()
            .id(self.next_row_id())
            .flex()
            .flex_row()
            .flex_1()
            .min_h(px(0.0));

        // File list (left).
        let mut list_column = div().id(self.next_row_id()).flex().flex_col().w(px(360.0));
        let header = match &self.list {
            Some(list) => {
                let total = list.total.unwrap_or(list.files.len() as u64);
                format!(
                    "{} file(s){}",
                    total,
                    if list.truncated { " (truncated)" } else { "" }
                )
            }
            None => "Loading…".to_string(),
        };
        list_column = list_column.child(
            div()
                .id(self.next_row_id())
                .px_3()
                .py_1()
                .text_size(px(11.0 * Theme::current().density_scale()))
                .text_color(Theme::current().text_dim)
                .child(header),
        );
        let mut file_rows = div().id(self.next_row_id()).flex().flex_col();
        if let Some(list) = self.list.clone() {
            for file in &list.files {
                let selected = self.selected_path.as_deref() == Some(file.path.as_str());
                let mark = file.status.mark();
                let mark_color = match file.status {
                    RepoDiffStatus::Added | RepoDiffStatus::Untracked => {
                        Theme::current().status_done
                    }
                    RepoDiffStatus::Deleted => Theme::current().status_blocked,
                    _ => Theme::current().notice,
                };
                let counts = match (file.additions, file.deletions) {
                    (Some(a), Some(d)) => format!("+{a} -{d}"),
                    _ => {
                        if file.binary == Some(true) {
                            "binary".to_string()
                        } else {
                            "—".to_string()
                        }
                    }
                };
                let path = file.path.clone();
                let label = if let Some(old) = &file.old_path {
                    format!("{old} → {path}")
                } else {
                    path.clone()
                };
                let file_clone = file.clone();
                file_rows = file_rows.child(
                    div()
                        .id(self.next_row_id())
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(if selected {
                            Theme::current().surface_elevated
                        } else {
                            Theme::current().surface_panel
                        })
                        .text_size(px(12.0 * Theme::current().density_scale()))
                        .child(
                            div()
                                .id(self.next_row_id())
                                .text_color(mark_color)
                                .child(mark.to_string()),
                        )
                        .child(
                            div()
                                .id(self.next_row_id())
                                .text_color(Theme::current().text)
                                .child(label),
                        )
                        .child(
                            div()
                                .id(self.next_row_id())
                                .text_color(Theme::current().text_dim)
                                .child(counts),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.select_file(&file_clone);
                            cx.notify();
                        })),
                );
            }
            if list.files.is_empty() {
                file_rows = file_rows.child(
                    div()
                        .id(self.next_row_id())
                        .px_2()
                        .py_1()
                        .text_size(px(12.0 * Theme::current().density_scale()))
                        .text_color(Theme::current().text_dim)
                        .child("Nothing changed (before the first commit, staged lists everything added so far)."),
                );
            }
        }
        list_column = list_column.child(
            div()
                .id("changes-list-scroll")
                .flex_1()
                .overflow_y_scroll()
                .track_scroll(&self.list_scroll)
                .child(file_rows),
        );
        panes = panes.child(list_column);

        // Patch (right).
        let mut patch_column = div().id(self.next_row_id()).flex().flex_col().flex_1();
        match self.patch.clone() {
            Some(patch) => {
                let title = format!("{} ({})", patch.file.path, patch.file.status.as_str());
                patch_column = patch_column.child(
                    div()
                        .id(self.next_row_id())
                        .px_3()
                        .py_1()
                        .text_size(px(12.0 * Theme::current().density_scale()))
                        .text_color(Theme::current().text)
                        .child(title),
                );
                if let Some(text) = patch.patch_text() {
                    // similar-driven: classify with ChangeTag semantics over
                    // the unified-diff line prefixes.
                    let diff = similar::TextDiff::from_lines(text, text);
                    let mut patch_lines = div().id(self.next_row_id()).flex().flex_col().px_3();
                    for change in diff.iter_all_changes() {
                        let line = change.to_string();
                        let line = line.trim_end_matches('\n');
                        let kind = classify_patch_line(line);
                        patch_lines = patch_lines.child(
                            div().id(self.next_row_id()).flex().flex_row().child(
                                StyledText::new(if line.is_empty() {
                                    " ".to_string()
                                } else {
                                    line.to_string()
                                })
                                .with_default_highlights(
                                    &TextStyle {
                                        color: patch_color(kind),
                                        font_size: px(12.0).into(),
                                        ..Default::default()
                                    },
                                    [(0..line.len(), patch_highlight(kind))],
                                ),
                            ),
                        );
                    }
                    patch_column = patch_column.child(
                        div()
                            .id("changes-patch-scroll")
                            .flex_1()
                            .overflow_y_scroll()
                            .track_scroll(&self.patch_scroll)
                            .child(patch_lines),
                    );
                } else {
                    let reason = patch
                        .unavailable
                        .clone()
                        .unwrap_or_else(|| "no patch".to_string());
                    patch_column = patch_column.child(
                        div()
                            .id(self.next_row_id())
                            .px_3()
                            .py_2()
                            .text_size(px(12.0 * Theme::current().density_scale()))
                            .text_color(Theme::current().text_dim)
                            .child(format!("No patch ({reason}).")),
                    );
                }
            }
            None => {
                patch_column = patch_column.child(
                    div()
                        .id(self.next_row_id())
                        .px_3()
                        .py_2()
                        .text_size(px(12.0 * Theme::current().density_scale()))
                        .text_color(Theme::current().text_dim)
                        .child("Select a file to see its patch."),
                );
            }
        }
        panes = panes.child(patch_column);

        root.child(panes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_unified_lines() {
        assert_eq!(classify_patch_line("+new"), PatchKind::Add);
        assert_eq!(classify_patch_line("-old"), PatchKind::Delete);
        assert_eq!(classify_patch_line("@@ -1 +1 @@"), PatchKind::Hunk);
        assert_eq!(classify_patch_line("diff --git a/x b/x"), PatchKind::Header);
        assert_eq!(classify_patch_line(" context"), PatchKind::Context);
    }

    #[test]
    fn patch_colors_differ_by_kind() {
        assert_ne!(patch_color(PatchKind::Add), patch_color(PatchKind::Delete));
        assert_ne!(
            patch_color(PatchKind::Hunk),
            patch_color(PatchKind::Context)
        );
    }

    #[test]
    fn highlight_uses_field_names() {
        // Compiles only with HighlightStyle field names (color/font_weight/…).
        let header = patch_highlight(PatchKind::Header);
        assert!(header.font_weight.is_some());
        assert!(header.color.is_some());
        let add = patch_highlight(PatchKind::Add);
        assert!(add.color.is_some());
    }

    #[test]
    fn commit_query_passes_base_head_only_in_commit_scope() {
        let mut view_scope = RepoDiffScope::Workspace;
        assert_eq!(view_scope.as_str(), "workspace");
        view_scope = RepoDiffScope::Commit;
        assert_eq!(view_scope.as_str(), "commit");
    }
}
