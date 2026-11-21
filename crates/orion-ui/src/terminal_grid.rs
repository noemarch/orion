//! The terminal grid as one custom GPUI element.
//!
//! The grid used to be a tree of per-row elements: every visible row was a `div`
//! carrying a `StyledText`, so a frame laid out and painted one element per row
//! — its text layout, its own bounds, its own paint pass — and nothing could be
//! merged across a row boundary, not runs and not backgrounds.
//!
//! This module is the replacement: one [`Element`] that paints the grid itself.
//! A frame is now one layout node, one merged set of background quads, one
//! shaped line per row (through GPUI's line-layout cache, so an unchanged row is
//! not re-shaped) and the cursor.
//!
//! Where the snapshot comes from: the element is `'static` and cannot borrow the
//! pane's [`TerminalEngine`](orion_core::terminal_engine::TerminalEngine), so the
//! view takes the snapshot — the visible cells, the cursor, the selection, the
//! find matches and the scroll offset, all read through the trait — and hands it
//! in as a [`GridSnapshot`]. The element then owns the usual three phases:
//! `request_layout` gives it the grid's box, `prepaint` records nothing extra,
//! and `paint` draws the snapshot into that box.
//!
//! Reading side, nothing changes: the same output, the same colours, the same
//! cursor, the same selection. What changes is what a frame costs.

use crate::theme::Theme;
use gpui_kit::{
    fill, point, px, size, App, Bounds, ContentMask, Element, ElementId, Font, FontStyle,
    FontWeight, GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId, Pixels, Point,
    Refineable, Rgba, SharedString, Style, StyleRefinement, Styled, TextAlign, TextRun,
    UnderlineStyle, Window,
};
use orion_core::terminal_engine::TermCell;
use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::OnceLock;

/// The size of one character cell, measured from the font that draws it.
#[derive(Debug, Clone, Copy)]
pub struct CellSize {
    pub width: f32,
    pub height: f32,
}

/// How much of the foreground a dim cell keeps: gpui's `fade_out` factor, so
/// `0.5` paints the foreground half-opaque over the background — a dim the
/// reader can still read.
const DIM_FADE: f32 = 0.5;

/// Debug switches from `ORION_TERMINAL_DEBUG`. All are off by default; `all`
/// turns them on, `grid` draws cell boundaries only, `repaint` bypasses the row
/// cache, `legacy` restores the pre-fix text shaping for an A/B comparison,
/// `mouse` prints the exact bytes of every SGR mouse report sent to the PTY, and
/// `perf` prints paint and reshape timing each frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TerminalDebug {
    pub full_repaint: bool,
    pub grid_lines: bool,
    pub legacy_width: bool,
    pub mouse: bool,
    pub perf: bool,
}

/// Parse the env value once. Unknown values are ignored, so a typo cannot change
/// what the reader sees.
pub fn parse_terminal_debug(value: &str) -> TerminalDebug {
    let mut flags = TerminalDebug::default();
    for token in value.split([',', ' ']).filter(|token| !token.is_empty()) {
        match token {
            "all" => {
                flags.full_repaint = true;
                flags.grid_lines = true;
                flags.mouse = true;
                flags.perf = true;
            }
            "grid" => flags.grid_lines = true,
            "repaint" => flags.full_repaint = true,
            "legacy" => flags.legacy_width = true,
            "mouse" => flags.mouse = true,
            "perf" => flags.perf = true,
            _ => {}
        }
    }
    flags
}

static TERMINAL_DEBUG: OnceLock<TerminalDebug> = OnceLock::new();

/// The parsed debug switches, read from the environment on first use.
pub fn terminal_debug() -> &'static TerminalDebug {
    TERMINAL_DEBUG.get_or_init(|| {
        let value = std::env::var("ORION_TERMINAL_DEBUG").unwrap_or_default();
        parse_terminal_debug(&value)
    })
}

/// The resolved visual style of one grid cell: the colours the program asked
/// for, resolved against the theme, with inverse already applied.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct CellStyle {
    pub fg: Rgba,
    pub bg: Rgba,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
}

/// The colour a cell asked for, as the theme paints it.
fn term_color(color: orion_core::terminal_engine::TermColor, default: Rgba) -> Rgba {
    match color {
        orion_core::terminal_engine::TermColor::Default => default,
        orion_core::terminal_engine::TermColor::Indexed(index) => crate::theme::xterm_256(index),
        orion_core::terminal_engine::TermColor::Rgb(red, green, blue) => {
            crate::theme::rgb_color(red, green, blue)
        }
    }
}

impl CellStyle {
    /// The style one cell draws with. Inverse exchanges the two colours after
    /// they are resolved, so an inverse of the defaults is visible too: the
    /// cell paints the foreground colour as its background.
    pub fn of_cell(cell: &TermCell, theme: &Theme) -> Self {
        let mut fg = term_color(cell.fg, theme.term_fg);
        let mut bg = term_color(cell.bg, theme.term_bg);
        if cell.inverse {
            std::mem::swap(&mut fg, &mut bg);
        }
        Self {
            fg,
            bg,
            bold: cell.bold,
            dim: cell.dim,
            italic: cell.italic,
            underline: cell.underline,
        }
    }

    /// The text run this style paints, over `len` bytes of the row's text.
    ///
    /// The font is the grid's font with this cell's weight and slant; the colour
    /// is the cell's foreground, faded for a dim cell the way the row path faded
    /// it. Backgrounds are not part of a run: they are painted as quads, in
    /// fewer of them, before any text.
    fn to_run(self, len: usize, font: &Font) -> TextRun {
        let mut run_font = font.clone();
        run_font.weight = if self.bold {
            FontWeight::BOLD
        } else {
            FontWeight::NORMAL
        };
        run_font.style = if self.italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        };
        let mut color: Hsla = self.fg.into();
        if self.dim {
            color.fade_out(DIM_FADE);
        }
        TextRun {
            len,
            font: run_font,
            color,
            // The background is a quad, not part of the run.
            background_color: None,
            underline: self.underline.then_some(UnderlineStyle {
                thickness: px(1.0),
                // The underline keeps the undimmed foreground, as it did when
                // the row painted it.
                color: Some(self.fg.into()),
                wavy: false,
            }),
            strikethrough: None,
        }
    }
}

/// One run of grid columns that shares one background colour.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct BgSpan {
    /// The first column the span covers.
    pub first_col: usize,
    /// One past the last column it covers.
    pub end_col: usize,
    /// The colour the span paints.
    pub color: Rgba,
}

/// The key a built row is valid for: a fingerprint of the cells' text and
/// styles, the overlays painted on it (the selection span in the row's columns,
/// the find spans, and the find revision — a counter, not the match count, so
/// same-count-different-spans still invalidates), and the theme colours the
/// runs were resolved against. The screen's global revision is deliberately
/// absent: a feed elsewhere on the screen must not rebuild an unchanged row.
#[derive(Clone, PartialEq)]
pub struct RowCacheKey {
    pub content_hash: u64,
    pub selection: Option<(usize, usize)>,
    pub find: Vec<Range<usize>>,
    pub find_revision: u64,
    pub theme_fg: Rgba,
    pub theme_bg: Rgba,
    pub theme_selection: Rgba,
    pub theme_accent: Rgba,
}

/// One visible row, ready to paint: its column-aligned text, the styled spans
/// that text is painted in, and the background rectangles its cells need.
#[derive(Clone)]
pub struct GridRow {
    pub key: RowCacheKey,
    /// The row's text: one character per grid column, except the empty second
    /// half of a wide character, which contributes nothing.
    pub text: SharedString,
    /// Owned handle to the same text, reserved for painting. `shape_line`
    /// consumes a `SharedString`, so paint takes this handle and returns it
    /// after shaping instead of cloning `text` on every frame.
    paint_text: RefCell<Option<SharedString>>,
    /// The styled spans, as byte ranges into `text`, in order, covering it.
    pub runs: Vec<(Range<usize>, CellStyle)>,
    /// The row's non-default background cells, merged into the fewest column
    /// spans. Rows merge with each other at paint time.
    pub bgs: Vec<BgSpan>,
    /// Whether any cell occupies two columns. `force_width` snaps glyphs by
    /// glyph index, not by column, so wide characters must keep natural layout.
    pub has_wide: bool,
}

/// The overlays a row's cells can carry, in the row's own coordinates: the
/// reader's selection as an inclusive grid-column span, and the find matches
/// as byte spans into the row's text.
#[derive(Default)]
pub struct RowOverlays {
    pub selection: Option<(usize, usize)>,
    pub find: Vec<Range<usize>>,
}

/// Per-row scratch reused across rows and frames: the byte span each grid
/// column occupies in the row's text, the style each cell resolves to, the
/// column each cell starts at, how many columns each cell takes, the run and
/// background buffers, and the text buffer itself. Cleared and refilled per row;
/// the capacities persist.
#[derive(Default)]
pub struct CellScratch {
    col_bytes: Vec<(usize, usize)>,
    styles: Vec<CellStyle>,
    cols: Vec<usize>,
    /// How many grid columns each cell occupies: 2 for a wide character, 0 for
    /// its empty second half, 1 for anything else.
    advance: Vec<usize>,
    runs: Vec<(Range<usize>, CellStyle)>,
    bgs: Vec<BgSpan>,
    text: String,
}

/// Build one row's text, its styled spans and its background spans from the
/// row's cells.
///
/// The text is column-aligned: every cell contributes exactly one character (a
/// space when the cell is empty), except the empty second half of a wide
/// character, which contributes nothing — the wide character before it already
/// occupies both columns. Consecutive cells that draw the same way batch into
/// one run, across the whole row; cells that share a background batch into one
/// span, whether or not their foregrounds differ. The overlays repaint the cells
/// they cover: the selection replaces the background and keeps the cell's own
/// colours, a find match repaints the background with the accent and the text
/// with the terminal background, and a cell under both is painted by the find
/// match (applied last).
pub fn build_row(
    cells: &[TermCell],
    theme: &Theme,
    overlays: &RowOverlays,
    key: RowCacheKey,
    scratch: &mut CellScratch,
) -> GridRow {
    scratch.text.clear();
    scratch.col_bytes.clear();
    scratch.styles.clear();
    scratch.cols.clear();
    scratch.advance.clear();
    scratch.runs.clear();
    scratch.bgs.clear();

    // The text, the byte span each column occupies in it, and how many columns
    // each cell takes.
    for cell in cells {
        if cell.wide_continuation {
            match scratch.col_bytes.last().copied() {
                Some(span) => {
                    scratch.col_bytes.push(span);
                    scratch.advance.push(0);
                }
                // Malformed content: a continuation with no wide character
                // before it. Render a space rather than panic.
                None => {
                    let start = scratch.text.len();
                    scratch.text.push(' ');
                    scratch.col_bytes.push((start, scratch.text.len()));
                    scratch.advance.push(1);
                }
            }
            continue;
        }
        let start = scratch.text.len();
        if cell.text.is_empty() {
            scratch.text.push(' ');
        } else {
            scratch.text.push_str(&cell.text);
        }
        scratch.col_bytes.push((start, scratch.text.len()));
        scratch.advance.push(if cell.wide { 2 } else { 1 });
    }

    // The selection's byte span: from the first column's first byte to the
    // last column's last.
    let selection = overlays.selection.and_then(|(first, last)| {
        let last = last.min(scratch.col_bytes.len().saturating_sub(1));
        let first = first.min(last);
        let start = scratch.col_bytes.get(first)?.0;
        let end = scratch.col_bytes.get(last)?.1;
        (start < end).then_some(start..end)
    });

    // The style each cell draws with, and the column each one starts at.
    let mut col = 0usize;
    for (index, cell) in cells.iter().enumerate() {
        let mut style = CellStyle::of_cell(cell, theme);
        let (cell_start, cell_end) = scratch.col_bytes[index];
        if let Some(span) = &selection {
            if cell_start < span.end && cell_end > span.start {
                style.bg = theme.term_selection;
            }
        }
        // The find matches, clamped to the row's own bytes.
        for span in overlays.find.iter().filter_map(|span| {
            let end = span.end.min(scratch.text.len());
            (span.start < end).then_some(span.start..end)
        }) {
            if cell_start < span.end && cell_end > span.start {
                style.bg = theme.accent;
                style.fg = theme.term_bg;
            }
        }
        scratch.styles.push(style);
        scratch.cols.push(col);
        col += scratch.advance[index];
    }

    // The runs: consecutive cells that draw the same way batch into one span,
    // and the spans cover the whole row (the base style paints the gaps).
    for index in 0..cells.len() {
        if scratch.advance[index] == 0 {
            continue; // no bytes of its own; the wide character's run covers it
        }
        let style = scratch.styles[index];
        let (start, end) = scratch.col_bytes[index];
        match scratch.runs.last_mut() {
            Some((last, last_style)) if *last_style == style && last.end == start => last.end = end,
            _ => scratch.runs.push((start..end, style)),
        }
    }

    // The backgrounds: consecutive cells that share a background batch into one
    // span, whether or not their foregrounds differ. The terminal's own
    // background is not painted: the pane already fills it.
    for index in 0..cells.len() {
        if scratch.advance[index] == 0 {
            continue;
        }
        let style = scratch.styles[index];
        let first = scratch.cols[index];
        let end = first + scratch.advance[index];
        match scratch.bgs.last_mut() {
            Some(last) if last.color == style.bg && last.end_col == first => last.end_col = end,
            _ if style.bg != theme.term_bg => scratch.bgs.push(BgSpan {
                first_col: first,
                end_col: end,
                color: style.bg,
            }),
            _ => {}
        }
    }

    let has_wide = scratch.advance.contains(&2);
    let text = SharedString::new(std::mem::take(&mut scratch.text));
    let runs = std::mem::take(&mut scratch.runs);
    let bgs = std::mem::take(&mut scratch.bgs);
    scratch.text = String::with_capacity(cells.len());
    scratch.runs = Vec::with_capacity(4);
    scratch.bgs = Vec::with_capacity(2);
    GridRow {
        key,
        paint_text: RefCell::new(Some(text.clone())),
        text,
        runs,
        bgs,
        has_wide,
    }
}

/// A run of columns sharing one background colour, possibly continued from
/// the rows above. `y` is the first row's height in pixels; `rows` is how
/// many consecutive rows the merge covers (each a full grid-row in height).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct MergedBackground {
    /// The columns and colour all rows share.
    pub span: BgSpan,
    /// The y position of the first row, in pixels (a float, not a row index).
    pub y: f32,
    /// How many consecutive rows the merge spans.
    pub rows: usize,
}

/// What one frame draws: the visible rows, where the cursor sits, and the cell
/// and font the grid draws with.
pub struct GridSnapshot {
    /// The visible rows, in draw order.
    pub rows: Vec<Rc<GridRow>>,
    /// The cursor, when it sits inside the viewport: `(row, col)` in visible
    /// cells. `None` when the reader has scrolled it off the top, when the pane
    /// is unfocused, or when the program hid it (DECTCEM, DEC mode 25).
    pub cursor: Option<(usize, usize)>,
    /// Whether the focused pane is allowed to show its cursor. DECTCEM off and
    /// an unfocused pane both clear this; paint consults it before drawing.
    pub cursor_visible: bool,
    /// The cursor cell's text and resolved style, so paint can keep the glyph
    /// readable on the block cursor instead of hiding it under the quad.
    pub cursor_glyph: Option<(String, CellStyle)>,
    /// The size of one character cell, measured from the font.
    pub cell: CellSize,
    /// The exact font the grid draws with, measured from the same metrics.
    pub font: Font,
    /// The font size, in pixels.
    pub font_size: f32,
    /// The palette the rows were resolved against, for the cursor.
    pub theme: Theme,
}

impl GridSnapshot {
    pub fn should_paint_cursor(&self) -> bool {
        self.cursor.is_some() && self.cursor_visible
    }
}

/// The terminal grid: one element, painted directly.
pub struct TerminalGrid {
    snapshot: GridSnapshot,
    style: StyleRefinement,
    /// Scratch run buffer reused across every visible row of a paint, so one
    /// `Vec<TextRun>` serves the whole grid instead of one per row.
    run_scratch: Vec<TextRun>,
}

impl TerminalGrid {
    pub fn new(snapshot: GridSnapshot) -> Self {
        Self {
            snapshot,
            style: StyleRefinement::default(),
            run_scratch: Vec::new(),
        }
    }

    /// The rectangles the rows need, merged across row boundaries: a span that
    /// continues from the row above extends that rectangle instead of starting
    /// a new one.
    fn background_rects(&self) -> Vec<MergedBackground> {
        let cell = self.snapshot.cell;
        let mut done: Vec<MergedBackground> = Vec::new();
        // The spans the previous row left open, and the rectangle each extends.
        let mut open: Vec<(BgSpan, usize)> = Vec::new();
        for (row_index, row) in self.snapshot.rows.iter().enumerate() {
            let y = row_index as f32 * cell.height;
            let mut next_open: Vec<(BgSpan, usize)> = Vec::with_capacity(row.bgs.len());
            for span in &row.bgs {
                let existing = open
                    .iter()
                    .find(|(open_span, _)| open_span == span)
                    .map(|(_, index)| *index);
                let index = match existing {
                    Some(index) => {
                        done[index].rows += 1;
                        index
                    }
                    None => {
                        done.push(MergedBackground {
                            span: *span,
                            y,
                            rows: 1,
                        });
                        done.len() - 1
                    }
                };
                next_open.push((*span, index));
            }
            open = next_open;
        }
        done
    }

    fn paint_backgrounds(&self, bounds: Bounds<Pixels>, window: &mut Window) {
        let cell = self.snapshot.cell;
        for MergedBackground { span, y, rows } in self.background_rects() {
            let width = (span.end_col - span.first_col) as f32 * cell.width;
            let height = rows as f32 * cell.height;
            window.paint_quad(fill(
                Bounds {
                    origin: point(
                        bounds.origin.x + px(span.first_col as f32 * cell.width),
                        bounds.origin.y + px(y),
                    ),
                    size: size(px(width), px(height)),
                },
                span.color,
            ));
        }
    }

    /// The width every glyph in a row is forced to. A normal row is one glyph
    /// per column, so snapping to the measured cell width keeps fallback faces
    /// from drifting off their backgrounds. Wide rows keep natural shaping.
    fn forced_cell_width(
        row_has_wide: bool,
        cell: CellSize,
        debug: &TerminalDebug,
    ) -> Option<Pixels> {
        if row_has_wide || debug.legacy_width {
            None
        } else {
            Some(px(cell.width))
        }
    }

    fn paint_grid_lines(&self, bounds: Bounds<Pixels>, window: &mut Window) {
        let cell = self.snapshot.cell;
        let mut color: Hsla = self.snapshot.theme.term_fg.into();
        color.fade_out(0.8);
        let cols = (f32::from(bounds.size.width) / cell.width).ceil() as usize;
        for col in 0..cols {
            window.paint_quad(fill(
                Bounds {
                    origin: point(
                        bounds.origin.x + px(col as f32 * cell.width),
                        bounds.origin.y,
                    ),
                    size: size(px(1.0), bounds.size.height),
                },
                color,
            ));
        }
        for row in 0..=self.snapshot.rows.len() {
            window.paint_quad(fill(
                Bounds {
                    origin: point(
                        bounds.origin.x,
                        bounds.origin.y + px(row as f32 * cell.height),
                    ),
                    size: size(bounds.size.width, px(1.0)),
                },
                color,
            ));
        }
    }

    fn paint_text(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let Self {
            snapshot,
            run_scratch,
            ..
        } = self;
        if snapshot.rows.is_empty() {
            return;
        }
        let cell = snapshot.cell;
        let font_size = px(snapshot.font_size);
        let base_font = snapshot.font.clone();
        // Shaping goes through the window's line-layout cache: a row whose text
        // and runs did not change is not re-shaped. The run buffer is cleared
        // and refilled per row, so one allocation serves the whole grid. The
        // text handle is taken from the row and returned after shaping, so no
        // `SharedString` is cloned per row per paint.
        let text_system = window.text_system().clone();
        for (row_index, row) in snapshot.rows.iter().enumerate() {
            if row.runs.is_empty() {
                continue;
            }
            run_scratch.clear();
            run_scratch.extend(
                row.runs
                    .iter()
                    .map(|(span, style)| style.to_run(span.len(), &base_font)),
            );
            let mut text_slot = row.paint_text.borrow_mut();
            let text = text_slot
                .take()
                .expect("row paint text handle is restored after every shape");
            let force_width = Self::forced_cell_width(row.has_wide, cell, terminal_debug());
            let mut line =
                text_system.shape_line(text, font_size, run_scratch.as_slice(), force_width);
            *text_slot = Some(std::mem::take(&mut line.text));
            let origin = Point {
                x: bounds.origin.x,
                y: bounds.origin.y + px(row_index as f32 * cell.height),
            };
            if let Err(error) =
                line.paint(origin, px(cell.height), TextAlign::Left, None, window, cx)
            {
                tracing::warn!(%error, "terminal row failed to paint");
            }
        }
    }

    fn paint_cursor(&self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        if !self.snapshot.should_paint_cursor() {
            return;
        }
        let Some((row, col)) = self.snapshot.cursor else {
            return;
        };
        let cell = self.snapshot.cell;
        window.paint_quad(fill(
            Bounds {
                origin: point(
                    bounds.origin.x + px(col as f32 * cell.width),
                    bounds.origin.y + px(row as f32 * cell.height),
                ),
                size: size(px(cell.width), px(cell.height)),
            },
            self.snapshot.theme.term_cursor,
        ));
        // A block cursor is a quad plus the glyph under it: keep the cell's
        // glyph visible instead of hiding it beneath the quad.
        let Some((text, style)) = &self.snapshot.cursor_glyph else {
            return;
        };
        let font_size = px(self.snapshot.font_size);
        let mut font = self.snapshot.font.clone();
        font.weight = if style.bold {
            FontWeight::BOLD
        } else {
            FontWeight::NORMAL
        };
        font.style = if style.italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        };
        let mut color: Hsla = self.snapshot.theme.term_bg.into();
        if style.dim {
            color.fade_out(DIM_FADE);
        }
        let run = TextRun {
            len: text.len(),
            font,
            color,
            background_color: None,
            underline: style.underline.then_some(UnderlineStyle {
                thickness: px(1.0),
                color: Some(color),
                wavy: false,
            }),
            strikethrough: None,
        };
        let text_system = window.text_system().clone();
        let line = text_system.shape_line(SharedString::new(text.clone()), font_size, &[run], None);
        let origin = Point {
            x: bounds.origin.x + px(col as f32 * cell.width),
            y: bounds.origin.y + px(row as f32 * cell.height),
        };
        if let Err(error) = line.paint(origin, px(cell.height), TextAlign::Left, None, window, cx) {
            tracing::warn!(%error, "cursor glyph failed to paint");
        }
    }
}

impl IntoElement for TerminalGrid {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Styled for TerminalGrid {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl Element for TerminalGrid {
    type RequestLayoutState = Style;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.refine(&self.style);
        let layout_id = window.request_layout(style.clone(), [], cx);
        (layout_id, style)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        #[cfg(debug_assertions)]
        let started = std::time::Instant::now();
        // The grid clips its own rows: the last line is often half a row tall,
        // and a glyph must never reach the scrollbar.
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            self.paint_backgrounds(bounds, window);
            if terminal_debug().grid_lines {
                self.paint_grid_lines(bounds, window);
            }
            self.paint_text(bounds, window, cx);
            self.paint_cursor(bounds, window, cx);
        });
        #[cfg(debug_assertions)]
        {
            let elapsed = started.elapsed();
            crate::perf::record_paint(elapsed);
            if terminal_debug().perf {
                eprintln!("ORION frame: paint {elapsed:?}");
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn forced_cell_width_matches_the_grid_column() {
        let flags = TerminalDebug::default();
        assert_eq!(
            TerminalGrid::forced_cell_width(
                false,
                CellSize {
                    width: 8.0,
                    height: 16.0
                },
                &flags
            ),
            Some(px(8.0)),
            "a normal row snaps glyphs to the measured cell"
        );
        assert_eq!(
            TerminalGrid::forced_cell_width(
                true,
                CellSize {
                    width: 8.0,
                    height: 16.0
                },
                &flags
            ),
            None,
            "a wide row keeps natural shaping"
        );
    }

    #[::core::prelude::v1::test]
    fn debug_flags_parse_from_one_env_value() {
        assert_eq!(
            parse_terminal_debug("all"),
            TerminalDebug {
                full_repaint: true,
                grid_lines: true,
                legacy_width: false,
                mouse: true,
                perf: true,
            }
        );
        assert!(parse_terminal_debug("grid").grid_lines);
        assert!(parse_terminal_debug("legacy").legacy_width);
        assert!(parse_terminal_debug("mouse").mouse);
        assert!(parse_terminal_debug("perf").perf);
        assert_eq!(parse_terminal_debug("nonsense"), TerminalDebug::default());
    }

    #[::core::prelude::v1::test]
    fn background_and_text_share_the_same_column_origin() {
        let cell = CellSize {
            width: 8.0,
            height: 16.0,
        };
        let span = BgSpan {
            first_col: 5,
            end_col: 8,
            color: crate::theme::rgb_color(1, 2, 3),
        };
        assert_eq!(span.first_col as f32 * cell.width, 40.0);
        assert_eq!(
            TerminalGrid::forced_cell_width(false, cell, &TerminalDebug::default()),
            Some(px(cell.width)),
            "text and background both start at the same column width"
        );
    }
    use gpui_kit::FontFeatures;
    use orion_core::terminal_engine::TermColor;

    /// The key the render path stamps on a built row. These tests do not care
    /// about the cache, only about what the row draws.
    fn key(theme: &Theme) -> RowCacheKey {
        RowCacheKey {
            content_hash: 0,
            selection: None,
            find: Vec::new(),
            find_revision: 0,
            theme_fg: theme.term_fg,
            theme_bg: theme.term_bg,
            theme_selection: theme.term_selection,
            theme_accent: theme.accent,
        }
    }

    /// Run `build_row` with a fresh scratch and hand back the row.
    fn run_build_row(cells: &[TermCell], theme: &Theme, overlays: &RowOverlays) -> GridRow {
        let mut scratch = CellScratch::default();
        build_row(cells, theme, overlays, key(theme), &mut scratch)
    }

    /// A plain cell carrying exactly the text and colours given.
    fn cell(text: &str, fg: TermColor, bg: TermColor) -> TermCell {
        TermCell {
            text: text.to_string(),
            fg,
            bg,
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            inverse: false,
            wide: false,
            wide_continuation: false,
        }
    }

    /// A row whose only background is one span, for the rectangle-merging tests.
    fn row_with_bg(first_col: usize, end_col: usize, color: Rgba) -> Rc<GridRow> {
        Rc::new(GridRow {
            key: RowCacheKey {
                content_hash: 0,
                selection: None,
                find: Vec::new(),
                find_revision: 0,
                theme_fg: color,
                theme_bg: color,
                theme_selection: color,
                theme_accent: color,
            },
            text: SharedString::new(""),
            paint_text: RefCell::new(Some(SharedString::new_static(""))),
            runs: Vec::new(),
            bgs: vec![BgSpan {
                first_col,
                end_col,
                color,
            }],
            has_wide: false,
        })
    }

    fn snapshot_of(rows: Vec<Rc<GridRow>>) -> GridSnapshot {
        GridSnapshot {
            rows,
            cursor: None,
            cursor_visible: false,
            cursor_glyph: None,
            cell: CellSize {
                width: 8.0,
                height: 16.0,
            },
            font: base_font(),
            font_size: 13.0,
            theme: Theme::dark(),
        }
    }

    fn base_font() -> Font {
        Font {
            family: SharedString::new("monospace"),
            features: FontFeatures::default(),
            fallbacks: None,
            weight: FontWeight::NORMAL,
            style: FontStyle::Normal,
        }
    }

    #[::core::prelude::v1::test]
    fn same_styled_cells_batch_into_one_run_with_the_requested_colour_and_weight() {
        let theme = Theme::dark();
        let red = TermColor::Indexed(1);
        let mut cells = vec![
            cell("r", red, TermColor::Default),
            cell("e", red, TermColor::Default),
            cell("d", red, TermColor::Default),
            cell(" ", TermColor::Default, TermColor::Default),
            cell("!", TermColor::Rgb(10, 20, 30), TermColor::Default),
        ];
        for cell in &mut cells[..3] {
            cell.bold = true;
        }

        let row = run_build_row(&cells, &theme, &RowOverlays::default());

        assert_eq!(
            row.text.as_str(),
            "red !",
            "every cell contributes one character"
        );
        // Three red cells batch into one run; the plain gap and the truecolor
        // cell get runs of their own, so the spans cover the whole row.
        assert_eq!(
            row.runs
                .iter()
                .map(|(span, _)| span.clone())
                .collect::<Vec<_>>(),
            vec![0..3, 3..4, 4..5],
            "the runs cover the row: {:?}",
            row.runs
        );
        assert_eq!(
            row.runs[0].1.fg,
            crate::theme::xterm_256(1),
            "indexed 1 is the theme's red"
        );
        assert!(row.runs[0].1.bold);
        assert!(
            !row.runs[1].1.bold,
            "the plain gap between them is not bold"
        );
        assert_eq!(
            row.runs[2].1.fg,
            crate::theme::rgb_color(10, 20, 30),
            "a truecolor cell keeps its own colour"
        );
    }

    #[::core::prelude::v1::test]
    fn the_run_a_cell_paints_carries_its_colour_weight_and_slant() {
        let theme = Theme::dark();
        let mut cells = vec![cell("b", TermColor::Indexed(1), TermColor::Default)];
        cells[0].bold = true;
        cells[0].italic = true;

        let row = run_build_row(&cells, &theme, &RowOverlays::default());
        let run = row.runs[0].1.to_run(1, &base_font());

        assert_eq!(run.font.weight, FontWeight::BOLD);
        assert_eq!(run.font.style, FontStyle::Italic);
        assert_eq!(run.color, crate::theme::xterm_256(1).into());
        assert!(
            run.underline.is_none(),
            "an undecorated cell is not underlined"
        );
        assert!(
            run.background_color.is_none(),
            "backgrounds are quads, not part of the run"
        );
    }

    #[::core::prelude::v1::test]
    fn an_inverse_cell_swaps_the_colours_the_program_asked_for() {
        let theme = Theme::dark();
        let mut inverse = cell("x", TermColor::Indexed(1), TermColor::Indexed(4));
        inverse.inverse = true;

        let row = run_build_row(&[inverse], &theme, &RowOverlays::default());

        assert_eq!(row.text.as_str(), "x");
        assert_eq!(
            row.runs[0].1.fg,
            crate::theme::xterm_256(4),
            "the background colour became the foreground"
        );
        assert_eq!(
            row.runs[0].1.bg,
            crate::theme::xterm_256(1),
            "the foreground colour became the background"
        );
        assert_eq!(
            row.bgs,
            vec![BgSpan {
                first_col: 0,
                end_col: 1,
                color: crate::theme::xterm_256(1),
            }],
            "the swapped background is the one painted"
        );
    }

    #[::core::prelude::v1::test]
    fn a_wide_character_and_its_continuation_keep_the_columns_aligned() {
        let theme = Theme::dark();
        let mut wide = cell("漢", TermColor::Default, TermColor::Default);
        wide.wide = true;
        let continuation = TermCell {
            text: String::new(),
            wide_continuation: true,
            ..cell("", TermColor::Default, TermColor::Default)
        };
        let cells = vec![
            wide,
            continuation,
            cell("x", TermColor::Default, TermColor::Default),
        ];

        let row = run_build_row(&cells, &theme, &RowOverlays::default());

        assert_eq!(
            row.text.as_str(),
            "漢x",
            "the continuation contributes no character"
        );
        assert!(
            row.runs.len() == 1 && row.runs[0].0 == (0..4),
            "the row is one plain run: {:?}",
            row.runs
        );

        // Selecting the continuation half selects the wide character itself, and
        // the background covers both of its columns.
        let overlays = RowOverlays {
            selection: Some((1, 1)),
            find: Vec::new(),
        };
        let row = run_build_row(&cells, &theme, &overlays);
        assert_eq!(
            row.bgs,
            vec![BgSpan {
                first_col: 0,
                end_col: 2,
                color: theme.term_selection,
            }],
            "the wide character's background covers both of its columns: {:?}",
            row.bgs
        );
    }

    #[::core::prelude::v1::test]
    fn a_dim_cell_fades_the_foreground_and_an_underlined_cell_underlines() {
        let theme = Theme::dark();
        let mut dim = cell("d", TermColor::Default, TermColor::Default);
        dim.dim = true;
        let mut underlined = cell("u", TermColor::Indexed(2), TermColor::Default);
        underlined.underline = true;

        let row = run_build_row(&[dim, underlined], &theme, &RowOverlays::default());
        let dim_run = row.runs[0].1.to_run(1, &base_font());
        let underlined_run = row.runs[1].1.to_run(1, &base_font());

        assert!(
            (dim_run.color.a - 0.5).abs() < f32::EPSILON,
            "a dim cell fades its foreground to half: {}",
            dim_run.color.a
        );
        let mut faded: Hsla = theme.term_fg.into();
        faded.fade_out(DIM_FADE);
        assert_eq!(
            dim_run.color, faded,
            "the dim fades the cell's own foreground"
        );
        let underline = underlined_run.underline.expect("the underline is set");
        assert_eq!(underline.thickness, px(1.0));
        assert_eq!(
            underline.color,
            Some(crate::theme::xterm_256(2).into()),
            "the underline is the cell's own foreground"
        );
    }

    #[::core::prelude::v1::test]
    fn the_selection_keeps_the_cells_colours_and_a_find_match_repaints_them() {
        let theme = Theme::dark();
        let cells = vec![
            cell("a", TermColor::Indexed(1), TermColor::Default),
            cell("b", TermColor::Default, TermColor::Default),
            cell("c", TermColor::Default, TermColor::Default),
        ];
        let mut overlays = RowOverlays {
            selection: Some((0, 2)),
            find: Vec::new(),
        };
        overlays.find.push(0..2);

        let row = run_build_row(&cells, &theme, &overlays);

        assert_eq!(row.text.as_str(), "abc");
        // Under both overlays, the find match wins (applied last); cells 0 and
        // 1 draw the same way, so they batch into one run.
        assert_eq!(row.runs[0].0, 0..2);
        assert_eq!(
            row.runs[0].1.fg, theme.term_bg,
            "the match paints the terminal background"
        );
        // Selected only: the cell keeps its own foreground.
        assert_eq!(row.runs[1].0, 2..3);
        assert_eq!(
            row.runs[1].1.fg, theme.term_fg,
            "the selection keeps the cell's foreground"
        );
        assert_eq!(
            row.bgs,
            vec![
                BgSpan {
                    first_col: 0,
                    end_col: 2,
                    color: theme.accent,
                },
                BgSpan {
                    first_col: 2,
                    end_col: 3,
                    color: theme.term_selection,
                },
            ],
            "the match's background and the selection's are spans: {:?}",
            row.bgs
        );
    }

    #[::core::prelude::v1::test]
    fn an_ansi_coloured_feed_reaches_the_runs() {
        // The exact sequence the manual-QA script prints: red, bold green and
        // a 24-bit colour, each reset before the next.
        let mut screen = orion_core::screen::ScreenState::new(4, 80);
        screen.process(
            "\x1b[31mRED\x1b[0m \x1b[1;32mBOLD GREEN\x1b[0m \x1b[38;2;255;100;0mTRUECOLOR\x1b[0m\n",
        );
        let rows = screen.viewport_cells(0);

        let row = run_build_row(&rows[0], &Theme::dark(), &RowOverlays::default());

        assert!(
            row.text.as_str().starts_with("RED BOLD GREEN TRUECOLOR"),
            "{:?}",
            row.text
        );
        let spans: Vec<Range<usize>> = row.runs.iter().map(|(span, _)| span.clone()).collect();
        assert_eq!(
            spans,
            vec![0..3, 3..4, 4..14, 14..15, 15..24, 24..80],
            "the styled stretches, the plain gaps between them, and the empty tail: {:?}",
            row.runs
        );
        assert_eq!(row.runs[0].1.fg, crate::theme::xterm_256(1));
        assert!(!row.runs[0].1.bold);
        assert_eq!(row.runs[2].1.fg, crate::theme::xterm_256(2));
        assert!(row.runs[2].1.bold);
        assert_eq!(
            row.runs[4].1.fg,
            crate::theme::rgb_color(255, 100, 0),
            "a truecolor cell keeps the colour it asked for"
        );
    }

    #[::core::prelude::v1::test]
    fn a_wide_char_row_with_selection_and_find_match_produces_stable_output() {
        // Wide char + selection + find match on the same row.
        let theme = Theme::dark();
        let mut wide = cell("漢", TermColor::Default, TermColor::Default);
        wide.wide = true;
        let continuation = TermCell {
            text: String::new(),
            wide_continuation: true,
            ..cell("", TermColor::Default, TermColor::Default)
        };
        let cells = vec![
            wide,
            continuation,
            cell("a", TermColor::Indexed(1), TermColor::Default),
            cell("b", TermColor::Default, TermColor::Default),
        ];
        let overlays = RowOverlays {
            selection: Some((0, 2)),
            find: std::iter::once(3..4).collect(),
        };

        let row = run_build_row(&cells, &theme, &overlays);

        assert_eq!(row.text.as_str(), "漢ab");
        // 漢 is bytes 0..3, 'a' is 3..4, 'b' is 4..5. The selection covers
        // columns 0..=2 (bytes 0..4); the find match covers byte 3..4 and
        // wins on the overlap. Runs break at cell boundaries.
        assert_eq!(
            row.runs
                .iter()
                .map(|(span, _)| span.clone())
                .collect::<Vec<_>>(),
            vec![0..3, 3..4, 4..5]
        );
        assert_eq!(row.runs[0].1.bg, theme.term_selection, "selection first");
        assert_eq!(
            row.runs[1].1.bg, theme.accent,
            "find second, find wins on overlap"
        );
        assert_eq!(
            row.runs[1].1.fg, theme.term_bg,
            "the match paints the terminal background"
        );
        assert_eq!(
            row.runs[2].1.bg, theme.term_bg,
            "the cell after the match is plain"
        );
        assert_eq!(
            row.bgs,
            vec![
                BgSpan {
                    first_col: 0,
                    end_col: 2,
                    color: theme.term_selection,
                },
                BgSpan {
                    first_col: 2,
                    end_col: 3,
                    color: theme.accent,
                },
            ],
            "the row's backgrounds, as spans: {:?}",
            row.bgs
        );
    }

    #[::core::prelude::v1::test]
    fn a_lone_wide_continuation_at_index_zero_renders_a_space() {
        // Malformed content: a continuation cell with no wide character
        // before it must not panic and renders a space.
        let theme = Theme::dark();
        let continuation = TermCell {
            text: String::new(),
            wide_continuation: true,
            ..cell("", TermColor::Default, TermColor::Default)
        };

        let row = run_build_row(&[continuation], &theme, &RowOverlays::default());

        assert_eq!(row.text.as_str(), " ");
        assert_eq!(
            row.runs.len(),
            1,
            "the space is a plain run: {:?}",
            row.runs
        );
        assert_eq!(row.runs[0].1.bg, theme.term_bg);
        assert!(row.bgs.is_empty(), "the terminal background is not painted");
    }

    #[::core::prelude::v1::test]
    fn the_terminal_background_is_never_painted() {
        let theme = Theme::dark();
        let cells = vec![
            cell("a", TermColor::Default, TermColor::Default),
            cell("b", TermColor::Default, TermColor::Default),
        ];

        let row = run_build_row(&cells, &theme, &RowOverlays::default());

        assert!(
            row.bgs.is_empty(),
            "the pane already fills the ground: {:?}",
            row.bgs
        );
    }

    #[::core::prelude::v1::test]
    fn a_background_batches_over_the_foregrounds_it_covers() {
        let theme = Theme::dark();
        let cells = vec![
            cell("a", TermColor::Default, TermColor::Indexed(1)),
            cell("b", TermColor::Default, TermColor::Indexed(1)),
            // The same background with a different foreground: still one span.
            cell("c", TermColor::Indexed(2), TermColor::Indexed(1)),
            cell("d", TermColor::Default, TermColor::Indexed(4)),
        ];

        let row = run_build_row(&cells, &theme, &RowOverlays::default());

        assert_eq!(
            row.bgs,
            vec![
                BgSpan {
                    first_col: 0,
                    end_col: 3,
                    color: crate::theme::xterm_256(1),
                },
                BgSpan {
                    first_col: 3,
                    end_col: 4,
                    color: crate::theme::xterm_256(4),
                },
            ],
            "a background batches across foregrounds: {:?}",
            row.bgs
        );
        // The foregrounds still split the text.
        assert_eq!(
            row.runs
                .iter()
                .map(|(span, _)| span.clone())
                .collect::<Vec<_>>(),
            vec![0..2, 2..3, 3..4]
        );
    }

    #[::core::prelude::v1::test]
    fn a_span_that_continues_from_the_row_above_extends_one_rectangle() {
        let red = crate::theme::xterm_256(1);
        let grid = TerminalGrid::new(snapshot_of(vec![
            row_with_bg(0, 4, red),
            row_with_bg(0, 4, red),
            row_with_bg(0, 4, red),
        ]));

        let rects = grid.background_rects();

        assert_eq!(rects.len(), 1, "three rows, one rectangle: {rects:?}");
        assert_eq!(rects[0].span.first_col, 0);
        assert_eq!(rects[0].span.end_col, 4);
        assert_eq!(rects[0].rows, 3, "the rectangle spans all three rows");
        assert_eq!(rects[0].y, 0.0, "it starts at the first row");
    }

    #[::core::prelude::v1::test]
    fn a_span_that_stops_does_not_extend_the_rectangle_below_it() {
        let red = crate::theme::xterm_256(1);
        let blue = crate::theme::xterm_256(4);
        let grid = TerminalGrid::new(snapshot_of(vec![
            row_with_bg(0, 4, red),
            row_with_bg(0, 4, red),
            row_with_bg(0, 4, blue),
        ]));

        let rects = grid.background_rects();

        assert_eq!(
            rects.len(),
            2,
            "the colour change starts a new rectangle: {rects:?}"
        );
        assert_eq!(rects[0].rows, 2, "the first colour covers two rows");
        assert_eq!(rects[1].rows, 1, "the second colour covers one");
        assert_eq!(
            rects[1].y,
            2.0 * 16.0,
            "the second rectangle starts at the third row"
        );
    }

    #[::core::prelude::v1::test]
    fn two_spans_side_by_side_on_one_row_stay_two_rectangles() {
        let red = crate::theme::xterm_256(1);
        let blue = crate::theme::xterm_256(4);
        let left = row_with_bg(0, 4, red);
        let right = Rc::new(GridRow {
            bgs: vec![BgSpan {
                first_col: 4,
                end_col: 8,
                color: blue,
            }],
            ..(*left).clone()
        });
        let grid = TerminalGrid::new(snapshot_of(vec![left, right]));

        let rects = grid.background_rects();

        assert_eq!(
            rects.len(),
            2,
            "side by side is still two rectangles: {rects:?}"
        );
        assert_eq!(rects[0].span.first_col, 0);
        assert_eq!(rects[1].span.first_col, 4);
    }

    #[::core::prelude::v1::test]
    fn the_cursor_is_painted_in_the_cell_the_terminal_put_it_in() {
        let mut snap = snapshot_of(Vec::new());
        snap.cursor = Some((3, 7));
        let grid = TerminalGrid::new(snap);

        // What the element paints with: the cell's own arithmetic, in grid
        // coordinates from the box's origin.
        let cell = grid.snapshot.cell;
        let (row, col) = grid.snapshot.cursor.expect("the cursor is in the snapshot");
        assert_eq!((row, col), (3, 7));
        assert_eq!(col as f32 * cell.width, 56.0);
        assert_eq!(row as f32 * cell.height, 48.0);
    }

    #[::core::prelude::v1::test]
    fn a_grid_with_no_rows_paints_nothing() {
        let grid = TerminalGrid::new(snapshot_of(Vec::new()));

        assert!(grid.background_rects().is_empty());
        assert!(grid.snapshot.rows.is_empty());
    }

    #[::core::prelude::v1::test]
    fn cursor_visibility_hidden_snapshot_paints_no_cursor() {
        let mut snap = snapshot_of(Vec::new());
        snap.cursor = Some((3, 7));
        snap.cursor_visible = false;
        assert!(
            !snap.should_paint_cursor(),
            "a hidden cursor (DECTCEM off) paints no quad"
        );
    }

    #[::core::prelude::v1::test]
    fn cursor_visibility_unfocused_snapshot_paints_no_cursor() {
        let snap = snapshot_of(Vec::new());
        assert!(
            !snap.should_paint_cursor(),
            "a pane without a cursor position paints no quad"
        );
    }

    #[::core::prelude::v1::test]
    fn cursor_visibility_focused_snapshot_paints_cursor_at_its_cell() {
        let mut snap = snapshot_of(Vec::new());
        snap.cursor = Some((3, 7));
        snap.cursor_visible = true;
        assert!(snap.should_paint_cursor());
        let cell = snap.cell;
        let (row, col) = snap.cursor.expect("cursor is present");
        assert_eq!(col as f32 * cell.width, 56.0);
        assert_eq!(row as f32 * cell.height, 48.0);
    }

    #[::core::prelude::v1::test]
    fn background_spans_cover_exactly_the_cells_with_a_non_default_background() {
        let theme = Theme::dark();
        let cells = vec![
            cell("a", TermColor::Default, TermColor::Default),
            cell("b", TermColor::Default, TermColor::Indexed(1)),
            cell("c", TermColor::Default, TermColor::Indexed(1)),
            cell("d", TermColor::Default, TermColor::Default),
        ];
        let row = run_build_row(&cells, &theme, &RowOverlays::default());
        assert_eq!(
            row.bgs,
            vec![BgSpan {
                first_col: 1,
                end_col: 3,
                color: crate::theme::xterm_256(1),
            }],
            "only the two coloured cells carry a background: {:?}",
            row.bgs
        );
    }

    #[::core::prelude::v1::test]
    fn background_spans_snap_partial_find_overlaps_to_cell_boundaries() {
        let theme = Theme::dark();
        let mut wide = cell("漢", TermColor::Default, TermColor::Default);
        wide.wide = true;
        let continuation = TermCell {
            text: String::new(),
            wide_continuation: true,
            ..cell("", TermColor::Default, TermColor::Default)
        };
        let cells = vec![
            wide,
            continuation,
            cell("a", TermColor::Default, TermColor::Default),
        ];
        let overlays = RowOverlays {
            selection: None,
            find: vec![1..4],
        };
        let row = run_build_row(&cells, &theme, &overlays);
        assert_eq!(
            row.bgs,
            vec![BgSpan {
                first_col: 0,
                end_col: 3,
                color: theme.accent,
            }],
            "a partial byte overlap highlights whole cells: {:?}",
            row.bgs
        );
    }
}
