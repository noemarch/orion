use std::marker::PhantomData;

use crate::ffi as raw;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    Press,
    Repeat,
    Release,
}

impl KeyAction {
    fn raw(self) -> raw::GhosttyEnum {
        match self {
            KeyAction::Release => raw::GHOSTTY_KEY_ACTION_RELEASE,
            KeyAction::Press => raw::GHOSTTY_KEY_ACTION_PRESS,
            KeyAction::Repeat => raw::GHOSTTY_KEY_ACTION_REPEAT,
        }
    }
}

#[derive(Debug, Clone)]
pub struct KeyInput {
    pub key: u32,
    pub mods: u16,
    pub action: KeyAction,
    pub utf8: Option<String>,
    pub unshifted_codepoint: Option<u32>,
}

pub struct KeyEncoder {
    raw: raw::GhosttyKeyEncoder,
    _not_sync: PhantomData<*const ()>,
}

unsafe impl Send for KeyEncoder {}

impl KeyEncoder {
    pub fn new() -> Result<Self, crate::Status> {
        let mut handle: raw::GhosttyKeyEncoder = std::ptr::null_mut();
        let result = unsafe { raw::ghostty_key_encoder_new(std::ptr::null(), &mut handle) };
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
            unsafe { raw::ghostty_key_encoder_set_from_terminal(self.raw, terminal.raw()) };
        if result == raw::GHOSTTY_SUCCESS {
            Ok(())
        } else {
            Err(crate::Status(result))
        }
    }

    pub fn encode(&mut self, key: &KeyInput, out: &mut Vec<u8>) -> Result<(), crate::Status> {
        let mut event: raw::GhosttyKeyEvent = std::ptr::null_mut();
        let result = unsafe { raw::ghostty_key_event_new(std::ptr::null(), &mut event) };
        if result != raw::GHOSTTY_SUCCESS {
            return Err(crate::Status(result));
        }
        let outcome = self.encode_event(event, key, out);
        unsafe { raw::ghostty_key_event_free(event) };
        outcome
    }

    fn encode_event(
        &mut self,
        event: raw::GhosttyKeyEvent,
        key: &KeyInput,
        out: &mut Vec<u8>,
    ) -> Result<(), crate::Status> {
        let mut result = unsafe { raw::ghostty_key_event_set_action(event, key.action.raw()) };
        if result != raw::GHOSTTY_SUCCESS {
            return Err(crate::Status(result));
        }
        result = unsafe { raw::ghostty_key_event_set_key(event, key.key as raw::GhosttyKey) };
        if result != raw::GHOSTTY_SUCCESS {
            return Err(crate::Status(result));
        }
        result = unsafe { raw::ghostty_key_event_set_mods(event, key.mods) };
        if result != raw::GHOSTTY_SUCCESS {
            return Err(crate::Status(result));
        }
        let utf8 = match key.utf8.as_deref() {
            Some(text) => raw::GhosttyString {
                ptr: text.as_ptr(),
                len: text.len(),
            },
            None => raw::GhosttyString::default(),
        };
        result = unsafe { raw::ghostty_key_event_set_utf8(event, utf8) };
        if result != raw::GHOSTTY_SUCCESS {
            return Err(crate::Status(result));
        }
        if let Some(codepoint) = key.unshifted_codepoint {
            result = unsafe { raw::ghostty_key_event_set_unshifted_codepoint(event, codepoint) };
            if result != raw::GHOSTTY_SUCCESS {
                return Err(crate::Status(result));
            }
        }

        let mut needed: usize = 0;
        result = unsafe {
            raw::ghostty_key_encoder_encode(self.raw, event, std::ptr::null_mut(), 0, &mut needed)
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
            raw::ghostty_key_encoder_encode(
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

impl Drop for KeyEncoder {
    fn drop(&mut self) {
        unsafe { raw::ghostty_key_encoder_free(self.raw) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{Terminal, TerminalOptions};

    #[test]
    fn encodes_a_key_to_non_empty_bytes() {
        let mut encoder = KeyEncoder::new().expect("key encoder creation failed");
        let mut out = Vec::new();
        encoder
            .encode(
                &KeyInput {
                    key: raw::GHOSTTY_KEY_A as u32,
                    mods: 0,
                    action: KeyAction::Press,
                    utf8: Some("a".to_string()),
                    unshifted_codepoint: Some(0x61),
                },
                &mut out,
            )
            .expect("key encode failed");
        assert_eq!(out, b"a", "unexpected encoding: {out:?}");
    }

    #[test]
    fn syncs_from_terminal() {
        let terminal =
            Terminal::new(&TerminalOptions::default()).expect("terminal creation failed");
        let mut encoder = KeyEncoder::new().expect("key encoder creation failed");
        encoder
            .set_from_terminal(&terminal)
            .expect("set_from_terminal failed");
        let mut out = Vec::new();
        encoder
            .encode(
                &KeyInput {
                    key: raw::GHOSTTY_KEY_A as u32,
                    mods: 0,
                    action: KeyAction::Press,
                    utf8: Some("a".to_string()),
                    unshifted_codepoint: None,
                },
                &mut out,
            )
            .expect("key encode failed");
        assert!(!out.is_empty());
    }
}
