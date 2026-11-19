use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::c_void;
use std::marker::PhantomData;

use crate::ffi as raw;

const MODE_GRAPHEME_CLUSTER: raw::GhosttyEnum = 2027;
const UNKNOWN_MAX_BYTES: usize = 4096;
const UNKNOWN_SEQUENCE_APC: raw::GhosttyEnum = 0;
const UNKNOWN_SEQUENCE_OSC: raw::GhosttyEnum = 1;

#[repr(C)]
struct ModeConfig {
    mode: u16,
    value: bool,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct UnknownStringSequence {
    truncated: bool,
    content: raw::GhosttyString,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct UnknownOscSequence {
    truncated: bool,
    content: raw::GhosttyString,
    terminator: raw::GhosttyEnum,
}

#[repr(C)]
union UnknownSequenceValue {
    apc: std::mem::ManuallyDrop<UnknownStringSequence>,
    osc: std::mem::ManuallyDrop<UnknownOscSequence>,
    padding: [u64; 16],
}

#[repr(C)]
struct UnknownSequence {
    tag: raw::GhosttyEnum,
    value: UnknownSequenceValue,
}

#[repr(C)]
struct ProgramStatusReport {
    state: raw::GhosttyEnum,
    kind: raw::GhosttyEnum,
    progress: i8,
    id: raw::GhosttyString,
    app: raw::GhosttyString,
    title: raw::GhosttyString,
    message: raw::GhosttyString,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallbackMask {
    pub write_pty: bool,
    pub title: bool,
    pub program_status: bool,
    pub bell: bool,
    pub size: bool,
    pub unknown_sequence: bool,
}

impl CallbackMask {
    pub fn all() -> Self {
        Self {
            write_pty: true,
            title: true,
            program_status: true,
            bell: true,
            size: true,
            unknown_sequence: true,
        }
    }
}

impl Default for CallbackMask {
    fn default() -> Self {
        Self::all()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TerminalOptions {
    pub cols: u16,
    pub rows: u16,
    pub scrollback_lines: usize,
    pub scrollback_bytes: usize,
    pub continuation_max_bytes: usize,
    pub grapheme_clustering: bool,
    pub callbacks: CallbackMask,
}

impl Default for TerminalOptions {
    fn default() -> Self {
        Self {
            cols: 80,
            rows: 24,
            scrollback_lines: 10_000,
            scrollback_bytes: 10_000_000,
            continuation_max_bytes: 4096,
            grapheme_clustering: true,
            callbacks: CallbackMask::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeReport {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgramStatus {
    pub state: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    WritePty(Vec<u8>),
    TitleChanged,
    Bell,
    Size(SizeReport),
    ProgramStatus(ProgramStatus),
    UnknownSequence(Vec<u8>),
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct SizeReportSize {
    pub(crate) rows: u16,
    pub(crate) columns: u16,
    pub(crate) cell_width: u32,
    pub(crate) cell_height: u32,
}

struct CallbackState {
    events: RefCell<VecDeque<Event>>,
    geometry: Cell<SizeReportSize>,
}

fn ok(result: raw::GhosttyResult) -> Result<(), crate::Status> {
    if result == raw::GHOSTTY_SUCCESS {
        Ok(())
    } else {
        Err(crate::Status(result))
    }
}

fn string_bytes(value: raw::GhosttyString) -> Vec<u8> {
    if value.ptr.is_null() || value.len == 0 {
        return Vec::new();
    }
    unsafe { std::slice::from_raw_parts(value.ptr, value.len) }.to_vec()
}

extern "C" fn on_write_pty(
    _terminal: raw::GhosttyTerminal,
    userdata: *mut c_void,
    data: *const u8,
    len: usize,
) {
    if userdata.is_null() || data.is_null() {
        return;
    }
    let state = unsafe { &*(userdata as *const CallbackState) };
    let bytes = unsafe { std::slice::from_raw_parts(data, len) }.to_vec();
    if let Ok(mut events) = state.events.try_borrow_mut() {
        events.push_back(Event::WritePty(bytes));
    }
}

extern "C" fn on_bell(_terminal: raw::GhosttyTerminal, userdata: *mut c_void) {
    if userdata.is_null() {
        return;
    }
    let state = unsafe { &*(userdata as *const CallbackState) };
    if let Ok(mut events) = state.events.try_borrow_mut() {
        events.push_back(Event::Bell);
    }
}

extern "C" fn on_title_changed(_terminal: raw::GhosttyTerminal, userdata: *mut c_void) {
    if userdata.is_null() {
        return;
    }
    let state = unsafe { &*(userdata as *const CallbackState) };
    if let Ok(mut events) = state.events.try_borrow_mut() {
        events.push_back(Event::TitleChanged);
    }
}

extern "C" fn on_size(
    _terminal: raw::GhosttyTerminal,
    userdata: *mut c_void,
    out_size: *mut SizeReportSize,
) -> bool {
    if userdata.is_null() || out_size.is_null() {
        return false;
    }
    let state = unsafe { &*(userdata as *const CallbackState) };
    let geometry = state.geometry.get();
    unsafe {
        *out_size = geometry;
    }
    if let Ok(mut events) = state.events.try_borrow_mut() {
        events.push_back(Event::Size(SizeReport {
            cols: geometry.columns,
            rows: geometry.rows,
        }));
    }
    true
}

extern "C" fn on_unknown_sequence(
    _terminal: raw::GhosttyTerminal,
    userdata: *mut c_void,
    sequence: *const UnknownSequence,
) {
    if userdata.is_null() || sequence.is_null() {
        return;
    }
    let state = unsafe { &*(userdata as *const CallbackState) };
    let seq = unsafe { &*sequence };
    let bytes = unsafe {
        match seq.tag {
            UNKNOWN_SEQUENCE_APC => string_bytes(seq.value.apc.content),
            UNKNOWN_SEQUENCE_OSC => string_bytes(seq.value.osc.content),
            _ => Vec::new(),
        }
    };
    if let Ok(mut events) = state.events.try_borrow_mut() {
        events.push_back(Event::UnknownSequence(bytes));
    }
}

extern "C" fn on_program_status(
    _terminal: raw::GhosttyTerminal,
    userdata: *mut c_void,
    report: *const ProgramStatusReport,
) {
    if userdata.is_null() || report.is_null() {
        return;
    }
    let state = unsafe { &*(userdata as *const CallbackState) };
    let status = unsafe { &*report };
    if let Ok(mut events) = state.events.try_borrow_mut() {
        events.push_back(Event::ProgramStatus(ProgramStatus {
            state: status.state as u8,
        }));
    }
}

pub struct Terminal {
    raw: raw::GhosttyTerminal,
    keepalive: Box<CallbackState>,
    _not_sync: PhantomData<*const ()>,
}

unsafe impl Send for Terminal {}

impl Terminal {
    pub fn new(opts: &TerminalOptions) -> Result<Self, crate::Status> {
        let mut handle: raw::GhosttyTerminal = std::ptr::null_mut();
        ok(unsafe {
            raw::ghostty_terminal_new(std::ptr::null(), &mut handle, opts.cols, opts.rows)
        })?;
        let keepalive = Box::new(CallbackState {
            events: RefCell::new(VecDeque::new()),
            geometry: Cell::new(SizeReportSize {
                rows: opts.rows,
                columns: opts.cols,
                cell_width: 0,
                cell_height: 0,
            }),
        });
        let mut terminal = Self {
            raw: handle,
            keepalive,
            _not_sync: PhantomData,
        };
        terminal.apply_options(opts)?;
        terminal.install_callbacks(&opts.callbacks)?;
        Ok(terminal)
    }

    pub(crate) fn adopt(
        handle: raw::GhosttyTerminal,
        mask: &CallbackMask,
        geometry: SizeReportSize,
    ) -> Result<Self, crate::Status> {
        if handle.is_null() {
            return Err(crate::Status(raw::GHOSTTY_INVALID_VALUE));
        }
        let keepalive = Box::new(CallbackState {
            events: RefCell::new(VecDeque::new()),
            geometry: Cell::new(geometry),
        });
        let mut terminal = Self {
            raw: handle,
            keepalive,
            _not_sync: PhantomData,
        };
        terminal.install_callbacks(mask)?;
        Ok(terminal)
    }

    pub(crate) fn raw(&self) -> raw::GhosttyTerminal {
        self.raw
    }

    fn apply_options(&mut self, opts: &TerminalOptions) -> Result<(), crate::Status> {
        ok(unsafe {
            raw::ghostty_terminal_set(
                self.raw,
                raw::GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_LINES,
                &opts.scrollback_lines as *const usize as *const c_void,
            )
        })?;
        ok(unsafe {
            raw::ghostty_terminal_set(
                self.raw,
                raw::GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_BYTES,
                &opts.scrollback_bytes as *const usize as *const c_void,
            )
        })?;
        ok(unsafe {
            raw::ghostty_terminal_set(
                self.raw,
                raw::GHOSTTY_TERMINAL_OPT_CONTINUATION_MAX_BYTES,
                &opts.continuation_max_bytes as *const usize as *const c_void,
            )
        })?;
        let mode = ModeConfig {
            mode: MODE_GRAPHEME_CLUSTER as u16,
            value: opts.grapheme_clustering,
        };
        ok(unsafe {
            raw::ghostty_terminal_set(
                self.raw,
                raw::GHOSTTY_TERMINAL_OPT_MODE,
                &mode as *const ModeConfig as *const c_void,
            )
        })
    }

    fn install_callbacks(&mut self, mask: &CallbackMask) -> Result<(), crate::Status> {
        let userdata = &*self.keepalive as *const CallbackState as *const c_void;
        ok(unsafe {
            raw::ghostty_terminal_set(self.raw, raw::GHOSTTY_TERMINAL_OPT_USERDATA, userdata)
        })?;
        if mask.write_pty {
            ok(unsafe {
                raw::ghostty_terminal_set(
                    self.raw,
                    raw::GHOSTTY_TERMINAL_OPT_WRITE_PTY,
                    on_write_pty as *const () as *const c_void,
                )
            })?;
        }
        if mask.title {
            ok(unsafe {
                raw::ghostty_terminal_set(
                    self.raw,
                    raw::GHOSTTY_TERMINAL_OPT_TITLE_CHANGED,
                    on_title_changed as *const () as *const c_void,
                )
            })?;
        }
        if mask.program_status {
            ok(unsafe {
                raw::ghostty_terminal_set(
                    self.raw,
                    raw::GHOSTTY_TERMINAL_OPT_PROGRAM_STATUS,
                    on_program_status as *const () as *const c_void,
                )
            })?;
        }
        if mask.bell {
            ok(unsafe {
                raw::ghostty_terminal_set(
                    self.raw,
                    raw::GHOSTTY_TERMINAL_OPT_BELL,
                    on_bell as *const () as *const c_void,
                )
            })?;
        }
        if mask.size {
            ok(unsafe {
                raw::ghostty_terminal_set(
                    self.raw,
                    raw::GHOSTTY_TERMINAL_OPT_SIZE,
                    on_size as *const () as *const c_void,
                )
            })?;
        }
        if mask.unknown_sequence {
            ok(unsafe {
                raw::ghostty_terminal_set(
                    self.raw,
                    raw::GHOSTTY_TERMINAL_OPT_UNKNOWN_MAX_BYTES,
                    &UNKNOWN_MAX_BYTES as *const usize as *const c_void,
                )
            })?;
            ok(unsafe {
                raw::ghostty_terminal_set(
                    self.raw,
                    raw::GHOSTTY_TERMINAL_OPT_UNKNOWN_SEQUENCE,
                    on_unknown_sequence as *const () as *const c_void,
                )
            })?;
        }
        Ok(())
    }

    fn drain_events(&mut self) -> Vec<Event> {
        match self.keepalive.events.try_borrow_mut() {
            Ok(mut events) => events.drain(..).collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn write(&mut self, bytes: &[u8]) -> Vec<Event> {
        unsafe { raw::ghostty_terminal_vt_write(self.raw, bytes.as_ptr(), bytes.len()) };
        self.drain_events()
    }

    pub fn write_until(&mut self, bytes: &[u8], boundaries: u32) -> (usize, bool, Vec<Event>) {
        let mut stop: raw::GhosttyTerminalWriteStop = raw::init_sized();
        let result = unsafe {
            raw::ghostty_terminal_vt_write_until(
                self.raw,
                bytes.as_ptr(),
                bytes.len(),
                boundaries,
                &mut stop,
            )
        };
        let reached = result == raw::GHOSTTY_SUCCESS && stop.reached != 0;
        (stop.consumed, reached, self.drain_events())
    }

    pub fn resize(
        &mut self,
        cols: u16,
        rows: u16,
        cell_w_px: u32,
        cell_h_px: u32,
    ) -> Result<(), crate::Status> {
        ok(unsafe { raw::ghostty_terminal_resize(self.raw, cols, rows, cell_w_px, cell_h_px) })?;
        self.keepalive.geometry.set(SizeReportSize {
            rows,
            columns: cols,
            cell_width: cell_w_px,
            cell_height: cell_h_px,
        });
        Ok(())
    }

    pub fn reset(&mut self) {
        unsafe { raw::ghostty_terminal_reset(self.raw) };
    }

    fn get_u16(&self, key: raw::GhosttyEnum) -> Result<u16, crate::Status> {
        let mut value: u16 = 0;
        ok(unsafe {
            raw::ghostty_terminal_get(self.raw, key, &mut value as *mut u16 as *mut c_void)
        })?;
        Ok(value)
    }

    fn get_usize(&self, key: raw::GhosttyEnum) -> Result<usize, crate::Status> {
        let mut value: usize = 0;
        ok(unsafe {
            raw::ghostty_terminal_get(self.raw, key, &mut value as *mut usize as *mut c_void)
        })?;
        Ok(value)
    }

    fn get_bool(&self, key: raw::GhosttyEnum) -> Result<bool, crate::Status> {
        let mut value = false;
        ok(unsafe {
            raw::ghostty_terminal_get(self.raw, key, &mut value as *mut bool as *mut c_void)
        })?;
        Ok(value)
    }

    fn get_u32(&self, key: raw::GhosttyEnum) -> Result<u32, crate::Status> {
        let mut value: u32 = 0;
        ok(unsafe {
            raw::ghostty_terminal_get(self.raw, key, &mut value as *mut u32 as *mut c_void)
        })?;
        Ok(value)
    }

    fn get_string(&self, key: raw::GhosttyEnum) -> Result<String, crate::Status> {
        let mut value = raw::GhosttyString::default();
        ok(unsafe {
            raw::ghostty_terminal_get(
                self.raw,
                key,
                &mut value as *mut raw::GhosttyString as *mut c_void,
            )
        })?;
        Ok(String::from_utf8_lossy(&string_bytes(value)).into_owned())
    }

    pub fn get(&self, key: DataKey) -> Result<DataValue, crate::Status> {
        match key {
            DataKey::Cols => self
                .get_u16(raw::GHOSTTY_TERMINAL_DATA_COLS)
                .map(DataValue::U16),
            DataKey::Rows => self
                .get_u16(raw::GHOSTTY_TERMINAL_DATA_ROWS)
                .map(DataValue::U16),
            DataKey::CursorX => self
                .get_u16(raw::GHOSTTY_TERMINAL_DATA_CURSOR_X)
                .map(DataValue::U16),
            DataKey::CursorY => self
                .get_u16(raw::GHOSTTY_TERMINAL_DATA_CURSOR_Y)
                .map(DataValue::U16),
            DataKey::TotalRows => self
                .get_usize(raw::GHOSTTY_TERMINAL_DATA_TOTAL_ROWS)
                .map(DataValue::Usize),
            DataKey::ScrollbackRows => self
                .get_usize(raw::GHOSTTY_TERMINAL_DATA_SCROLLBACK_ROWS)
                .map(DataValue::Usize),
            DataKey::CursorVisible => self
                .get_bool(raw::GHOSTTY_TERMINAL_DATA_CURSOR_VISIBLE)
                .map(DataValue::Bool),
            DataKey::CursorPendingWrap => self
                .get_bool(raw::GHOSTTY_TERMINAL_DATA_CURSOR_PENDING_WRAP)
                .map(DataValue::Bool),
            DataKey::MouseTracking => self
                .get_bool(raw::GHOSTTY_TERMINAL_DATA_MOUSE_TRACKING)
                .map(DataValue::Bool),
            DataKey::ViewportActive => self
                .get_bool(raw::GHOSTTY_TERMINAL_DATA_VIEWPORT_ACTIVE)
                .map(DataValue::Bool),
            DataKey::VtGround => self
                .get_bool(raw::GHOSTTY_TERMINAL_DATA_VT_GROUND)
                .map(DataValue::Bool),
            DataKey::VtProcessingError => self
                .get_bool(raw::GHOSTTY_TERMINAL_DATA_VT_PROCESSING_ERROR)
                .map(DataValue::Bool),
            DataKey::WidthPx => self
                .get_u32(raw::GHOSTTY_TERMINAL_DATA_WIDTH_PX)
                .map(DataValue::U32),
            DataKey::HeightPx => self
                .get_u32(raw::GHOSTTY_TERMINAL_DATA_HEIGHT_PX)
                .map(DataValue::U32),
            DataKey::Title => self
                .get_string(raw::GHOSTTY_TERMINAL_DATA_TITLE)
                .map(DataValue::String),
            DataKey::Pwd => self
                .get_string(raw::GHOSTTY_TERMINAL_DATA_PWD)
                .map(DataValue::String),
            DataKey::Selection => {
                let mut selection = raw::GhosttySelection::default();
                ok(unsafe {
                    raw::ghostty_terminal_get(
                        self.raw,
                        raw::GHOSTTY_TERMINAL_DATA_SELECTION,
                        &mut selection as *mut raw::GhosttySelection as *mut c_void,
                    )
                })?;
                let mut start = raw::GhosttyPointCoordinate::default();
                let mut end = raw::GhosttyPointCoordinate::default();
                ok(unsafe {
                    raw::ghostty_terminal_point_from_grid_ref(
                        self.raw,
                        &selection.start,
                        raw::GHOSTTY_POINT_TAG_ACTIVE,
                        &mut start,
                    )
                })?;
                ok(unsafe {
                    raw::ghostty_terminal_point_from_grid_ref(
                        self.raw,
                        &selection.end,
                        raw::GHOSTTY_POINT_TAG_ACTIVE,
                        &mut end,
                    )
                })?;
                Ok(DataValue::Selection((
                    start.x,
                    start.y as u16,
                    end.x,
                    end.y as u16,
                )))
            }
        }
    }

    pub fn continuation_bytes(&self) -> Vec<u8> {
        let mut written: usize = 0;
        let result = unsafe {
            raw::ghostty_terminal_continuation_buf(self.raw, std::ptr::null_mut(), 0, &mut written)
        };
        if result == raw::GHOSTTY_SUCCESS || result == raw::GHOSTTY_INVALID_VALUE {
            return Vec::new();
        }
        assert_eq!(
            result,
            raw::GHOSTTY_OUT_OF_SPACE,
            "continuation size query failed"
        );
        let mut bytes = vec![0u8; written];
        let result = unsafe {
            raw::ghostty_terminal_continuation_buf(
                self.raw,
                bytes.as_mut_ptr(),
                bytes.len(),
                &mut written,
            )
        };
        assert_eq!(result, raw::GHOSTTY_SUCCESS, "continuation export failed");
        bytes.truncate(written);
        bytes
    }

    pub fn snapshot_bytes(&self) -> Result<Vec<u8>, crate::Status> {
        let mut written: usize = 0;
        let result = unsafe {
            raw::ghostty_snapshot_encode_buf(self.raw, std::ptr::null_mut(), 0, &mut written)
        };
        if result == raw::GHOSTTY_SUCCESS {
            return Ok(Vec::new());
        }
        if result != raw::GHOSTTY_OUT_OF_SPACE {
            return Err(crate::Status(result));
        }
        let mut bytes = vec![0u8; written];
        let result = unsafe {
            raw::ghostty_snapshot_encode_buf(
                self.raw,
                bytes.as_mut_ptr(),
                bytes.len(),
                &mut written,
            )
        };
        ok(result)?;
        bytes.truncate(written);
        Ok(bytes)
    }

    pub fn formatter(&mut self) -> crate::formatter::Formatter<'_> {
        crate::formatter::Formatter::new(self).expect("formatter creation failed")
    }

    pub fn update_render(&mut self, rs: &mut crate::render::RenderState) -> crate::render::Dirty {
        const _: () = assert!(
            std::mem::size_of::<crate::render::Terminal>()
                == std::mem::size_of::<raw::GhosttyTerminal>()
        );
        let mut proxy: crate::render::Terminal = unsafe { std::mem::transmute_copy(&self.raw) };
        let dirty = rs.update(&mut proxy);
        std::mem::forget(proxy);
        dirty
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        unsafe { raw::ghostty_terminal_free(self.raw) };
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataKey {
    Cols,
    Rows,
    TotalRows,
    ScrollbackRows,
    CursorX,
    CursorY,
    CursorVisible,
    CursorPendingWrap,
    MouseTracking,
    ViewportActive,
    VtGround,
    VtProcessingError,
    WidthPx,
    HeightPx,
    Title,
    Pwd,
    Selection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataValue {
    U16(u16),
    U32(u32),
    Usize(usize),
    Bool(bool),
    String(String),
    Selection((u16, u16, u16, u16)),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> TerminalOptions {
        TerminalOptions {
            cols: 40,
            rows: 12,
            scrollback_lines: 100,
            scrollback_bytes: 4096,
            continuation_max_bytes: 1024,
            grapheme_clustering: true,
            callbacks: CallbackMask::all(),
        }
    }

    #[test]
    fn options_are_applied_before_first_write() {
        let mut terminal = Terminal::new(&options()).expect("terminal creation failed");
        assert_eq!(terminal.get(DataKey::Cols), Ok(DataValue::U16(40)));
        assert_eq!(terminal.get(DataKey::Rows), Ok(DataValue::U16(12)));
        assert!(terminal.get(DataKey::CursorVisible).is_ok());
        terminal.write(b"hello");
    }

    #[test]
    fn query_sequence_produces_write_pty_event() {
        let mut terminal = Terminal::new(&options()).expect("terminal creation failed");
        let events = terminal.write(b"\x1b[c");
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::WritePty(bytes) if !bytes.is_empty())),
            "expected a WritePty reply, got {events:?}"
        );
    }

    #[test]
    fn events_are_drained() {
        let mut terminal = Terminal::new(&options()).expect("terminal creation failed");
        let _ = terminal.write(b"\x07");
        let first = terminal.write(b"\x1b[c");
        assert!(first.iter().any(|e| matches!(e, Event::WritePty(_))));
        let second = terminal.write(b"plain text");
        assert!(
            !second.iter().any(|e| matches!(e, Event::WritePty(_))),
            "events were not drained: {second:?}"
        );
    }

    #[test]
    fn resize_and_write_until_work() {
        let mut terminal = Terminal::new(&options()).expect("terminal creation failed");
        terminal.resize(80, 24, 8, 16).expect("resize failed");
        assert_eq!(terminal.get(DataKey::Cols), Ok(DataValue::U16(80)));
        let (consumed, _, _) = terminal.write_until(b"abc\x1b]0;title\x07", 1 << 5);
        assert!(consumed > 0);
    }
}
