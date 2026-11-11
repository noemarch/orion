//! File tree view (P3): an expandable tree of the pane's working directory,
//! beside a resizable file reader.
//!
//! Each expanded folder's listing comes from
//! `GET /api/workspace/directories?path=<ABSOLUTE>&files=1` through
//! `Engine::http` — in-process, no HTTP hop. The route takes the path as
//! given (only `~` is expanded; it does not resolve against a pane), so the
//! tree always sends an absolute path and stores every path with forward
//! slashes: the core echoes back the path it was given and accepts `/` on
//! Windows, so one separator keeps the tree's folder keys matching across
//! `join_path` and the wire. Clicking a folder expands or collapses it in
//! place; clicking a file opens it in the [`FileReader`] child. A chat path
//! link lands in [`FileTree::reveal`], which resolves the raw path against
//! the root, expands the ancestors, and opens the file.
//!
//! Layout: the tree and the reader form a resizable split
//! ([`crate::shell_model::clamp_tree_width`]) so the reader keeps at least
//! [`crate::shell_model::READER_WIDTH_MIN`] px at any window width; `app.rs`
//! pushes the width the view actually receives in via
//! [`FileTree::set_available_width`].
//!
//! Polling: 2s timer like `ChatView` (the engine already owns `notify`
//! watchers; GPUI views poll rather than adding a second watcher to
//! `orion-ui`), and only while visible (the Changes lens is active).

use crate::file_reader::FileReader;
use crate::files_entities::{join_path, url_encode, DirectoryListing};
use crate::shell_model::{clamp_tree_width, SPLITTER_WIDTH_PX, TREE_WIDTH_DEFAULT};
use crate::theme::Theme;
use gpui::prelude::*;
use gpui::{
    div, px, white, Context, Div, Entity, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Render, ScrollHandle, Stateful, Window,
};
use orion_core::engine::pane::Pane;
use orion_core::Engine;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

/// Plain-language copy for a failed directory fetch. The raw status or error
/// body never reaches the reader (issue #13: failures speak human).
pub const DIRECTORY_ERROR_MESSAGE: &str = "Couldn't load this folder. Try again.";

/// Every path the tree stores or sends uses forward slashes: the core echoes
/// back the path it was given, so one separator keeps the tree's folder keys
/// matching across `join_path` and the wire on Windows.
fn normalize_path(path: &str) -> String {
    let mut out = path.replace('\\', "/");
    while out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    out
}

/// Whether `path` (already normalized) stands on its own instead of hanging
/// off the tree's root: a leading `/`, or a Windows drive like `C:/`.
fn is_absolute_path(path: &str) -> bool {
    if path.starts_with('/') {
        return true;
    }
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[1] == b':'
}

pub struct FileTree {
    engine: Option<Arc<Engine>>,
    /// The pane's cwd, absolute, forward slashes: the tree's root.
    root_path: Option<String>,
    /// Absolute folder paths that are open (the root always is).
    expanded: BTreeSet<String>,
    /// One listing per expanded folder, keyed by the folder's absolute path.
    listings: BTreeMap<String, DirectoryListing>,
    /// The file the reader is showing.
    selected_file: Option<String>,
    file_reader: Entity<FileReader>,
    error: Option<String>,
    visible: bool,
    scroll_handle: ScrollHandle,
    row_seq: usize,
    /// The tree's width the reader dragged; None until then.
    tree_width: Option<f32>,
    /// A splitter drag is in progress: the reader has the handle down.
    resizing: bool,
    /// Where the current splitter drag began, in window pixels.
    resize_start_x: f32,
    /// The tree width the current splitter drag began from.
    resize_start_width: f32,
    /// Width the parent gave this view (px); `app.rs` pushes it in each render.
    available_width: f32,
}

impl FileTree {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let file_reader = cx.new(FileReader::new);
        let view = Self {
            engine: None,
            root_path: None,
            expanded: BTreeSet::new(),
            listings: BTreeMap::new(),
            selected_file: None,
            file_reader,
            error: None,
            visible: false,
            scroll_handle: ScrollHandle::new(),
            row_seq: 0,
            tree_width: None,
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

    /// Wire the engine + pane once they exist; the tree roots at the pane's
    /// own cwd with only the root expanded. The file reader child is attached
    /// to the same engine + pane_id.
    pub fn attach(
        &mut self,
        cx: &mut Context<Self>,
        engine: Arc<Engine>,
        pane: Arc<Pane>,
        pane_id: String,
    ) {
        self.engine = Some(engine.clone());
        let root = normalize_path(&pane.cwd.to_string_lossy());
        self.root_path = Some(root.clone());
        self.expanded.clear();
        self.expanded.insert(root);
        self.listings.clear();
        let reader = self.file_reader.clone();
        reader.update(cx, |reader, _cx| reader.attach(engine, pane_id));
        self.refresh();
    }

    /// Whether the 2s poll runs (the Changes lens is active).
    pub fn set_visible(&mut self, cx: &mut Context<Self>, visible: bool) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        let reader = self.file_reader.clone();
        reader.update(cx, |reader, _cx| reader.set_visible(visible));
        if visible {
            self.refresh();
        }
    }

    pub fn visible(&self) -> bool {
        self.visible
    }

    /// Fetch a listing for EVERY expanded folder through the wire seam
    /// (`Engine::http`). A folder whose fetch fails — it vanished, or it is
    /// not a directory — is dropped from the tree and the failure is recorded
    /// as the tree's error; the root itself stays expanded (it is the pane's
    /// cwd) so the tree keeps its anchor. When every fetch succeeds, the
    /// error clears.
    pub fn refresh(&mut self) {
        let Some(engine) = self.engine.clone() else {
            return;
        };
        let mut any_failed = false;
        for path in self.expanded.clone() {
            let route = format!(
                "/api/workspace/directories?path={}&files=1",
                url_encode(&path)
            );
            let (status, body) = engine.http("GET", &route, &Value::Null);
            let parsed = if status == 200 {
                DirectoryListing::parse(&body).ok()
            } else {
                None
            };
            match parsed {
                Some(listing) => {
                    self.listings.insert(path, listing);
                }
                None => {
                    any_failed = true;
                    if Some(path.as_str()) != self.root_path.as_deref() {
                        self.expanded.remove(&path);
                        self.listings.remove(&path);
                    }
                }
            }
        }
        self.error = if any_failed {
            Some(DIRECTORY_ERROR_MESSAGE.to_string())
        } else {
            None
        };
    }

    /// Expand or collapse one folder. Expanding fetches its listing so the
    /// children appear; collapsing drops the folder and its listing.
    pub fn toggle_folder(&mut self, path: String) {
        if self.expanded.contains(&path) {
            self.expanded.remove(&path);
            self.listings.remove(&path);
        } else {
            self.expanded.insert(path);
            self.refresh();
        }
    }

    /// Open a chat path link: resolve `path` against the root (an absolute
    /// path stands as given; a relative one joins onto the root — the chat's
    /// paths are raw, both forms arrive), expand every ancestor folder
    /// between the root and the file's own folder so the file is visible in
    /// the tree, select it, open it in the reader, and refresh so the newly
    /// expanded folders fill in.
    pub fn reveal(&mut self, cx: &mut Context<Self>, path: String) {
        let Some(root) = self.root_path.clone() else {
            return;
        };
        let normalized = normalize_path(&path);
        let resolved = if is_absolute_path(&normalized) {
            normalized
        } else {
            join_path(&root, &normalized)
        };
        // The file's folder and every ancestor above it up to (but not
        // including) the root, which is always expanded. Folders outside the
        // root are left alone: the tree cannot show them.
        if let Some(slash) = resolved.rfind('/') {
            let mut folder = resolved[..slash].to_string();
            while folder
                .strip_prefix(root.as_str())
                .is_some_and(|rest| root == "/" || rest.starts_with('/'))
            {
                self.expanded.insert(folder.clone());
                let Some(slash) = folder.rfind('/') else {
                    break;
                };
                folder.truncate(slash);
            }
        }
        self.selected_file = Some(resolved.clone());
        let reader = self.file_reader.clone();
        reader.update(cx, |reader, _cx| reader.open(resolved));
        self.refresh();
    }

    /// The reader asked for the data again: clear the failure and re-issue
    /// the fetches.
    pub fn retry(&mut self) {
        self.error = None;
        self.refresh();
    }

    /// The tree's root (the pane's cwd), absolute, forward slashes.
    pub fn root_path(&self) -> Option<&str> {
        self.root_path.as_deref()
    }

    /// The file the reader is showing, resolved absolute.
    pub fn selected_file(&self) -> Option<&str> {
        self.selected_file.as_deref()
    }

    /// The expanded folders, absolute, forward slashes.
    pub fn expanded(&self) -> impl Iterator<Item = &str> {
        self.expanded.iter().map(String::as_str)
    }

    /// A directory fetch answered (test seam mirroring `apply_list`): store
    /// the listing and make sure its folder is expanded so the rows render.
    /// The first listing also roots the tree when no pane has been attached
    /// yet.
    pub fn apply_listing(&mut self, path: String, listing: DirectoryListing) {
        if self.root_path.is_none() {
            self.root_path = Some(path.clone());
        }
        self.expanded.insert(path.clone());
        self.listings.insert(path, listing);
        self.error = None;
    }

    /// The width the parent gave this view (px); `app.rs` pushes it in during
    /// its own render so the split clamps against real room, not guesses.
    pub fn set_available_width(&mut self, width: f32) {
        self.available_width = width;
    }

    /// The tree's width on screen: the reader's drag, or the default, always
    /// clamped so the reader keeps its minimum.
    pub fn effective_tree_width(&self) -> f32 {
        clamp_tree_width(
            self.tree_width.unwrap_or(TREE_WIDTH_DEFAULT),
            self.available_width,
        )
    }

    /// Remember where a splitter drag began, so the width it ends on is the
    /// drag's distance, not the pointer's absolute position.
    pub fn begin_resize(&mut self, x: f32) {
        self.resizing = true;
        self.resize_start_x = x;
        self.resize_start_width = self.effective_tree_width();
    }

    /// Apply a splitter drag's position to the tree's width.
    pub fn track_resize(&mut self, x: f32, cx: &mut Context<Self>) {
        if !self.resizing {
            return;
        }
        self.tree_width = Some(clamp_tree_width(
            self.resize_start_width + (x - self.resize_start_x),
            self.available_width,
        ));
        cx.notify();
    }

    /// The drag is over; the next pointer move must not resize anything.
    pub fn end_resize(&mut self, cx: &mut Context<Self>) {
        if !self.resizing {
            return;
        }
        self.resizing = false;
        cx.notify();
    }

    /// Clicking a file row: the tree and the reader agree on the open file.
    fn open_file(&mut self, path: String, cx: &mut Context<Self>) {
        self.selected_file = Some(path.clone());
        let reader = self.file_reader.clone();
        reader.update(cx, |reader, _cx| reader.open(path));
        cx.notify();
    }

    fn start_poll(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |view, cx| loop {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            let _ = view.update(cx, |view, _cx| {
                if view.visible {
                    view.refresh();
                }
            });
            if view.upgrade().is_none() {
                break;
            }
        })
        .detach();
    }

    /// The file reader child, rendered beside the tree.
    pub fn file_reader(&self) -> &Entity<FileReader> {
        &self.file_reader
    }

    /// The tree column's content: the root label, the failure (with a Retry),
    /// and the expandable rows in a scroll container.
    fn render_tree(&mut self, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = Theme::current();
        let mut tree = div()
            .id("file-tree")
            .debug_selector(|| "file-tree".to_string())
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.surface_base)
            .text_color(white());

        let label = self.root_path.clone().unwrap_or_else(|| "…".to_string());
        tree = tree.child(
            div()
                .id(self.next_row_id())
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .px_3()
                .py_1()
                .text_size(px(11.0 * theme.density_scale()))
                .text_color(theme.text_dim)
                .child(label),
        );

        // A failed fetch speaks plain language and offers a Retry (the chat
        // lens's voice); the raw status or error body never reaches the reader.
        if let Some(message) = self.error.clone() {
            tree = tree.child(
                div()
                    .id("file-tree-error")
                    .debug_selector(|| "file-tree-error".to_string())
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
                            .id("file-tree-retry")
                            .debug_selector(|| "file-tree-retry".to_string())
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

        let mut rows = div().id(self.next_row_id()).flex().flex_col().px_3().py_1();
        if let Some(root) = self.root_path.clone() {
            rows = self.render_folder_rows(&root, 0, rows, cx);
        }
        tree = tree.child(
            div()
                .id("file-tree-scroll")
                .debug_selector(|| "file-tree-scroll".to_string())
                .flex_1()
                .overflow_y_scroll()
                .track_scroll(&self.scroll_handle)
                .child(rows),
        );
        tree
    }

    /// One expanded folder's rows: its folders (chevron + name, the row
    /// toggles it, recursing into the open ones) and its files (name + size,
    /// the row opens it), indented by depth.
    fn render_folder_rows(
        &mut self,
        path: &str,
        depth: usize,
        rows: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let Some(listing) = self.listings.get(path).cloned() else {
            return rows;
        };
        let theme = Theme::current();
        let indent = px((depth * 16) as f32);
        let mut rows = rows;
        for name in &listing.directories {
            let child = join_path(&listing.path, name);
            let is_expanded = self.expanded.contains(&child);
            let chevron = if is_expanded { "▾" } else { "▸" };
            let child_clone = child.clone();
            rows = rows.child(
                div()
                    .id(self.next_row_id())
                    .debug_selector(|| "file-tree-folder".to_string())
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(4.0))
                    .pl(indent)
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_size(px(12.0 * theme.density_scale()))
                    .text_color(theme.primary)
                    .child(format!("{chevron} {name}"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_folder(child_clone.clone());
                        cx.notify();
                    })),
            );
            if is_expanded {
                rows = self.render_folder_rows(&child, depth + 1, rows, cx);
            }
        }
        for file in listing.file_entries() {
            let child = join_path(&listing.path, &file.name);
            let selected = self.selected_file.as_deref() == Some(child.as_str());
            let child_clone = child.clone();
            let mut row = div()
                .id(self.next_row_id())
                .debug_selector(|| "file-tree-row".to_string())
                .flex()
                .flex_row()
                .pl(indent)
                .px_2()
                .py_1()
                .rounded_md()
                .text_size(px(12.0 * theme.density_scale()))
                .text_color(theme.text)
                .child(format!("{}  ({} bytes)", file.name, file.size));
            // The selected file's row is lit so the reader and the tree agree
            // on which file is open; the rest stay plain.
            if selected {
                row = row.bg(theme.surface_elevated);
            }
            rows = rows.child(row.on_click(cx.listener(move |this, _, _, cx| {
                this.open_file(child_clone.clone(), cx);
            })));
        }
        if listing.truncated {
            rows = rows.child(
                div()
                    .id(self.next_row_id())
                    .pl(indent)
                    .px_2()
                    .py_1()
                    .text_size(px(11.0 * theme.density_scale()))
                    .text_color(theme.text_dim)
                    .child("Showing the first entries; go deeper to narrow."),
            );
        }
        if listing.directories.is_empty() && listing.file_entries().is_empty() {
            rows = rows.child(
                div()
                    .id(self.next_row_id())
                    .pl(indent)
                    .px_2()
                    .py_1()
                    .text_size(px(12.0 * theme.density_scale()))
                    .text_color(theme.text_dim)
                    .child("Nothing here"),
            );
        }
        rows
    }
}

impl Render for FileTree {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.row_seq = 0;
        let theme = Theme::current();
        let tree = self.render_tree(cx);
        let tree_width = self.effective_tree_width();

        let mut surface = div()
            .id("files-surface")
            .flex()
            .flex_row()
            .size_full()
            .child(
                div()
                    .id("files-left")
                    .debug_selector(|| "files-left".to_string())
                    .flex()
                    .flex_col()
                    // The tree takes the width the reader dragged (or the
                    // default), clamped so the reader beside it keeps a body
                    // instead of being squeezed to nothing (issue #13).
                    .w(px(tree_width))
                    .flex_shrink_0()
                    .child(tree),
            )
            // The splitter is a column of its own, not an overlay on the
            // tree's edge: an element that overlaps a scroll container is
            // hidden from gpui's hovered-hit test, and the handle would never
            // see the drag.
            .child(
                div()
                    .id("files-splitter")
                    .debug_selector(|| "files-splitter".to_string())
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
            )
            .child(
                div()
                    .id("files-right")
                    .debug_selector(|| "files-right".to_string())
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.0))
                    .child(self.file_reader.clone()),
            );

        // While the reader holds the splitter, a layer over the whole surface
        // tracks the pointer. The handle is eight pixels wide, and gpui only
        // tells a mouse-move listener about the element the pointer is
        // directly over; a layer is a leaf, so it is the one element that
        // always receives the move, wherever the pointer goes.
        if self.resizing {
            surface = surface.child(
                div()
                    .id("files-drag-layer")
                    .debug_selector(|| "files-drag-layer".to_string())
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

        surface
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell_model::{FILES_SURFACE_WIDTH_WIDE, READER_WIDTH_MIN};
    use gpui::{point, size};
    use serde_json::json;

    /// A wire-shaped listing for one directory, parsed the way the route
    /// answers.
    fn listing_json(
        path: &str,
        directories: &[&str],
        files: serde_json::Value,
    ) -> DirectoryListing {
        DirectoryListing::parse(&json!({
            "path": path,
            "parent": null,
            "home": "/home/u",
            "directories": directories,
            "truncated": false,
            "files": files,
        }))
        .expect("parse")
    }

    /// The old fixed-column test, rewritten for the split: `files-left` is
    /// the tree's own column now, so its width is the tree's width (the
    /// reader's drag, or the default), not the shell's — and the reader
    /// beside it keeps a body.
    #[gpui::test]
    fn the_reader_keeps_a_body_beside_the_tree(cx: &mut gpui::TestAppContext) {
        let cx = cx.add_empty_window();
        let view = cx.update(|_window, app| app.new(FileTree::new));

        // The app gives the surface its own column; the tree takes its
        // default width and the reader keeps the rest.
        cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, cx| {
            view.update(cx, |v, _cx| v.set_available_width(FILES_SURFACE_WIDTH_WIDE));
            div()
                .w(px(FILES_SURFACE_WIDTH_WIDE))
                .h_full()
                .child(view.clone())
        });

        let tree = cx.debug_bounds("files-left").expect("the tree column");
        assert_eq!(
            tree.size.width,
            px(TREE_WIDTH_DEFAULT),
            "the tree takes its own width, not the shell's"
        );
        let reader = cx.debug_bounds("files-right").expect("the reader column");
        assert!(
            reader.size.width >= px(READER_WIDTH_MIN),
            "the reader keeps a body to show a file in (got {})",
            reader.size.width
        );
    }

    #[gpui::test]
    fn the_files_surface_shows_the_tree_and_reader(cx: &mut gpui::TestAppContext) {
        let cx = cx.add_empty_window();
        let view = cx.update(|_window, app| app.new(FileTree::new));
        cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, cx| {
            view.update(cx, |v, _cx| v.set_available_width(FILES_SURFACE_WIDTH_WIDE));
            div()
                .w(px(FILES_SURFACE_WIDTH_WIDE))
                .h_full()
                .child(view.clone())
        });
        assert!(
            cx.debug_bounds("file-tree").is_some(),
            "the tree is on screen"
        );
        assert!(
            cx.debug_bounds("files-right").is_some(),
            "the reader is on screen"
        );
    }

    #[gpui::test]
    fn the_file_tree_expands_and_collapses_a_folder(cx: &mut gpui::TestAppContext) {
        let cx = cx.add_empty_window();
        let view = cx.update(|_window, app| app.new(FileTree::new));
        cx.update(|_window, app| {
            view.update(app, |v, _cx| {
                v.apply_listing(
                    "/proj".to_string(),
                    listing_json("/proj", &["src", "docs"], json!([])),
                );
                v.toggle_folder("/proj/src".to_string());
            });
            assert!(
                view.read(app).expanded().any(|p| p == "/proj/src"),
                "the folder is expanded"
            );
            view.update(app, |v, _cx| v.toggle_folder("/proj/src".to_string()));
            assert!(
                !view.read(app).expanded().any(|p| p == "/proj/src"),
                "the folder is collapsed again"
            );
        });
    }

    /// A real root view for interaction tests: a click's down-up pair must
    /// survive the refresh the down triggers, and only a root-view render
    /// keeps the tree on screen for the up.
    struct FilesRoot {
        view: Entity<FileTree>,
    }

    impl Render for FilesRoot {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            self.view.clone()
        }
    }

    #[gpui::test]
    fn a_file_in_the_tree_opens_in_the_reader(cx: &mut gpui::TestAppContext) {
        let (root, cx) = cx.add_window_view(|_, cx| FilesRoot {
            view: cx.new(FileTree::new),
        });
        cx.update(|_window, app| {
            let view = root.read(app).view.clone();
            view.update(app, |v, _cx| {
                v.apply_listing(
                    "/proj".to_string(),
                    listing_json("/proj", &[], json!([{"name": "main.rs", "size": 123}])),
                );
            });
        });
        cx.update(|window, cx| window.draw(cx).clear());

        // The listing holds one file, so the only tree row is that file's.
        let row = cx.debug_bounds("file-tree-row").expect("the file row");
        cx.simulate_click(row.center(), gpui::Modifiers::none());
        cx.update(|_window, app| {
            let view = root.read(app).view.clone();
            assert_eq!(
                view.read(app).selected_file(),
                Some("/proj/main.rs"),
                "the tree agrees on the open file"
            );
            assert_eq!(
                view.read(app).file_reader().read(app).path(),
                Some("/proj/main.rs"),
                "the clicked file opened in the reader"
            );
        });
    }

    #[gpui::test]
    fn a_chat_path_link_reveals_the_file_in_the_tree(cx: &mut gpui::TestAppContext) {
        let cx = cx.add_empty_window();
        let view = cx.update(|_window, app| app.new(FileTree::new));
        cx.update(|_window, app| {
            view.update(app, |v, cx| {
                v.apply_listing("/proj".to_string(), listing_json("/proj", &[], json!([])));
                v.reveal(cx, "src/main.rs".to_string());
            });
            let v = view.read(app);
            assert_eq!(
                v.selected_file(),
                Some("/proj/src/main.rs"),
                "the relative chat path resolved against the root"
            );
            assert!(
                v.expanded().any(|p| p == "/proj/src"),
                "the file's folder is expanded so the file is visible"
            );
            assert_eq!(
                v.file_reader().read(app).path(),
                Some("/proj/src/main.rs"),
                "the reader holds the resolved path"
            );
        });
    }

    #[gpui::test]
    fn the_tree_and_reader_split_drags(cx: &mut gpui::TestAppContext) {
        let cx = cx.add_empty_window();
        let view = cx.update(|_window, app| app.new(FileTree::new));
        cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, cx| {
            view.update(cx, |v, _cx| v.set_available_width(FILES_SURFACE_WIDTH_WIDE));
            div()
                .w(px(FILES_SURFACE_WIDTH_WIDE))
                .h_full()
                .child(view.clone())
        });

        let before = cx.debug_bounds("files-left").expect("the tree column");
        assert_eq!(
            before.size.width,
            px(TREE_WIDTH_DEFAULT),
            "the tree starts at its default"
        );

        // Grab the splitter and drag it 60px to the left, across the tree.
        let splitter = cx.debug_bounds("files-splitter").expect("the splitter");
        let grip = splitter.center();
        let target = point(px(f32::from(grip.x) - 60.0), grip.y);
        cx.simulate_mouse_down(grip, gpui::MouseButton::Left, gpui::Modifiers::none());
        // The drag layer is on screen from the next frame on.
        cx.draw(
            point(px(0.), px(0.)),
            size(px(1280.), px(800.)),
            |_, _cx| {
                div()
                    .w(px(FILES_SURFACE_WIDTH_WIDE))
                    .h_full()
                    .child(view.clone())
            },
        );
        assert!(
            cx.debug_bounds("files-drag-layer").is_some(),
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
            |_, _cx| {
                div()
                    .w(px(FILES_SURFACE_WIDTH_WIDE))
                    .h_full()
                    .child(view.clone())
            },
        );

        let after = cx.debug_bounds("files-left").expect("the tree column");
        assert_eq!(
            after.size.width,
            px(TREE_WIDTH_DEFAULT - 60.0),
            "the drag made the tree 60px narrower"
        );
        let reader = cx.debug_bounds("files-right").expect("the reader column");
        assert_eq!(
            reader.size.width,
            px(FILES_SURFACE_WIDTH_WIDE - (TREE_WIDTH_DEFAULT - 60.0) - SPLITTER_WIDTH_PX),
            "the reader took the room"
        );
    }

    #[gpui::test]
    fn the_tree_and_reader_split_stays_usable_at_1280_and_narrow(cx: &mut gpui::TestAppContext) {
        let cx = cx.add_empty_window();
        let view = cx.update(|_window, app| app.new(FileTree::new));

        // 1280px window: the app gives the surface 460px; the default split
        // leaves the reader exactly its minimum.
        cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, cx| {
            view.update(cx, |v, _cx| v.set_available_width(FILES_SURFACE_WIDTH_WIDE));
            div()
                .w(px(FILES_SURFACE_WIDTH_WIDE))
                .h_full()
                .child(view.clone())
        });
        let reader = cx.debug_bounds("files-right").expect("the reader at 1280");
        assert!(
            reader.size.width >= px(READER_WIDTH_MIN),
            "the reader keeps its minimum at 1280 (got {})",
            reader.size.width
        );

        // Narrow: less room; the tree yields before the reader does.
        cx.draw(point(px(0.), px(0.)), size(px(800.), px(600.)), |_, cx| {
            view.update(cx, |v, _cx| v.set_available_width(400.0));
            div().w(px(400.0)).h_full().child(view.clone())
        });
        let tree = cx.debug_bounds("files-left").expect("the tree at narrow");
        assert_eq!(tree.size.width, px(152.0), "the tree yields first");
        let reader = cx
            .debug_bounds("files-right")
            .expect("the reader at narrow");
        assert!(
            reader.size.width >= px(READER_WIDTH_MIN),
            "the reader keeps its minimum at the narrow breakpoint (got {})",
            reader.size.width
        );
    }

    #[gpui::test]
    fn a_lens_switch_keeps_the_open_file_and_the_expansion(cx: &mut gpui::TestAppContext) {
        let cx = cx.add_empty_window();
        let view = cx.update(|_window, app| app.new(FileTree::new));
        cx.update(|_window, app| {
            view.update(app, |v, cx| {
                v.apply_listing(
                    "/proj".to_string(),
                    listing_json("/proj", &["src"], json!([{"name": "main.rs", "size": 5}])),
                );
                v.reveal(cx, "src/main.rs".to_string());
            });
            let selected = view.read(app).selected_file().map(str::to_string);
            let expanded_had_src = view.read(app).expanded().any(|p| p == "/proj/src");
            let reader_path = view
                .read(app)
                .file_reader()
                .read(app)
                .path()
                .map(str::to_string);
            assert_eq!(selected.as_deref(), Some("/proj/src/main.rs"));
            assert!(expanded_had_src, "the folder is expanded");
            assert_eq!(reader_path.as_deref(), Some("/proj/src/main.rs"));

            // Away and back: the lens switch must not reset the tree's state.
            view.update(app, |v, cx| v.set_visible(cx, false));
            view.update(app, |v, cx| v.set_visible(cx, true));
            let v = view.read(app);
            assert_eq!(
                v.selected_file(),
                Some("/proj/src/main.rs"),
                "the open file survives the lens switch"
            );
            assert!(
                v.expanded().any(|p| p == "/proj/src"),
                "the expansion survives the lens switch"
            );
            assert_eq!(
                v.file_reader().read(app).path(),
                Some("/proj/src/main.rs"),
                "the reader keeps the file"
            );
        });
    }
}
