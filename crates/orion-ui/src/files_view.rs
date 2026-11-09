//! File tree view (P3): one directory at a time, from the engine wire seam.
//!
//! `GET /api/workspace/directories?pane_id=…&path=…&files=1` through
//! `Engine::http` — in-process, no HTTP hop. Clicking a folder descends;
//! parent/home rows go back up. Clicking a file opens it in the
//! [`FileReader`] child. Nothing is kept but the folder shown now.
//!
//! Polling: 2s timer like `ChatView` (the engine already owns `notify`
//! watchers; GPUI views poll rather than adding a second watcher to
//! `orion-ui`), and only while visible (the Changes lens is active).

use crate::file_reader::FileReader;
use crate::files_entities::{join_path, url_encode, DirectoryListing};
use crate::theme::Theme;
use gpui::prelude::*;
use gpui::{div, px, white, Context, Entity, Render, ScrollHandle, Window};
use orion_core::engine::pane::Pane;
use orion_core::Engine;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

pub struct FileTree {
    engine: Option<Arc<Engine>>,
    pane_id: String,
    current_path: Option<String>,
    listing: Option<DirectoryListing>,
    file_reader: Entity<FileReader>,
    error: Option<String>,
    visible: bool,
    scroll_handle: ScrollHandle,
    row_seq: usize,
    last_body: Option<String>,
}

impl FileTree {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let file_reader = cx.new(FileReader::new);
        let view = Self {
            engine: None,
            pane_id: "pane-1".to_string(),
            current_path: None,
            listing: None,
            file_reader,
            error: None,
            visible: false,
            scroll_handle: ScrollHandle::new(),
            row_seq: 0,
            last_body: None,
        };
        view.start_poll(cx);
        view
    }

    fn next_row_id(&mut self) -> usize {
        let id = self.row_seq;
        self.row_seq += 1;
        id
    }

    /// Wire the engine + pane once they exist; starts at the pane's own cwd.
    /// The file reader child is attached to the same engine + pane_id.
    pub fn attach(
        &mut self,
        cx: &mut Context<Self>,
        engine: Arc<Engine>,
        pane: Arc<Pane>,
        pane_id: String,
    ) {
        self.engine = Some(engine.clone());
        self.pane_id = pane_id.clone();
        self.current_path = Some(pane.cwd.to_string_lossy().to_string());
        let reader = self.file_reader.clone();
        reader.update(cx, |reader, _cx| reader.attach(engine, pane_id));
        self.refresh();
    }

    /// Whether the 2s poll runs (the Changes lens is active).
    pub fn set_visible(&mut self, cx: &mut Context<Self>, visible: bool) {
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

    /// Go to one directory (absolute, or `~`-home-relative as the wire takes it).
    pub fn go_to(&mut self, path: String) {
        self.current_path = Some(path);
        self.last_body = None;
        self.refresh();
    }

    /// Fetch the current directory through the wire seam (`Engine::http`).
    pub fn refresh(&mut self) {
        let Some(engine) = self.engine.clone() else {
            return;
        };
        let mut path = format!(
            "/api/workspace/directories?pane_id={}",
            url_encode(&self.pane_id)
        );
        if let Some(current) = &self.current_path {
            path.push_str(&format!("&path={}", url_encode(current)));
        }
        path.push_str("&files=1");
        let (status, body) = engine.http("GET", &path, &Value::Null);
        if status != 200 {
            self.error = Some(format!("directory fetch failed ({status})"));
            return;
        }
        let encoded = body.to_string();
        if self.last_body.as_deref() == Some(encoded.as_str()) {
            return;
        }
        self.last_body = Some(encoded);
        match DirectoryListing::parse(&body) {
            Ok(listing) => {
                self.current_path = Some(listing.path.clone());
                self.listing = Some(listing);
                self.error = None;
            }
            Err(error) => self.error = Some(format!("directory parse failed: {error}")),
        }
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
}

impl Render for FileTree {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.row_seq = 0;
        let mut tree = div()
            .id("file-tree")
            .flex_col()
            .size_full()
            .bg(Theme::current().surface_base)
            .text_color(white());

        let path_label = self
            .listing
            .as_ref()
            .map(|l| l.path.clone())
            .unwrap_or_else(|| self.current_path.clone().unwrap_or_else(|| "…".to_string()));
        tree = tree.child(
            div()
                .id(self.next_row_id())
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .px_3()
                .py_1()
                .text_size(px(11.0 * Theme::current().density_scale()))
                .text_color(Theme::current().text_dim)
                .child(path_label),
        );

        if let Some(error) = self.error.clone() {
            tree = tree.child(
                div()
                    .id(self.next_row_id())
                    .px_3()
                    .py_1()
                    .text_size(px(12.0 * Theme::current().density_scale()))
                    .text_color(Theme::current().danger_text)
                    .child(error.clone()),
            );
        }

        if let Some(listing) = self.listing.clone() {
            let mut nav = div()
                .id(self.next_row_id())
                .flex_row()
                .gap(px(4.0))
                .px_3()
                .py_1();
            if let Some(parent) = &listing.parent {
                let parent = parent.clone();
                nav = nav.child(
                    div()
                        .id(self.next_row_id())
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(Theme::current().surface_hover)
                        .text_size(px(12.0 * Theme::current().density_scale()))
                        .child("↑ Parent")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.go_to(parent.clone());
                            cx.notify();
                        })),
                );
            }
            let home = listing.home.clone();
            nav = nav.child(
                div()
                    .id(self.next_row_id())
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(Theme::current().surface_hover)
                    .text_size(px(12.0 * Theme::current().density_scale()))
                    .child("~ Home")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.go_to(home.clone());
                        cx.notify();
                    })),
            );
            tree = tree.child(nav);

            let mut rows = div().id(self.next_row_id()).flex_col().px_3().py_1();
            for name in &listing.directories {
                let child = join_path(&listing.path, name);
                let label = format!("▸ {name}");
                rows = rows.child(
                    div()
                        .id(self.next_row_id())
                        .flex_row()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .text_size(px(12.0 * Theme::current().density_scale()))
                        .text_color(Theme::current().primary)
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.go_to(child.clone());
                            cx.notify();
                        })),
                );
            }
            for file in listing.file_entries() {
                let child = join_path(&listing.path, &file.name);
                let label = format!("{}  ({} bytes)", file.name, file.size);
                rows = rows.child(
                    div()
                        .id(self.next_row_id())
                        .flex_row()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .text_size(px(12.0 * Theme::current().density_scale()))
                        .text_color(Theme::current().text)
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let path = child.clone();
                            let reader = this.file_reader.clone();
                            reader.update(cx, |reader, _cx| reader.open(path));
                            cx.notify();
                        })),
                );
            }
            if listing.truncated {
                rows = rows.child(
                    div()
                        .id(self.next_row_id())
                        .text_size(px(11.0 * Theme::current().density_scale()))
                        .text_color(Theme::current().text_dim)
                        .child("Showing the first entries; go deeper to narrow."),
                );
            }
            if listing.directories.is_empty() && listing.file_entries().is_empty() {
                rows = rows.child(
                    div()
                        .id(self.next_row_id())
                        .text_size(px(12.0 * Theme::current().density_scale()))
                        .text_color(Theme::current().text_dim)
                        .child("Nothing here"),
                );
            }
            tree = tree.child(
                div()
                    .id("file-tree-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll_handle)
                    .child(rows),
            );
        }

        div()
            .id("files-surface")
            .flex_row()
            .size_full()
            .child(div().id("files-left").flex_col().w(px(320.0)).child(tree))
            .child(
                div()
                    .id("files-right")
                    .flex_col()
                    .flex_1()
                    .child(self.file_reader.clone()),
            )
    }
}
