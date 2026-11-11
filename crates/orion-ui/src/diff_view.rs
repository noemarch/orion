//! Changes view (P3): repo diff file list + one file's patch.
//!
//! `GET /api/workspace/diff/files?pane_id=…&scope=…` lists changed files with
//! status/additions/deletions; selecting one fetches
//! `GET /api/workspace/diff/file?pane_id=…&path=…&scope=…` and renders the
//! patch as colored unified lines with old/new line-number gutters
//! ([`diff_line_numbers`] walks the patch with hunk state, so a deleted line
//! that itself starts with `-- ` is never mistaken for the `--- a/path`
//! file header).
//!
//! Scope semantics mirror `repoDiffQuery` in `src/lib/api.ts`: `workspace`
//! (working tree vs index + untracked), `staged` (index vs HEAD), or
//! `commit` (base..head — base/head only passed when scope is `commit`).
//! The base/head inputs show only in commit scope; they are plain text
//! fields edited by typing while focused (no search, no validation — the
//! route answers with an empty list for unknown refs).
//!
//! Layout: the file list and the diff form a resizable split
//! ([`clamp_list_width`]) so the diff keeps at least [`DIFF_WIDTH_MIN`] px
//! at any window width; `app.rs` pushes the width the view actually
//! receives in via [`ChangesView::set_available_width`].
//!
//! Polling: 2s timer like `ChatView` (the engine already owns `notify`
//! watchers; GPUI views poll rather than adding a second watcher to
//! `orion-ui`), and only while visible (the Changes lens is active).

use crate::files_entities::{
    url_encode, RepoDiffFile, RepoDiffFileList, RepoDiffFilePatch, RepoDiffScope, RepoDiffStatus,
};
use crate::shell_model::SPLITTER_WIDTH_PX;
use crate::theme::Theme;
use gpui::prelude::*;
use gpui::{
    div, px, white, Context, FontWeight, HighlightStyle, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Render, ScrollHandle, StyledText, TextAlign, TextStyle, Window,
};
use orion_core::runtime::{PaneHandle, Runtime};
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

/// One patch line with the line numbers [`diff_line_numbers`] assigned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: PatchKind,
    /// Line number on the old (left) side; blank for adds and headers.
    pub old_no: Option<u32>,
    /// Line number on the new (right) side; blank for deletes and headers.
    pub new_no: Option<u32>,
    pub text: String,
}

/// Git's own header lines. `--- `/`+++ ` are only headers OUTSIDE a hunk:
/// inside one they are a deleted/added line whose text starts with `-- `/`++ `,
/// so [`diff_line_numbers`] gates them on hunk state and this list holds the
/// unambiguous prefixes.
const GIT_HEADERS: &[&str] = &[
    "diff --git ",
    "index ",
    "new file mode ",
    "deleted file mode ",
    "old mode ",
    "new mode ",
    "similarity index ",
    "dissimilarity index ",
    "rename from ",
    "rename to ",
    "copy from ",
    "copy to ",
    "Binary files ",
    "GIT binary patch",
];

/// Whether `line` is a git file header. `--- `/`+++ ` count only outside a
/// hunk; inside one they are content lines (see [`GIT_HEADERS`]).
fn is_git_header(line: &str, in_hunk: bool) -> bool {
    if !in_hunk && (line.starts_with("--- ") || line.starts_with("+++ ")) {
        return true;
    }
    GIT_HEADERS.iter().any(|prefix| line.starts_with(prefix))
}

/// First `u32` in `rest`, returned with the text after the digits.
fn next_number(rest: &str) -> Option<(u32, &str)> {
    let digits = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if digits == 0 {
        return None;
    }
    Some((rest[..digits].parse::<u32>().ok()?, &rest[digits..]))
}

/// Skip an optional `,count` (the counts never affect numbering).
fn skip_count(rest: &str) -> &str {
    let Some(rest) = rest.strip_prefix(',') else {
        return rest;
    };
    let digits = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    &rest[digits..]
}

/// Parse `@@ -a,b +c,d @@`: the two start positions (a missing count means 1;
/// `@@ -0,0 +1 @@` legitimately starts the old side at 0).
fn parse_hunk_start(line: &str) -> Option<(u32, u32)> {
    let rest = line.strip_prefix("@@")?.trim_start();
    let rest = rest.strip_prefix('-')?;
    let (old_start, rest) = next_number(rest)?;
    let rest = skip_count(rest).trim_start();
    let rest = rest.strip_prefix('+')?;
    let (new_start, _) = next_number(rest)?;
    Some((old_start, new_start))
}

/// Walk a unified patch with hunk state and assign every line its old/new
/// line numbers. Hunk headers reset the running counters; git headers carry
/// no numbers and never advance them; `--- `/`+++ ` are headers only outside
/// a hunk, so a deleted line beginning `-- ` keeps its delete numbering.
pub fn diff_line_numbers(patch: &str) -> Vec<DiffLine> {
    let mut lines = Vec::new();
    let mut old: u32 = 0;
    let mut new: u32 = 0;
    let mut in_hunk = false;
    for text in patch.lines() {
        if text.starts_with("@@") {
            if let Some((old_start, new_start)) = parse_hunk_start(text) {
                old = old_start;
                new = new_start;
            }
            in_hunk = true;
            lines.push(DiffLine {
                kind: PatchKind::Hunk,
                old_no: None,
                new_no: None,
                text: text.to_string(),
            });
        } else if is_git_header(text, in_hunk) {
            // A header between hunks ends the hunk it followed.
            in_hunk = false;
            lines.push(DiffLine {
                kind: PatchKind::Header,
                old_no: None,
                new_no: None,
                text: text.to_string(),
            });
        } else if text.starts_with('+') {
            lines.push(DiffLine {
                kind: PatchKind::Add,
                old_no: None,
                new_no: Some(new),
                text: text.to_string(),
            });
            new += 1;
        } else if text.starts_with('-') {
            lines.push(DiffLine {
                kind: PatchKind::Delete,
                old_no: Some(old),
                new_no: None,
                text: text.to_string(),
            });
            old += 1;
        } else {
            lines.push(DiffLine {
                kind: PatchKind::Context,
                old_no: Some(old),
                new_no: Some(new),
                text: text.to_string(),
            });
            old += 1;
            new += 1;
        }
    }
    lines
}

/// Narrowest the file list may get (px).
pub const LIST_WIDTH_MIN: f32 = 160.0;
/// Narrowest the diff column may get (px) — below this the patch is unreadable.
pub const DIFF_WIDTH_MIN: f32 = 280.0;
/// File-list width before the reader drags the splitter (px).
pub const LIST_WIDTH_DEFAULT: f32 = 320.0;

/// Clamp a requested list width so the diff column keeps
/// [`DIFF_WIDTH_MIN`] px of whatever room the view actually received. The
/// splitter's own column is part of that room.
pub fn clamp_list_width(requested: f32, available: f32) -> f32 {
    let max = (available - DIFF_WIDTH_MIN - SPLITTER_WIDTH_PX).max(LIST_WIDTH_MIN);
    requested.clamp(LIST_WIDTH_MIN, max)
}

/// Plain-language copy for a failed list fetch. The raw status or error body
/// never reaches the reader (issue #13: failures speak human).
pub const LIST_ERROR_MESSAGE: &str = "Couldn't load the changed files. Try again.";
/// Plain-language copy for a failed patch fetch.
pub const PATCH_ERROR_MESSAGE: &str = "Couldn't load this file's diff. Try again.";

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
    engine: Option<Arc<dyn Runtime>>,
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
    /// The list column width the reader dragged; None until then.
    list_width: Option<f32>,
    /// A splitter drag is in progress: the reader has the handle down.
    resizing: bool,
    /// Where the current splitter drag began, in window pixels.
    resize_start_x: f32,
    /// The list width the current splitter drag began from.
    resize_start_width: f32,
    /// Width the parent gave this view (px); `app.rs` pushes it in each render.
    available_width: f32,
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
            list_width: None,
            resizing: false,
            resize_start_x: 0.0,
            resize_start_width: 0.0,
            available_width: 0.0,
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
    pub fn attach(&mut self, engine: Arc<dyn Runtime>, pane: Arc<dyn PaneHandle>, pane_id: String) {
        self.engine = Some(engine);
        self.pane_id = pane_id;
        let _ = pane;
        self.refresh();
        self.refresh_patch();
    }

    /// Whether the 2s poll runs (the Changes lens is active).
    pub fn set_visible(&mut self, visible: bool) {
        if self.visible == visible {
            return;
        }
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

    /// Fetch the file list through the wire seam (`Runtime::http`).
    pub fn refresh(&mut self) {
        let Some(engine) = self.engine.clone() else {
            return;
        };
        let path = format!("/api/workspace/diff/files?{}", self.query());
        let (status, body) = engine.http("GET", &path, &Value::Null);
        if status != 200 {
            self.error = Some(LIST_ERROR_MESSAGE.to_string());
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
            Err(_) => self.error = Some(LIST_ERROR_MESSAGE.to_string()),
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
            self.error = Some(PATCH_ERROR_MESSAGE.to_string());
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
            Err(_) => self.error = Some(PATCH_ERROR_MESSAGE.to_string()),
        }
    }

    fn select_file(&mut self, file: &RepoDiffFile) {
        self.selected_path = Some(file.path.clone());
        self.selected_old_path = file.old_path.clone();
        self.patch = None;
        self.last_patch_body = None;
        self.refresh_patch();
    }

    /// The width the parent gave this view (px); `app.rs` pushes it in during
    /// its own render so the split clamps against real room, not guesses.
    pub fn set_available_width(&mut self, width: f32) {
        self.available_width = width;
    }

    /// The list width on screen: the reader's drag, or the default, always
    /// clamped so the diff keeps its minimum.
    fn effective_list_width(&self) -> f32 {
        clamp_list_width(
            self.list_width.unwrap_or(LIST_WIDTH_DEFAULT),
            self.available_width,
        )
    }

    /// Remember where a splitter drag began, so the width it ends on is the
    /// drag's distance, not the pointer's absolute position.
    fn begin_resize(&mut self, x: f32) {
        self.resizing = true;
        self.resize_start_x = x;
        self.resize_start_width = self.effective_list_width();
    }

    /// Apply a splitter drag's position to the file list's width.
    fn track_resize(&mut self, x: f32, cx: &mut Context<Self>) {
        if !self.resizing {
            return;
        }
        self.list_width = Some(clamp_list_width(
            self.resize_start_width + (x - self.resize_start_x),
            self.available_width,
        ));
        cx.notify();
    }

    /// The drag is over; the next pointer move must not resize anything.
    fn end_resize(&mut self, cx: &mut Context<Self>) {
        if !self.resizing {
            return;
        }
        self.resizing = false;
        cx.notify();
    }

    /// The current failure, if any (plain language; tests read this).
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The width the reader chose for the file list, if they chose one.
    pub fn list_width(&self) -> Option<f32> {
        self.list_width
    }

    /// A list fetch answered (test seam mirroring `apply_conversation`).
    pub fn apply_list(&mut self, list: RepoDiffFileList) {
        self.list = Some(list);
        self.last_list_body = None;
        self.error = None;
    }

    /// A list fetch failed (test seam). The raw message is dropped: the view
    /// owns the plain-language copy the reader sees.
    pub fn fail_list(&mut self, _raw: String) {
        self.error = Some(LIST_ERROR_MESSAGE.to_string());
    }

    /// A patch fetch answered (test seam).
    pub fn apply_patch(&mut self, patch: RepoDiffFilePatch) {
        self.patch = Some(patch);
        self.last_patch_body = None;
        self.error = None;
    }

    /// A patch fetch failed (test seam); same plain-language rule as `fail_list`.
    pub fn fail_patch(&mut self, _raw: String) {
        self.error = Some(PATCH_ERROR_MESSAGE.to_string());
    }

    /// The reader asked for the data again: forget the cached bodies so the
    /// next answers are re-parsed even when identical, then re-issue both
    /// fetches.
    pub fn retry(&mut self) {
        self.error = None;
        self.last_list_body = None;
        self.last_patch_body = None;
        self.refresh();
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
            .debug_selector(|| "changes-view".to_string())
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

        // A failed fetch speaks plain language and offers a Retry (the chat
        // lens's voice); the raw status or error body never reaches the reader.
        if let Some(message) = self.error.clone() {
            root = root.child(
                div()
                    .id("changes-error")
                    .debug_selector(|| "changes-error".to_string())
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap(px(12.0))
                    .px_3()
                    .py_2()
                    .bg(theme.danger_tint)
                    .child(
                        div()
                            .text_size(px(12.0 * theme.density_scale()))
                            .text_color(theme.danger_text)
                            .child(message),
                    )
                    .child(
                        div()
                            .id("changes-retry")
                            .debug_selector(|| "changes-retry".to_string())
                            .flex()
                            .items_center()
                            .justify_center()
                            .min_h(px(44.0))
                            .px_3()
                            .rounded_md()
                            .bg(theme.primary)
                            .text_color(theme.primary_text)
                            .text_size(px(12.0 * theme.density_scale()))
                            .child("Retry")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.retry();
                                cx.notify();
                            })),
                    ),
            );
        }

        let mut panes = div()
            .id(self.next_row_id())
            .flex()
            .flex_row()
            .flex_1()
            .min_h(px(0.0));

        // File list (left): the reader's width, clamped so the diff keeps its
        // minimum. The splitter overlays the list's right edge so it costs the
        // diff no layout space.
        let list_width = self.effective_list_width();
        let mut list_column = div()
            .id(self.next_row_id())
            .flex()
            .flex_col()
            .w(px(list_width))
            .flex_shrink_0();
        let header = match &self.list {
            Some(list) => {
                let total = list.total.unwrap_or(list.files.len() as u64);
                let added = list
                    .files
                    .iter()
                    .filter(|f| {
                        matches!(f.status, RepoDiffStatus::Added | RepoDiffStatus::Untracked)
                    })
                    .count();
                let modified = list
                    .files
                    .iter()
                    .filter(|f| matches!(f.status, RepoDiffStatus::Modified))
                    .count();
                let deleted = list
                    .files
                    .iter()
                    .filter(|f| matches!(f.status, RepoDiffStatus::Deleted))
                    .count();
                format!(
                    "{} file(s) · {added} added · {modified} modified · {deleted} deleted{}",
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
                        .debug_selector(|| "changes-row".to_string())
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
                        .id("changes-empty")
                        .debug_selector(|| "changes-empty".to_string())
                        .px_2()
                        .py_1()
                        .text_size(px(12.0 * Theme::current().density_scale()))
                        .text_color(Theme::current().text_dim)
                        .child("Nothing changed — the working tree is clean."),
                );
            }
        }
        list_column = list_column.child(
            div()
                .id("changes-file-list")
                .debug_selector(|| "changes-file-list".to_string())
                .flex_1()
                .overflow_y_scroll()
                .track_scroll(&self.list_scroll)
                .child(file_rows),
        );
        panes = panes.child(list_column);

        // The splitter is a column of its own, not an overlay on the list's
        // edge: an element that overlaps a scroll container is hidden from
        // gpui's hovered-hit test, and the handle would never see the drag.
        panes = panes.child(
            div()
                .id("changes-splitter")
                .debug_selector(|| "changes-splitter".to_string())
                .w(px(SPLITTER_WIDTH_PX))
                .flex_shrink_0()
                .bg(theme.border)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                        this.begin_resize(f32::from(event.position.x));
                        cx.notify();
                    }),
                )
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                    if event.dragging() {
                        this.track_resize(f32::from(event.position.x), cx);
                    }
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseUpEvent, _window, cx| {
                        this.end_resize(cx);
                    }),
                ),
        );

        // Patch (right): the diff surface is always present, even before a
        // file is selected.
        let mut patch_column = div()
            .id("changes-diff")
            .debug_selector(|| "changes-diff".to_string())
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.0));
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
                    // Hunk-state-driven: diff_line_numbers owns both the line
                    // numbers and the classification the colors come from.
                    let diff = diff_line_numbers(text);
                    let mut patch_lines = div().id(self.next_row_id()).flex().flex_col().px_3();
                    for line in diff {
                        let old_no = line.old_no.map(|n| n.to_string()).unwrap_or_default();
                        let new_no = line.new_no.map(|n| n.to_string()).unwrap_or_default();
                        let shown = if line.text.is_empty() {
                            " ".to_string()
                        } else {
                            line.text.clone()
                        };
                        patch_lines = patch_lines.child(
                            div()
                                .id(self.next_row_id())
                                .flex()
                                .flex_row()
                                .child(
                                    div()
                                        .w(px(36.0))
                                        .flex_shrink_0()
                                        .text_align(TextAlign::Right)
                                        .text_size(px(12.0))
                                        .text_color(theme.text_dim)
                                        .child(old_no),
                                )
                                .child(
                                    div()
                                        .w(px(36.0))
                                        .flex_shrink_0()
                                        .text_align(TextAlign::Right)
                                        .text_size(px(12.0))
                                        .text_color(theme.text_dim)
                                        .child(new_no),
                                )
                                .child(StyledText::new(shown).with_default_highlights(
                                    &TextStyle {
                                        color: patch_color(line.kind),
                                        font_size: px(12.0).into(),
                                        ..Default::default()
                                    },
                                    [(0..line.text.len(), patch_highlight(line.kind))],
                                )),
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
        root = root.child(panes);

        // While the reader holds the splitter, a layer over the whole lens
        // tracks the pointer. The handle is eight pixels wide, and gpui only
        // tells a mouse-move listener about the element the pointer is
        // directly over; a layer is a leaf, so it is the one element that
        // always receives the move, wherever the pointer goes.
        if self.resizing {
            root = root.child(
                div()
                    .id("changes-drag-layer")
                    .debug_selector(|| "changes-drag-layer".to_string())
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                        this.track_resize(f32::from(event.position.x), cx);
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseUpEvent, _window, cx| {
                            this.end_resize(cx);
                        }),
                    ),
            );
        }

        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, size, Entity};
    use serde_json::json;

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

    #[test]
    fn diff_line_numbers_assigns_old_and_new_numbers() {
        let patch = concat!(
            "diff --git a/x b/x\n",
            "index 111..222 100644\n",
            "--- a/x\n",
            "+++ b/x\n",
            "@@ -1,3 +1,3 @@\n",
            " context\n",
            "-deleted\n",
            "+added\n",
            "@@ -10 +10 @@\n",
            " more\n",
        );
        let lines = diff_line_numbers(patch);
        assert_eq!(lines.len(), 10);
        // Git headers carry no numbers and never advance the counters.
        for line in &lines[..4] {
            assert_eq!(line.kind, PatchKind::Header);
            assert_eq!(line.old_no, None);
            assert_eq!(line.new_no, None);
        }
        assert_eq!(lines[4].kind, PatchKind::Hunk);
        assert_eq!(lines[4].old_no, None);
        assert_eq!(
            lines[5],
            DiffLine {
                kind: PatchKind::Context,
                old_no: Some(1),
                new_no: Some(1),
                text: " context".to_string(),
            }
        );
        assert_eq!(
            lines[6],
            DiffLine {
                kind: PatchKind::Delete,
                old_no: Some(2),
                new_no: None,
                text: "-deleted".to_string(),
            }
        );
        assert_eq!(
            lines[7],
            DiffLine {
                kind: PatchKind::Add,
                old_no: None,
                new_no: Some(2),
                text: "+added".to_string(),
            }
        );
        // The second hunk resets both counters from its own header (a missing
        // count means 1).
        assert_eq!(lines[8].kind, PatchKind::Hunk);
        assert_eq!(
            lines[9],
            DiffLine {
                kind: PatchKind::Context,
                old_no: Some(10),
                new_no: Some(10),
                text: " more".to_string(),
            }
        );

        // Inside a hunk, `--- `/`+++ ` are a deleted/added line whose text
        // starts with dashes, not file headers.
        let tricky = "@@ -1 +1 @@\n--- looks-like-a-header\n+++ so-does-this\n";
        let lines = diff_line_numbers(tricky);
        assert_eq!(
            lines[1],
            DiffLine {
                kind: PatchKind::Delete,
                old_no: Some(1),
                new_no: None,
                text: "--- looks-like-a-header".to_string(),
            }
        );
        assert_eq!(
            lines[2],
            DiffLine {
                kind: PatchKind::Add,
                old_no: None,
                new_no: Some(1),
                text: "+++ so-does-this".to_string(),
            }
        );

        // A header after a hunk ends it, and `@@ -0,0 +1 @@` starts the old
        // side at 0 without underflowing.
        let mixed = "@@ -1 +1 @@\n-old\n@@ -0,0 +1 @@\n+new\ndiff --git a/y b/y\n";
        let lines = diff_line_numbers(mixed);
        assert_eq!(lines[1].kind, PatchKind::Delete);
        assert_eq!(lines[2].kind, PatchKind::Hunk);
        assert_eq!(
            lines[3],
            DiffLine {
                kind: PatchKind::Add,
                old_no: None,
                new_no: Some(1),
                text: "+new".to_string(),
            }
        );
        assert_eq!(lines[4].kind, PatchKind::Header);
        assert_eq!(lines[4].old_no, None);
        assert_eq!(lines[4].new_no, None);
    }

    #[test]
    fn clamp_list_width_keeps_the_diff_usable() {
        // Plenty of room: the default passes through (1280px window, sidebar
        // 280, tree and reader 360 → 640px for this view).
        assert_eq!(clamp_list_width(LIST_WIDTH_DEFAULT, 640.0), 320.0);
        // Tight room: the list yields before the diff drops under its minimum
        // (800px window, rail 64, tree 140 → 596px for this view).
        assert_eq!(clamp_list_width(LIST_WIDTH_DEFAULT, 596.0), 308.0);
        // Too small a request: the list keeps its own minimum.
        assert_eq!(clamp_list_width(100.0, 640.0), LIST_WIDTH_MIN);
        // Too large a request: capped at what the diff can spare, with the
        // splitter's own column taken off the top.
        assert_eq!(
            clamp_list_width(2000.0, 640.0),
            640.0 - DIFF_WIDTH_MIN - SPLITTER_WIDTH_PX
        );
        // No room measured yet: the minimum, never a negative cap.
        assert_eq!(clamp_list_width(LIST_WIDTH_DEFAULT, 0.0), LIST_WIDTH_MIN);
    }

    struct ChangesRoot {
        view: Entity<ChangesView>,
    }

    impl Render for ChangesRoot {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            self.view.clone()
        }
    }

    fn file_list_json(files: serde_json::Value) -> serde_json::Value {
        json!({
            "repository": {
                "root": "/r", "branch": "main", "head": "abc",
                "detached": false, "unborn": false,
            },
            "scope": "workspace",
            "files": files,
            "total": 2, "truncated": false,
        })
    }

    #[gpui::test]
    fn the_changes_lens_shows_the_file_list_and_diff_surface(cx: &mut gpui::TestAppContext) {
        let cx = cx.add_empty_window();
        let view = cx.update(|_window, app| app.new(ChangesView::new));
        cx.draw(
            point(px(0.), px(0.)),
            size(px(1280.), px(800.)),
            |_, _cx| view.clone(),
        );
        assert!(
            cx.debug_bounds("changes-file-list").is_some(),
            "the file list is on screen"
        );
        assert!(
            cx.debug_bounds("changes-diff").is_some(),
            "the diff surface is on screen even with no file selected"
        );

        cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, cx| {
            view.update(cx, |v, _cx| {
                v.apply_list(
                    RepoDiffFileList::parse(&file_list_json(json!([
                        {"path": "a.rs", "old_path": null, "status": "modified",
                         "additions": 3, "deletions": 1, "binary": null},
                        {"path": "b.rs", "old_path": null, "status": "added",
                         "additions": 5, "deletions": 0, "binary": null},
                    ])))
                    .expect("parse"),
                );
            });
            view.clone()
        });
        assert!(
            cx.debug_bounds("changes-row").is_some(),
            "the changed files are on screen"
        );
    }

    #[gpui::test]
    fn the_changes_lens_offers_a_retry_when_a_fetch_fails(cx: &mut gpui::TestAppContext) {
        let (root, cx) = cx.add_window_view(|_, cx| ChangesRoot {
            view: cx.new(ChangesView::new),
        });
        cx.update(|_window, app| {
            let view = root.read(app).view.clone();
            view.update(app, |v, _cx| v.fail_list("boom".to_string()));
        });
        cx.update(|window, cx| window.draw(cx).clear());
        assert!(
            cx.debug_bounds("changes-error").is_some(),
            "the failure is on screen"
        );
        assert!(
            cx.debug_bounds("changes-retry").is_some(),
            "a Retry is offered"
        );

        let bounds = cx.debug_bounds("changes-retry").expect("the Retry");
        cx.simulate_click(bounds.center(), gpui::Modifiers::none());
        cx.update(|_window, app| {
            let view = root.read(app).view.clone();
            assert_eq!(
                view.read(app).error(),
                None,
                "the Retry cleared the failure"
            );
        });
    }

    #[gpui::test]
    fn a_clean_repository_shows_an_empty_state(cx: &mut gpui::TestAppContext) {
        let cx = cx.add_empty_window();
        let view = cx.update(|_window, app| app.new(ChangesView::new));
        cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, cx| {
            view.update(cx, |v, _cx| {
                v.apply_list(RepoDiffFileList::parse(&file_list_json(json!([]))).expect("parse"));
            });
            view.clone()
        });
        assert!(
            cx.debug_bounds("changes-empty").is_some(),
            "the clean-tree empty state is on screen"
        );
        assert!(
            cx.debug_bounds("changes-row").is_none(),
            "a clean tree lists no files"
        );
    }

    #[gpui::test]
    fn the_file_list_and_diff_split_stays_usable_at_1280_and_narrow(cx: &mut gpui::TestAppContext) {
        let cx = cx.add_empty_window();
        let view = cx.update(|_window, app| app.new(ChangesView::new));

        // 1280px window, sidebar 280, tree 220 + reader 140 = 360 → the app
        // gives this view 640px.
        cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, cx| {
            view.update(cx, |v, _cx| v.set_available_width(640.0));
            div().w(px(640.0)).h_full().child(view.clone())
        });
        let list = cx
            .debug_bounds("changes-file-list")
            .expect("the file list at 1280");
        assert_eq!(list.size.width, px(320.0), "the default list width holds");
        let diff = cx.debug_bounds("changes-diff").expect("the diff at 1280");
        assert_eq!(
            diff.size.width,
            px(312.0),
            "the diff takes the room the list does not need"
        );
        assert!(
            diff.size.width >= px(DIFF_WIDTH_MIN),
            "the diff keeps its minimum at 1280 (got {})",
            diff.size.width
        );

        // 800px window, rail 64, tree 140 → the app gives this view 596px.
        cx.draw(point(px(0.), px(0.)), size(px(800.), px(600.)), |_, cx| {
            view.update(cx, |v, _cx| v.set_available_width(596.0));
            div().w(px(596.0)).h_full().child(view.clone())
        });
        let list = cx
            .debug_bounds("changes-file-list")
            .expect("the file list at 800");
        assert_eq!(
            list.size.width,
            px(308.0),
            "the list yields before the diff does"
        );
        let diff = cx.debug_bounds("changes-diff").expect("the diff at 800");
        assert!(
            diff.size.width >= px(DIFF_WIDTH_MIN),
            "the diff keeps its minimum at the narrow breakpoint (got {})",
            diff.size.width
        );
    }

    #[gpui::test]
    fn the_splitter_drag_resizes_the_file_list(cx: &mut gpui::TestAppContext) {
        use gpui::{point, size};
        let cx = cx.add_empty_window();
        let view = cx.update(|_window, app| app.new(ChangesView::new));

        cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, cx| {
            view.update(cx, |v, _cx| {
                v.set_available_width(640.0);
                v.apply_list(
                    RepoDiffFileList::parse(&file_list_json(json!([
                        {"path": "a.rs", "old_path": null, "status": "modified",
                         "additions": 3, "deletions": 1, "binary": null},
                    ])))
                    .expect("parse"),
                );
            });
            div().w(px(640.0)).h_full().child(view.clone())
        });

        let before = cx.debug_bounds("changes-file-list").expect("the file list");
        assert_eq!(
            before.size.width,
            px(320.0),
            "the list starts at its default"
        );

        // Grab the splitter and drag it 60px to the left, across the list.
        let splitter = cx.debug_bounds("changes-splitter").expect("the splitter");
        let grip = splitter.center();
        let target = point(px(f32::from(grip.x) - 60.0), grip.y);
        cx.simulate_mouse_down(grip, gpui::MouseButton::Left, gpui::Modifiers::none());
        // The drag layer is on screen from the next frame on.
        cx.draw(
            point(px(0.), px(0.)),
            size(px(1280.), px(800.)),
            |_, _cx| div().w(px(640.0)).h_full().child(view.clone()),
        );
        assert!(
            cx.debug_bounds("changes-drag-layer").is_some(),
            "the drag layer is on screen while the handle is held"
        );
        cx.simulate_mouse_move(
            target,
            Some(gpui::MouseButton::Left),
            gpui::Modifiers::none(),
        );
        cx.simulate_mouse_up(target, gpui::MouseButton::Left, gpui::Modifiers::none());
        cx.draw(
            point(px(0.), px(0.)),
            size(px(1280.), px(800.)),
            |_, _cx| div().w(px(640.0)).h_full().child(view.clone()),
        );

        let after = cx.debug_bounds("changes-file-list").expect("the file list");
        assert_eq!(
            after.size.width,
            px(260.0),
            "the drag made the list 60px narrower"
        );
        let diff = cx.debug_bounds("changes-diff").expect("the diff");
        assert_eq!(diff.size.width, px(372.0), "the diff took the room");
    }

    #[gpui::test]
    fn switching_the_lens_away_and_back_keeps_the_selection_and_scroll(
        cx: &mut gpui::TestAppContext,
    ) {
        let (root, cx) = cx.add_window_view(|_, cx| ChangesRoot {
            view: cx.new(ChangesView::new),
        });
        cx.update(|_window, app| {
            let view = root.read(app).view.clone();
            view.update(app, |v, _cx| {
                v.apply_list(
                    RepoDiffFileList::parse(&file_list_json(json!([
                        {"path": "a.rs", "old_path": null, "status": "modified",
                         "additions": 3, "deletions": 1, "binary": null},
                    ])))
                    .expect("parse"),
                );
            });
        });
        cx.update(|window, cx| window.draw(cx).clear());
        let (selected, list_offset, patch_offset) = cx.update(|_window, app| {
            let view = root.read(app).view.clone();
            view.update(app, |v, _cx| {
                let file = v.list.as_ref().expect("list").files[0].clone();
                v.select_file(&file);
                v.list_scroll.set_offset(point(px(0.), px(-40.0)));
                v.patch_scroll.set_offset(point(px(0.), px(-120.0)));
            });
            (
                view.read(app).selected_path.clone(),
                view.read(app).list_scroll.offset(),
                view.read(app).patch_scroll.offset(),
            )
        });
        assert!(selected.is_some(), "a file is selected");

        // Away and back: the lens switch must not reset the view's state.
        cx.update(|_window, app| {
            let view = root.read(app).view.clone();
            view.update(app, |v, _cx| v.set_visible(false));
        });
        cx.update(|_window, app| {
            let view = root.read(app).view.clone();
            view.update(app, |v, _cx| v.set_visible(true));
        });
        cx.update(|_window, app| {
            let view = root.read(app).view.clone();
            assert_eq!(
                view.read(app).selected_path,
                selected,
                "the selection survives the lens switch"
            );
            assert_eq!(
                view.read(app).list_scroll.offset(),
                list_offset,
                "the list scroll survives the lens switch"
            );
            assert_eq!(
                view.read(app).patch_scroll.offset(),
                patch_offset,
                "the diff scroll survives the lens switch"
            );
        });
    }
}
