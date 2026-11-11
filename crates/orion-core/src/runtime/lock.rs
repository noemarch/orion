//! The lock that keeps one runtime per session.
//!
//! The lock is a file the daemon holds open; a second daemon finds the file and
//! refuses to start. The file names its owner so a crashed runtime — whose file
//! is all that is left — does not lock the session forever.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::error::{OrionError, Result};
use crate::runtime::transport::{ensure_runtime_dir, runtime_dir, sanitize};

/// The lock that keeps one runtime per session.
///
/// The lock is a file the daemon holds open; a second daemon finds the file
/// and refuses to start.
pub struct RuntimeLock {
    path: PathBuf,
}

impl RuntimeLock {
    /// Takes the lock for `session`, or reports that another runtime has it.
    pub fn acquire(session: &str) -> Result<Self> {
        let dir = ensure_runtime_dir()?;
        let path = dir.join(format!("{}.lock", sanitize(session)));
        let pid = std::process::id();
        loop {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    writeln!(file, "{pid}")?;
                    file.flush()?;
                    return Ok(Self { path });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    if !lock_is_stale(&path) {
                        return Err(OrionError::Internal(format!(
                            "a runtime already holds the lock for session `{session}`"
                        )));
                    }
                    // The owner is gone; only its file is left.
                    match fs::remove_file(&path) {
                        Ok(()) => continue,
                        Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                        Err(error) => return Err(error.into()),
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    /// Whether a runtime already holds the lock for `session`.
    pub fn is_held(session: &str) -> bool {
        match read_pid(&lock_path(session)) {
            Some(pid) => process_is_alive(pid),
            // A file without an owner is what `acquire` treats as free.
            None => false,
        }
    }
}

impl Drop for RuntimeLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn lock_path(session: &str) -> PathBuf {
    runtime_dir().join(format!("{}.lock", sanitize(session)))
}

fn read_pid(path: &Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// A lock whose owner is gone — or whose file cannot name an owner — is stale.
fn lock_is_stale(path: &Path) -> bool {
    match read_pid(path) {
        Some(pid) => !process_is_alive(pid),
        None => true,
    }
}

#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};

    if pid == 0 {
        return false;
    }
    // SAFETY: `OpenProcess` is a plain query; a non-null handle is closed below
    // and no handle is leaked.
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if handle.is_null() {
        // Access denied means the process exists but belongs to someone else.
        return io::Error::last_os_error().raw_os_error() == Some(ERROR_ACCESS_DENIED as i32);
    }
    // SAFETY: `handle` was returned by `OpenProcess` and is owned here.
    unsafe { CloseHandle(handle) };
    true
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    // `EPERM`: the process exists but belongs to someone else.
    const EPERM: i32 = 1;

    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    // SAFETY: signal 0 delivers nothing; it only performs the existence and
    // permission check.
    let result = unsafe { kill(pid as i32, 0) };
    if result == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() == Some(EPERM)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    fn unique_session(tag: &str) -> String {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        format!(
            "{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )
    }

    #[test]
    fn the_second_runtime_cannot_take_the_lock() {
        let session = unique_session("second");
        let lock = RuntimeLock::acquire(&session).expect("the first runtime takes the lock");
        assert!(
            RuntimeLock::is_held(&session),
            "the lock is reported as held"
        );

        let second = RuntimeLock::acquire(&session);
        assert!(second.is_err(), "a second runtime must be refused");
        drop(lock);
    }

    #[test]
    fn the_lock_is_free_again_when_it_is_dropped() {
        let session = unique_session("free");
        let lock = RuntimeLock::acquire(&session).expect("the first runtime takes the lock");
        drop(lock);

        assert!(!RuntimeLock::is_held(&session), "the lock is released");
        let again = RuntimeLock::acquire(&session).expect("the lock is free again");
        drop(again);
    }

    #[test]
    fn a_lock_left_by_a_dead_process_is_taken_over() {
        let session = unique_session("stale");
        ensure_runtime_dir().expect("the runtime directory exists");
        let path = lock_path(&session);
        // A pid no machine hands out: negative as a unix pid, invalid as a win32 pid.
        std::fs::write(&path, "4294967294\n").expect("a dead owner's lock file");
        assert!(!RuntimeLock::is_held(&session), "a dead pid holds nothing");

        let lock = RuntimeLock::acquire(&session).expect("the stale lock is taken over");
        assert!(
            RuntimeLock::is_held(&session),
            "the new owner holds the lock"
        );
        drop(lock);
    }
}
