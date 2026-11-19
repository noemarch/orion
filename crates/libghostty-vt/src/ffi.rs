//! Raw C ABI declarations for libghostty-vt 1.0.1.
//!
//! Transcribed from the `vt-1.0.1` headers. The numeric values below are the
//! 1.0.1 values; pre-1.0 bindings are not ABI compatible.

use std::ffi::{c_char, c_void};

pub type GhosttyResult = i32;
pub type GhosttyEnum = i32;

pub const GHOSTTY_SUCCESS: GhosttyResult = 0;
pub const GHOSTTY_OUT_OF_MEMORY: GhosttyResult = -1;
pub const GHOSTTY_INVALID_VALUE: GhosttyResult = -2;
pub const GHOSTTY_OUT_OF_SPACE: GhosttyResult = -3;
pub const GHOSTTY_NO_VALUE: GhosttyResult = -4;
pub const GHOSTTY_IO_ERROR: GhosttyResult = -5;
pub const GHOSTTY_LIMIT_EXCEEDED: GhosttyResult = -6;
pub const GHOSTTY_REJECTED: GhosttyResult = -7;
pub const GHOSTTY_UNSUPPORTED: GhosttyResult = -8;

macro_rules! opaque {
    ($name:ident) => {
        #[repr(C)]
        pub struct $name {
            _p: [u8; 0],
        }
    };
}

opaque!(GhosttyTerminalImpl);
opaque!(GhosttyAllocator);
opaque!(GhosttyRenderStateImpl);
opaque!(GhosttyRenderStateRowIteratorImpl);
opaque!(GhosttyRenderStateRowCellsImpl);
opaque!(GhosttyRenderStateImageIteratorImpl);
opaque!(GhosttyFormatterImpl);
opaque!(GhosttyKeyEncoderImpl);
opaque!(GhosttyKeyEventImpl);
opaque!(GhosttyMouseEncoderImpl);
opaque!(GhosttyMouseEventImpl);
opaque!(GhosttySnapshotDecoderImpl);
opaque!(GhosttyTrackedGridRefImpl);
opaque!(GhosttyKittyGraphicsImpl);
opaque!(GhosttySelectionGestureImpl);
opaque!(GhosttySelectionGestureEventImpl);

pub type GhosttyTerminal = *mut GhosttyTerminalImpl;
pub type GhosttyRenderState = *mut GhosttyRenderStateImpl;
pub type GhosttyRenderStateRowIterator = *mut GhosttyRenderStateRowIteratorImpl;
pub type GhosttyRenderStateRowCells = *mut GhosttyRenderStateRowCellsImpl;
pub type GhosttyRenderStateImageIterator = *mut GhosttyRenderStateImageIteratorImpl;
pub type GhosttyFormatter = *mut GhosttyFormatterImpl;
pub type GhosttyKeyEncoder = *mut GhosttyKeyEncoderImpl;
pub type GhosttyKeyEvent = *mut GhosttyKeyEventImpl;
pub type GhosttyMouseEncoder = *mut GhosttyMouseEncoderImpl;
pub type GhosttyMouseEvent = *mut GhosttyMouseEventImpl;
pub type GhosttySnapshotDecoder = *mut GhosttySnapshotDecoderImpl;
pub type GhosttyTrackedGridRef = *mut GhosttyTrackedGridRefImpl;
pub type GhosttyKittyGraphics = *mut GhosttyKittyGraphicsImpl;
pub type GhosttySelectionGesture = *mut GhosttySelectionGestureImpl;
pub type GhosttySelectionGestureEvent = *mut GhosttySelectionGestureEventImpl;

pub type GhosttyCell = u64;
pub type GhosttyRow = u64;
pub type GhosttyStyleId = u16;
pub type GhosttyMods = u16;
pub type GhosttyKey = GhosttyEnum;
pub type GhosttyKittyKeyFlags = u8;
pub type GhosttyFormatterExtras = u32;
pub type GhosttyStyleFlags = u16;
pub type GhosttyColorPaletteIndex = u8;

pub type GhosttyReaderFn = unsafe extern "C" fn(*mut c_void, *mut u8, usize, *mut usize) -> bool;
pub type GhosttyWriterFn = unsafe extern "C" fn(*mut c_void, *const u8, usize) -> bool;
pub type GhosttyMimeReaderFn =
    unsafe extern "C" fn(*mut c_void, GhosttyString, GhosttyWriter) -> bool;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyReader {
    pub read: GhosttyReaderFn,
    pub userdata: *mut c_void,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyWriter {
    pub write: GhosttyWriterFn,
    pub userdata: *mut c_void,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyMimeReader {
    pub read: GhosttyMimeReaderFn,
    pub userdata: *mut c_void,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct GhosttyString {
    pub ptr: *const u8,
    pub len: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct GhosttyBuffer {
    pub ptr: *mut u8,
    pub cap: usize,
    pub len: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct GhosttyCodepointBuffer {
    pub ptr: *mut u32,
    pub cap: usize,
    pub len: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct GhosttyCodepoints {
    pub ptr: *const u32,
    pub len: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct GhosttyCellsView {
    pub ptr: *const GhosttyCell,
    pub len: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct GhosttyColorRgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct GhosttyColorPaletteMask {
    pub bits: [u64; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyColorX11Entry {
    pub name: *const c_char,
    pub color: GhosttyColorRgb,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union GhosttyStyleColorValue {
    pub palette: GhosttyColorPaletteIndex,
    pub rgb: GhosttyColorRgb,
    pub bytes: [u8; 3],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyStyleColor {
    pub tag: u8,
    pub value: GhosttyStyleColorValue,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyStyle {
    pub fg_color: GhosttyStyleColor,
    pub bg_color: GhosttyStyleColor,
    pub underline_color: GhosttyStyleColor,
    pub flags: GhosttyStyleFlags,
    pub reserved: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct GhosttySurfacePosition {
    pub x: f64,
    pub y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct GhosttyPointCoordinate {
    pub x: u16,
    pub y: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union GhosttyPointValue {
    pub coordinate: GhosttyPointCoordinate,
    pub padding: [u64; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyPoint {
    pub tag: GhosttyEnum,
    pub value: GhosttyPointValue,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct GhosttyGridRef {
    pub node: *mut c_void,
    pub x: u16,
    pub y: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct GhosttySelection {
    pub start: GhosttyGridRef,
    pub end: GhosttyGridRef,
    pub rectangle: bool,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct GhosttySelectionBuffer {
    pub ptr: *mut GhosttySelection,
    pub cap: usize,
    pub len: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct GhosttyMousePosition {
    pub x: f32,
    pub y: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct GhosttyRenderStateOverscan {
    pub above: u16,
    pub below: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct GhosttyRenderStateRowId {
    pub bits: [u64; 2],
}

pub trait GhosttySized: Sized {
    fn set_size(&mut self, size: usize);
    fn sized() -> Self {
        let mut value: Self = unsafe { std::mem::zeroed() };
        value.set_size(std::mem::size_of::<Self>());
        value
    }
}

pub fn init_sized<T: GhosttySized>() -> T {
    T::sized()
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GhosttyRenderStateCursor {
    pub size: usize,
    pub viewport_has_value: bool,
    pub viewport_x: u16,
    pub viewport_y: u16,
    pub wide_tail: bool,
    pub visible: bool,
    pub blinking: bool,
    pub password_input: bool,
    pub visual_style: GhosttyEnum,
}

impl GhosttySized for GhosttyRenderStateCursor {
    fn set_size(&mut self, size: usize) {
        self.size = size;
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GhosttyRenderStateColors {
    pub size: usize,
    pub background: GhosttyColorRgb,
    pub foreground: GhosttyColorRgb,
    pub cursor: GhosttyColorRgb,
    pub cursor_has_value: bool,
    pub palette: [GhosttyColorRgb; 256],
}

impl GhosttySized for GhosttyRenderStateColors {
    fn set_size(&mut self, size: usize) {
        self.size = size;
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GhosttyRenderStateRowSelection {
    pub size: usize,
    pub start_x: u16,
    pub end_x: u16,
}

impl GhosttySized for GhosttyRenderStateRowSelection {
    fn set_size(&mut self, size: usize) {
        self.size = size;
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GhosttyMouseEncoderSize {
    pub size: usize,
    pub screen_width: u32,
    pub screen_height: u32,
    pub cell_width: u32,
    pub cell_height: u32,
    pub padding_top: u32,
    pub padding_bottom: u32,
    pub padding_right: u32,
    pub padding_left: u32,
}

impl GhosttySized for GhosttyMouseEncoderSize {
    fn set_size(&mut self, size: usize) {
        self.size = size;
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GhosttyTerminalWriteStop {
    pub size: usize,
    pub consumed: usize,
    pub reached: u32,
    pub kind: GhosttyEnum,
    pub introducer: GhosttyEnum,
    pub end: GhosttyEnum,
    pub byte: u8,
}

impl GhosttySized for GhosttyTerminalWriteStop {
    fn set_size(&mut self, size: usize) {
        self.size = size;
    }
}

pub const GHOSTTY_CURSOR_STYLE_BAR: GhosttyEnum = 0;
pub const GHOSTTY_CURSOR_STYLE_BLOCK: GhosttyEnum = 1;
pub const GHOSTTY_CURSOR_STYLE_UNDERLINE: GhosttyEnum = 2;
pub const GHOSTTY_CURSOR_STYLE_BLOCK_HOLLOW: GhosttyEnum = 3;

pub const GHOSTTY_RENDER_STATE_DIRTY_FALSE: GhosttyEnum = 0;
pub const GHOSTTY_RENDER_STATE_DIRTY_PARTIAL: GhosttyEnum = 1;
pub const GHOSTTY_RENDER_STATE_DIRTY_FULL: GhosttyEnum = 2;

pub const GHOSTTY_RENDER_STATE_DATA_INVALID: GhosttyEnum = 0;
pub const GHOSTTY_RENDER_STATE_DATA_COLS: GhosttyEnum = 1;
pub const GHOSTTY_RENDER_STATE_DATA_ROWS: GhosttyEnum = 2;
pub const GHOSTTY_RENDER_STATE_DATA_DIRTY: GhosttyEnum = 3;
pub const GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR: GhosttyEnum = 4;
pub const GHOSTTY_RENDER_STATE_DATA_COLOR_BACKGROUND: GhosttyEnum = 5;
pub const GHOSTTY_RENDER_STATE_DATA_COLOR_FOREGROUND: GhosttyEnum = 6;
pub const GHOSTTY_RENDER_STATE_DATA_COLOR_CURSOR: GhosttyEnum = 7;
pub const GHOSTTY_RENDER_STATE_DATA_COLOR_CURSOR_HAS_VALUE: GhosttyEnum = 8;
pub const GHOSTTY_RENDER_STATE_DATA_COLOR_PALETTE: GhosttyEnum = 9;
pub const GHOSTTY_RENDER_STATE_DATA_CURSOR_VISUAL_STYLE: GhosttyEnum = 10;
pub const GHOSTTY_RENDER_STATE_DATA_CURSOR_VISIBLE: GhosttyEnum = 11;
pub const GHOSTTY_RENDER_STATE_DATA_CURSOR_BLINKING: GhosttyEnum = 12;
pub const GHOSTTY_RENDER_STATE_DATA_CURSOR_PASSWORD_INPUT: GhosttyEnum = 13;
pub const GHOSTTY_RENDER_STATE_DATA_CURSOR_VIEWPORT_HAS_VALUE: GhosttyEnum = 14;
pub const GHOSTTY_RENDER_STATE_DATA_CURSOR_VIEWPORT_X: GhosttyEnum = 15;
pub const GHOSTTY_RENDER_STATE_DATA_CURSOR_VIEWPORT_Y: GhosttyEnum = 16;
pub const GHOSTTY_RENDER_STATE_DATA_CURSOR_VIEWPORT_WIDE_TAIL: GhosttyEnum = 17;
pub const GHOSTTY_RENDER_STATE_DATA_CURSOR: GhosttyEnum = 18;
pub const GHOSTTY_RENDER_STATE_DATA_COLORS: GhosttyEnum = 19;
pub const GHOSTTY_RENDER_STATE_DATA_OVERSCAN: GhosttyEnum = 20;
pub const GHOSTTY_RENDER_STATE_DATA_OVERSCAN_REQUEST: GhosttyEnum = 21;
pub const GHOSTTY_RENDER_STATE_DATA_IMAGE_ITERATOR: GhosttyEnum = 22;
pub const GHOSTTY_RENDER_STATE_DATA_IMAGE_PLACEMENTS_GENERATION: GhosttyEnum = 23;

pub const GHOSTTY_RENDER_STATE_OPT_INVALID: GhosttyEnum = 0;
pub const GHOSTTY_RENDER_STATE_OPT_DIRTY: GhosttyEnum = 1;
pub const GHOSTTY_RENDER_STATE_OPT_OVERSCAN: GhosttyEnum = 2;

pub const GHOSTTY_RENDER_STATE_ROW_DATA_INVALID: GhosttyEnum = 0;
pub const GHOSTTY_RENDER_STATE_ROW_DATA_DIRTY: GhosttyEnum = 1;
pub const GHOSTTY_RENDER_STATE_ROW_DATA_RAW: GhosttyEnum = 2;
pub const GHOSTTY_RENDER_STATE_ROW_DATA_CELLS: GhosttyEnum = 3;
pub const GHOSTTY_RENDER_STATE_ROW_DATA_SELECTION: GhosttyEnum = 4;
pub const GHOSTTY_RENDER_STATE_ROW_DATA_CELLS_RAW: GhosttyEnum = 5;
pub const GHOSTTY_RENDER_STATE_ROW_DATA_VIEWPORT_Y: GhosttyEnum = 6;
pub const GHOSTTY_RENDER_STATE_ROW_DATA_ID: GhosttyEnum = 7;

pub const GHOSTTY_RENDER_STATE_ROW_OPT_INVALID: GhosttyEnum = 0;
pub const GHOSTTY_RENDER_STATE_ROW_OPT_DIRTY: GhosttyEnum = 1;

pub const GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_INVALID: GhosttyEnum = 0;
pub const GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_RAW: GhosttyEnum = 1;
pub const GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE: GhosttyEnum = 2;
pub const GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_LEN: GhosttyEnum = 3;
pub const GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_BUF: GhosttyEnum = 4;
pub const GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_BG_COLOR: GhosttyEnum = 5;
pub const GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_FG_COLOR: GhosttyEnum = 6;
pub const GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_SELECTED: GhosttyEnum = 7;
pub const GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_HAS_STYLING: GhosttyEnum = 8;
pub const GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_UTF8: GhosttyEnum = 9;

pub const GHOSTTY_STYLE_COLOR_NONE: u8 = 0;
pub const GHOSTTY_STYLE_COLOR_PALETTE: u8 = 1;
pub const GHOSTTY_STYLE_COLOR_RGB: u8 = 2;

pub const GHOSTTY_STYLE_FLAG_BOLD: GhosttyStyleFlags = 1 << 0;
pub const GHOSTTY_STYLE_FLAG_ITALIC: GhosttyStyleFlags = 1 << 1;
pub const GHOSTTY_STYLE_FLAG_FAINT: GhosttyStyleFlags = 1 << 2;
pub const GHOSTTY_STYLE_FLAG_BLINK: GhosttyStyleFlags = 1 << 3;
pub const GHOSTTY_STYLE_FLAG_INVERSE: GhosttyStyleFlags = 1 << 4;
pub const GHOSTTY_STYLE_FLAG_INVISIBLE: GhosttyStyleFlags = 1 << 5;
pub const GHOSTTY_STYLE_FLAG_STRIKETHROUGH: GhosttyStyleFlags = 1 << 6;
pub const GHOSTTY_STYLE_FLAG_OVERLINE: GhosttyStyleFlags = 1 << 7;
pub const GHOSTTY_STYLE_UNDERLINE_SHIFT: u32 = 8;
pub const GHOSTTY_STYLE_UNDERLINE_MASK: GhosttyStyleFlags = 0x7 << 8;

pub const GHOSTTY_CELL_WIDE_NARROW: GhosttyEnum = 0;
pub const GHOSTTY_CELL_WIDE_WIDE: GhosttyEnum = 1;
pub const GHOSTTY_CELL_WIDE_SPACER_TAIL: GhosttyEnum = 2;
pub const GHOSTTY_CELL_WIDE_SPACER_HEAD: GhosttyEnum = 3;

pub const GHOSTTY_CELL_DATA_INVALID: GhosttyEnum = 0;
pub const GHOSTTY_CELL_DATA_CODEPOINT: GhosttyEnum = 1;
pub const GHOSTTY_CELL_DATA_CONTENT_TAG: GhosttyEnum = 2;
pub const GHOSTTY_CELL_DATA_WIDE: GhosttyEnum = 3;
pub const GHOSTTY_CELL_DATA_HAS_TEXT: GhosttyEnum = 4;
pub const GHOSTTY_CELL_DATA_HAS_STYLING: GhosttyEnum = 5;
pub const GHOSTTY_CELL_DATA_STYLE_ID: GhosttyEnum = 6;
pub const GHOSTTY_CELL_DATA_HAS_HYPERLINK: GhosttyEnum = 7;
pub const GHOSTTY_CELL_DATA_PROTECTED: GhosttyEnum = 8;
pub const GHOSTTY_CELL_DATA_SEMANTIC_CONTENT: GhosttyEnum = 9;
pub const GHOSTTY_CELL_DATA_COLOR_PALETTE: GhosttyEnum = 10;
pub const GHOSTTY_CELL_DATA_COLOR_RGB: GhosttyEnum = 11;

pub const GHOSTTY_ROW_DATA_INVALID: GhosttyEnum = 0;
pub const GHOSTTY_ROW_DATA_WRAP: GhosttyEnum = 1;
pub const GHOSTTY_ROW_DATA_WRAP_CONTINUATION: GhosttyEnum = 2;
pub const GHOSTTY_ROW_DATA_GRAPHEME: GhosttyEnum = 3;
pub const GHOSTTY_ROW_DATA_STYLED: GhosttyEnum = 4;
pub const GHOSTTY_ROW_DATA_HYPERLINK: GhosttyEnum = 5;
pub const GHOSTTY_ROW_DATA_SEMANTIC_PROMPT: GhosttyEnum = 6;
pub const GHOSTTY_ROW_DATA_KITTY_VIRTUAL_PLACEHOLDER: GhosttyEnum = 7;
pub const GHOSTTY_ROW_DATA_DIRTY: GhosttyEnum = 8;

pub const GHOSTTY_POINT_TAG_ACTIVE: GhosttyEnum = 0;
pub const GHOSTTY_POINT_TAG_VIEWPORT: GhosttyEnum = 1;
pub const GHOSTTY_POINT_TAG_SCREEN: GhosttyEnum = 2;
pub const GHOSTTY_POINT_TAG_HISTORY: GhosttyEnum = 3;

pub const GHOSTTY_TERMINAL_SCREEN_PRIMARY: GhosttyEnum = 0;
pub const GHOSTTY_TERMINAL_SCREEN_ALTERNATE: GhosttyEnum = 1;

pub const GHOSTTY_TERMINAL_BOUNDARY_GROUND: u32 = 1 << 0;
pub const GHOSTTY_TERMINAL_BOUNDARY_APC_START: u32 = 1 << 1;
pub const GHOSTTY_TERMINAL_BOUNDARY_APC_END: u32 = 1 << 2;
pub const GHOSTTY_TERMINAL_BOUNDARY_DCS_START: u32 = 1 << 3;
pub const GHOSTTY_TERMINAL_BOUNDARY_DCS_END: u32 = 1 << 4;
pub const GHOSTTY_TERMINAL_BOUNDARY_OSC_START: u32 = 1 << 5;
pub const GHOSTTY_TERMINAL_BOUNDARY_OSC_END: u32 = 1 << 6;

pub const GHOSTTY_TERMINAL_STRING_KIND_APC: GhosttyEnum = 0;
pub const GHOSTTY_TERMINAL_STRING_KIND_DCS: GhosttyEnum = 1;
pub const GHOSTTY_TERMINAL_STRING_KIND_OSC: GhosttyEnum = 2;
pub const GHOSTTY_TERMINAL_STRING_INTRODUCER_ESC: GhosttyEnum = 0;
pub const GHOSTTY_TERMINAL_STRING_INTRODUCER_C1: GhosttyEnum = 1;
pub const GHOSTTY_TERMINAL_STRING_END_ST_ESC: GhosttyEnum = 0;
pub const GHOSTTY_TERMINAL_STRING_END_ST_C1: GhosttyEnum = 1;
pub const GHOSTTY_TERMINAL_STRING_END_CANCELLED: GhosttyEnum = 2;
pub const GHOSTTY_TERMINAL_STRING_END_ABORTED: GhosttyEnum = 3;
pub const GHOSTTY_TERMINAL_STRING_END_BEL: GhosttyEnum = 4;

pub const GHOSTTY_TERMINAL_DATA_INVALID: GhosttyEnum = 0;
pub const GHOSTTY_TERMINAL_DATA_COLS: GhosttyEnum = 1;
pub const GHOSTTY_TERMINAL_DATA_ROWS: GhosttyEnum = 2;
pub const GHOSTTY_TERMINAL_DATA_CURSOR_X: GhosttyEnum = 3;
pub const GHOSTTY_TERMINAL_DATA_CURSOR_Y: GhosttyEnum = 4;
pub const GHOSTTY_TERMINAL_DATA_CURSOR_PENDING_WRAP: GhosttyEnum = 5;
pub const GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN: GhosttyEnum = 6;
pub const GHOSTTY_TERMINAL_DATA_CURSOR_VISIBLE: GhosttyEnum = 7;
pub const GHOSTTY_TERMINAL_DATA_KITTY_KEYBOARD_FLAGS: GhosttyEnum = 8;
pub const GHOSTTY_TERMINAL_DATA_SCROLLBAR: GhosttyEnum = 9;
pub const GHOSTTY_TERMINAL_DATA_CURSOR_SGR_STYLE: GhosttyEnum = 10;
pub const GHOSTTY_TERMINAL_DATA_MOUSE_TRACKING: GhosttyEnum = 11;
pub const GHOSTTY_TERMINAL_DATA_TITLE: GhosttyEnum = 12;
pub const GHOSTTY_TERMINAL_DATA_PWD: GhosttyEnum = 13;
pub const GHOSTTY_TERMINAL_DATA_TOTAL_ROWS: GhosttyEnum = 14;
pub const GHOSTTY_TERMINAL_DATA_SCROLLBACK_ROWS: GhosttyEnum = 15;
pub const GHOSTTY_TERMINAL_DATA_WIDTH_PX: GhosttyEnum = 16;
pub const GHOSTTY_TERMINAL_DATA_HEIGHT_PX: GhosttyEnum = 17;
pub const GHOSTTY_TERMINAL_DATA_COLOR_FOREGROUND: GhosttyEnum = 18;
pub const GHOSTTY_TERMINAL_DATA_COLOR_BACKGROUND: GhosttyEnum = 19;
pub const GHOSTTY_TERMINAL_DATA_COLOR_CURSOR: GhosttyEnum = 20;
pub const GHOSTTY_TERMINAL_DATA_COLOR_PALETTE: GhosttyEnum = 21;
pub const GHOSTTY_TERMINAL_DATA_COLOR_FOREGROUND_DEFAULT: GhosttyEnum = 22;
pub const GHOSTTY_TERMINAL_DATA_COLOR_BACKGROUND_DEFAULT: GhosttyEnum = 23;
pub const GHOSTTY_TERMINAL_DATA_COLOR_CURSOR_DEFAULT: GhosttyEnum = 24;
pub const GHOSTTY_TERMINAL_DATA_COLOR_PALETTE_DEFAULT: GhosttyEnum = 25;
pub const GHOSTTY_TERMINAL_DATA_KITTY_IMAGE_STORAGE_LIMIT: GhosttyEnum = 26;
pub const GHOSTTY_TERMINAL_DATA_KITTY_IMAGE_MEDIUM_FILE: GhosttyEnum = 27;
pub const GHOSTTY_TERMINAL_DATA_KITTY_IMAGE_MEDIUM_TEMP_FILE: GhosttyEnum = 28;
pub const GHOSTTY_TERMINAL_DATA_KITTY_IMAGE_MEDIUM_SHARED_MEM: GhosttyEnum = 29;
pub const GHOSTTY_TERMINAL_DATA_KITTY_GRAPHICS: GhosttyEnum = 30;
pub const GHOSTTY_TERMINAL_DATA_SELECTION: GhosttyEnum = 31;
pub const GHOSTTY_TERMINAL_DATA_VIEWPORT_ACTIVE: GhosttyEnum = 32;
pub const GHOSTTY_TERMINAL_DATA_VT_PROCESSING_ERROR: GhosttyEnum = 33;
pub const GHOSTTY_TERMINAL_DATA_SCROLLBACK_MAX_BYTES: GhosttyEnum = 34;
pub const GHOSTTY_TERMINAL_DATA_SCROLLBACK_MAX_LINES: GhosttyEnum = 35;
pub const GHOSTTY_TERMINAL_DATA_CONTINUATION_MAX_BYTES: GhosttyEnum = 36;
pub const GHOSTTY_TERMINAL_DATA_MODE: GhosttyEnum = 37;
pub const GHOSTTY_TERMINAL_DATA_VT_GROUND: GhosttyEnum = 38;
pub const GHOSTTY_TERMINAL_DATA_CURSOR_AT_PROMPT: GhosttyEnum = 39;
pub const GHOSTTY_TERMINAL_DATA_CLIPBOARD_WRITE_MAX_BYTES: GhosttyEnum = 40;
pub const GHOSTTY_TERMINAL_DATA_MOUSE_SHAPE: GhosttyEnum = 41;
pub const GHOSTTY_TERMINAL_DATA_MEMORY_USAGE: GhosttyEnum = 42;

pub const GHOSTTY_TERMINAL_OPT_INVALID: GhosttyEnum = 0;
pub const GHOSTTY_TERMINAL_OPT_USERDATA: GhosttyEnum = 1;
pub const GHOSTTY_TERMINAL_OPT_WRITE_PTY: GhosttyEnum = 2;
pub const GHOSTTY_TERMINAL_OPT_BELL: GhosttyEnum = 3;
pub const GHOSTTY_TERMINAL_OPT_ENQUIRY: GhosttyEnum = 4;
pub const GHOSTTY_TERMINAL_OPT_XTVERSION: GhosttyEnum = 5;
pub const GHOSTTY_TERMINAL_OPT_TITLE_CHANGED: GhosttyEnum = 6;
pub const GHOSTTY_TERMINAL_OPT_SIZE: GhosttyEnum = 7;
pub const GHOSTTY_TERMINAL_OPT_COLOR_SCHEME: GhosttyEnum = 8;
pub const GHOSTTY_TERMINAL_OPT_DEVICE_ATTRIBUTES: GhosttyEnum = 9;
pub const GHOSTTY_TERMINAL_OPT_TITLE: GhosttyEnum = 10;
pub const GHOSTTY_TERMINAL_OPT_PWD: GhosttyEnum = 11;
pub const GHOSTTY_TERMINAL_OPT_COLOR_FOREGROUND: GhosttyEnum = 12;
pub const GHOSTTY_TERMINAL_OPT_COLOR_BACKGROUND: GhosttyEnum = 13;
pub const GHOSTTY_TERMINAL_OPT_COLOR_CURSOR: GhosttyEnum = 14;
pub const GHOSTTY_TERMINAL_OPT_COLOR_PALETTE: GhosttyEnum = 15;
pub const GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_STORAGE_LIMIT: GhosttyEnum = 16;
pub const GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_FILE: GhosttyEnum = 17;
pub const GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_TEMP_FILE: GhosttyEnum = 18;
pub const GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_SHARED_MEM: GhosttyEnum = 19;
pub const GHOSTTY_TERMINAL_OPT_APC_MAX_BYTES: GhosttyEnum = 20;
pub const GHOSTTY_TERMINAL_OPT_APC_MAX_BYTES_KITTY: GhosttyEnum = 21;
pub const GHOSTTY_TERMINAL_OPT_SELECTION: GhosttyEnum = 22;
pub const GHOSTTY_TERMINAL_OPT_DEFAULT_CURSOR_STYLE: GhosttyEnum = 23;
pub const GHOSTTY_TERMINAL_OPT_DEFAULT_CURSOR_BLINK: GhosttyEnum = 24;
pub const GHOSTTY_TERMINAL_OPT_GLYPH_PROTOCOL: GhosttyEnum = 25;
pub const GHOSTTY_TERMINAL_OPT_PWD_CHANGED: GhosttyEnum = 26;
pub const GHOSTTY_TERMINAL_OPT_CLIPBOARD_WRITE: GhosttyEnum = 27;
pub const GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_BYTES: GhosttyEnum = 28;
pub const GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_LINES: GhosttyEnum = 29;
pub const GHOSTTY_TERMINAL_OPT_DESKTOP_NOTIFICATION: GhosttyEnum = 30;
pub const GHOSTTY_TERMINAL_OPT_PROGRESS_REPORT: GhosttyEnum = 31;
pub const GHOSTTY_TERMINAL_OPT_CONTINUATION_MAX_BYTES: GhosttyEnum = 32;
pub const GHOSTTY_TERMINAL_OPT_TITLE_REPORT: GhosttyEnum = 33;
pub const GHOSTTY_TERMINAL_OPT_MODE_DEFAULT: GhosttyEnum = 34;
pub const GHOSTTY_TERMINAL_OPT_MODE: GhosttyEnum = 35;
pub const GHOSTTY_TERMINAL_OPT_UNKNOWN_SEQUENCE: GhosttyEnum = 36;
pub const GHOSTTY_TERMINAL_OPT_UNKNOWN_MAX_BYTES: GhosttyEnum = 37;
pub const GHOSTTY_TERMINAL_OPT_TERMINFO_NAME: GhosttyEnum = 38;
pub const GHOSTTY_TERMINAL_OPT_CLIPBOARD_READ: GhosttyEnum = 39;
pub const GHOSTTY_TERMINAL_OPT_CLIPBOARD_WRITE_MAX_BYTES: GhosttyEnum = 40;
pub const GHOSTTY_TERMINAL_OPT_RESIZE_PULL_SCROLLBACK: GhosttyEnum = 41;
pub const GHOSTTY_TERMINAL_OPT_RENDER_HOLD: GhosttyEnum = 42;
pub const GHOSTTY_TERMINAL_OPT_SEMANTIC_PROMPT: GhosttyEnum = 43;
pub const GHOSTTY_TERMINAL_OPT_RESET: GhosttyEnum = 44;
pub const GHOSTTY_TERMINAL_OPT_XT_CHECKSUM_REPORT: GhosttyEnum = 45;
pub const GHOSTTY_TERMINAL_OPT_XT_CHECKSUM_EXTENSION: GhosttyEnum = 46;
pub const GHOSTTY_TERMINAL_OPT_PROGRAM_STATUS: GhosttyEnum = 47;
pub const GHOSTTY_TERMINAL_OPT_RESIZE_WINDOW: GhosttyEnum = 48;

pub const GHOSTTY_FORMATTER_FORMAT_PLAIN: GhosttyEnum = 0;
pub const GHOSTTY_FORMATTER_FORMAT_VT: GhosttyEnum = 1;
pub const GHOSTTY_FORMATTER_FORMAT_HTML: GhosttyEnum = 2;

pub const GHOSTTY_FORMATTER_EXTRA_PALETTE: GhosttyFormatterExtras = 1 << 0;
pub const GHOSTTY_FORMATTER_EXTRA_MODES: GhosttyFormatterExtras = 1 << 1;
pub const GHOSTTY_FORMATTER_EXTRA_SCROLLING_REGION: GhosttyFormatterExtras = 1 << 2;
pub const GHOSTTY_FORMATTER_EXTRA_TABSTOPS: GhosttyFormatterExtras = 1 << 3;
pub const GHOSTTY_FORMATTER_EXTRA_PWD: GhosttyFormatterExtras = 1 << 4;
pub const GHOSTTY_FORMATTER_EXTRA_KEYBOARD: GhosttyFormatterExtras = 1 << 5;
pub const GHOSTTY_FORMATTER_EXTRA_CURSOR: GhosttyFormatterExtras = 1 << 6;
pub const GHOSTTY_FORMATTER_EXTRA_STYLE: GhosttyFormatterExtras = 1 << 7;
pub const GHOSTTY_FORMATTER_EXTRA_HYPERLINK: GhosttyFormatterExtras = 1 << 8;
pub const GHOSTTY_FORMATTER_EXTRA_PROTECTION: GhosttyFormatterExtras = 1 << 9;
pub const GHOSTTY_FORMATTER_EXTRA_KITTY_KEYBOARD: GhosttyFormatterExtras = 1 << 10;
pub const GHOSTTY_FORMATTER_EXTRA_CHARSETS: GhosttyFormatterExtras = 1 << 11;
pub const GHOSTTY_FORMATTER_EXTRAS_NONE: GhosttyFormatterExtras = 0;
pub const GHOSTTY_FORMATTER_EXTRAS_ALL: GhosttyFormatterExtras = 0xFFF;
pub const GHOSTTY_FORMATTER_EXTRAS_DEFAULT: GhosttyFormatterExtras = GHOSTTY_FORMATTER_EXTRA_PALETTE
    | GHOSTTY_FORMATTER_EXTRA_STYLE
    | GHOSTTY_FORMATTER_EXTRA_HYPERLINK;

pub const GHOSTTY_FORMATTER_OPT_INVALID: GhosttyEnum = 0;
pub const GHOSTTY_FORMATTER_OPT_EMIT: GhosttyEnum = 1;
pub const GHOSTTY_FORMATTER_OPT_UNWRAP: GhosttyEnum = 2;
pub const GHOSTTY_FORMATTER_OPT_TRIM: GhosttyEnum = 3;
pub const GHOSTTY_FORMATTER_OPT_SELECTION: GhosttyEnum = 4;
pub const GHOSTTY_FORMATTER_OPT_EXTRAS: GhosttyEnum = 5;

pub const GHOSTTY_MODS_SHIFT: GhosttyMods = 1 << 0;
pub const GHOSTTY_MODS_CTRL: GhosttyMods = 1 << 1;
pub const GHOSTTY_MODS_ALT: GhosttyMods = 1 << 2;
pub const GHOSTTY_MODS_SUPER: GhosttyMods = 1 << 3;
pub const GHOSTTY_MODS_CAPS_LOCK: GhosttyMods = 1 << 4;
pub const GHOSTTY_MODS_NUM_LOCK: GhosttyMods = 1 << 5;
pub const GHOSTTY_MODS_SHIFT_SIDE: GhosttyMods = 1 << 6;
pub const GHOSTTY_MODS_CTRL_SIDE: GhosttyMods = 1 << 7;
pub const GHOSTTY_MODS_ALT_SIDE: GhosttyMods = 1 << 8;
pub const GHOSTTY_MODS_SUPER_SIDE: GhosttyMods = 1 << 9;

pub const GHOSTTY_KEY_ACTION_RELEASE: GhosttyEnum = 0;
pub const GHOSTTY_KEY_ACTION_PRESS: GhosttyEnum = 1;
pub const GHOSTTY_KEY_ACTION_REPEAT: GhosttyEnum = 2;

pub const GHOSTTY_KEY_UNIDENTIFIED: GhosttyKey = 0;
pub const GHOSTTY_KEY_BACKQUOTE: GhosttyKey = 1;
pub const GHOSTTY_KEY_BACKSLASH: GhosttyKey = 2;
pub const GHOSTTY_KEY_BRACKET_LEFT: GhosttyKey = 3;
pub const GHOSTTY_KEY_BRACKET_RIGHT: GhosttyKey = 4;
pub const GHOSTTY_KEY_COMMA: GhosttyKey = 5;
pub const GHOSTTY_KEY_DIGIT_0: GhosttyKey = 6;
pub const GHOSTTY_KEY_DIGIT_1: GhosttyKey = 7;
pub const GHOSTTY_KEY_DIGIT_2: GhosttyKey = 8;
pub const GHOSTTY_KEY_DIGIT_3: GhosttyKey = 9;
pub const GHOSTTY_KEY_DIGIT_4: GhosttyKey = 10;
pub const GHOSTTY_KEY_DIGIT_5: GhosttyKey = 11;
pub const GHOSTTY_KEY_DIGIT_6: GhosttyKey = 12;
pub const GHOSTTY_KEY_DIGIT_7: GhosttyKey = 13;
pub const GHOSTTY_KEY_DIGIT_8: GhosttyKey = 14;
pub const GHOSTTY_KEY_DIGIT_9: GhosttyKey = 15;
pub const GHOSTTY_KEY_EQUAL: GhosttyKey = 16;
pub const GHOSTTY_KEY_INTL_BACKSLASH: GhosttyKey = 17;
pub const GHOSTTY_KEY_INTL_RO: GhosttyKey = 18;
pub const GHOSTTY_KEY_INTL_YEN: GhosttyKey = 19;
pub const GHOSTTY_KEY_A: GhosttyKey = 20;
pub const GHOSTTY_KEY_B: GhosttyKey = 21;
pub const GHOSTTY_KEY_C: GhosttyKey = 22;
pub const GHOSTTY_KEY_D: GhosttyKey = 23;
pub const GHOSTTY_KEY_E: GhosttyKey = 24;
pub const GHOSTTY_KEY_F: GhosttyKey = 25;
pub const GHOSTTY_KEY_G: GhosttyKey = 26;
pub const GHOSTTY_KEY_H: GhosttyKey = 27;
pub const GHOSTTY_KEY_I: GhosttyKey = 28;
pub const GHOSTTY_KEY_J: GhosttyKey = 29;
pub const GHOSTTY_KEY_K: GhosttyKey = 30;
pub const GHOSTTY_KEY_L: GhosttyKey = 31;
pub const GHOSTTY_KEY_M: GhosttyKey = 32;
pub const GHOSTTY_KEY_N: GhosttyKey = 33;
pub const GHOSTTY_KEY_O: GhosttyKey = 34;
pub const GHOSTTY_KEY_P: GhosttyKey = 35;
pub const GHOSTTY_KEY_Q: GhosttyKey = 36;
pub const GHOSTTY_KEY_R: GhosttyKey = 37;
pub const GHOSTTY_KEY_S: GhosttyKey = 38;
pub const GHOSTTY_KEY_T: GhosttyKey = 39;
pub const GHOSTTY_KEY_U: GhosttyKey = 40;
pub const GHOSTTY_KEY_V: GhosttyKey = 41;
pub const GHOSTTY_KEY_W: GhosttyKey = 42;
pub const GHOSTTY_KEY_X: GhosttyKey = 43;
pub const GHOSTTY_KEY_Y: GhosttyKey = 44;
pub const GHOSTTY_KEY_Z: GhosttyKey = 45;
pub const GHOSTTY_KEY_MINUS: GhosttyKey = 46;
pub const GHOSTTY_KEY_PERIOD: GhosttyKey = 47;
pub const GHOSTTY_KEY_QUOTE: GhosttyKey = 48;
pub const GHOSTTY_KEY_SEMICOLON: GhosttyKey = 49;
pub const GHOSTTY_KEY_SLASH: GhosttyKey = 50;
pub const GHOSTTY_KEY_ALT_LEFT: GhosttyKey = 51;
pub const GHOSTTY_KEY_ALT_RIGHT: GhosttyKey = 52;
pub const GHOSTTY_KEY_BACKSPACE: GhosttyKey = 53;
pub const GHOSTTY_KEY_CAPS_LOCK: GhosttyKey = 54;
pub const GHOSTTY_KEY_CONTEXT_MENU: GhosttyKey = 55;
pub const GHOSTTY_KEY_CONTROL_LEFT: GhosttyKey = 56;
pub const GHOSTTY_KEY_CONTROL_RIGHT: GhosttyKey = 57;
pub const GHOSTTY_KEY_ENTER: GhosttyKey = 58;
pub const GHOSTTY_KEY_META_LEFT: GhosttyKey = 59;
pub const GHOSTTY_KEY_META_RIGHT: GhosttyKey = 60;
pub const GHOSTTY_KEY_SHIFT_LEFT: GhosttyKey = 61;
pub const GHOSTTY_KEY_SHIFT_RIGHT: GhosttyKey = 62;
pub const GHOSTTY_KEY_SPACE: GhosttyKey = 63;
pub const GHOSTTY_KEY_TAB: GhosttyKey = 64;
pub const GHOSTTY_KEY_CONVERT: GhosttyKey = 65;
pub const GHOSTTY_KEY_KANA_MODE: GhosttyKey = 66;
pub const GHOSTTY_KEY_NON_CONVERT: GhosttyKey = 67;
pub const GHOSTTY_KEY_DELETE: GhosttyKey = 68;
pub const GHOSTTY_KEY_END: GhosttyKey = 69;
pub const GHOSTTY_KEY_HELP: GhosttyKey = 70;
pub const GHOSTTY_KEY_HOME: GhosttyKey = 71;
pub const GHOSTTY_KEY_INSERT: GhosttyKey = 72;
pub const GHOSTTY_KEY_PAGE_DOWN: GhosttyKey = 73;
pub const GHOSTTY_KEY_PAGE_UP: GhosttyKey = 74;
pub const GHOSTTY_KEY_ARROW_DOWN: GhosttyKey = 75;
pub const GHOSTTY_KEY_ARROW_LEFT: GhosttyKey = 76;
pub const GHOSTTY_KEY_ARROW_RIGHT: GhosttyKey = 77;
pub const GHOSTTY_KEY_ARROW_UP: GhosttyKey = 78;
pub const GHOSTTY_KEY_NUM_LOCK: GhosttyKey = 79;
pub const GHOSTTY_KEY_NUMPAD_0: GhosttyKey = 80;
pub const GHOSTTY_KEY_NUMPAD_1: GhosttyKey = 81;
pub const GHOSTTY_KEY_NUMPAD_2: GhosttyKey = 82;
pub const GHOSTTY_KEY_NUMPAD_3: GhosttyKey = 83;
pub const GHOSTTY_KEY_NUMPAD_4: GhosttyKey = 84;
pub const GHOSTTY_KEY_NUMPAD_5: GhosttyKey = 85;
pub const GHOSTTY_KEY_NUMPAD_6: GhosttyKey = 86;
pub const GHOSTTY_KEY_NUMPAD_7: GhosttyKey = 87;
pub const GHOSTTY_KEY_NUMPAD_8: GhosttyKey = 88;
pub const GHOSTTY_KEY_NUMPAD_9: GhosttyKey = 89;
pub const GHOSTTY_KEY_NUMPAD_ADD: GhosttyKey = 90;
pub const GHOSTTY_KEY_NUMPAD_BACKSPACE: GhosttyKey = 91;
pub const GHOSTTY_KEY_NUMPAD_CLEAR: GhosttyKey = 92;
pub const GHOSTTY_KEY_NUMPAD_CLEAR_ENTRY: GhosttyKey = 93;
pub const GHOSTTY_KEY_NUMPAD_COMMA: GhosttyKey = 94;
pub const GHOSTTY_KEY_NUMPAD_DECIMAL: GhosttyKey = 95;
pub const GHOSTTY_KEY_NUMPAD_DIVIDE: GhosttyKey = 96;
pub const GHOSTTY_KEY_NUMPAD_ENTER: GhosttyKey = 97;
pub const GHOSTTY_KEY_NUMPAD_EQUAL: GhosttyKey = 98;
pub const GHOSTTY_KEY_NUMPAD_MEMORY_ADD: GhosttyKey = 99;
pub const GHOSTTY_KEY_NUMPAD_MEMORY_CLEAR: GhosttyKey = 100;
pub const GHOSTTY_KEY_NUMPAD_MEMORY_RECALL: GhosttyKey = 101;
pub const GHOSTTY_KEY_NUMPAD_MEMORY_STORE: GhosttyKey = 102;
pub const GHOSTTY_KEY_NUMPAD_MEMORY_SUBTRACT: GhosttyKey = 103;
pub const GHOSTTY_KEY_NUMPAD_MULTIPLY: GhosttyKey = 104;
pub const GHOSTTY_KEY_NUMPAD_PAREN_LEFT: GhosttyKey = 105;
pub const GHOSTTY_KEY_NUMPAD_PAREN_RIGHT: GhosttyKey = 106;
pub const GHOSTTY_KEY_NUMPAD_SUBTRACT: GhosttyKey = 107;
pub const GHOSTTY_KEY_NUMPAD_SEPARATOR: GhosttyKey = 108;
pub const GHOSTTY_KEY_NUMPAD_UP: GhosttyKey = 109;
pub const GHOSTTY_KEY_NUMPAD_DOWN: GhosttyKey = 110;
pub const GHOSTTY_KEY_NUMPAD_RIGHT: GhosttyKey = 111;
pub const GHOSTTY_KEY_NUMPAD_LEFT: GhosttyKey = 112;
pub const GHOSTTY_KEY_NUMPAD_BEGIN: GhosttyKey = 113;
pub const GHOSTTY_KEY_NUMPAD_HOME: GhosttyKey = 114;
pub const GHOSTTY_KEY_NUMPAD_END: GhosttyKey = 115;
pub const GHOSTTY_KEY_NUMPAD_INSERT: GhosttyKey = 116;
pub const GHOSTTY_KEY_NUMPAD_DELETE: GhosttyKey = 117;
pub const GHOSTTY_KEY_NUMPAD_PAGE_UP: GhosttyKey = 118;
pub const GHOSTTY_KEY_NUMPAD_PAGE_DOWN: GhosttyKey = 119;
pub const GHOSTTY_KEY_ESCAPE: GhosttyKey = 120;
pub const GHOSTTY_KEY_F1: GhosttyKey = 121;
pub const GHOSTTY_KEY_F2: GhosttyKey = 122;
pub const GHOSTTY_KEY_F3: GhosttyKey = 123;
pub const GHOSTTY_KEY_F4: GhosttyKey = 124;
pub const GHOSTTY_KEY_F5: GhosttyKey = 125;
pub const GHOSTTY_KEY_F6: GhosttyKey = 126;
pub const GHOSTTY_KEY_F7: GhosttyKey = 127;
pub const GHOSTTY_KEY_F8: GhosttyKey = 128;
pub const GHOSTTY_KEY_F9: GhosttyKey = 129;
pub const GHOSTTY_KEY_F10: GhosttyKey = 130;
pub const GHOSTTY_KEY_F11: GhosttyKey = 131;
pub const GHOSTTY_KEY_F12: GhosttyKey = 132;
pub const GHOSTTY_KEY_F13: GhosttyKey = 133;
pub const GHOSTTY_KEY_F14: GhosttyKey = 134;
pub const GHOSTTY_KEY_F15: GhosttyKey = 135;
pub const GHOSTTY_KEY_F16: GhosttyKey = 136;
pub const GHOSTTY_KEY_F17: GhosttyKey = 137;
pub const GHOSTTY_KEY_F18: GhosttyKey = 138;
pub const GHOSTTY_KEY_F19: GhosttyKey = 139;
pub const GHOSTTY_KEY_F20: GhosttyKey = 140;
pub const GHOSTTY_KEY_F21: GhosttyKey = 141;
pub const GHOSTTY_KEY_F22: GhosttyKey = 142;
pub const GHOSTTY_KEY_F23: GhosttyKey = 143;
pub const GHOSTTY_KEY_F24: GhosttyKey = 144;
pub const GHOSTTY_KEY_F25: GhosttyKey = 145;
pub const GHOSTTY_KEY_FN: GhosttyKey = 146;
pub const GHOSTTY_KEY_FN_LOCK: GhosttyKey = 147;
pub const GHOSTTY_KEY_PRINT_SCREEN: GhosttyKey = 148;
pub const GHOSTTY_KEY_SCROLL_LOCK: GhosttyKey = 149;
pub const GHOSTTY_KEY_PAUSE: GhosttyKey = 150;
pub const GHOSTTY_KEY_BROWSER_BACK: GhosttyKey = 151;
pub const GHOSTTY_KEY_BROWSER_FAVORITES: GhosttyKey = 152;
pub const GHOSTTY_KEY_BROWSER_FORWARD: GhosttyKey = 153;
pub const GHOSTTY_KEY_BROWSER_HOME: GhosttyKey = 154;
pub const GHOSTTY_KEY_BROWSER_REFRESH: GhosttyKey = 155;
pub const GHOSTTY_KEY_BROWSER_SEARCH: GhosttyKey = 156;
pub const GHOSTTY_KEY_BROWSER_STOP: GhosttyKey = 157;
pub const GHOSTTY_KEY_EJECT: GhosttyKey = 158;
pub const GHOSTTY_KEY_LAUNCH_APP_1: GhosttyKey = 159;
pub const GHOSTTY_KEY_LAUNCH_APP_2: GhosttyKey = 160;
pub const GHOSTTY_KEY_LAUNCH_MAIL: GhosttyKey = 161;
pub const GHOSTTY_KEY_MEDIA_PLAY_PAUSE: GhosttyKey = 162;
pub const GHOSTTY_KEY_MEDIA_SELECT: GhosttyKey = 163;
pub const GHOSTTY_KEY_MEDIA_STOP: GhosttyKey = 164;
pub const GHOSTTY_KEY_MEDIA_TRACK_NEXT: GhosttyKey = 165;
pub const GHOSTTY_KEY_MEDIA_TRACK_PREVIOUS: GhosttyKey = 166;
pub const GHOSTTY_KEY_POWER: GhosttyKey = 167;
pub const GHOSTTY_KEY_SLEEP: GhosttyKey = 168;
pub const GHOSTTY_KEY_AUDIO_VOLUME_DOWN: GhosttyKey = 169;
pub const GHOSTTY_KEY_AUDIO_VOLUME_MUTE: GhosttyKey = 170;
pub const GHOSTTY_KEY_AUDIO_VOLUME_UP: GhosttyKey = 171;
pub const GHOSTTY_KEY_WAKE_UP: GhosttyKey = 172;
pub const GHOSTTY_KEY_COPY: GhosttyKey = 173;
pub const GHOSTTY_KEY_CUT: GhosttyKey = 174;
pub const GHOSTTY_KEY_PASTE: GhosttyKey = 175;

pub const GHOSTTY_KITTY_KEY_DISABLED: GhosttyKittyKeyFlags = 0;
pub const GHOSTTY_KITTY_KEY_DISAMBIGUATE: GhosttyKittyKeyFlags = 1 << 0;
pub const GHOSTTY_KITTY_KEY_REPORT_EVENTS: GhosttyKittyKeyFlags = 1 << 1;
pub const GHOSTTY_KITTY_KEY_REPORT_ALTERNATES: GhosttyKittyKeyFlags = 1 << 2;
pub const GHOSTTY_KITTY_KEY_REPORT_ALL: GhosttyKittyKeyFlags = 1 << 3;
pub const GHOSTTY_KITTY_KEY_REPORT_ASSOCIATED: GhosttyKittyKeyFlags = 1 << 4;

pub const GHOSTTY_OPTION_AS_ALT_FALSE: GhosttyEnum = 0;
pub const GHOSTTY_OPTION_AS_ALT_TRUE: GhosttyEnum = 1;
pub const GHOSTTY_OPTION_AS_ALT_LEFT: GhosttyEnum = 2;
pub const GHOSTTY_OPTION_AS_ALT_RIGHT: GhosttyEnum = 3;

pub const GHOSTTY_KEY_ENCODER_OPT_INVALID: GhosttyEnum = 0;
pub const GHOSTTY_KEY_ENCODER_OPT_CURSOR_KEY_APPLICATION: GhosttyEnum = 1;
pub const GHOSTTY_KEY_ENCODER_OPT_KEYPAD_KEY_APPLICATION: GhosttyEnum = 2;
pub const GHOSTTY_KEY_ENCODER_OPT_IGNORE_KEYPAD_WITH_NUMLOCK: GhosttyEnum = 3;
pub const GHOSTTY_KEY_ENCODER_OPT_ALT_ESC_PREFIX: GhosttyEnum = 4;
pub const GHOSTTY_KEY_ENCODER_OPT_MODIFY_OTHER_KEYS_STATE_2: GhosttyEnum = 5;
pub const GHOSTTY_KEY_ENCODER_OPT_KITTY_FLAGS: GhosttyEnum = 6;
pub const GHOSTTY_KEY_ENCODER_OPT_MACOS_OPTION_AS_ALT: GhosttyEnum = 7;
pub const GHOSTTY_KEY_ENCODER_OPT_BACKARROW_KEY_MODE: GhosttyEnum = 8;

pub const GHOSTTY_MOUSE_TRACKING_NONE: GhosttyEnum = 0;
pub const GHOSTTY_MOUSE_TRACKING_X10: GhosttyEnum = 1;
pub const GHOSTTY_MOUSE_TRACKING_NORMAL: GhosttyEnum = 2;
pub const GHOSTTY_MOUSE_TRACKING_BUTTON: GhosttyEnum = 3;
pub const GHOSTTY_MOUSE_TRACKING_ANY: GhosttyEnum = 4;

pub const GHOSTTY_MOUSE_FORMAT_X10: GhosttyEnum = 0;
pub const GHOSTTY_MOUSE_FORMAT_UTF8: GhosttyEnum = 1;
pub const GHOSTTY_MOUSE_FORMAT_SGR: GhosttyEnum = 2;
pub const GHOSTTY_MOUSE_FORMAT_URXVT: GhosttyEnum = 3;
pub const GHOSTTY_MOUSE_FORMAT_SGR_PIXELS: GhosttyEnum = 4;

pub const GHOSTTY_MOUSE_ENCODER_OPT_INVALID: GhosttyEnum = 0;
pub const GHOSTTY_MOUSE_ENCODER_OPT_EVENT: GhosttyEnum = 1;
pub const GHOSTTY_MOUSE_ENCODER_OPT_FORMAT: GhosttyEnum = 2;
pub const GHOSTTY_MOUSE_ENCODER_OPT_SIZE: GhosttyEnum = 3;
pub const GHOSTTY_MOUSE_ENCODER_OPT_ANY_BUTTON_PRESSED: GhosttyEnum = 4;
pub const GHOSTTY_MOUSE_ENCODER_OPT_TRACK_LAST_CELL: GhosttyEnum = 5;

pub const GHOSTTY_MOUSE_ACTION_PRESS: GhosttyEnum = 0;
pub const GHOSTTY_MOUSE_ACTION_RELEASE: GhosttyEnum = 1;
pub const GHOSTTY_MOUSE_ACTION_MOTION: GhosttyEnum = 2;

pub const GHOSTTY_MOUSE_BUTTON_UNKNOWN: GhosttyEnum = 0;
pub const GHOSTTY_MOUSE_BUTTON_LEFT: GhosttyEnum = 1;
pub const GHOSTTY_MOUSE_BUTTON_RIGHT: GhosttyEnum = 2;
pub const GHOSTTY_MOUSE_BUTTON_MIDDLE: GhosttyEnum = 3;
pub const GHOSTTY_MOUSE_BUTTON_FOUR: GhosttyEnum = 4;
pub const GHOSTTY_MOUSE_BUTTON_FIVE: GhosttyEnum = 5;
pub const GHOSTTY_MOUSE_BUTTON_SIX: GhosttyEnum = 6;
pub const GHOSTTY_MOUSE_BUTTON_SEVEN: GhosttyEnum = 7;
pub const GHOSTTY_MOUSE_BUTTON_EIGHT: GhosttyEnum = 8;
pub const GHOSTTY_MOUSE_BUTTON_NINE: GhosttyEnum = 9;
pub const GHOSTTY_MOUSE_BUTTON_TEN: GhosttyEnum = 10;
pub const GHOSTTY_MOUSE_BUTTON_ELEVEN: GhosttyEnum = 11;

pub const GHOSTTY_MOUSE_SHAPE_DEFAULT: GhosttyEnum = 0;
pub const GHOSTTY_MOUSE_SHAPE_CONTEXT_MENU: GhosttyEnum = 1;
pub const GHOSTTY_MOUSE_SHAPE_HELP: GhosttyEnum = 2;
pub const GHOSTTY_MOUSE_SHAPE_POINTER: GhosttyEnum = 3;
pub const GHOSTTY_MOUSE_SHAPE_PROGRESS: GhosttyEnum = 4;
pub const GHOSTTY_MOUSE_SHAPE_WAIT: GhosttyEnum = 5;
pub const GHOSTTY_MOUSE_SHAPE_CELL: GhosttyEnum = 6;
pub const GHOSTTY_MOUSE_SHAPE_CROSSHAIR: GhosttyEnum = 7;
pub const GHOSTTY_MOUSE_SHAPE_TEXT: GhosttyEnum = 8;
pub const GHOSTTY_MOUSE_SHAPE_VERTICAL_TEXT: GhosttyEnum = 9;
pub const GHOSTTY_MOUSE_SHAPE_ALIAS: GhosttyEnum = 10;
pub const GHOSTTY_MOUSE_SHAPE_COPY: GhosttyEnum = 11;
pub const GHOSTTY_MOUSE_SHAPE_MOVE: GhosttyEnum = 12;
pub const GHOSTTY_MOUSE_SHAPE_NO_DROP: GhosttyEnum = 13;
pub const GHOSTTY_MOUSE_SHAPE_NOT_ALLOWED: GhosttyEnum = 14;
pub const GHOSTTY_MOUSE_SHAPE_GRAB: GhosttyEnum = 15;
pub const GHOSTTY_MOUSE_SHAPE_GRABBING: GhosttyEnum = 16;
pub const GHOSTTY_MOUSE_SHAPE_ALL_SCROLL: GhosttyEnum = 17;
pub const GHOSTTY_MOUSE_SHAPE_COL_RESIZE: GhosttyEnum = 18;
pub const GHOSTTY_MOUSE_SHAPE_ROW_RESIZE: GhosttyEnum = 19;
pub const GHOSTTY_MOUSE_SHAPE_N_RESIZE: GhosttyEnum = 20;
pub const GHOSTTY_MOUSE_SHAPE_E_RESIZE: GhosttyEnum = 21;
pub const GHOSTTY_MOUSE_SHAPE_S_RESIZE: GhosttyEnum = 22;
pub const GHOSTTY_MOUSE_SHAPE_W_RESIZE: GhosttyEnum = 23;
pub const GHOSTTY_MOUSE_SHAPE_NE_RESIZE: GhosttyEnum = 24;
pub const GHOSTTY_MOUSE_SHAPE_NW_RESIZE: GhosttyEnum = 25;
pub const GHOSTTY_MOUSE_SHAPE_SE_RESIZE: GhosttyEnum = 26;
pub const GHOSTTY_MOUSE_SHAPE_SW_RESIZE: GhosttyEnum = 27;
pub const GHOSTTY_MOUSE_SHAPE_EW_RESIZE: GhosttyEnum = 28;
pub const GHOSTTY_MOUSE_SHAPE_NS_RESIZE: GhosttyEnum = 29;
pub const GHOSTTY_MOUSE_SHAPE_NESW_RESIZE: GhosttyEnum = 30;
pub const GHOSTTY_MOUSE_SHAPE_NWSE_RESIZE: GhosttyEnum = 31;
pub const GHOSTTY_MOUSE_SHAPE_ZOOM_IN: GhosttyEnum = 32;
pub const GHOSTTY_MOUSE_SHAPE_ZOOM_OUT: GhosttyEnum = 33;

pub const GHOSTTY_SNAPSHOT_DECODER_OPT_INVALID: GhosttyEnum = 0;
pub const GHOSTTY_SNAPSHOT_DECODER_OPT_MAX_CONTINUATION_BYTES: GhosttyEnum = 1;
pub const GHOSTTY_SNAPSHOT_DECODER_OPT_RETAIN_CONTINUATION: GhosttyEnum = 2;
pub const GHOSTTY_SNAPSHOT_DECODER_OPT_COMPRESS_HISTORY: GhosttyEnum = 3;
pub const GHOSTTY_SNAPSHOT_DECODER_OPT_KITTY_IMAGE_STORAGE_LIMIT: GhosttyEnum = 4;

pub const GHOSTTY_SNAPSHOT_DECODER_PROGRESS_KIND_PAGE: GhosttyEnum = 0;
pub const GHOSTTY_SNAPSHOT_DECODER_PROGRESS_KIND_IMAGE: GhosttyEnum = 1;

pub const GHOSTTY_SNAPSHOT_DECODER_DATA_INVALID: GhosttyEnum = 0;
pub const GHOSTTY_SNAPSHOT_DECODER_DATA_MAX_CONTINUATION_BYTES: GhosttyEnum = 1;
pub const GHOSTTY_SNAPSHOT_DECODER_DATA_SOURCE_OFFSET: GhosttyEnum = 2;
pub const GHOSTTY_SNAPSHOT_DECODER_DATA_HISTORY_ROWS_PRIMARY: GhosttyEnum = 3;
pub const GHOSTTY_SNAPSHOT_DECODER_DATA_HISTORY_ROWS_ALTERNATE: GhosttyEnum = 4;
pub const GHOSTTY_SNAPSHOT_DECODER_DATA_PROGRESS_SCREEN: GhosttyEnum = 5;
pub const GHOSTTY_SNAPSHOT_DECODER_DATA_PROGRESS_ROWS: GhosttyEnum = 6;
pub const GHOSTTY_SNAPSHOT_DECODER_DATA_PROGRESS_REMAINING: GhosttyEnum = 7;
pub const GHOSTTY_SNAPSHOT_DECODER_DATA_RETAIN_CONTINUATION: GhosttyEnum = 8;
pub const GHOSTTY_SNAPSHOT_DECODER_DATA_COMPRESS_HISTORY: GhosttyEnum = 9;
pub const GHOSTTY_SNAPSHOT_DECODER_DATA_KITTY_IMAGE_STORAGE_LIMIT: GhosttyEnum = 10;
pub const GHOSTTY_SNAPSHOT_DECODER_DATA_PROGRESS_KIND: GhosttyEnum = 11;
pub const GHOSTTY_SNAPSHOT_DECODER_DATA_PROGRESS_IMAGE_ID: GhosttyEnum = 12;
pub const GHOSTTY_SNAPSHOT_DECODER_DATA_PROGRESS_IMAGE_FRAME: GhosttyEnum = 13;
pub const GHOSTTY_SNAPSHOT_DECODER_DATA_PROGRESS_IMAGE_BYTES: GhosttyEnum = 14;
pub const GHOSTTY_SNAPSHOT_DECODER_DATA_KITTY_IMAGES_PRIMARY: GhosttyEnum = 15;
pub const GHOSTTY_SNAPSHOT_DECODER_DATA_KITTY_IMAGES_ALTERNATE: GhosttyEnum = 16;
pub const GHOSTTY_SNAPSHOT_DECODER_DATA_KITTY_PENDING_BYTES_PRIMARY: GhosttyEnum = 17;
pub const GHOSTTY_SNAPSHOT_DECODER_DATA_KITTY_PENDING_BYTES_ALTERNATE: GhosttyEnum = 18;

extern "C" {
    pub fn ghostty_sized_init(ptr: *mut c_void, size: usize);
    pub fn ghostty_type_json() -> *const c_char;
    pub fn ghostty_free(allocator: *const GhosttyAllocator, ptr: *mut u8, len: usize);
    pub fn ghostty_style_default(style: *mut GhosttyStyle);
    pub fn ghostty_style_is_default(style: *const GhosttyStyle) -> bool;
    pub fn ghostty_color_rgb_get(color: *const GhosttyColorRgb, r: *mut u8, g: *mut u8, b: *mut u8);
    pub fn ghostty_color_parse_x11(
        name: *const c_char,
        len: usize,
        out: *mut GhosttyColorRgb,
    ) -> GhosttyResult;
    pub fn ghostty_color_parse(
        value: *const c_char,
        len: usize,
        out: *mut GhosttyColorRgb,
    ) -> GhosttyResult;
    pub fn ghostty_color_parse_palette_entry(
        value: *const c_char,
        len: usize,
        out_index: *mut u8,
        out_rgb: *mut GhosttyColorRgb,
    ) -> GhosttyResult;
    pub fn ghostty_color_palette_default(out: *mut GhosttyColorRgb);
    pub fn ghostty_color_palette_generate(
        base: *const GhosttyColorRgb,
        skip: *const GhosttyColorPaletteMask,
        bg: *const GhosttyColorRgb,
        fg: *const GhosttyColorRgb,
        harmonious: bool,
        out: *mut GhosttyColorRgb,
    );
    pub fn ghostty_color_luminance(color: *const GhosttyColorRgb) -> f64;
    pub fn ghostty_color_perceived_luminance(color: *const GhosttyColorRgb) -> f64;
    pub fn ghostty_color_contrast(a: *const GhosttyColorRgb, b: *const GhosttyColorRgb) -> f64;
    pub fn ghostty_color_x11_names() -> *const GhosttyColorX11Entry;
    pub fn ghostty_color_x11_name_count() -> usize;

    pub fn ghostty_terminal_new(
        allocator: *const GhosttyAllocator,
        terminal: *mut GhosttyTerminal,
        cols: u16,
        rows: u16,
    ) -> GhosttyResult;
    pub fn ghostty_terminal_free(terminal: GhosttyTerminal);
    pub fn ghostty_terminal_reset(terminal: GhosttyTerminal);
    pub fn ghostty_terminal_resize(
        terminal: GhosttyTerminal,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) -> GhosttyResult;
    pub fn ghostty_terminal_set(
        terminal: GhosttyTerminal,
        option: GhosttyEnum,
        value: *const c_void,
    ) -> GhosttyResult;
    pub fn ghostty_terminal_vt_write(terminal: GhosttyTerminal, data: *const u8, len: usize);
    pub fn ghostty_terminal_vt_write_until(
        terminal: GhosttyTerminal,
        data: *const u8,
        len: usize,
        boundaries: u32,
        out: *mut GhosttyTerminalWriteStop,
    ) -> GhosttyResult;
    pub fn ghostty_terminal_continuation_write(
        terminal: GhosttyTerminal,
        writer: GhosttyWriter,
    ) -> GhosttyResult;
    pub fn ghostty_terminal_continuation_buf(
        terminal: GhosttyTerminal,
        buf: *mut u8,
        buf_len: usize,
        out_written: *mut usize,
    ) -> GhosttyResult;
    pub fn ghostty_terminal_continuation_alloc(
        terminal: GhosttyTerminal,
        allocator: *const GhosttyAllocator,
        out_ptr: *mut *mut u8,
        out_len: *mut usize,
    ) -> GhosttyResult;
    pub fn ghostty_terminal_get(
        terminal: GhosttyTerminal,
        data: GhosttyEnum,
        out: *mut c_void,
    ) -> GhosttyResult;
    pub fn ghostty_terminal_get_multi(
        terminal: GhosttyTerminal,
        count: usize,
        keys: *const GhosttyEnum,
        values: *mut *mut c_void,
        out_written: *mut usize,
    ) -> GhosttyResult;
    pub fn ghostty_terminal_grid_ref(
        terminal: GhosttyTerminal,
        point: GhosttyPoint,
        out_ref: *mut GhosttyGridRef,
    ) -> GhosttyResult;
    pub fn ghostty_terminal_grid_ref_track(
        terminal: GhosttyTerminal,
        point: GhosttyPoint,
        out_ref: *mut GhosttyTrackedGridRef,
    ) -> GhosttyResult;
    pub fn ghostty_terminal_point_from_grid_ref(
        terminal: GhosttyTerminal,
        reference: *const GhosttyGridRef,
        tag: GhosttyEnum,
        out: *mut GhosttyPointCoordinate,
    ) -> GhosttyResult;
    pub fn ghostty_grid_ref_cell(
        reference: *const GhosttyGridRef,
        out_cell: *mut GhosttyCell,
    ) -> GhosttyResult;
    pub fn ghostty_grid_ref_row(
        reference: *const GhosttyGridRef,
        out_row: *mut GhosttyRow,
    ) -> GhosttyResult;
    pub fn ghostty_grid_ref_graphemes(
        reference: *const GhosttyGridRef,
        buf: *mut u32,
        buf_len: usize,
        out_written: *mut usize,
    ) -> GhosttyResult;
    pub fn ghostty_grid_ref_hyperlink_uri(
        reference: *const GhosttyGridRef,
        buf: *mut u8,
        buf_len: usize,
        out_written: *mut usize,
    ) -> GhosttyResult;
    pub fn ghostty_grid_ref_style(
        reference: *const GhosttyGridRef,
        out_style: *mut GhosttyStyle,
    ) -> GhosttyResult;

    pub fn ghostty_cell_get(
        cell: GhosttyCell,
        data: GhosttyEnum,
        out: *mut c_void,
    ) -> GhosttyResult;
    pub fn ghostty_cell_get_multi(
        cell: GhosttyCell,
        count: usize,
        keys: *const GhosttyEnum,
        values: *mut *mut c_void,
        out_written: *mut usize,
    ) -> GhosttyResult;
    pub fn ghostty_row_get(row: GhosttyRow, data: GhosttyEnum, out: *mut c_void) -> GhosttyResult;
    pub fn ghostty_row_get_multi(
        row: GhosttyRow,
        count: usize,
        keys: *const GhosttyEnum,
        values: *mut *mut c_void,
        out_written: *mut usize,
    ) -> GhosttyResult;

    pub fn ghostty_render_state_new(
        allocator: *const GhosttyAllocator,
        state: *mut GhosttyRenderState,
    ) -> GhosttyResult;
    pub fn ghostty_render_state_free(state: GhosttyRenderState);
    pub fn ghostty_render_state_update(
        state: GhosttyRenderState,
        terminal: GhosttyTerminal,
    ) -> GhosttyResult;
    pub fn ghostty_render_state_begin_update(
        state: GhosttyRenderState,
        terminal: GhosttyTerminal,
    ) -> GhosttyResult;
    pub fn ghostty_render_state_end_update(state: GhosttyRenderState) -> GhosttyResult;
    pub fn ghostty_render_state_clean(state: GhosttyRenderState) -> GhosttyResult;
    pub fn ghostty_render_state_get(
        state: GhosttyRenderState,
        data: GhosttyEnum,
        out: *mut c_void,
    ) -> GhosttyResult;
    pub fn ghostty_render_state_get_multi(
        state: GhosttyRenderState,
        count: usize,
        keys: *const GhosttyEnum,
        values: *mut *mut c_void,
        out_written: *mut usize,
    ) -> GhosttyResult;
    pub fn ghostty_render_state_set(
        state: GhosttyRenderState,
        option: GhosttyEnum,
        value: *const c_void,
    ) -> GhosttyResult;

    pub fn ghostty_render_state_row_iterator_new(
        allocator: *const GhosttyAllocator,
        out_iterator: *mut GhosttyRenderStateRowIterator,
    ) -> GhosttyResult;
    pub fn ghostty_render_state_row_iterator_free(iterator: GhosttyRenderStateRowIterator);
    pub fn ghostty_render_state_row_iterator_next(iterator: GhosttyRenderStateRowIterator) -> bool;
    pub fn ghostty_render_state_row_iterator_next_dirty(
        iterator: GhosttyRenderStateRowIterator,
        out_y: *mut u16,
    ) -> bool;
    pub fn ghostty_render_state_row_get(
        iterator: GhosttyRenderStateRowIterator,
        data: GhosttyEnum,
        out: *mut c_void,
    ) -> GhosttyResult;
    pub fn ghostty_render_state_row_get_multi(
        iterator: GhosttyRenderStateRowIterator,
        count: usize,
        keys: *const GhosttyEnum,
        values: *mut *mut c_void,
        out_written: *mut usize,
    ) -> GhosttyResult;
    pub fn ghostty_render_state_row_set(
        iterator: GhosttyRenderStateRowIterator,
        option: GhosttyEnum,
        value: *const c_void,
    ) -> GhosttyResult;

    pub fn ghostty_render_state_row_cells_new(
        allocator: *const GhosttyAllocator,
        out_cells: *mut GhosttyRenderStateRowCells,
    ) -> GhosttyResult;
    pub fn ghostty_render_state_row_cells_next(cells: GhosttyRenderStateRowCells) -> bool;
    pub fn ghostty_render_state_row_cells_select(
        cells: GhosttyRenderStateRowCells,
        x: u16,
    ) -> GhosttyResult;
    pub fn ghostty_render_state_row_cells_get(
        cells: GhosttyRenderStateRowCells,
        data: GhosttyEnum,
        out: *mut c_void,
    ) -> GhosttyResult;
    pub fn ghostty_render_state_row_cells_get_multi(
        cells: GhosttyRenderStateRowCells,
        count: usize,
        keys: *const GhosttyEnum,
        values: *mut *mut c_void,
        out_written: *mut usize,
    ) -> GhosttyResult;
    pub fn ghostty_render_state_row_cells_free(cells: GhosttyRenderStateRowCells);

    pub fn ghostty_render_state_image_iterator_new(
        allocator: *const GhosttyAllocator,
        out_iterator: *mut GhosttyRenderStateImageIterator,
    ) -> GhosttyResult;
    pub fn ghostty_render_state_image_iterator_free(iterator: GhosttyRenderStateImageIterator);
    pub fn ghostty_render_state_image_iterator_next(
        iterator: GhosttyRenderStateImageIterator,
    ) -> bool;
    pub fn ghostty_render_state_image_get(
        iterator: GhosttyRenderStateImageIterator,
        data: GhosttyEnum,
        out: *mut c_void,
    ) -> GhosttyResult;
    pub fn ghostty_render_state_image_get_multi(
        iterator: GhosttyRenderStateImageIterator,
        count: usize,
        keys: *const GhosttyEnum,
        values: *mut *mut c_void,
        out_written: *mut usize,
    ) -> GhosttyResult;

    pub fn ghostty_formatter_terminal_new(
        allocator: *const GhosttyAllocator,
        formatter: *mut GhosttyFormatter,
        terminal: GhosttyTerminal,
    ) -> GhosttyResult;
    pub fn ghostty_formatter_set(
        formatter: GhosttyFormatter,
        option: GhosttyEnum,
        value: *const c_void,
    ) -> GhosttyResult;
    pub fn ghostty_formatter_get(
        formatter: GhosttyFormatter,
        option: GhosttyEnum,
        out: *mut c_void,
    ) -> GhosttyResult;
    pub fn ghostty_formatter_format(
        formatter: GhosttyFormatter,
        writer: GhosttyWriter,
    ) -> GhosttyResult;
    pub fn ghostty_formatter_format_buf(
        formatter: GhosttyFormatter,
        buf: *mut u8,
        buf_len: usize,
        out_written: *mut usize,
    ) -> GhosttyResult;
    pub fn ghostty_formatter_format_alloc(
        formatter: GhosttyFormatter,
        allocator: *const GhosttyAllocator,
        out_ptr: *mut *mut u8,
        out_len: *mut usize,
    ) -> GhosttyResult;
    pub fn ghostty_formatter_free(formatter: GhosttyFormatter);

    pub fn ghostty_key_event_new(
        allocator: *const GhosttyAllocator,
        event: *mut GhosttyKeyEvent,
    ) -> GhosttyResult;
    pub fn ghostty_key_event_free(event: GhosttyKeyEvent);
    pub fn ghostty_key_event_set_action(
        event: GhosttyKeyEvent,
        action: GhosttyEnum,
    ) -> GhosttyResult;
    pub fn ghostty_key_event_get_action(event: GhosttyKeyEvent) -> GhosttyEnum;
    pub fn ghostty_key_event_set_key(event: GhosttyKeyEvent, key: GhosttyKey) -> GhosttyResult;
    pub fn ghostty_key_event_get_key(event: GhosttyKeyEvent) -> GhosttyKey;
    pub fn ghostty_key_event_set_mods(event: GhosttyKeyEvent, mods: GhosttyMods) -> GhosttyResult;
    pub fn ghostty_key_event_get_mods(event: GhosttyKeyEvent) -> GhosttyMods;
    pub fn ghostty_key_event_set_consumed_mods(
        event: GhosttyKeyEvent,
        mods: GhosttyMods,
    ) -> GhosttyResult;
    pub fn ghostty_key_event_get_consumed_mods(event: GhosttyKeyEvent) -> GhosttyMods;
    pub fn ghostty_key_event_set_composing(
        event: GhosttyKeyEvent,
        composing: bool,
    ) -> GhosttyResult;
    pub fn ghostty_key_event_get_composing(event: GhosttyKeyEvent) -> bool;
    pub fn ghostty_key_event_set_utf8(event: GhosttyKeyEvent, utf8: GhosttyString)
        -> GhosttyResult;
    pub fn ghostty_key_event_get_utf8(event: GhosttyKeyEvent) -> GhosttyString;
    pub fn ghostty_key_event_set_unshifted_codepoint(
        event: GhosttyKeyEvent,
        codepoint: u32,
    ) -> GhosttyResult;
    pub fn ghostty_key_event_get_unshifted_codepoint(event: GhosttyKeyEvent) -> u32;
    pub fn ghostty_key_encoder_new(
        allocator: *const GhosttyAllocator,
        encoder: *mut GhosttyKeyEncoder,
    ) -> GhosttyResult;
    pub fn ghostty_key_encoder_free(encoder: GhosttyKeyEncoder);
    pub fn ghostty_key_encoder_set(
        encoder: GhosttyKeyEncoder,
        option: GhosttyEnum,
        value: *const c_void,
    ) -> GhosttyResult;
    pub fn ghostty_key_encoder_set_from_terminal(
        encoder: GhosttyKeyEncoder,
        terminal: GhosttyTerminal,
    ) -> GhosttyResult;
    pub fn ghostty_key_encoder_encode(
        encoder: GhosttyKeyEncoder,
        event: GhosttyKeyEvent,
        buf: *mut u8,
        buf_len: usize,
        out_written: *mut usize,
    ) -> GhosttyResult;

    pub fn ghostty_mouse_event_new(
        allocator: *const GhosttyAllocator,
        event: *mut GhosttyMouseEvent,
    ) -> GhosttyResult;
    pub fn ghostty_mouse_event_free(event: GhosttyMouseEvent);
    pub fn ghostty_mouse_event_set_action(
        event: GhosttyMouseEvent,
        action: GhosttyEnum,
    ) -> GhosttyResult;
    pub fn ghostty_mouse_event_get_action(event: GhosttyMouseEvent) -> GhosttyEnum;
    pub fn ghostty_mouse_event_set_button(
        event: GhosttyMouseEvent,
        button: GhosttyEnum,
    ) -> GhosttyResult;
    pub fn ghostty_mouse_event_clear_button(event: GhosttyMouseEvent) -> GhosttyResult;
    pub fn ghostty_mouse_event_get_button(
        event: GhosttyMouseEvent,
        out_button: *mut GhosttyEnum,
    ) -> GhosttyResult;
    pub fn ghostty_mouse_event_set_mods(
        event: GhosttyMouseEvent,
        mods: GhosttyMods,
    ) -> GhosttyResult;
    pub fn ghostty_mouse_event_get_mods(event: GhosttyMouseEvent) -> GhosttyMods;
    pub fn ghostty_mouse_event_set_position(
        event: GhosttyMouseEvent,
        position: GhosttyMousePosition,
    ) -> GhosttyResult;
    pub fn ghostty_mouse_event_get_position(event: GhosttyMouseEvent) -> GhosttyMousePosition;
    pub fn ghostty_mouse_encoder_new(
        allocator: *const GhosttyAllocator,
        encoder: *mut GhosttyMouseEncoder,
    ) -> GhosttyResult;
    pub fn ghostty_mouse_encoder_free(encoder: GhosttyMouseEncoder);
    pub fn ghostty_mouse_encoder_set(
        encoder: GhosttyMouseEncoder,
        option: GhosttyEnum,
        value: *const c_void,
    ) -> GhosttyResult;
    pub fn ghostty_mouse_encoder_set_from_terminal(
        encoder: GhosttyMouseEncoder,
        terminal: GhosttyTerminal,
    ) -> GhosttyResult;
    pub fn ghostty_mouse_encoder_reset(encoder: GhosttyMouseEncoder) -> GhosttyResult;
    pub fn ghostty_mouse_encoder_encode(
        encoder: GhosttyMouseEncoder,
        event: GhosttyMouseEvent,
        buf: *mut u8,
        buf_len: usize,
        out_written: *mut usize,
    ) -> GhosttyResult;

    pub fn ghostty_snapshot_encode(
        terminal: GhosttyTerminal,
        writer: GhosttyWriter,
    ) -> GhosttyResult;
    pub fn ghostty_snapshot_encode_buf(
        terminal: GhosttyTerminal,
        buf: *mut u8,
        buf_len: usize,
        out_written: *mut usize,
    ) -> GhosttyResult;
    pub fn ghostty_snapshot_encode_alloc(
        terminal: GhosttyTerminal,
        allocator: *const GhosttyAllocator,
        out_ptr: *mut *mut u8,
        out_len: *mut usize,
    ) -> GhosttyResult;
    pub fn ghostty_snapshot_decoder_new(
        allocator: *const GhosttyAllocator,
        decoder: *mut GhosttySnapshotDecoder,
        reader: GhosttyReader,
    ) -> GhosttyResult;
    pub fn ghostty_snapshot_decoder_new_buf(
        allocator: *const GhosttyAllocator,
        decoder: *mut GhosttySnapshotDecoder,
        ptr: *const u8,
        len: usize,
    ) -> GhosttyResult;
    pub fn ghostty_snapshot_decoder_free(decoder: GhosttySnapshotDecoder);
    pub fn ghostty_snapshot_decoder_set(
        decoder: GhosttySnapshotDecoder,
        option: GhosttyEnum,
        value: *const c_void,
    ) -> GhosttyResult;
    pub fn ghostty_snapshot_decoder_ready(
        decoder: GhosttySnapshotDecoder,
        terminal: *mut GhosttyTerminal,
    ) -> GhosttyResult;
    pub fn ghostty_snapshot_decoder_next(decoder: GhosttySnapshotDecoder) -> GhosttyResult;
    pub fn ghostty_snapshot_decoder_decode(
        decoder: GhosttySnapshotDecoder,
        terminal: *mut GhosttyTerminal,
    ) -> GhosttyResult;
    pub fn ghostty_snapshot_decoder_get(
        decoder: GhosttySnapshotDecoder,
        data: GhosttyEnum,
        out: *mut c_void,
    ) -> GhosttyResult;
    pub fn ghostty_snapshot_decoder_get_multi(
        decoder: GhosttySnapshotDecoder,
        count: usize,
        keys: *const GhosttyEnum,
        values: *mut *mut c_void,
        out_written: *mut usize,
    ) -> GhosttyResult;
}
