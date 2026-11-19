use crate::{ffi, Status};
use std::ffi::c_void;
use std::marker::PhantomData;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dirty {
    None,
    Partial,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorStyle {
    Bar,
    Block,
    Underline,
    BlockHollow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellFlags(pub u16);

impl CellFlags {
    pub const BOLD: u16 = ffi::GHOSTTY_STYLE_FLAG_BOLD;
    pub const ITALIC: u16 = ffi::GHOSTTY_STYLE_FLAG_ITALIC;
    pub const FAINT: u16 = ffi::GHOSTTY_STYLE_FLAG_FAINT;
    pub const BLINK: u16 = ffi::GHOSTTY_STYLE_FLAG_BLINK;
    pub const INVERSE: u16 = ffi::GHOSTTY_STYLE_FLAG_INVERSE;
    pub const INVISIBLE: u16 = ffi::GHOSTTY_STYLE_FLAG_INVISIBLE;
    pub const STRIKETHROUGH: u16 = ffi::GHOSTTY_STYLE_FLAG_STRIKETHROUGH;
    pub const OVERLINE: u16 = ffi::GHOSTTY_STYLE_FLAG_OVERLINE;

    pub fn contains(self, flag: u16) -> bool {
        self.0 & flag != 0
    }

    pub fn underline(self) -> bool {
        self.0 & ffi::GHOSTTY_STYLE_UNDERLINE_MASK != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub x: u16,
    pub y: u16,
    pub visible: bool,
    pub blinking: bool,
    pub password_input: bool,
    pub wide_tail: bool,
    pub style: CursorStyle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedCell {
    pub graphemes: String,
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    pub wide: bool,
    pub wide_continuation: bool,
    pub selected: bool,
}

pub struct Terminal {
    raw: ffi::GhosttyTerminal,
    _not_sync: PhantomData<*const ()>,
}

unsafe impl Send for Terminal {}

impl Terminal {
    pub fn new(cols: u16, rows: u16) -> Result<Self, Status> {
        let mut raw: ffi::GhosttyTerminal = std::ptr::null_mut();
        let rc = unsafe { ffi::ghostty_terminal_new(std::ptr::null(), &mut raw, cols, rows) };
        if rc != ffi::GHOSTTY_SUCCESS {
            return Err(Status(rc));
        }
        Ok(Self {
            raw,
            _not_sync: PhantomData,
        })
    }

    pub fn write(&mut self, data: &[u8]) {
        unsafe { ffi::ghostty_terminal_vt_write(self.raw, data.as_ptr(), data.len()) }
    }

    pub fn reset(&mut self) {
        unsafe { ffi::ghostty_terminal_reset(self.raw) }
    }

    pub fn resize(
        &mut self,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) -> Status {
        Status(unsafe {
            ffi::ghostty_terminal_resize(self.raw, cols, rows, cell_width_px, cell_height_px)
        })
    }

    pub fn as_raw(&self) -> ffi::GhosttyTerminal {
        self.raw
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        unsafe { ffi::ghostty_terminal_free(self.raw) }
    }
}

pub struct RenderState {
    raw: ffi::GhosttyRenderState,
    total_rows: usize,
    scrollback_rows: usize,
    _not_sync: PhantomData<*const ()>,
}

unsafe impl Send for RenderState {}

impl RenderState {
    pub fn new() -> Result<Self, Status> {
        let mut raw: ffi::GhosttyRenderState = std::ptr::null_mut();
        let rc = unsafe { ffi::ghostty_render_state_new(std::ptr::null(), &mut raw) };
        if rc != ffi::GHOSTTY_SUCCESS {
            return Err(Status(rc));
        }
        Ok(Self {
            raw,
            total_rows: 0,
            scrollback_rows: 0,
            _not_sync: PhantomData,
        })
    }

    fn read_dirty(&self) -> Dirty {
        let mut value: ffi::GhosttyEnum = ffi::GHOSTTY_RENDER_STATE_DIRTY_FALSE;
        let rc = unsafe {
            ffi::ghostty_render_state_get(
                self.raw,
                ffi::GHOSTTY_RENDER_STATE_DATA_DIRTY,
                &mut value as *mut _ as *mut c_void,
            )
        };
        assert_eq!(
            rc,
            ffi::GHOSTTY_SUCCESS,
            "reading render state dirty failed"
        );
        match value {
            ffi::GHOSTTY_RENDER_STATE_DIRTY_PARTIAL => Dirty::Partial,
            ffi::GHOSTTY_RENDER_STATE_DIRTY_FULL => Dirty::Full,
            _ => Dirty::None,
        }
    }

    fn read_terminal_metrics(&mut self, terminal: &Terminal) {
        let mut total: usize = 0;
        let mut scrollback: usize = 0;
        let keys = [
            ffi::GHOSTTY_TERMINAL_DATA_TOTAL_ROWS,
            ffi::GHOSTTY_TERMINAL_DATA_SCROLLBACK_ROWS,
        ];
        let mut values: [*mut c_void; 2] = [
            &mut total as *mut _ as *mut c_void,
            &mut scrollback as *mut _ as *mut c_void,
        ];
        let rc = unsafe {
            ffi::ghostty_terminal_get_multi(
                terminal.as_raw(),
                keys.len(),
                keys.as_ptr(),
                values.as_mut_ptr(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(rc, ffi::GHOSTTY_SUCCESS, "reading terminal metrics failed");
        self.total_rows = total;
        self.scrollback_rows = scrollback;
    }

    pub fn update(&mut self, terminal: &mut Terminal) -> Dirty {
        let rc = unsafe { ffi::ghostty_render_state_update(self.raw, terminal.as_raw()) };
        assert_eq!(rc, ffi::GHOSTTY_SUCCESS, "render state update failed");
        let dirty = self.read_dirty();
        self.read_terminal_metrics(terminal);
        dirty
    }

    pub fn begin_update(&mut self, terminal: &mut Terminal) -> Dirty {
        let rc = unsafe { ffi::ghostty_render_state_begin_update(self.raw, terminal.as_raw()) };
        assert_eq!(rc, ffi::GHOSTTY_SUCCESS, "render state begin_update failed");
        let dirty = self.read_dirty();
        self.read_terminal_metrics(terminal);
        dirty
    }

    pub fn end_update(&mut self) {
        let rc = unsafe { ffi::ghostty_render_state_end_update(self.raw) };
        assert_eq!(rc, ffi::GHOSTTY_SUCCESS, "render state end_update failed");
    }

    pub fn frame(&self) -> Frame<'_> {
        Frame {
            state: self,
            _not_sync: PhantomData,
        }
    }
}

impl Drop for RenderState {
    fn drop(&mut self) {
        unsafe { ffi::ghostty_render_state_free(self.raw) }
    }
}

pub struct Frame<'a> {
    state: &'a RenderState,
    _not_sync: PhantomData<*const ()>,
}

unsafe impl Send for Frame<'_> {}

impl<'a> Frame<'a> {
    pub fn size(&self) -> (u16, u16) {
        let mut rows: u16 = 0;
        let mut cols: u16 = 0;
        let keys = [
            ffi::GHOSTTY_RENDER_STATE_DATA_ROWS,
            ffi::GHOSTTY_RENDER_STATE_DATA_COLS,
        ];
        let mut values: [*mut c_void; 2] = [
            &mut rows as *mut _ as *mut c_void,
            &mut cols as *mut _ as *mut c_void,
        ];
        let rc = unsafe {
            ffi::ghostty_render_state_get_multi(
                self.state.raw,
                keys.len(),
                keys.as_ptr(),
                values.as_mut_ptr(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(rc, ffi::GHOSTTY_SUCCESS, "reading render state size failed");
        (rows, cols)
    }

    pub fn total_rows(&self) -> usize {
        self.state.total_rows
    }

    pub fn scrollback_rows(&self) -> usize {
        self.state.scrollback_rows
    }

    pub fn cursor(&self) -> Cursor {
        let mut raw = ffi::init_sized::<ffi::GhosttyRenderStateCursor>();
        let rc = unsafe {
            ffi::ghostty_render_state_get(
                self.state.raw,
                ffi::GHOSTTY_RENDER_STATE_DATA_CURSOR,
                &mut raw as *mut _ as *mut c_void,
            )
        };
        assert_eq!(
            rc,
            ffi::GHOSTTY_SUCCESS,
            "reading render state cursor failed"
        );
        let (x, y) = if raw.viewport_has_value {
            (raw.viewport_x, raw.viewport_y)
        } else {
            (0, 0)
        };
        let style = match raw.visual_style {
            ffi::GHOSTTY_CURSOR_STYLE_BLOCK => CursorStyle::Block,
            ffi::GHOSTTY_CURSOR_STYLE_UNDERLINE => CursorStyle::Underline,
            ffi::GHOSTTY_CURSOR_STYLE_BLOCK_HOLLOW => CursorStyle::BlockHollow,
            _ => CursorStyle::Bar,
        };
        Cursor {
            x,
            y,
            visible: raw.visible,
            blinking: raw.blinking,
            password_input: raw.password_input,
            wide_tail: raw.viewport_has_value && raw.wide_tail,
            style,
        }
    }

    pub fn palette(&self) -> [Color; 256] {
        let mut raw = [ffi::GhosttyColorRgb::default(); 256];
        let rc = unsafe {
            ffi::ghostty_render_state_get(
                self.state.raw,
                ffi::GHOSTTY_RENDER_STATE_DATA_COLOR_PALETTE,
                raw.as_mut_ptr() as *mut c_void,
            )
        };
        assert_eq!(rc, ffi::GHOSTTY_SUCCESS, "reading palette failed");
        let mut colors = [Color::Default; 256];
        for (i, entry) in raw.iter().enumerate() {
            colors[i] = Color::Rgb(entry.r, entry.g, entry.b);
        }
        colors
    }

    pub fn default_colors(&self) -> (Color, Color) {
        let mut raw = ffi::init_sized::<ffi::GhosttyRenderStateColors>();
        let rc = unsafe {
            ffi::ghostty_render_state_get(
                self.state.raw,
                ffi::GHOSTTY_RENDER_STATE_DATA_COLORS,
                &mut raw as *mut _ as *mut c_void,
            )
        };
        assert_eq!(rc, ffi::GHOSTTY_SUCCESS, "reading default colors failed");
        (
            rgb_color(raw.foreground.r, raw.foreground.g, raw.foreground.b),
            rgb_color(raw.background.r, raw.background.g, raw.background.b),
        )
    }

    pub fn row(&self, y: u16) -> Option<RenderedRow<'a>> {
        let (rows, cols) = self.size();
        if y >= rows {
            return None;
        }
        let iter = RowIterator::populate(self.state.raw)?;
        while unsafe { ffi::ghostty_render_state_row_iterator_next(iter.0) } {
            let mut viewport_y: i32 = 0;
            let rc = unsafe {
                ffi::ghostty_render_state_row_get(
                    iter.0,
                    ffi::GHOSTTY_RENDER_STATE_ROW_DATA_VIEWPORT_Y,
                    &mut viewport_y as *mut _ as *mut c_void,
                )
            };
            assert_eq!(rc, ffi::GHOSTTY_SUCCESS, "reading row viewport y failed");
            if viewport_y == i32::from(y) {
                return Some(snapshot_row(iter.0, y, cols));
            }
        }
        None
    }

    pub fn rows(&self) -> impl Iterator<Item = RenderedRow<'a>> {
        Rows {
            iter: RowIterator::populate(self.state.raw),
            cols: self.size().1,
            next_y: 0,
            _not_sync: PhantomData,
        }
    }
}

struct RowIterator(ffi::GhosttyRenderStateRowIterator);

unsafe impl Send for RowIterator {}

impl RowIterator {
    fn populate(state: ffi::GhosttyRenderState) -> Option<Self> {
        let mut raw: ffi::GhosttyRenderStateRowIterator = std::ptr::null_mut();
        let rc = unsafe { ffi::ghostty_render_state_row_iterator_new(std::ptr::null(), &mut raw) };
        assert_eq!(rc, ffi::GHOSTTY_SUCCESS, "creating row iterator failed");
        let rc = unsafe {
            ffi::ghostty_render_state_get(
                state,
                ffi::GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR,
                &mut raw as *mut _ as *mut c_void,
            )
        };
        if rc != ffi::GHOSTTY_SUCCESS {
            unsafe { ffi::ghostty_render_state_row_iterator_free(raw) };
            return None;
        }
        Some(Self(raw))
    }
}

impl Drop for RowIterator {
    fn drop(&mut self) {
        unsafe { ffi::ghostty_render_state_row_iterator_free(self.0) }
    }
}

struct Rows<'a> {
    iter: Option<RowIterator>,
    cols: u16,
    next_y: u16,
    _not_sync: PhantomData<(&'a (), *const ())>,
}

impl<'a> Iterator for Rows<'a> {
    type Item = RenderedRow<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let iter = self.iter.as_ref()?;
        if !unsafe { ffi::ghostty_render_state_row_iterator_next(iter.0) } {
            self.iter = None;
            return None;
        }
        let mut viewport_y: i32 = 0;
        let rc = unsafe {
            ffi::ghostty_render_state_row_get(
                iter.0,
                ffi::GHOSTTY_RENDER_STATE_ROW_DATA_VIEWPORT_Y,
                &mut viewport_y as *mut _ as *mut c_void,
            )
        };
        assert_eq!(rc, ffi::GHOSTTY_SUCCESS, "reading row viewport y failed");
        if viewport_y < 0 {
            self.next_y = 0;
        }
        let y = if viewport_y >= 0 {
            viewport_y as u16
        } else {
            self.next_y
        };
        self.next_y = y.saturating_add(1);
        Some(snapshot_row(iter.0, y, self.cols))
    }
}

impl Drop for Rows<'_> {
    fn drop(&mut self) {
        self.iter = None;
    }
}

pub struct RenderedRow<'a> {
    y: u16,
    cells: Vec<RenderedCell>,
    _marker: PhantomData<&'a ()>,
}

unsafe impl Send for RenderedRow<'_> {}

impl<'a> RenderedRow<'a> {
    pub fn y(&self) -> u16 {
        self.y
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    pub fn cells(&self) -> std::slice::Iter<'_, RenderedCell> {
        self.cells.iter()
    }

    pub fn cell(&self, x: u16) -> Option<&RenderedCell> {
        self.cells.get(usize::from(x))
    }

    pub fn into_cells(self) -> Vec<RenderedCell> {
        self.cells
    }
}

fn snapshot_row<'a>(
    iter: ffi::GhosttyRenderStateRowIterator,
    y: u16,
    cols: u16,
) -> RenderedRow<'a> {
    let mut raw: ffi::GhosttyRenderStateRowCells = std::ptr::null_mut();
    let rc = unsafe { ffi::ghostty_render_state_row_cells_new(std::ptr::null(), &mut raw) };
    assert_eq!(rc, ffi::GHOSTTY_SUCCESS, "creating row cells failed");
    let rc = unsafe {
        ffi::ghostty_render_state_row_get(
            iter,
            ffi::GHOSTTY_RENDER_STATE_ROW_DATA_CELLS,
            &mut raw as *mut _ as *mut c_void,
        )
    };
    assert_eq!(rc, ffi::GHOSTTY_SUCCESS, "populating row cells failed");

    let mut cells = Vec::with_capacity(usize::from(cols));
    for x in 0..cols {
        let rc = unsafe { ffi::ghostty_render_state_row_cells_select(raw, x) };
        assert_eq!(rc, ffi::GHOSTTY_SUCCESS, "selecting cell failed");
        cells.push(read_cell(raw));
    }
    unsafe { ffi::ghostty_render_state_row_cells_free(raw) };
    RenderedRow {
        y,
        cells,
        _marker: PhantomData,
    }
}

struct CellData {
    raw: ffi::GhosttyCell,
    style: ffi::GhosttyStyle,
    has_raw: bool,
    has_style: bool,
    fg: Option<ffi::GhosttyColorRgb>,
    bg: Option<ffi::GhosttyColorRgb>,
    selected: bool,
    utf8: Vec<u8>,
}

impl CellData {
    fn new() -> Self {
        Self {
            raw: 0,
            style: unsafe { std::mem::zeroed() },
            has_raw: false,
            has_style: false,
            fg: None,
            bg: None,
            selected: false,
            utf8: Vec::new(),
        }
    }

    fn reset(&mut self) {
        self.raw = 0;
        self.style = unsafe { std::mem::zeroed() };
        self.has_raw = false;
        self.has_style = false;
        self.fg = None;
        self.bg = None;
        self.selected = false;
        self.utf8.clear();
    }
}

fn read_cell(cells: ffi::GhosttyRenderStateRowCells) -> RenderedCell {
    let mut data = CellData::new();
    let mut scratch = vec![0u8; 64];

    loop {
        data.reset();
        let mut buffer = ffi::GhosttyBuffer {
            ptr: scratch.as_mut_ptr(),
            cap: scratch.len(),
            len: 0,
        };
        let keys = [
            ffi::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_RAW,
            ffi::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE,
            ffi::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_UTF8,
            ffi::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_BG_COLOR,
            ffi::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_FG_COLOR,
            ffi::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_SELECTED,
        ];
        let mut fg = ffi::GhosttyColorRgb::default();
        let mut bg = ffi::GhosttyColorRgb::default();
        let mut values: [*mut c_void; 6] = [
            &mut data.raw as *mut _ as *mut c_void,
            &mut data.style as *mut _ as *mut c_void,
            &mut buffer as *mut _ as *mut c_void,
            &mut bg as *mut _ as *mut c_void,
            &mut fg as *mut _ as *mut c_void,
            &mut data.selected as *mut _ as *mut c_void,
        ];
        let rc = unsafe {
            ffi::ghostty_render_state_row_cells_get_multi(
                cells,
                keys.len(),
                keys.as_ptr(),
                values.as_mut_ptr(),
                std::ptr::null_mut(),
            )
        };
        if rc == ffi::GHOSTTY_OUT_OF_SPACE {
            scratch.resize(buffer.len.max(scratch.len().saturating_mul(2)), 0);
            continue;
        }
        if rc == ffi::GHOSTTY_SUCCESS {
            data.has_raw = true;
            data.has_style = true;
            data.fg = Some(fg);
            data.bg = Some(bg);
            data.utf8.extend_from_slice(buffer_bytes(&buffer));
            break;
        }
        read_cell_individually(cells, &mut data, &mut scratch);
        break;
    }

    build_cell(&data)
}

fn buffer_bytes(buffer: &ffi::GhosttyBuffer) -> &[u8] {
    unsafe { std::slice::from_raw_parts(buffer.ptr, buffer.len) }
}

fn read_cell_individually(
    cells: ffi::GhosttyRenderStateRowCells,
    data: &mut CellData,
    scratch: &mut Vec<u8>,
) {
    let rc = unsafe {
        ffi::ghostty_render_state_row_cells_get(
            cells,
            ffi::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_RAW,
            &mut data.raw as *mut _ as *mut c_void,
        )
    };
    data.has_raw = rc == ffi::GHOSTTY_SUCCESS;

    let rc = unsafe {
        ffi::ghostty_render_state_row_cells_get(
            cells,
            ffi::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE,
            &mut data.style as *mut _ as *mut c_void,
        )
    };
    data.has_style = rc == ffi::GHOSTTY_SUCCESS;

    loop {
        let mut buffer = ffi::GhosttyBuffer {
            ptr: scratch.as_mut_ptr(),
            cap: scratch.len(),
            len: 0,
        };
        let rc = unsafe {
            ffi::ghostty_render_state_row_cells_get(
                cells,
                ffi::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_UTF8,
                &mut buffer as *mut _ as *mut c_void,
            )
        };
        if rc == ffi::GHOSTTY_OUT_OF_SPACE {
            scratch.resize(buffer.len.max(scratch.len().saturating_mul(2)), 0);
            continue;
        }
        if rc == ffi::GHOSTTY_SUCCESS {
            data.utf8.extend_from_slice(buffer_bytes(&buffer));
        }
        break;
    }

    let mut fg = ffi::GhosttyColorRgb::default();
    let rc = unsafe {
        ffi::ghostty_render_state_row_cells_get(
            cells,
            ffi::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_FG_COLOR,
            &mut fg as *mut _ as *mut c_void,
        )
    };
    if rc == ffi::GHOSTTY_SUCCESS {
        data.fg = Some(fg);
    }

    let mut bg = ffi::GhosttyColorRgb::default();
    let rc = unsafe {
        ffi::ghostty_render_state_row_cells_get(
            cells,
            ffi::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_BG_COLOR,
            &mut bg as *mut _ as *mut c_void,
        )
    };
    if rc == ffi::GHOSTTY_SUCCESS {
        data.bg = Some(bg);
    }

    let rc = unsafe {
        ffi::ghostty_render_state_row_cells_get(
            cells,
            ffi::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_SELECTED,
            &mut data.selected as *mut _ as *mut c_void,
        )
    };
    if rc != ffi::GHOSTTY_SUCCESS {
        data.selected = false;
    }
}

fn build_cell(data: &CellData) -> RenderedCell {
    let flags = if data.has_style { data.style.flags } else { 0 };
    let (fg, bg) = if data.has_style {
        (
            style_color(data.style.fg_color),
            style_color(data.style.bg_color),
        )
    } else {
        (rgb_option_color(data.fg), rgb_option_color(data.bg))
    };

    let mut wide = false;
    let mut wide_continuation = false;
    if data.has_raw {
        let mut value: ffi::GhosttyEnum = ffi::GHOSTTY_CELL_WIDE_NARROW;
        let rc = unsafe {
            ffi::ghostty_cell_get(
                data.raw,
                ffi::GHOSTTY_CELL_DATA_WIDE,
                &mut value as *mut _ as *mut c_void,
            )
        };
        if rc == ffi::GHOSTTY_SUCCESS {
            wide = value == ffi::GHOSTTY_CELL_WIDE_WIDE;
            wide_continuation = value == ffi::GHOSTTY_CELL_WIDE_SPACER_TAIL;
        }
    }

    RenderedCell {
        graphemes: String::from_utf8_lossy(&data.utf8).into_owned(),
        fg,
        bg,
        bold: flags & ffi::GHOSTTY_STYLE_FLAG_BOLD != 0,
        dim: flags & ffi::GHOSTTY_STYLE_FLAG_FAINT != 0,
        italic: flags & ffi::GHOSTTY_STYLE_FLAG_ITALIC != 0,
        underline: flags & ffi::GHOSTTY_STYLE_UNDERLINE_MASK != 0,
        inverse: flags & ffi::GHOSTTY_STYLE_FLAG_INVERSE != 0,
        wide,
        wide_continuation,
        selected: data.selected,
    }
}

fn style_color(value: ffi::GhosttyStyleColor) -> Color {
    match value.tag {
        ffi::GHOSTTY_STYLE_COLOR_PALETTE => Color::Indexed(unsafe { value.value.palette }),
        ffi::GHOSTTY_STYLE_COLOR_RGB => {
            let rgb = unsafe { value.value.rgb };
            Color::Rgb(rgb.r, rgb.g, rgb.b)
        }
        _ => Color::Default,
    }
}

fn rgb_option_color(value: Option<ffi::GhosttyColorRgb>) -> Color {
    match value {
        Some(rgb) => rgb_color(rgb.r, rgb.g, rgb.b),
        None => Color::Default,
    }
}

fn rgb_color(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}
