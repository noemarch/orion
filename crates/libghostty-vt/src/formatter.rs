use std::ffi::c_void;
use std::marker::PhantomData;

use crate::ffi as raw;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Plain,
    Vt,
}

#[derive(Debug, Clone, Copy)]
pub struct FormatOptions {
    pub emit: Format,
    pub unwrap: bool,
    pub trim: bool,
    pub selection: Option<(u16, u16, u16, u16)>,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            emit: Format::Plain,
            unwrap: false,
            trim: true,
            selection: None,
        }
    }
}

fn active_point(x: u16, y: u32) -> raw::GhosttyPoint {
    raw::GhosttyPoint {
        tag: raw::GHOSTTY_POINT_TAG_ACTIVE,
        value: raw::GhosttyPointValue {
            coordinate: raw::GhosttyPointCoordinate { x, y },
        },
    }
}

pub struct Formatter<'a> {
    raw: raw::GhosttyFormatter,
    terminal: raw::GhosttyTerminal,
    _marker: PhantomData<&'a crate::terminal::Terminal>,
}

impl<'a> Formatter<'a> {
    pub(crate) fn new(terminal: &crate::terminal::Terminal) -> Result<Self, crate::Status> {
        let mut handle: raw::GhosttyFormatter = std::ptr::null_mut();
        let result = unsafe {
            raw::ghostty_formatter_terminal_new(std::ptr::null(), &mut handle, terminal.raw())
        };
        if result != raw::GHOSTTY_SUCCESS {
            return Err(crate::Status(result));
        }
        Ok(Self {
            raw: handle,
            terminal: terminal.raw(),
            _marker: PhantomData,
        })
    }

    pub fn set(&mut self, opts: &FormatOptions) -> Result<(), crate::Status> {
        let emit: raw::GhosttyEnum = match opts.emit {
            Format::Plain => raw::GHOSTTY_FORMATTER_FORMAT_PLAIN,
            Format::Vt => raw::GHOSTTY_FORMATTER_FORMAT_VT,
        };
        Self::set_option(
            self.raw,
            raw::GHOSTTY_FORMATTER_OPT_EMIT,
            &emit as *const raw::GhosttyEnum as *const c_void,
        )?;
        Self::set_option(
            self.raw,
            raw::GHOSTTY_FORMATTER_OPT_UNWRAP,
            &opts.unwrap as *const bool as *const c_void,
        )?;
        Self::set_option(
            self.raw,
            raw::GHOSTTY_FORMATTER_OPT_TRIM,
            &opts.trim as *const bool as *const c_void,
        )?;
        match opts.selection {
            Some((start_x, start_y, end_x, end_y)) => {
                let mut start = raw::GhosttyGridRef::default();
                let mut end = raw::GhosttyGridRef::default();
                let result = unsafe {
                    raw::ghostty_terminal_grid_ref(
                        self.terminal,
                        active_point(start_x, start_y as u32),
                        &mut start,
                    )
                };
                if result != raw::GHOSTTY_SUCCESS {
                    return Err(crate::Status(result));
                }
                let result = unsafe {
                    raw::ghostty_terminal_grid_ref(
                        self.terminal,
                        active_point(end_x, end_y as u32),
                        &mut end,
                    )
                };
                if result != raw::GHOSTTY_SUCCESS {
                    return Err(crate::Status(result));
                }
                let selection = raw::GhosttySelection {
                    start,
                    end,
                    rectangle: false,
                };
                Self::set_option(
                    self.raw,
                    raw::GHOSTTY_FORMATTER_OPT_SELECTION,
                    &selection as *const raw::GhosttySelection as *const c_void,
                )?;
            }
            None => {
                Self::set_option(
                    self.raw,
                    raw::GHOSTTY_FORMATTER_OPT_SELECTION,
                    std::ptr::null(),
                )?;
            }
        }
        Ok(())
    }

    fn set_option(
        formatter: raw::GhosttyFormatter,
        option: raw::GhosttyEnum,
        value: *const c_void,
    ) -> Result<(), crate::Status> {
        let result = unsafe { raw::ghostty_formatter_set(formatter, option, value) };
        if result == raw::GHOSTTY_SUCCESS {
            Ok(())
        } else {
            Err(crate::Status(result))
        }
    }

    pub fn format_to_vec(&mut self, out: &mut Vec<u8>) -> Result<(), crate::Status> {
        let mut needed: usize = 0;
        let result = unsafe {
            raw::ghostty_formatter_format_buf(self.raw, std::ptr::null_mut(), 0, &mut needed)
        };
        if result == raw::GHOSTTY_SUCCESS {
            return Ok(());
        }
        if result != raw::GHOSTTY_OUT_OF_SPACE {
            return Err(crate::Status(result));
        }
        let start = out.len();
        out.resize(start + needed, 0);
        let mut written: usize = 0;
        let result = unsafe {
            raw::ghostty_formatter_format_buf(
                self.raw,
                out.as_mut_ptr().add(start),
                needed,
                &mut written,
            )
        };
        if result != raw::GHOSTTY_SUCCESS {
            out.truncate(start);
            return Err(crate::Status(result));
        }
        out.truncate(start + written);
        Ok(())
    }

    pub fn format_string(&mut self) -> Result<String, crate::Status> {
        let mut bytes = Vec::new();
        self.format_to_vec(&mut bytes)?;
        String::from_utf8(bytes).map_err(|_| crate::Status(raw::GHOSTTY_INVALID_VALUE))
    }
}

impl Drop for Formatter<'_> {
    fn drop(&mut self) {
        unsafe { raw::ghostty_formatter_free(self.raw) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{CallbackMask, Terminal, TerminalOptions};

    fn terminal() -> Terminal {
        Terminal::new(&TerminalOptions {
            cols: 30,
            rows: 10,
            scrollback_lines: 100,
            scrollback_bytes: 4096,
            continuation_max_bytes: 1024,
            grapheme_clustering: true,
            callbacks: CallbackMask::all(),
        })
        .expect("terminal creation failed")
    }

    #[test]
    fn plain_and_vt_formats_differ() {
        let mut terminal = terminal();
        let _ = terminal.write(b"colored text");

        let mut formatter = terminal.formatter();
        formatter
            .set(&FormatOptions {
                emit: Format::Plain,
                unwrap: false,
                trim: true,
                selection: None,
            })
            .expect("plain option failed");
        let plain = formatter.format_string().expect("plain format failed");
        assert!(plain.contains("colored text"), "plain output: {plain:?}");
        assert!(
            !plain.contains('\x1b'),
            "plain output had escapes: {plain:?}"
        );

        formatter
            .set(&FormatOptions {
                emit: Format::Vt,
                unwrap: false,
                trim: true,
                selection: None,
            })
            .expect("vt option failed");
        let mut vt = Vec::new();
        formatter.format_to_vec(&mut vt).expect("vt format failed");
        assert!(!vt.is_empty(), "vt output was empty");
    }
}
