//! Small C ABI surface not yet exposed by `libghostty-vt`'s safe wrapper.
//!
//! These declarations mirror libghostty-vt 1.0.1 exactly. Fold them into the
//! wrapper crate once its owning agent extends the safe API; until then this
//! module is the single place Orion reaches for them.

use std::ffi::c_void;

/// Opaque terminal handle. ABI-compatible with `GhosttyTerminal`.
pub type RawTerminal = *mut c_void;

/// Opaque row value. ABI-compatible with `GhosttyRow` (`uint64_t`).
#[allow(dead_code)]
pub type RawRow = u64;

pub const GHOSTTY_SUCCESS: i32 = 0;

pub const GHOSTTY_TERMINAL_SCROLL_VIEWPORT_BOTTOM: i32 = 1;
pub const GHOSTTY_TERMINAL_SCROLL_VIEWPORT_ROW: i32 = 3;
#[allow(dead_code)]
pub const GHOSTTY_ROW_DATA_WRAP: i32 = 1;

pub const OPT_SCROLLBACK_MAX_BYTES: i32 = 28;
pub const OPT_SCROLLBACK_MAX_LINES: i32 = 29;
pub const OPT_CONTINUATION_MAX_BYTES: i32 = 32;
pub const OPT_MODE: i32 = 35;
pub const MODE_GRAPHEME_CLUSTER: u16 = 2027;

// `ghostty_terminal_get` data keys (vt 1.0.1 terminal.h).
pub const GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN: i32 = 6;
pub const GHOSTTY_TERMINAL_DATA_MODE: i32 = 37;
// `GhosttyTerminalScreen` values.
pub const GHOSTTY_TERMINAL_SCREEN_PRIMARY: i32 = 0;
pub const GHOSTTY_TERMINAL_SCREEN_ALTERNATE: i32 = 1;

/// `GhosttyTerminalScrollViewportValue`: a 16-byte, 8-byte-aligned union.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ScrollViewportValue {
    pub raw: [u64; 2],
}

impl ScrollViewportValue {
    fn zero() -> Self {
        Self { raw: [0; 2] }
    }
}

/// `GhosttyTerminalScrollViewport`: tag (a C `enum`, i.e. `c_int`) + union.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ScrollViewport {
    pub tag: i32,
    pub value: ScrollViewportValue,
}

/// `GhosttyTerminalModeConfig`.
#[repr(C)]
pub struct ModeConfig {
    pub mode: u16,
    pub value: bool,
}

extern "C" {
    pub fn ghostty_terminal_set(terminal: RawTerminal, option: i32, value: *const c_void) -> i32;

    /// Read a typed property of the terminal. The output type depends on `data`:
    /// `ACTIVE_SCREEN` writes an `i32`, `MODE` reads/writes a `ModeConfig`.
    pub fn ghostty_terminal_get(terminal: RawTerminal, data: i32, out: *mut c_void) -> i32;

    /// Scroll the terminal viewport to an absolute scrollable-area row, or to
    /// the live bottom.
    pub fn ghostty_terminal_scroll_viewport(terminal: RawTerminal, behavior: ScrollViewport)
        -> i32;

    /// Read a typed property of a row. `GHOSTTY_ROW_DATA_WRAP` writes a C
    /// `bool` into `out`.
    #[allow(dead_code)]
    pub fn ghostty_row_get(row: RawRow, data: i32, out: *mut c_void) -> i32;
}

/// Configure the scrollback and continuation limits before the first write.
pub fn configure(
    terminal: RawTerminal,
    scrollback_lines: usize,
    scrollback_bytes: usize,
    continuation_max_bytes: usize,
    grapheme_clustering: bool,
) {
    let mut status = unsafe {
        ghostty_terminal_set(
            terminal,
            OPT_SCROLLBACK_MAX_LINES,
            &scrollback_lines as *const usize as *const c_void,
        )
    };
    assert_eq!(status, GHOSTTY_SUCCESS, "setting scrollback lines failed");
    status = unsafe {
        ghostty_terminal_set(
            terminal,
            OPT_SCROLLBACK_MAX_BYTES,
            &scrollback_bytes as *const usize as *const c_void,
        )
    };
    assert_eq!(status, GHOSTTY_SUCCESS, "setting scrollback bytes failed");
    status = unsafe {
        ghostty_terminal_set(
            terminal,
            OPT_CONTINUATION_MAX_BYTES,
            &continuation_max_bytes as *const usize as *const c_void,
        )
    };
    assert_eq!(status, GHOSTTY_SUCCESS, "setting continuation bytes failed");
    let mode = ModeConfig {
        mode: MODE_GRAPHEME_CLUSTER,
        value: grapheme_clustering,
    };
    status = unsafe {
        ghostty_terminal_set(
            terminal,
            OPT_MODE,
            &mode as *const ModeConfig as *const c_void,
        )
    };
    assert_eq!(
        status, GHOSTTY_SUCCESS,
        "setting grapheme clustering failed"
    );
}

/// Move the viewport so `row` is the first visible row of the scrollable area.
///
/// Returns the C status; zero means success.
pub fn scroll_to_row(terminal: RawTerminal, row: usize) -> i32 {
    let mut value = ScrollViewportValue::zero();
    value.raw[0] = row as u64;
    unsafe {
        ghostty_terminal_scroll_viewport(
            terminal,
            ScrollViewport {
                tag: GHOSTTY_TERMINAL_SCROLL_VIEWPORT_ROW,
                value,
            },
        )
    }
}

/// Return the viewport to the live (bottom) screen.
///
/// Returns the C status; zero means success.
pub fn scroll_bottom(terminal: RawTerminal) -> i32 {
    unsafe {
        ghostty_terminal_scroll_viewport(
            terminal,
            ScrollViewport {
                tag: GHOSTTY_TERMINAL_SCROLL_VIEWPORT_BOTTOM,
                value: ScrollViewportValue::zero(),
            },
        )
    }
}

/// Whether the emulator is showing the alternate screen right now.
pub fn active_screen_is_alternate(terminal: RawTerminal) -> bool {
    let mut screen = GHOSTTY_TERMINAL_SCREEN_PRIMARY;
    let status = unsafe {
        ghostty_terminal_get(
            terminal,
            GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN,
            &mut screen as *mut i32 as *mut c_void,
        )
    };
    status == GHOSTTY_SUCCESS && screen == GHOSTTY_TERMINAL_SCREEN_ALTERNATE
}

/// Whether a DEC private mode is set. `mode` is the raw mode number (1, 1000,
/// 1002, 1003, 1007, ...); DEC modes pack to that number with `ansi=false`.
pub fn mode_enabled(terminal: RawTerminal, mode: u16) -> bool {
    let mut config = ModeConfig { mode, value: false };
    let status = unsafe {
        ghostty_terminal_get(
            terminal,
            GHOSTTY_TERMINAL_DATA_MODE,
            &mut config as *mut ModeConfig as *mut c_void,
        )
    };
    status == GHOSTTY_SUCCESS && config.value
}

/// Whether the given row was soft-wrapped by the terminal.
#[allow(dead_code)]
pub fn row_wrapped(row: RawRow) -> bool {
    let mut wrapped = false;
    let status = unsafe {
        ghostty_row_get(
            row,
            GHOSTTY_ROW_DATA_WRAP,
            &mut wrapped as *mut bool as *mut c_void,
        )
    };
    assert_eq!(status, GHOSTTY_SUCCESS, "ghostty_row_get(WRAP) failed");
    wrapped
}
