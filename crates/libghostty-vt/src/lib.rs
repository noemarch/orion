//! Rust bindings for the headless libghostty-vt 1.0.1 C ABI.
//!
//! This crate vendors the official `vt-1.0.1` source and dependency tarballs and
//! builds them with Zig at compile time. It exposes a small safe wrapper over the
//! terminal-state, render-state, formatter, and input-encoder APIs.

pub mod ffi;
pub mod formatter;
pub mod key;
pub mod mouse;
pub mod render;
pub mod snapshot;
pub mod terminal;

/// The vendored libghostty-vt release.
pub const VERSION: &str = "1.0.1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status(pub ffi::GhosttyResult);

impl Status {
    pub fn is_ok(self) -> bool {
        self.0 == ffi::GHOSTTY_SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::ffi;

    #[test]
    fn terminal_create_and_free() {
        unsafe {
            let mut terminal: ffi::GhosttyTerminal = std::ptr::null_mut();
            let rc = ffi::ghostty_terminal_new(std::ptr::null(), &mut terminal, 80, 24);
            assert_eq!(rc, ffi::GHOSTTY_SUCCESS, "ghostty_terminal_new failed");
            assert!(!terminal.is_null());
            ffi::ghostty_terminal_free(terminal);
        }
    }
}
