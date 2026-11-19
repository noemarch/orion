use crate::ffi as raw;
use crate::terminal::SizeReportSize;
use crate::terminal::Terminal;

pub fn snapshot_encode(terminal: &Terminal) -> Result<Vec<u8>, crate::Status> {
    terminal.snapshot_bytes()
}

pub fn restore_snapshot(bytes: &[u8]) -> Result<Terminal, crate::Status> {
    let mut decoder: raw::GhosttySnapshotDecoder = std::ptr::null_mut();
    let result = unsafe {
        raw::ghostty_snapshot_decoder_new_buf(
            std::ptr::null(),
            &mut decoder,
            bytes.as_ptr(),
            bytes.len(),
        )
    };
    if result != raw::GHOSTTY_SUCCESS {
        return Err(crate::Status(result));
    }

    let mut handle: raw::GhosttyTerminal = std::ptr::null_mut();
    let result = unsafe { raw::ghostty_snapshot_decoder_decode(decoder, &mut handle) };
    unsafe { raw::ghostty_snapshot_decoder_free(decoder) };
    if result != raw::GHOSTTY_SUCCESS {
        if !handle.is_null() {
            unsafe { raw::ghostty_terminal_free(handle) };
        }
        return Err(crate::Status(result));
    }

    let geometry = geometry_of(handle);
    Terminal::adopt(handle, &crate::terminal::CallbackMask::default(), geometry)
}

fn geometry_of(handle: raw::GhosttyTerminal) -> SizeReportSize {
    let mut rows: u16 = 0;
    let mut cols: u16 = 0;
    let mut width_px: u32 = 0;
    let mut height_px: u32 = 0;
    unsafe {
        raw::ghostty_terminal_get(
            handle,
            raw::GHOSTTY_TERMINAL_DATA_ROWS,
            &mut rows as *mut u16 as *mut _,
        );
        raw::ghostty_terminal_get(
            handle,
            raw::GHOSTTY_TERMINAL_DATA_COLS,
            &mut cols as *mut u16 as *mut _,
        );
        raw::ghostty_terminal_get(
            handle,
            raw::GHOSTTY_TERMINAL_DATA_WIDTH_PX,
            &mut width_px as *mut u32 as *mut _,
        );
        raw::ghostty_terminal_get(
            handle,
            raw::GHOSTTY_TERMINAL_DATA_HEIGHT_PX,
            &mut height_px as *mut u32 as *mut _,
        );
    }
    SizeReportSize {
        rows,
        columns: cols,
        cell_width: if cols == 0 { 0 } else { width_px / cols as u32 },
        cell_height: if rows == 0 {
            0
        } else {
            height_px / rows as u32
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formatter::{Format, FormatOptions};
    use crate::terminal::{DataKey, DataValue, TerminalOptions};

    #[test]
    fn snapshot_roundtrip_preserves_screen() {
        let opts = TerminalOptions {
            cols: 32,
            rows: 8,
            ..TerminalOptions::default()
        };
        let mut original = Terminal::new(&opts).expect("terminal creation failed");
        original.write(b"snapshot payload");
        let bytes = snapshot_encode(&original).expect("snapshot encode failed");
        assert!(!bytes.is_empty(), "snapshot was empty");

        let restored = restore_snapshot(&bytes).expect("snapshot decode failed");
        assert_eq!(restored.get(DataKey::Cols), Ok(DataValue::U16(32)));
        assert_eq!(restored.get(DataKey::Rows), Ok(DataValue::U16(8)));

        let mut restored = restored;
        let mut formatter = restored.formatter();
        formatter
            .set(&FormatOptions {
                emit: Format::Plain,
                unwrap: false,
                trim: true,
                selection: None,
            })
            .expect("formatter set failed");
        let text = formatter.format_string().expect("format failed");
        assert!(text.contains("snapshot payload"), "restored text: {text:?}");
    }

    #[test]
    fn reject_invalid_snapshot() {
        let result = restore_snapshot(b"not a snapshot");
        assert!(result.is_err(), "invalid snapshot was accepted");
    }
}
