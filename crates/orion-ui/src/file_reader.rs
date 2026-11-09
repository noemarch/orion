//! File reader view (P3): one file's text, plainly (mono, no syntax highlight).
//!
//! Text comes from `Engine::http("GET", "/api/fs/file?pane_id=…&path=…")`,
//! whose in-process body is `{ "__raw_text__": <text> }` (the HTTP server
//! unwraps it to the raw file bytes; the view reads the `Value` directly).
//! Kind/size/modified come from `GET /api/fs/stat` for the header row.
//! Binary files show a stub row; images show a placeholder row (no image
//! decode in P3).
//!
//! Polling: the reader polls on the shared 2s timer like `ChatView` — the
//! engine already owns `notify` watchers for session-changed, and GPUI views
//! poll rather than adding a second watcher to `orion-ui`. The poll only
//! runs while the reader is visible (the Changes lens is active).

use crate::files_entities::{url_encode, FileInfo, FileKind};
use crate::theme::Theme;
use gpui::prelude::*;
use gpui::{div, px, white, Context, Render, ScrollHandle, Window};
use orion_core::Engine;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

/// Byte cap for the text body: the viewer is a preview, not an editor.
pub const MAX_FILE_CHARS: usize = 200_000;

pub struct FileReader {
    engine: Option<Arc<Engine>>,
    pane_id: String,
    path: Option<String>,
    info: Option<FileInfo>,
    text: Option<String>,
    error: Option<String>,
    visible: bool,
    scroll_handle: ScrollHandle,
    row_seq: usize,
    last_body: Option<String>,
}

impl FileReader {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let view = Self {
            engine: None,
            pane_id: "pane-1".to_string(),
            path: None,
            info: None,
            text: None,
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

    /// Wire the engine once it exists (called from `app.rs` after spawn).
    pub fn attach(&mut self, engine: Arc<Engine>, pane_id: String) {
        self.engine = Some(engine);
        self.pane_id = pane_id;
        self.refresh();
    }

    /// Whether the 2s poll runs (the Changes lens is active).
    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
        if visible {
            self.refresh();
        }
    }

    pub fn visible(&self) -> bool {
        self.visible
    }

    /// Open one file in this reader.
    pub fn open(&mut self, path: String) {
        self.path = Some(path);
        self.info = None;
        self.text = None;
        self.error = None;
        self.last_body = None;
        self.refresh();
    }

    /// Fetch stat then text through the wire seam (`Engine::http`).
    pub fn refresh(&mut self) {
        let Some(engine) = self.engine.clone() else {
            return;
        };
        let Some(path) = self.path.clone() else {
            return;
        };
        let stat_path = format!(
            "/api/fs/stat?pane_id={}&path={}",
            url_encode(&self.pane_id),
            url_encode(&path),
        );
        let (status, body) = engine.http("GET", &stat_path, &Value::Null);
        if status != 200 {
            self.error = Some(format!("file stat failed ({status}): {path}"));
            self.info = None;
            self.text = None;
            return;
        }
        let info = match FileInfo::parse(&body) {
            Ok(info) => info,
            Err(error) => {
                self.error = Some(format!("file stat parse failed: {error}"));
                return;
            }
        };
        let encoded = body.to_string();
        if self.last_body.as_deref() == Some(encoded.as_str()) && self.text.is_some() {
            return;
        }
        let is_binary = info.kind == FileKind::Binary;
        self.last_body = Some(encoded.clone());
        self.info = Some(info);
        self.error = None;
        if is_binary {
            self.text = None;
            return;
        }
        let file_path = format!(
            "/api/fs/file?pane_id={}&path={}",
            url_encode(&self.pane_id),
            url_encode(&path),
        );
        let (status, body) = engine.http("GET", &file_path, &Value::Null);
        if status != 200 {
            self.error = Some(format!("file read failed ({status}): {path}"));
            self.text = None;
            return;
        }
        match body.get("__raw_text__").and_then(Value::as_str) {
            Some(text) => {
                let cut = text.floor_char_boundary(MAX_FILE_CHARS.min(text.len()));
                let truncated = cut < text.len();
                self.text = Some(if truncated {
                    format!(
                        "{}\n… (preview truncated at {MAX_FILE_CHARS} chars)",
                        &text[..cut]
                    )
                } else {
                    text.to_string()
                });
            }
            None => self.error = Some(format!("file body had no text: {path}")),
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
}

impl Render for FileReader {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.row_seq = 0;
        let mut root = div()
            .id("file-reader")
            .flex_col()
            .size_full()
            .bg(Theme::current().surface_base)
            .text_color(white());

        let Some(path) = self.path.clone() else {
            return root.child(
                div()
                    .id(self.next_row_id())
                    .px_3()
                    .py_2()
                    .text_size(px(12.0 * Theme::current().density_scale()))
                    .text_color(Theme::current().text_dim)
                    .child("No file open — pick a file to read it."),
            );
        };

        root = root.child(
            div()
                .id(self.next_row_id())
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .px_3()
                .py_1()
                .text_size(px(11.0 * Theme::current().density_scale()))
                .text_color(Theme::current().text_dim)
                .child(path.clone()),
        );

        if let Some(info) = self.info.clone() {
            root = root.child(
                div()
                    .id(self.next_row_id())
                    .px_3()
                    .py_1()
                    .text_size(px(11.0 * Theme::current().density_scale()))
                    .text_color(Theme::current().text_dim)
                    .child(format!(
                        "{} · {} bytes · modified {} · {} · {}",
                        info.name,
                        info.size,
                        info.modified,
                        info.mime,
                        info.kind.as_str(),
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

        match self.info.as_ref().map(|info| info.kind) {
            Some(FileKind::Binary) => {
                root = root.child(
                    div()
                        .id(self.next_row_id())
                        .px_3()
                        .py_2()
                        .text_size(px(12.0 * Theme::current().density_scale()))
                        .text_color(Theme::current().text_dim)
                        .child("Binary file — no text preview."),
                );
            }
            Some(FileKind::Image) => {
                root = root.child(
                    div()
                        .id(self.next_row_id())
                        .px_3()
                        .py_2()
                        .text_size(px(12.0 * Theme::current().density_scale()))
                        .text_color(Theme::current().text_dim)
                        .child("Image — preview not decoded (P3)."),
                );
            }
            Some(FileKind::Video) | Some(FileKind::Audio) | Some(FileKind::Pdf) => {
                root = root.child(
                    div()
                        .id(self.next_row_id())
                        .px_3()
                        .py_2()
                        .text_size(px(12.0 * Theme::current().density_scale()))
                        .text_color(Theme::current().text_dim)
                        .child("Media file — no text preview."),
                );
            }
            Some(_) => {
                let mut body = div().id(self.next_row_id()).flex_col().px_3().py_2();
                if let Some(text) = self.text.clone() {
                    for line in text.lines() {
                        body = body.child(
                            div()
                                .id(self.next_row_id())
                                .text_size(px(12.0 * Theme::current().density_scale()))
                                .text_color(Theme::current().text)
                                .child(if line.is_empty() {
                                    " ".to_string()
                                } else {
                                    line.to_string()
                                }),
                        );
                    }
                } else if self.error.is_none() {
                    body = body.child(
                        div()
                            .id(self.next_row_id())
                            .text_size(px(12.0 * Theme::current().density_scale()))
                            .text_color(Theme::current().text_dim)
                            .child("Loading…"),
                    );
                }
                root = root.child(
                    div()
                        .id("file-reader-scroll")
                        .flex_1()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll_handle)
                        .child(body),
                );
            }
            None => {
                root = root.child(
                    div()
                        .id(self.next_row_id())
                        .px_3()
                        .py_2()
                        .text_size(px(12.0 * Theme::current().density_scale()))
                        .text_color(Theme::current().text_dim)
                        .child("Loading…"),
                );
            }
        }

        let _ = cx;
        root
    }
}
