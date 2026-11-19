//! Review pane: the right-hand PR/diff column (Herd-reference layout).
//!
//! The pane is a presentation over the same [`ChangesView`] the Changes lens
//! renders: it owns no repository state of its own. A header names the
//! comparison, a toolbar carries the real line totals, the tab strip lists
//! every changed file, and the body draws the selected file's patch as a
//! two-column grid (old line no, old code, new line no, new code).
//!
//! [`build_rows`] is the pure seam: it takes the classified patch lines and
//! pairs context lines on both sides, leaving an added line on the new side
//! and a deleted line on the old side. The renderer only styles what that
//! function decides, so the row geometry is testable without a window.

use crate::diff_view::{diff_line_numbers, ChangesView, DiffLine, PatchKind};
use crate::files_entities::{RepoDiffFile, RepoDiffFileList, RepoDiffFilePatch, RepoDiffStatus};
use crate::theme::Theme;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{Icon, IconName, Sizable};
use gpui_kit::*;
use gpui_kit::{
    div, px, Context, ElementId, Entity, Render, Rgba, ScrollHandle, Subscription, Window,
};

/// The monospace face the diff gutter, code and tab counts read in.
const MONO: &str = "JetBrains Mono";
/// Header strip height (px, before density scaling).
const HEADER_HEIGHT: f32 = 55.0;
/// Toolbar strip height (px, before density scaling).
const TOOLBAR_HEIGHT: f32 = 55.0;
/// File-tab strip height (px, before density scaling).
const TABS_HEIGHT: f32 = 78.0;
/// One file tab's width (px, before density scaling).
const TAB_WIDTH: f32 = 216.0;
/// Line-number gutter width (px, before density scaling).
const GUTTER_WIDTH: f32 = 34.0;
/// One code row's height (px, before density scaling).
const CODE_ROW_HEIGHT: f32 = 19.0;

// Spacing scale from DESIGN.md §4 (4px base). The Rust theme exposes density
// scaling but no spacing tokens, so keep the scale named and local instead of
// repeating bare literals across the view (matches sidebar.rs).
const SPACE_1: f32 = 4.0; // --space-1: tight icon and control gaps
const SPACE_2: f32 = 8.0; // --space-2: row and menu gaps
const SPACE_4: f32 = 16.0; // --space-4: section padding
/// Upper bound for one review tab: the review column's 320px minimum, so a
/// single tab never outgrows the column at 1280x800 with the sidebar shown.
const TAB_MAX_WIDTH: f32 = 320.0;

/// What one rendered row means to the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// Unchanged on both sides.
    Context,
    /// Present on the new side alone.
    Add,
    /// Present on the old side alone.
    Delete,
    /// A `@@` hunk header: a full-width strip, not a grid row.
    Hunk,
}

/// One two-column diff row: `old_*` is the left half, `new_*` the right.
///
/// Added lines leave `old_no`/`old_text` empty; deleted lines leave
/// `new_no`/`new_text` empty; context lines fill both sides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRow {
    pub old_no: Option<usize>,
    pub old_text: String,
    pub new_no: Option<usize>,
    pub new_text: String,
    pub kind: RowKind,
}

/// Pair classified patch lines into the review grid's rows.
///
/// Git headers (`diff --git`, `index`, …) are dropped: the card above already
/// names the file. Hunk headers are kept as their own row so the body renders
/// them in place; every other line lands on the side its number belongs to.
pub fn build_rows(lines: &[DiffLine]) -> Vec<ReviewRow> {
    let mut rows = Vec::new();
    for line in lines {
        match line.kind {
            PatchKind::Header => {}
            PatchKind::Hunk => rows.push(ReviewRow {
                old_no: None,
                old_text: line.text.clone(),
                new_no: None,
                new_text: String::new(),
                kind: RowKind::Hunk,
            }),
            PatchKind::Context => {
                let text = strip_marker(&line.text, ' ');
                rows.push(ReviewRow {
                    old_no: line.old_no.map(|number| number as usize),
                    old_text: text.clone(),
                    new_no: line.new_no.map(|number| number as usize),
                    new_text: text,
                    kind: RowKind::Context,
                });
            }
            PatchKind::Add => rows.push(ReviewRow {
                old_no: None,
                old_text: String::new(),
                new_no: line.new_no.map(|number| number as usize),
                new_text: strip_marker(&line.text, '+'),
                kind: RowKind::Add,
            }),
            PatchKind::Delete => rows.push(ReviewRow {
                old_no: line.old_no.map(|number| number as usize),
                old_text: strip_marker(&line.text, '-'),
                new_no: None,
                new_text: String::new(),
                kind: RowKind::Delete,
            }),
        }
    }
    rows
}

/// The code text without its one-character unified-diff marker.
fn strip_marker(text: &str, marker: char) -> String {
    text.strip_prefix(marker).unwrap_or(text).to_string()
}

/// The last path segment: a tab shows the file's name, the card its full path.
fn file_basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Shorten a commit hash for display; a ref name passes through unchanged.
fn short_ref(reference: &str) -> &str {
    if reference.len() > 7 && reference.chars().all(|c| c.is_ascii_hexdigit()) {
        &reference[..7]
    } else {
        reference
    }
}

/// The header's comparison text, from real data only: the compared commits
/// when the scope names them, otherwise the checked-out branch.
fn header_label(list: Option<&RepoDiffFileList>, base: Option<&str>, head: Option<&str>) -> String {
    if let (Some(base), Some(head)) = (base, head) {
        return format!("{} vs {}", short_ref(base), short_ref(head));
    }
    match list {
        Some(list) => {
            if let Some(branch) = list.repository.branch.as_deref() {
                branch.to_string()
            } else if list.repository.detached {
                "Detached HEAD".to_string()
            } else if list.repository.unborn {
                "No commits yet".to_string()
            } else {
                "No branch".to_string()
            }
        }
        None => "No repository".to_string(),
    }
}

/// Sum the real additions and deletions in the changed-file list. `None` when
/// no file carried counts (a binary-only change), so the toolbar stays silent
/// instead of claiming `+0 -0`.
fn line_totals(files: &[RepoDiffFile]) -> Option<(u64, u64)> {
    let mut known = false;
    let mut added = 0;
    let mut deleted = 0;
    for file in files {
        if let Some(count) = file.additions {
            added += count;
            known = true;
        }
        if let Some(count) = file.deletions {
            deleted += count;
            known = true;
        }
    }
    known.then_some((added, deleted))
}

/// The toolbar's scope chip label, read from what the list says it compared.
fn scope_label(list: Option<&RepoDiffFileList>) -> &'static str {
    match list.and_then(|list| list.scope.as_deref()) {
        Some("staged") => "Staged changes",
        Some("commit") => "Commit changes",
        _ => "All changes",
    }
}

/// The color a changed file's status mark reads in.
fn status_color(status: RepoDiffStatus, theme: &Theme) -> Rgba {
    match status {
        RepoDiffStatus::Added | RepoDiffStatus::Untracked => theme.diff_add,
        RepoDiffStatus::Deleted => theme.diff_del,
        RepoDiffStatus::Unmerged => theme.status_blocked,
        RepoDiffStatus::Modified => theme.warning_amber,
        _ => theme.text_muted,
    }
}

/// The review column: one [`ChangesView`]'s data, laid out as a PR/diff pane.
pub struct ReviewPane {
    /// The view that owns the repository data this pane presents.
    changes: Entity<ChangesView>,
    /// The tab the reader last chose; `render` prefers the patch's own file
    /// when the Changes lens moved the selection instead.
    selected: usize,
    scroll: ScrollHandle,
    /// Re-render when the Changes view answers a fetch or changes selection.
    _changes_subscription: Subscription,
}

impl ReviewPane {
    /// Build the pane over the shared Changes view.
    pub fn new(changes: Entity<ChangesView>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&changes, |_pane, _changes, cx| cx.notify());
        Self {
            changes,
            selected: 0,
            scroll: ScrollHandle::new(),
            _changes_subscription: subscription,
        }
    }

    /// Ask the Changes view for fresh list and patch data.
    fn reanalyze(&mut self, cx: &mut Context<Self>) {
        self.changes.update(cx, |view, view_cx| {
            view.reanalyze();
            view_cx.notify();
        });
        cx.notify();
    }

    /// Select one changed file, the same selection a click in the file list
    /// makes, so the body follows the tab.
    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        self.selected = index;
        self.changes.update(cx, |view, view_cx| {
            view.select_file_at(index);
            view_cx.notify();
        });
        cx.notify();
    }

    fn render_header(&self, label: String, theme: &Theme) -> impl IntoElement {
        let scale = theme.density_scale();
        div()
            .id("review-header")
            .debug_selector(|| "review-header".to_string())
            .flex()
            .flex_row()
            .items_center()
            .gap(px(9.0 * scale))
            .h(px(HEADER_HEIGHT * scale))
            .flex_shrink_0()
            .px(px(theme.space_5()))
            .border_b_1()
            .border_color(theme.border_subtle())
            .text_size(px(theme.font_ui_body()))
            .text_color(theme.text_primary())
            .child(
                Icon::new(IconName::FileText)
                    .small()
                    .text_color(theme.token_text_dim()),
            )
            .child(div().min_w(px(0.0)).truncate().child(label))
    }

    fn render_toolbar(
        &self,
        list: Option<&RepoDiffFileList>,
        files: &[RepoDiffFile],
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let scale = theme.density_scale();
        let mut row = div()
            .id("review-toolbar")
            .debug_selector(|| "review-toolbar".to_string())
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0 * scale))
            .h(px(TOOLBAR_HEIGHT * scale))
            .flex_shrink_0()
            .px(px(theme.space_5()))
            .text_size(px(theme.font_ui_secondary()))
            .text_color(theme.text_primary())
            .child(
                Button::new("review-scope")
                    .secondary()
                    .compact()
                    .label(scope_label(list))
                    .icon(IconName::ChevronDown),
            );

        if let Some((added, deleted)) = line_totals(files) {
            let mut totals = div()
                .id("review-totals")
                .flex()
                .flex_row()
                .items_center()
                .gap(px(theme.space_2()))
                .child(
                    div()
                        .text_color(theme.token_diff_add_text())
                        .child(format!("+{added}")),
                )
                .child(
                    div()
                        .text_color(theme.diff_remove_text())
                        .child(format!("-{deleted}")),
                );
            if let Some(total) = list.map(|list| list.total.unwrap_or(list.files.len() as u64)) {
                totals = totals.child(
                    div()
                        .id("review-progress")
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(SPACE_2 * scale))
                        .text_color(theme.token_text_muted())
                        .child(
                            div()
                                .w(px(11.0 * scale))
                                .h(px(11.0 * scale))
                                .rounded_full()
                                .border_1()
                                .border_color(theme.token_text_dim()),
                        )
                        .child(format!("0/{total}")),
                );
            }
            row = row.child(totals);
        }

        row.child(div().flex_1())
            .child(
                Button::new("review-reanalyze")
                    .secondary()
                    .compact()
                    .label("Re-analyze")
                    .icon(IconName::RefreshCw)
                    .on_click(cx.listener(|this, _, _, cx| this.reanalyze(cx))),
            )
            .child(
                Button::new("review-settings")
                    .secondary()
                    .compact()
                    .icon(IconName::Settings),
            )
    }

    fn render_tabs(
        &self,
        files: &[RepoDiffFile],
        active: Option<usize>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let scale = theme.density_scale();
        let mut strip = div()
            .id("review-tabs")
            .debug_selector(|| "review-tabs".to_string())
            .flex()
            .flex_row()
            .flex_nowrap()
            .items_stretch()
            .h(px(TABS_HEIGHT * scale))
            .min_w_0()
            .flex_shrink_0()
            .border_b_1()
            .border_color(theme.border_subtle())
            .overflow_x_scrollbar()
            .id("review-tabs-scroll");

        for (index, file) in files.iter().enumerate() {
            let is_active = active == Some(index);
            let title_color = if is_active {
                theme.text_primary()
            } else {
                theme.token_text_muted()
            };

            let mut content = div()
                .relative()
                .flex()
                .flex_col()
                .gap(px(SPACE_1 * scale))
                .w_full()
                .h_full()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(SPACE_2 * scale))
                        .text_size(px(12.5 * scale))
                        .child(
                            Icon::new(IconName::FileText)
                                .small()
                                .text_color(theme.token_text_dim()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_color(title_color)
                                .child(file_basename(&file.path).to_string()),
                        ),
                );

            let mut stats = div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(SPACE_2 * scale))
                .font_family(MONO)
                .text_size(px(theme.font_mono()))
                .text_color(theme.token_text_dim());
            match (file.additions, file.deletions) {
                (Some(added), Some(deleted)) => {
                    stats = stats
                        .child(
                            div()
                                .text_color(theme.token_diff_add_text())
                                .child(format!("+{added}")),
                        )
                        .child(
                            div()
                                .text_color(theme.diff_remove_text())
                                .child(format!("-{deleted}")),
                        );
                }
                _ => {
                    let label = if file.binary == Some(true) {
                        "binary"
                    } else {
                        "—"
                    };
                    stats = stats.child(label);
                }
            }
            content = content.child(stats);

            if is_active {
                content = content.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .h(px(2.0 * scale))
                        .bg(theme.token_accent()),
                );
            }

            let mut tab = Button::new(ElementId::Name(format!("review-tab:{}", file.path).into()))
                .debug_selector(|| "review-tab".to_string())
                .text()
                .min_w(px(TAB_WIDTH * scale))
                .w(px(TAB_WIDTH * scale))
                .max_w(px(TAB_MAX_WIDTH * scale))
                .flex_shrink_0()
                .px(px(SPACE_4 * scale))
                .pt(px(SPACE_2 * scale))
                .pb(px(SPACE_2 * scale))
                .rounded(px(theme.radius_card()))
                .border_1()
                .border_color(theme.border_subtle())
                .on_click(cx.listener(move |this, _, _, cx| this.select(index, cx)));
            if is_active {
                tab = tab.bg(theme.selected_bg());
            }
            strip = strip.child(tab.child(content));
        }

        strip
    }

    fn render_row(
        &self,
        path: &str,
        sequence: usize,
        row: &ReviewRow,
        theme: &Theme,
    ) -> impl IntoElement {
        let scale = theme.density_scale();
        let line_key = row
            .new_no
            .or(row.old_no)
            .map(|number| number.to_string())
            .unwrap_or_else(|| format!("hunk-{sequence}"));
        let side = match row.kind {
            RowKind::Add => "add",
            RowKind::Delete => "del",
            RowKind::Context => "ctx",
            RowKind::Hunk => "hunk",
        };
        let id = ElementId::Name(format!("review-row:{path}:{side}:{line_key}").into());

        match row.kind {
            RowKind::Hunk => div()
                .id(id)
                .w_full()
                .px(px(theme.space_3()))
                .py(px(5.0 * scale))
                .bg(theme.token_diff_hunk_bg())
                .border_t_1()
                .border_b_1()
                .border_color(theme.border_subtle())
                .font_family(MONO)
                .text_size(px(theme.font_mono()))
                .text_color(theme.token_text_muted())
                .overflow_hidden()
                .whitespace_nowrap()
                .child(row.old_text.clone()),
            kind => {
                let added = kind == RowKind::Add;
                let deleted = kind == RowKind::Delete;
                let old_gutter_fill = if deleted {
                    theme.diff_remove_bg()
                } else {
                    theme.panel_bg()
                };
                let new_gutter_fill = if added {
                    theme.diff_add_bg()
                } else {
                    theme.panel_bg()
                };
                let old_fill = if deleted {
                    Some(theme.diff_remove_bg())
                } else if added {
                    Some(theme.panel_bg())
                } else {
                    None
                };
                let new_fill = if added {
                    Some(theme.diff_add_bg())
                } else if deleted {
                    Some(theme.panel_bg())
                } else {
                    None
                };
                let old_color = if deleted {
                    theme.diff_remove_text()
                } else if added {
                    theme.token_text_dim()
                } else {
                    theme.text_primary()
                };
                let new_color = if added {
                    theme.token_diff_add_text()
                } else if deleted {
                    theme.token_text_dim()
                } else {
                    theme.text_primary()
                };
                let old_text = if deleted {
                    format!("- {}", row.old_text)
                } else {
                    row.old_text.clone()
                };
                let new_text = if added {
                    format!("+ {}", row.new_text)
                } else {
                    row.new_text.clone()
                };

                div()
                    .id(id)
                    .flex()
                    .flex_row()
                    .w_full()
                    .child(gutter_cell(
                        row.old_no,
                        old_gutter_fill,
                        false,
                        theme,
                        scale,
                    ))
                    .child(code_cell(
                        old_text, old_fill, old_color, false, theme, scale,
                    ))
                    .child(gutter_cell(row.new_no, new_gutter_fill, true, theme, scale))
                    .child(code_cell(new_text, new_fill, new_color, true, theme, scale))
            }
        }
    }

    fn render_body(
        &self,
        list: Option<&RepoDiffFileList>,
        files: &[RepoDiffFile],
        patch: Option<&RepoDiffFilePatch>,
        active: Option<usize>,
        theme: &Theme,
    ) -> impl IntoElement {
        let scale = theme.density_scale();
        let mut content = div()
            .id("review-body-content")
            .flex()
            .flex_col()
            .px(px(22.0 * scale))
            .pt(px(theme.space_5() + theme.space_1()))
            .pb(px(theme.space_5() + theme.space_5() + theme.space_3()));

        let title = patch.map(|patch| patch.file.path.clone()).or_else(|| {
            active
                .and_then(|index| files.get(index))
                .map(|f| f.path.clone())
        });

        match (&title, list) {
            (Some(path), _) => {
                content = content.child(
                    div()
                        .id("review-title")
                        .debug_selector(|| "review-title".to_string())
                        .text_size(px(theme.font_title_lg()))
                        .text_color(theme.text_primary())
                        .child(path.clone()),
                );
            }
            (None, Some(_)) => {
                content = content.child(
                    div()
                        .id("review-empty")
                        .debug_selector(|| "review-empty".to_string())
                        .text_size(px(theme.font_ui_body()))
                        .text_color(theme.token_text_muted())
                        .child("Nothing changed — the working tree is clean."),
                );
                return content;
            }
            (None, None) => {
                content = content.child(
                    div()
                        .id("review-empty")
                        .debug_selector(|| "review-empty".to_string())
                        .text_size(px(theme.font_ui_body()))
                        .text_color(theme.token_text_muted())
                        .child("No changed files yet."),
                );
                return content;
            }
        }

        let Some(patch) = patch else {
            content = content.child(
                div()
                    .id("review-no-patch")
                    .debug_selector(|| "review-no-patch".to_string())
                    .mt(px(theme.space_5()))
                    .text_size(px(theme.font_ui_secondary()))
                    .text_color(theme.token_text_muted())
                    .child("No patch loaded for this file yet."),
            );
            return content;
        };

        let file = &patch.file;
        let mut head = div()
            .id("review-file-head")
            .debug_selector(|| "review-file-head".to_string())
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0 * scale))
            .h(px(36.0 * scale))
            .px(px(theme.space_3()))
            .bg(theme.selected_bg())
            .font_family(MONO)
            .text_size(px(theme.font_ui_secondary()))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_color(theme.text_primary())
                    .child(file.path.clone()),
            );
        let mut stats = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(theme.space_2()))
            .flex_shrink_0();
        match (file.additions, file.deletions) {
            (Some(added), Some(deleted)) => {
                stats = stats
                    .child(
                        div()
                            .text_color(theme.token_diff_add_text())
                            .child(format!("+{added}")),
                    )
                    .child(
                        div()
                            .text_color(theme.diff_remove_text())
                            .child(format!("-{deleted}")),
                    );
            }
            _ => {
                if file.binary == Some(true) {
                    stats = stats.child("binary");
                }
            }
        }
        head = head.child(stats).child(
            div()
                .text_size(px(theme.font_ui_secondary()))
                .px(px(5.0 * scale))
                .rounded(px(theme.radius_control()))
                .border_1()
                .border_color(theme.border_subtle())
                .bg(theme.hover_bg())
                .text_color(status_color(file.status, theme))
                .child(file.status.mark().to_string()),
        );

        let mut card = div()
            .id(ElementId::Name(format!("review-card:{}", file.path).into()))
            .flex()
            .flex_col()
            .mt(px(theme.space_5()))
            .border_1()
            .border_color(theme.border_subtle())
            .rounded(px(theme.radius_card()))
            .overflow_hidden()
            .child(head);

        if let Some(text) = patch.patch_text() {
            let rows = build_rows(&diff_line_numbers(text));
            let mut grid = div().id("review-grid").flex().flex_col();
            for (sequence, row) in rows.iter().enumerate() {
                grid = grid.child(self.render_row(&file.path, sequence, row, theme));
            }
            card = card.child(grid);
        } else {
            let reason = patch
                .unavailable
                .clone()
                .unwrap_or_else(|| "no patch".to_string());
            card = card.child(
                div()
                    .px(px(theme.space_3()))
                    .py(px(10.0 * scale))
                    .text_size(px(theme.font_ui_secondary()))
                    .text_color(theme.token_text_muted())
                    .child(format!("No patch ({reason}).")),
            );
        }

        content.child(card)
    }
}

/// One right-aligned line-number cell.
fn gutter_cell(
    number: Option<usize>,
    fill: Rgba,
    edge: bool,
    theme: &Theme,
    scale: f32,
) -> impl IntoElement {
    let mut cell = div()
        .w(px(GUTTER_WIDTH * scale))
        .h(px(CODE_ROW_HEIGHT * scale))
        .flex_shrink_0()
        .flex()
        .flex_row()
        .items_center()
        .justify_end()
        .pr(px(9.0 * scale))
        .bg(fill)
        .font_family(MONO)
        .text_size(px(theme.font_mono()))
        .text_color(theme.token_text_dim());
    if edge {
        cell = cell.border_l_1().border_color(theme.border_subtle());
    }
    cell.child(number.map(|number| number.to_string()).unwrap_or_default())
}

/// One code cell: the left half is the old file, the right half the new one.
fn code_cell(
    text: String,
    fill: Option<Rgba>,
    color: Rgba,
    edge: bool,
    theme: &Theme,
    scale: f32,
) -> impl IntoElement {
    let mut cell = div()
        .flex_1()
        .min_w(px(0.0))
        .h(px(CODE_ROW_HEIGHT * scale))
        .flex()
        .flex_row()
        .items_center()
        .px(px(theme.space_2()))
        .overflow_hidden()
        .font_family(MONO)
        .text_size(px(theme.font_mono()))
        .text_color(color)
        .whitespace_nowrap();
    if let Some(fill) = fill {
        cell = cell.bg(fill);
    }
    if edge {
        cell = cell.border_l_1().border_color(theme.border_subtle());
    }
    cell.child(text)
}

impl Render for ReviewPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current();
        let (list, patch, base, head) = {
            let changes = self.changes.read(cx);
            (
                changes.file_list().cloned(),
                changes.current_patch().cloned(),
                changes.base().map(str::to_string),
                changes.head().map(str::to_string),
            )
        };
        let files = list
            .as_ref()
            .map(|list| list.files.clone())
            .unwrap_or_default();
        let active = patch
            .as_ref()
            .and_then(|patch| files.iter().position(|file| file.path == patch.file.path))
            .or_else(|| (self.selected < files.len()).then_some(self.selected));
        let label = header_label(list.as_ref(), base.as_deref(), head.as_deref());

        div()
            .id("review-pane")
            .debug_selector(|| "review-pane".to_string())
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .bg(theme.panel_bg())
            .rounded(px(theme.radius_panel()))
            .text_color(theme.text)
            .child(self.render_header(label, &theme))
            .child(self.render_toolbar(list.as_ref(), &files, &theme, cx))
            .child(self.render_tabs(&files, active, &theme, cx))
            .child(
                div()
                    .id("review-body")
                    .debug_selector(|| "review-body".to_string())
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(self.render_body(list.as_ref(), &files, patch.as_ref(), active, &theme)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(patch: &str) -> Vec<ReviewRow> {
        build_rows(&diff_line_numbers(patch))
    }

    #[::core::prelude::v1::test]
    fn context_lines_fill_both_sides() {
        let rows = rows("@@ -1,2 +1,2 @@\n same\n next\n");
        assert_eq!(rows.len(), 3, "one hunk header and two context rows");
        assert_eq!(rows[0].kind, RowKind::Hunk);
        assert_eq!(rows[0].old_text, "@@ -1,2 +1,2 @@");
        assert_eq!(
            rows[1],
            ReviewRow {
                old_no: Some(1),
                old_text: "same".to_string(),
                new_no: Some(1),
                new_text: "same".to_string(),
                kind: RowKind::Context,
            }
        );
        assert_eq!(rows[2].old_no, Some(2));
        assert_eq!(rows[2].new_no, Some(2));
    }

    #[::core::prelude::v1::test]
    fn added_lines_land_on_the_new_side_only() {
        let rows = rows("@@ -1 +1,2 @@\n context\n+added\n");
        let added = &rows[2];
        assert_eq!(added.kind, RowKind::Add);
        assert_eq!(added.old_no, None);
        assert_eq!(added.old_text, "");
        assert_eq!(added.new_no, Some(2));
        assert_eq!(added.new_text, "added");
        assert_eq!(rows[1].kind, RowKind::Context);
    }

    #[::core::prelude::v1::test]
    fn deleted_lines_land_on_the_old_side_only() {
        let rows = rows("@@ -1,2 +1 @@\n context\n-removed\n");
        let deleted = &rows[2];
        assert_eq!(deleted.kind, RowKind::Delete);
        assert_eq!(deleted.old_no, Some(2));
        assert_eq!(deleted.old_text, "removed");
        assert_eq!(deleted.new_no, None);
        assert_eq!(deleted.new_text, "");
    }

    #[::core::prelude::v1::test]
    fn a_mixed_hunk_keeps_order_and_numbers_and_drops_git_headers() {
        let rows = rows(concat!(
            "diff --git a/x b/x\n",
            "index 111..222 100644\n",
            "--- a/x\n",
            "+++ b/x\n",
            "@@ -1,3 +1,3 @@\n",
            " a\n",
            "-b\n",
            "+B\n",
            " c\n",
        ));
        assert_eq!(rows.len(), 5, "git headers are dropped");
        assert_eq!(
            rows[0],
            ReviewRow {
                old_no: None,
                old_text: "@@ -1,3 +1,3 @@".to_string(),
                new_no: None,
                new_text: String::new(),
                kind: RowKind::Hunk,
            }
        );
        assert_eq!(
            rows[1],
            ReviewRow {
                old_no: Some(1),
                old_text: "a".to_string(),
                new_no: Some(1),
                new_text: "a".to_string(),
                kind: RowKind::Context,
            }
        );
        assert_eq!(
            rows[2],
            ReviewRow {
                old_no: Some(2),
                old_text: "b".to_string(),
                new_no: None,
                new_text: String::new(),
                kind: RowKind::Delete,
            }
        );
        assert_eq!(
            rows[3],
            ReviewRow {
                old_no: None,
                old_text: String::new(),
                new_no: Some(2),
                new_text: "B".to_string(),
                kind: RowKind::Add,
            }
        );
        assert_eq!(
            rows[4],
            ReviewRow {
                old_no: Some(3),
                old_text: "c".to_string(),
                new_no: Some(3),
                new_text: "c".to_string(),
                kind: RowKind::Context,
            }
        );
    }

    #[::core::prelude::v1::test]
    fn tab_headers_use_the_basename_and_real_totals() {
        assert_eq!(file_basename("packages/cli/src/index.ts"), "index.ts");
        assert_eq!(file_basename("index.ts"), "index.ts");

        let list = RepoDiffFileList::parse(&serde_json::json!({
            "repository": {
                "root": "/r", "branch": "main", "head": "abc",
                "detached": false, "unborn": false,
            },
            "scope": "workspace",
            "files": [
                {"path": "a.rs", "old_path": null, "status": "modified",
                 "additions": 3, "deletions": 1, "binary": null},
                {"path": "b.png", "old_path": null, "status": "added",
                 "additions": null, "deletions": null, "binary": true},
            ],
            "total": 2, "truncated": false,
        }))
        .expect("parse");
        assert_eq!(header_label(Some(&list), None, None), "main");
        assert_eq!(
            header_label(Some(&list), Some("abc1234def"), Some("9876543abc")),
            "abc1234 vs 9876543"
        );
        assert_eq!(line_totals(&list.files), Some((3, 1)));
        assert_eq!(scope_label(Some(&list)), "All changes");
    }

    crate::orion_test!(
        fn eight_file_tabs_scroll_inside_the_review_column(cx: &mut gpui_kit::TestAppContext) {
            use gpui_kit::{point, size};

            let files = serde_json::json!([
                {"path": "Cargo.lock", "old_path": null, "status": "modified", "additions": 3, "deletions": 1, "binary": null},
                {"path": "Cargo.toml", "old_path": null, "status": "modified", "additions": 5, "deletions": 2, "binary": null},
                {"path": "README.md", "old_path": null, "status": "modified", "additions": 1, "deletions": 0, "binary": null},
                {"path": "build.rs", "old_path": null, "status": "modified", "additions": 7, "deletions": 3, "binary": null},
                {"path": "src/main.rs", "old_path": null, "status": "modified", "additions": 2, "deletions": 1, "binary": null},
                {"path": "src/lib.rs", "old_path": null, "status": "modified", "additions": 9, "deletions": 4, "binary": null},
                {"path": "tests/ui.rs", "old_path": null, "status": "modified", "additions": 6, "deletions": 0, "binary": null},
                {"path": "docs/adr/0001.md", "old_path": null, "status": "modified", "additions": 4, "deletions": 2, "binary": null},
            ]);
            let list = RepoDiffFileList::parse(&serde_json::json!({
                "repository": {
                    "root": "/r", "branch": "main", "head": "abc",
                    "detached": false, "unborn": false,
                },
                "scope": "workspace",
                "files": files,
                "total": 8, "truncated": false,
            }))
            .expect("parse");
            assert_eq!(list.files.len(), 8);

            let cx = cx.add_empty_window();
            let changes = cx.update(|_window, app| app.new(ChangesView::new));
            cx.update(|_window, app| {
                changes.update(app, |view, _cx| view.apply_list(list));
            });
            let pane = cx.update(|_window, app| app.new(|cx| ReviewPane::new(changes.clone(), cx)));

            let layout = crate::shell_model::reference_layout(1400.0);
            cx.draw(
                point(px(0.), px(0.)),
                size(px(1400.), px(800.)),
                |_, _cx| {
                    div()
                        .size_full()
                        .flex()
                        .flex_row()
                        .child(
                            div()
                                .id("sidebar")
                                .h_full()
                                .w(px(layout.sidebar_w))
                                .min_w(px(0.0))
                                .flex_shrink_0(),
                        )
                        .child(
                            div()
                                .id("conversation-column")
                                .h_full()
                                .w(px(layout.conversation_w))
                                .min_w(px(0.0))
                                .flex_shrink_0(),
                        )
                        .child(
                            div()
                                .id("review-column")
                                .h_full()
                                .flex_1()
                                .min_w(px(0.0))
                                .child(pane.clone()),
                        )
                },
            );

            let window_width = px(1400.0);
            let pane_bounds = cx.debug_bounds("review-pane").expect("review pane");
            let strip = cx.debug_bounds("review-tabs").expect("tab strip");
            let last_tab = cx.debug_bounds("review-tab").expect("last file tab");
            println!(
                "GEOMETRY window={:?} pane=[{:?}..{:?}] strip=[{:?}..{:?}] last_tab=[{:?}..{:?}]",
                window_width,
                pane_bounds.left(),
                pane_bounds.right(),
                strip.left(),
                strip.right(),
                last_tab.left(),
                last_tab.right(),
            );

            assert!(
                pane_bounds.right() <= window_width,
                "the review pane must stay inside the window"
            );
            assert!(
                last_tab.right() > pane_bounds.right(),
                "eight fixed-width tabs must overflow the pane, not widen it"
            );

            println!(
                "GEOMETRY pane.top={:?} strip.top={:?} strip_width={:?}",
                pane_bounds.top(),
                strip.top(),
                strip.size.width,
            );
            assert!(
                strip.size.width > pane_bounds.size.width,
                "eight tabs must form a scrollable content row wider than the pane"
            );
            assert!(
                cx.debug_bounds("scrollbar-overlay").is_some(),
                "the tab strip must own a horizontal scrollbar"
            );

            let narrow_layout = crate::shell_model::reference_layout(1260.0);
            cx.draw(
                point(px(0.), px(0.)),
                size(px(1260.), px(800.)),
                |_, _cx| {
                    div()
                        .size_full()
                        .flex()
                        .flex_row()
                        .child(
                            div()
                                .id("sidebar")
                                .h_full()
                                .w(px(narrow_layout.sidebar_w))
                                .min_w(px(0.0))
                                .flex_shrink_0(),
                        )
                        .child(
                            div()
                                .id("conversation-column")
                                .h_full()
                                .w(px(narrow_layout.conversation_w))
                                .min_w(px(0.0))
                                .flex_shrink_0(),
                        )
                        .child(
                            div()
                                .id("review-column")
                                .h_full()
                                .flex_1()
                                .min_w(px(0.0))
                                .child(pane.clone()),
                        )
                },
            );
            let narrow_window = px(1260.0);
            let narrow_pane = cx
                .debug_bounds("review-pane")
                .expect("review pane when narrow");
            let narrow_strip = cx
                .debug_bounds("review-tabs")
                .expect("tab strip when narrow");
            println!(
                "GEOMETRY narrow window={:?} pane=[{:?}..{:?}] strip=[{:?}..{:?}]",
                narrow_window,
                narrow_pane.left(),
                narrow_pane.right(),
                narrow_strip.left(),
                narrow_strip.right(),
            );
            assert!(
                narrow_pane.right() <= narrow_window,
                "the review pane must stay inside the narrower window"
            );
            assert!(
                narrow_strip.left() >= narrow_pane.left(),
                "the tab strip must start inside the pane when narrow"
            );
        }
    );
}
