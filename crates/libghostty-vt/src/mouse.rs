use std::ffi::c_void;
use std::marker::PhantomData;

use crate::ffi as raw;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}

impl MouseAction {
    fn raw(self) -> raw::GhosttyEnum {
        match self {
            MouseAction::Press => raw::GHOSTTY_MOUSE_ACTION_PRESS,
            MouseAction::Release => raw::GHOSTTY_MOUSE_ACTION_RELEASE,
            MouseAction::Motion => raw::GHOSTTY_MOUSE_ACTION_MOTION,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Unknown,
    Left,
    Right,
    Middle,
    Four,
    Five,
    Six,
    Seven,
    Eight,
    Nine,
    Ten,
    Eleven,
}

impl MouseButton {
    fn raw(self) -> raw::GhosttyEnum {
        match self {
            MouseButton::Unknown => raw::GHOSTTY_MOUSE_BUTTON_UNKNOWN,
            MouseButton::Left => raw::GHOSTTY_MOUSE_BUTTON_LEFT,
            MouseButton::Right => raw::GHOSTTY_MOUSE_BUTTON_RIGHT,
            MouseButton::Middle => raw::GHOSTTY_MOUSE_BUTTON_MIDDLE,
            MouseButton::Four => raw::GHOSTTY_MOUSE_BUTTON_FOUR,
            MouseButton::Five => raw::GHOSTTY_MOUSE_BUTTON_FIVE,
            MouseButton::Six => raw::GHOSTTY_MOUSE_BUTTON_SIX,
            MouseButton::Seven => raw::GHOSTTY_MOUSE_BUTTON_SEVEN,
            MouseButton::Eight => raw::GHOSTTY_MOUSE_BUTTON_EIGHT,
            MouseButton::Nine => raw::GHOSTTY_MOUSE_BUTTON_NINE,
            MouseButton::Ten => raw::GHOSTTY_MOUSE_BUTTON_TEN,
            MouseButton::Eleven => raw::GHOSTTY_MOUSE_BUTTON_ELEVEN,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MouseInput {
    pub action: MouseAction,
    pub button: Option<MouseButton>,
    pub mods: u16,
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseTrackingMode {
    None,
    X10,
    Normal,
    Button,
    Any,
}

impl MouseTrackingMode {
    fn raw(self) -> raw::GhosttyEnum {
        match self {
            MouseTrackingMode::None => raw::GHOSTTY_MOUSE_TRACKING_NONE,
            MouseTrackingMode::X10 => raw::GHOSTTY_MOUSE_TRACKING_X10,
            MouseTrackingMode::Normal => raw::GHOSTTY_MOUSE_TRACKING_NORMAL,
            MouseTrackingMode::Button => raw::GHOSTTY_MOUSE_TRACKING_BUTTON,
            MouseTrackingMode::Any => raw::GHOSTTY_MOUSE_TRACKING_ANY,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseFormat {
    X10,
    Utf8,
    Sgr,
    Urxvt,
    SgrPixels,
}

impl MouseFormat {
    fn raw(self) -> raw::GhosttyEnum {
        match self {
            MouseFormat::X10 => raw::GHOSTTY_MOUSE_FORMAT_X10,
            MouseFormat::Utf8 => raw::GHOSTTY_MOUSE_FORMAT_UTF8,
            MouseFormat::Sgr => raw::GHOSTTY_MOUSE_FORMAT_SGR,
            MouseFormat::Urxvt => raw::GHOSTTY_MOUSE_FORMAT_URXVT,
            MouseFormat::SgrPixels => raw::GHOSTTY_MOUSE_FORMAT_SGR_PIXELS,
        }
    }
}

/// SGR (1006) modifier bits, matching Ghostty's `mouse_encode.zig`.
pub mod sgr_mods {
    pub const SHIFT: u8 = 4;
    pub const ALT: u8 = 8;
    pub const CTRL: u8 = 16;
    pub const MOTION: u8 = 32;
}

/// Encode one SGR (1006) mouse report into `out`.
///
/// `button` is the SGR button number (0 left, 64 wheel up, 65 wheel down),
/// `col`/`row` are clamped to at least 1, and `mods` is a bitmask of
/// [`sgr_mods`]. A press writes an uppercase `M` final byte; a release writes
/// lowercase `m`.
pub fn encode_sgr_mouse(out: &mut Vec<u8>, button: u8, col: u32, row: u32, press: bool, mods: u8) {
    use std::io::Write as _;
    let final_byte = if press { 'M' } else { 'm' };
    let _ = write!(
        out,
        "\x1b[<{};{};{}{}",
        button | mods,
        col.max(1),
        row.max(1),
        final_byte
    );
}

#[derive(Debug, Clone, Copy)]
pub struct MouseGeometry {
    pub screen_width: u32,
    pub screen_height: u32,
    pub cell_width: u32,
    pub cell_height: u32,
    pub padding_top: u32,
    pub padding_bottom: u32,
    pub padding_right: u32,
    pub padding_left: u32,
}

pub struct MouseEncoder {
    raw: raw::GhosttyMouseEncoder,
    _not_sync: PhantomData<*const ()>,
}

unsafe impl Send for MouseEncoder {}

impl MouseEncoder {
    pub fn new() -> Result<Self, crate::Status> {
        let mut handle: raw::GhosttyMouseEncoder = std::ptr::null_mut();
        let result = unsafe { raw::ghostty_mouse_encoder_new(std::ptr::null(), &mut handle) };
        if result != raw::GHOSTTY_SUCCESS {
            return Err(crate::Status(result));
        }
        Ok(Self {
            raw: handle,
            _not_sync: PhantomData,
        })
    }

    pub fn set_from_terminal(
        &mut self,
        terminal: &crate::terminal::Terminal,
    ) -> Result<(), crate::Status> {
        let result =
            unsafe { raw::ghostty_mouse_encoder_set_from_terminal(self.raw, terminal.raw()) };
        if result == raw::GHOSTTY_SUCCESS {
            Ok(())
        } else {
            Err(crate::Status(result))
        }
    }

    pub fn set_tracking(&mut self, mode: MouseTrackingMode) -> Result<(), crate::Status> {
        let value = mode.raw();
        self.set_option(
            raw::GHOSTTY_MOUSE_ENCODER_OPT_EVENT,
            &value as *const raw::GhosttyEnum as *const c_void,
        )
    }

    pub fn set_format(&mut self, format: MouseFormat) -> Result<(), crate::Status> {
        let value = format.raw();
        self.set_option(
            raw::GHOSTTY_MOUSE_ENCODER_OPT_FORMAT,
            &value as *const raw::GhosttyEnum as *const c_void,
        )
    }

    pub fn set_geometry(&mut self, geometry: MouseGeometry) -> Result<(), crate::Status> {
        let mut value: raw::GhosttyMouseEncoderSize = raw::init_sized();
        value.screen_width = geometry.screen_width;
        value.screen_height = geometry.screen_height;
        value.cell_width = geometry.cell_width;
        value.cell_height = geometry.cell_height;
        value.padding_top = geometry.padding_top;
        value.padding_bottom = geometry.padding_bottom;
        value.padding_right = geometry.padding_right;
        value.padding_left = geometry.padding_left;
        self.set_option(
            raw::GHOSTTY_MOUSE_ENCODER_OPT_SIZE,
            &value as *const raw::GhosttyMouseEncoderSize as *const c_void,
        )
    }

    fn set_option(
        &mut self,
        option: raw::GhosttyEnum,
        value: *const c_void,
    ) -> Result<(), crate::Status> {
        let result = unsafe { raw::ghostty_mouse_encoder_set(self.raw, option, value) };
        if result == raw::GHOSTTY_SUCCESS {
            Ok(())
        } else {
            Err(crate::Status(result))
        }
    }

    pub fn encode(&mut self, input: &MouseInput, out: &mut Vec<u8>) -> Result<(), crate::Status> {
        let mut event: raw::GhosttyMouseEvent = std::ptr::null_mut();
        let result = unsafe { raw::ghostty_mouse_event_new(std::ptr::null(), &mut event) };
        if result != raw::GHOSTTY_SUCCESS {
            return Err(crate::Status(result));
        }
        let outcome = self.encode_event(event, input, out);
        unsafe { raw::ghostty_mouse_event_free(event) };
        outcome
    }

    fn encode_event(
        &mut self,
        event: raw::GhosttyMouseEvent,
        input: &MouseInput,
        out: &mut Vec<u8>,
    ) -> Result<(), crate::Status> {
        let mut result = unsafe { raw::ghostty_mouse_event_set_action(event, input.action.raw()) };
        if result != raw::GHOSTTY_SUCCESS {
            return Err(crate::Status(result));
        }
        result = match input.button {
            Some(button) => unsafe { raw::ghostty_mouse_event_set_button(event, button.raw()) },
            None => unsafe { raw::ghostty_mouse_event_clear_button(event) },
        };
        if result != raw::GHOSTTY_SUCCESS {
            return Err(crate::Status(result));
        }
        result = unsafe { raw::ghostty_mouse_event_set_mods(event, input.mods) };
        if result != raw::GHOSTTY_SUCCESS {
            return Err(crate::Status(result));
        }
        result = unsafe {
            raw::ghostty_mouse_event_set_position(
                event,
                raw::GhosttyMousePosition {
                    x: input.x,
                    y: input.y,
                },
            )
        };
        if result != raw::GHOSTTY_SUCCESS {
            return Err(crate::Status(result));
        }

        let mut needed: usize = 0;
        result = unsafe {
            raw::ghostty_mouse_encoder_encode(self.raw, event, std::ptr::null_mut(), 0, &mut needed)
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
        result = unsafe {
            raw::ghostty_mouse_encoder_encode(
                self.raw,
                event,
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
}

impl Drop for MouseEncoder {
    fn drop(&mut self) {
        unsafe { raw::ghostty_mouse_encoder_free(self.raw) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{Terminal, TerminalOptions};

    #[test]
    fn encodes_a_press_to_non_empty_bytes() {
        let mut encoder = MouseEncoder::new().expect("mouse encoder creation failed");
        encoder
            .set_tracking(MouseTrackingMode::Normal)
            .expect("tracking set failed");
        encoder
            .set_format(MouseFormat::Sgr)
            .expect("format set failed");
        encoder
            .set_geometry(MouseGeometry {
                screen_width: 80,
                screen_height: 24,
                cell_width: 8,
                cell_height: 16,
                padding_top: 0,
                padding_bottom: 0,
                padding_right: 0,
                padding_left: 0,
            })
            .expect("geometry set failed");
        let mut out = Vec::new();
        encoder
            .encode(
                &MouseInput {
                    action: MouseAction::Press,
                    button: Some(MouseButton::Left),
                    mods: 0,
                    x: 12.0,
                    y: 16.0,
                },
                &mut out,
            )
            .expect("mouse encode failed");
        assert!(!out.is_empty(), "mouse encoding was empty");
    }

    #[test]
    fn syncs_from_terminal() {
        let terminal =
            Terminal::new(&TerminalOptions::default()).expect("terminal creation failed");
        let mut encoder = MouseEncoder::new().expect("mouse encoder creation failed");
        encoder
            .set_from_terminal(&terminal)
            .expect("set_from_terminal failed");
    }

    #[test]
    fn sgr_wheel_up_is_press_then_release() {
        let mut out = Vec::new();
        encode_sgr_mouse(&mut out, 64, 1, 1, true, 0);
        encode_sgr_mouse(&mut out, 64, 1, 1, false, 0);
        assert_eq!(
            out, b"\x1b[<64;1;1M\x1b[<64;1;1m",
            "wheel up must be button 64 press+release at 1;1"
        );
    }

    #[test]
    fn sgr_wheel_down_is_button_65() {
        let mut out = Vec::new();
        encode_sgr_mouse(&mut out, 65, 10, 20, true, 0);
        assert_eq!(
            out, b"\x1b[<65;10;20M",
            "wheel down must be button 65 at 1-based 10;20"
        );
    }

    #[test]
    fn sgr_left_press_motion_release() {
        let mut out = Vec::new();
        encode_sgr_mouse(&mut out, 0, 3, 4, true, 0);
        encode_sgr_mouse(&mut out, 0, 3, 5, true, sgr_mods::MOTION);
        encode_sgr_mouse(&mut out, 0, 3, 5, false, 0);
        assert_eq!(
            out, b"\x1b[<0;3;4M\x1b[<32;3;5M\x1b[<0;3;5m",
            "left press, motion (button 0 + 32), release must use M/m finals"
        );
    }

    #[test]
    fn sgr_modifier_bits_are_shift_4_alt_8_ctrl_16() {
        let mut out = Vec::new();
        encode_sgr_mouse(&mut out, 0, 1, 1, true, sgr_mods::SHIFT);
        encode_sgr_mouse(&mut out, 0, 1, 1, true, sgr_mods::ALT);
        encode_sgr_mouse(&mut out, 0, 1, 1, true, sgr_mods::CTRL);
        assert_eq!(
            out, b"\x1b[<4;1;1M\x1b[<8;1;1M\x1b[<16;1;1M",
            "SGR modifier bits must be SHIFT=4, ALT=8, CTRL=16"
        );
    }

    #[test]
    fn sgr_coords_are_one_based() {
        let mut out = Vec::new();
        encode_sgr_mouse(&mut out, 0, 0, 0, true, 0);
        assert_eq!(
            out, b"\x1b[<0;1;1M",
            "input (0,0) must clamp to 1-based 1;1"
        );
    }
}
