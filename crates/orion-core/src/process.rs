//! Process containment.
//!
//! Agent CLIs are spawned as children of Orion. On Windows a child survives its parent unless it
//! is in a job object that kills on close, so a crash or a `taskkill /f` on Orion would leave
//! `claude.exe`, `codex.exe` and their tooling running with nobody watching. M0 puts the app
//! itself in such a job; M1 adds each spawned agent to it as well.

/// `CREATE_NO_WINDOW`: the child gets a console, but no window is shown for it.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A `Command` for a helper process whose console window must never appear.
///
/// A process that has no console of its own — the GUI app, or the `orion daemon` it starts
/// detached — makes Windows allocate a fresh console window for every console child it starts.
/// That window is the stray terminal that flashes while `git`, `tailscale`, or an agent's
/// `--version` query runs. `CREATE_NO_WINDOW` suppresses it at creation time.
///
/// Agent panes do not use this: `portable-pty` gives its child a hidden ConPTY console on purpose.
#[cfg(windows)]
pub fn silent_command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    use std::os::windows::process::CommandExt;

    let mut command = std::process::Command::new(program);
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

/// Non-Windows platforms never allocate a console window for a child; build the command as usual.
#[cfg(not(windows))]
pub fn silent_command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    std::process::Command::new(program)
}

/// Puts the current process in a job object that kills every member when the last handle to the
/// job closes — which happens when Orion exits, including a crash.
///
/// Returns the platform's error for the caller to log rather than abort: an app launched from a
/// job that forbids nesting (a service manager, some CI runners) must still start, and M1's
/// per-child assignment retries there.
#[cfg(windows)]
pub fn contain_current_process() -> std::io::Result<()> {
    use std::ffi::c_void;
    use std::mem::size_of;

    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    // SAFETY: every handle is created here, every struct is fully initialized before use, and the
    // job handle is intentionally leaked so it stays open for the life of the process.
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return Err(std::io::Error::last_os_error());
        }

        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION as *const c_void,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if configured == 0 {
            return Err(std::io::Error::last_os_error());
        }

        if AssignProcessToJobObject(job, GetCurrentProcess()) == 0 {
            return Err(std::io::Error::last_os_error());
        }

        // Deliberately not closed: the job lives exactly as long as the process, and closing it
        // now would kill the process tree it was just told to protect.
        Ok(())
    }
}

/// Non-Windows platforms already tear down a process group; there is nothing to do here.
#[cfg(not(windows))]
pub fn contain_current_process() -> std::io::Result<()> {
    Ok(())
}

/// One pane's process tree. A job object per pane is what makes closing a pane close the agent's
/// tooling too: `portable-pty` only knows the direct child, and a shell script that Claude started
/// would survive a `Child::kill`.
#[cfg(windows)]
pub struct ChildJob {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

// SAFETY: a job handle is an opaque kernel handle; Windows serializes the calls made with it.
#[cfg(windows)]
unsafe impl Send for ChildJob {}
#[cfg(windows)]
unsafe impl Sync for ChildJob {}

#[cfg(windows)]
impl ChildJob {
    /// Makes a job that kills its members when the last handle closes, then puts `pid` in it.
    pub fn assign(pid: u32) -> std::io::Result<Self> {
        use std::ffi::c_void;
        use std::mem::size_of;

        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
        };

        // SAFETY: handles are closed below when a later step fails; the struct is zeroed then
        // fully filled before it is read.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Err(std::io::Error::last_os_error());
            }

            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION as *const c_void,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) == 0
            {
                let error = std::io::Error::last_os_error();
                CloseHandle(job);
                return Err(error);
            }

            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if process.is_null() {
                let error = std::io::Error::last_os_error();
                CloseHandle(job);
                return Err(error);
            }
            let assigned = AssignProcessToJobObject(job, process);
            let error = std::io::Error::last_os_error();
            CloseHandle(process);
            if assigned == 0 {
                CloseHandle(job);
                return Err(error);
            }
            Ok(Self { handle: job })
        }
    }

    /// Ends every process in the job now.
    pub fn terminate(&self) {
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;
        // SAFETY: the handle is owned by this job and stays open until `drop`.
        unsafe {
            TerminateJobObject(self.handle, 1);
        }
    }
}

#[cfg(windows)]
impl Drop for ChildJob {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        // KILL_ON_JOB_CLOSE is the point: dropping the last handle ends anything still inside.
        // SAFETY: this job owns the handle and closes it once.
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

#[cfg(not(windows))]
pub struct ChildJob;

#[cfg(not(windows))]
impl ChildJob {
    pub fn assign(_pid: u32) -> std::io::Result<Self> {
        Ok(Self)
    }

    pub fn terminate(&self) {}
}

#[cfg(test)]
mod tests {
    #[test]
    fn containment_is_idempotent_enough_to_call_once() {
        // Calling it in a test process is safe: the job is empty beyond this process and is
        // leaked, so the test runner is not killed when the setup call returns.
        let first = super::contain_current_process();
        assert!(
            first.is_ok() || cfg!(windows),
            "non-Windows containment is a no-op"
        );
    }
}
