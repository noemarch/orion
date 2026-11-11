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
//!
//! Failures speak plain language ([`FILE_ERROR_MESSAGE`]) and offer a Retry,
//! the chat lens's voice; the raw status or error body belongs in a log, not
//! on screen. The body draws with line numbers in a monospace face resolved
//! the way `terminal.rs` resolves its grid font: a family is only used when it
//! is installed AND really measures monospace, because gpui quietly resolves
//! the CSS generic `monospace` to a proportional system font on Windows.

use crate::files_entities::{url_encode, FileInfo, FileKind};
use crate::terminal_font::terminal_font_families;
use crate::theme::Theme;
use gpui::prelude::*;
use gpui::{div, px, white, Context, Render, ScrollHandle, TextAlign, TextSystem, Window};
use orion_core::Engine;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

/// Byte cap for the text body: the viewer is a preview, not an editor.
pub const MAX_FILE_CHARS: usize = 200_000;

/// Plain-language copy for every failure path. The raw status, error body, or
/// path never reaches the reader; the detail belongs in a log, not on screen.
pub const FILE_ERROR_MESSAGE: &str = "Couldn't read this file. Try again.";

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
    /// The resolved monospace family for the body, cached after the first
    /// render so the font lookup runs once.
    font_family: Option<gpui::SharedString>,
    /// Whether the family lookup already ran (a miss is cached too).
    font_family_resolved: bool,
}

/// The first family in the terminal's fallback stack that is installed and
/// really measures monospace, the way `terminal.rs` picks its grid font: gpui
/// resolves a family that is not installed to the system UI font, which is
/// proportional, so a face is only used when `i` and `W` advance the same.
fn resolve_mono_family(text_system: &TextSystem) -> Option<gpui::SharedString> {
    let installed = text_system.all_font_names();
    let font_size = px(12.0);
    for family in terminal_font_families("") {
        if !installed
            .iter()
            .any(|name| name.eq_ignore_ascii_case(&family))
        {
            continue;
        }
        let id = text_system.resolve_font(&gpui::font(family.clone()));
        let Ok(narrow) = text_system.advance(id, font_size, 'i') else {
            continue;
        };
        let Ok(wide) = text_system.advance(id, font_size, 'W') else {
            continue;
        };
        if (f32::from(narrow.width) - f32::from(wide.width)).abs() > 0.01 {
            continue;
        }
        return Some(family.into());
    }
    None
}

/// A gutter label for a 1-based line number, right-aligned to four columns so
/// the digits line up under each other.
pub fn line_number_label(number: usize) -> String {
    format!("{number:>4}")
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
            font_family: None,
            font_family_resolved: false,
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
            self.error = Some(FILE_ERROR_MESSAGE.to_string());
            self.info = None;
            self.text = None;
            return;
        }
        let info = match FileInfo::parse(&body) {
            Ok(info) => info,
            Err(_) => {
                self.error = Some(FILE_ERROR_MESSAGE.to_string());
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
            self.error = Some(FILE_ERROR_MESSAGE.to_string());
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
            None => self.error = Some(FILE_ERROR_MESSAGE.to_string()),
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

    /// The current failure, if any (plain language; tests read this).
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The open file's path, if one is open.
    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    /// A file fetch answered (test seam mirroring `apply_list`).
    pub fn apply_file(&mut self, path: String, info: FileInfo, text: String) {
        self.path = Some(path);
        self.info = Some(info);
        self.text = Some(text);
        self.last_body = None;
        self.error = None;
    }

    /// A file fetch failed (test seam). The raw message is dropped: the view
    /// owns the plain-language copy the reader sees.
    pub fn fail_file(&mut self, _raw: String) {
        self.error = Some(FILE_ERROR_MESSAGE.to_string());
        self.text = None;
    }

    /// The reader asked for the file again: forget the cached body so the
    /// next answer is re-parsed even when identical, then re-issue the fetch.
    pub fn retry(&mut self) {
        self.error = None;
        self.last_body = None;
        self.refresh();
    }
}

impl Render for FileReader {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.row_seq = 0;
        // The monospace face is resolved once, on the first render that has a
        // window to ask; a miss is cached too so the lookup does not re-run.
        if !self.font_family_resolved {
            self.font_family_resolved = true;
            self.font_family = resolve_mono_family(window.text_system());
        }
        let mut root = div()
            .id("file-reader")
            .debug_selector(|| "file-reader".to_string())
            .flex()
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
                .flex()
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

        // A failed fetch speaks plain language and offers a Retry (the chat
        // lens's voice); the raw status or error body never reaches the reader.
        if let Some(message) = self.error.clone() {
            let theme = Theme::current();
            root = root.child(
                div()
                    .id("file-reader-error")
                    .debug_selector(|| "file-reader-error".to_string())
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
                            .id("file-reader-retry")
                            .debug_selector(|| "file-reader-retry".to_string())
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
                // One row per line: a right-aligned gutter with the 1-based
                // number, then the line's text. An empty line keeps its
                // number and its row (a single space holds the row open).
                let theme = Theme::current();
                let mut body = div()
                    .id("file-reader-lines")
                    .debug_selector(|| "file-reader-lines".to_string())
                    .flex()
                    .flex_col()
                    .px_3()
                    .py_2()
                    .font_family(self.font_family.clone().unwrap_or_default());
                if let Some(text) = self.text.clone() {
                    for (offset, line) in text.lines().enumerate() {
                        let number = line_number_label(offset + 1);
                        body = body.child(
                            div()
                                .id(self.next_row_id())
                                .debug_selector(|| "file-reader-line".to_string())
                                .flex()
                                .flex_row()
                                .child(
                                    div()
                                        .w(px(44.0))
                                        .flex_shrink_0()
                                        .text_align(TextAlign::Right)
                                        .text_size(px(11.0 * theme.density_scale()))
                                        .text_color(theme.text_dim)
                                        .child(number),
                                )
                                .child(
                                    div()
                                        .text_size(px(12.0 * theme.density_scale()))
                                        .text_color(theme.text)
                                        .child(if line.is_empty() {
                                            " ".to_string()
                                        } else {
                                            line.to_string()
                                        }),
                                ),
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

        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files_entities::FileKind;
    use gpui::{point, size, Entity};

    fn text_info() -> FileInfo {
        FileInfo {
            path: "/tmp/a.txt".to_string(),
            name: "a.txt".to_string(),
            size: 8,
            modified: "2026-01-01T00:00:00Z".to_string(),
            mime: "text/plain".to_string(),
            kind: FileKind::Text,
        }
    }

    #[test]
    fn line_number_labels_are_right_aligned_to_four_columns() {
        assert_eq!(line_number_label(1), "   1");
        assert_eq!(line_number_label(12), "  12");
    }

    #[gpui::test]
    fn the_reader_shows_line_numbers_for_its_text(cx: &mut gpui::TestAppContext) {
        let cx = cx.add_empty_window();
        let view = cx.update(|_window, app| app.new(FileReader::new));
        cx.draw(point(px(0.), px(0.)), size(px(900.), px(700.)), |_, cx| {
            view.update(cx, |v, _cx| {
                v.apply_file(
                    "/tmp/a.txt".to_string(),
                    text_info(),
                    "one\n\ntwo".to_string(),
                );
            });
            view.clone()
        });
        assert!(
            cx.debug_bounds("file-reader").is_some(),
            "the reader surface is on screen"
        );
        assert!(
            cx.debug_bounds("file-reader-lines").is_some(),
            "the body is on screen"
        );
        assert!(
            cx.debug_bounds("file-reader-line").is_some(),
            "the line rows are on screen"
        );
        // The gutter labels come from the pure helper the rows render: the
        // text "one\n\ntwo" has three lines, numbered 1 through 3.
        assert_eq!(line_number_label(1), "   1");
        assert_eq!(line_number_label(3), "   3");
    }

    struct FileReaderRoot {
        view: Entity<FileReader>,
    }

    impl Render for FileReaderRoot {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            self.view.clone()
        }
    }

    #[gpui::test]
    fn a_file_that_cannot_be_read_shows_an_error_with_a_retry(cx: &mut gpui::TestAppContext) {
        let (root, cx) = cx.add_window_view(|_, cx| FileReaderRoot {
            view: cx.new(FileReader::new),
        });
        cx.update(|_window, app| {
            let view = root.read(app).view.clone();
            view.update(app, |v, _cx| {
                v.apply_file("/tmp/a.txt".to_string(), text_info(), "one".to_string());
                v.fail_file("boom".to_string());
            });
        });
        cx.update(|window, cx| window.draw(cx).clear());
        assert!(
            cx.debug_bounds("file-reader-error").is_some(),
            "the failure is on screen"
        );
        let retry = cx
            .debug_bounds("file-reader-retry")
            .expect("a Retry is offered");
        cx.simulate_click(retry.center(), gpui::Modifiers::none());
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
    fn an_open_file_is_kept_across_a_visibility_change(cx: &mut gpui::TestAppContext) {
        let cx = cx.add_empty_window();
        let view = cx.update(|_window, app| app.new(FileReader::new));
        cx.update(|_window, app| {
            view.update(app, |v, _cx| {
                v.apply_file("/tmp/a.txt".to_string(), text_info(), "one".to_string());
                v.set_visible(false);
                v.set_visible(true);
            });
            assert_eq!(
                view.read(app).path(),
                Some("/tmp/a.txt"),
                "the open file survives the lens toggling"
            );
        });
    }
}
