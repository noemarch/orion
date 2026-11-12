//! The local socket a runtime listens on.
//!
//! A runtime is reached over a named pipe on Windows and a unix domain socket
//! elsewhere, never over TCP. Only the current user may open it, and its name
//! carries the session so two runtimes on one machine never meet. The bytes on
//! the wire are [`crate::runtime::framing`]'s; this module only moves them.

use std::io;
use std::path::PathBuf;

/// The environment variable that selects an isolated runtime. Tests use it to
/// keep out of each other's way.
const SESSION_ENV: &str = "ORION_RUNTIME_SESSION";

/// The longest session fragment kept in a pipe or socket name. Windows pipe
/// names are short and a truncated session is still distinct enough.
const MAX_NAME_CHARS: usize = 48;

/// Where the runtime socket lives for this session.
///
/// The name carries the session, so two sessions on one machine never meet.
pub fn socket_path(session: &str) -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(pipe_path(session))
    }
    #[cfg(unix)]
    {
        runtime_dir().join(format!("{}.sock", sanitize(session)))
    }
}

/// The session this process belongs to.
///
/// `ORION_RUNTIME_SESSION` picks an isolated runtime, which is how the tests
/// keep out of each other's way.
pub fn current_session() -> String {
    std::env::var(SESSION_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "default".to_string())
}

/// The directory both the lock and the unix socket live in.
pub(crate) fn runtime_dir() -> PathBuf {
    crate::engine::store::default_data_dir().join("runtime")
}

/// Creates the runtime directory if it is missing, private to this user.
pub(crate) fn ensure_runtime_dir() -> io::Result<PathBuf> {
    let dir = runtime_dir();
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}

/// Keeps only characters a pipe or file name may carry, and keeps it short.
pub(crate) fn sanitize(session: &str) -> String {
    let mut clean: String = session
        .chars()
        .take(MAX_NAME_CHARS)
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if clean.is_empty() {
        clean.push_str("default");
    }
    clean
}

#[cfg(windows)]
fn pipe_name(clean: &str) -> String {
    format!("orion-runtime-{clean}")
}

#[cfg(windows)]
fn pipe_path(session: &str) -> String {
    format!(r"\\.\pipe\{}", pipe_name(&sanitize(session)))
}

#[cfg(windows)]
mod imp {
    use std::io::{self, Read, Write};
    use std::mem;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::ptr;
    use std::sync::Arc;

    use parking_lot::Mutex;
    use serde_json::Value;
    use windows_sys::Win32::Foundation::{
        LocalFree, ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_CONNECTED,
        ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, OPEN_EXISTING,
        PIPE_ACCESS_DUPLEX,
    };
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
        PIPE_UNLIMITED_INSTANCES,
    };

    use crate::error::Result;
    use crate::runtime::client::FrameConnection;
    use crate::runtime::framing;

    /// Grants the pipe to its owner and to nobody else.
    ///
    /// `OW` is the Owner Rights SID, so the ACE follows the object's owner (this
    /// process's user) instead of naming a SID we would have to look up. `P`
    /// keeps inherited ACEs out, so the permissive default DACL is never
    /// applied.
    const PIPE_SDDL: &str = "D:P(A;;GA;;;OW)";

    /// Per-direction kernel buffer for one pipe instance.
    const BUFFER_BYTES: u32 = 64 * 1024;

    /// A listener that hands out connections to the runtime socket.
    pub struct RuntimeListener {
        name: String,
        /// A duplicate handle on the first instance, kept open for the life of
        /// the listener so the pipe's name stays in the namespace.
        _holder: Pipe,
        pending: tokio::sync::Mutex<Option<Pipe>>,
    }

    impl RuntimeListener {
        /// Starts listening on the runtime socket for `session`.
        pub fn bind(session: &str) -> Result<Self> {
            let name = super::pipe_path(session);
            let first = create_instance(&name, true)?;
            // Windows removes the pipe name from the namespace when the
            // instance created with `FILE_FLAG_FIRST_PIPE_INSTANCE` closes,
            // even while other instances of the same name are open. That first
            // instance is the one the first client connects to, so a duplicate
            // of its handle is kept for the whole life of the listener: the
            // instance stays open, and so does the name.
            let holder = duplicate_handle(&first)?;
            Ok(Self {
                name,
                _holder: holder,
                pending: tokio::sync::Mutex::new(Some(first)),
            })
        }

        /// Waits for the next connection.
        pub async fn accept(&self) -> io::Result<RuntimeStream> {
            let mut pending = self.pending.lock().await;
            let server = pending.take().ok_or_else(|| {
                io::Error::new(io::ErrorKind::BrokenPipe, "the runtime listener is closed")
            })?;
            // `ConnectNamedPipe` blocks until a client arrives, so it must not
            // run on an async worker thread. Exactly one instance is listening
            // while this waits, so the client that arrives connects to THIS
            // instance and not to the next one.
            let connected = tokio::task::spawn_blocking(move || connect_instance(server))
                .await
                .map_err(io::Error::other)??;
            // The next instance exists before the connected one is handed out,
            // so the pipe is never without an instance: a client that arrives
            // mid hand-off still finds a listening pipe, and the name stays in
            // the namespace.
            *pending = Some(create_instance(&self.name, false)?);
            Ok(RuntimeStream::from_pipe(connected))
        }
    }

    /// Duplicates a pipe instance's handle.
    ///
    /// The duplicate refers to the same instance, so keeping it open keeps the
    /// instance — and the pipe's name — alive.
    fn duplicate_handle(pipe: &Pipe) -> io::Result<Pipe> {
        use windows_sys::Win32::Foundation::DuplicateHandle;
        use windows_sys::Win32::System::Threading::GetCurrentProcess;

        const DUPLICATE_SAME_ACCESS: u32 = 2;
        let mut duplicate: windows_sys::Win32::Foundation::HANDLE = std::ptr::null_mut();
        // SAFETY: the process handle is the one this thread runs in, and the
        // duplicated handle is written only on success.
        let ok = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                pipe.handle.as_raw_handle(),
                GetCurrentProcess(),
                &mut duplicate,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Pipe {
            // SAFETY: `DuplicateHandle` wrote a fresh handle here and nobody
            // else owns it.
            handle: unsafe { OwnedHandle::from_raw_handle(duplicate) },
        })
    }

    fn wide(path: &str) -> Vec<u16> {
        path.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Creates one listening instance of the runtime pipe.
    ///
    /// Instances are deliberately *not* opened with `FILE_FLAG_OVERLAPPED`: the
    /// frame contract is synchronous, and a synchronous handle keeps blocking
    /// `ReadFile`/`WriteFile` free of reactor `OVERLAPPED` state.
    fn create_instance(name: &str, first: bool) -> io::Result<Pipe> {
        let security = SecurityDescriptor::new()?;
        let path = wide(name);
        let open_mode = if first {
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE
        } else {
            PIPE_ACCESS_DUPLEX
        };
        // SAFETY: `path` is NUL-terminated UTF-16, `security.attributes` points
        // at a descriptor that lives through the call, and the pipe copies it
        // into its own security context.
        let handle = unsafe {
            CreateNamedPipeW(
                path.as_ptr(),
                open_mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE,
                PIPE_UNLIMITED_INSTANCES,
                BUFFER_BYTES,
                BUFFER_BYTES,
                0,
                &security.attributes,
            )
        };
        owned(handle)
    }

    /// Blocks until a client connects to `pipe`, then returns the connection.
    fn connect_instance(pipe: Pipe) -> io::Result<Pipe> {
        // SAFETY: `pipe` owns a live server handle and a null `OVERLAPPED` asks
        // for synchronous completion.
        let connected = unsafe { ConnectNamedPipe(pipe.handle.as_raw_handle(), ptr::null_mut()) };
        if connected != 0 {
            return Ok(pipe);
        }
        let error = io::Error::last_os_error();
        // The client can finish connecting between creation and this call.
        if error.raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32) {
            Ok(pipe)
        } else {
            Err(error)
        }
    }

    fn owned(handle: windows_sys::Win32::Foundation::HANDLE) -> io::Result<Pipe> {
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(Pipe {
            // SAFETY: `handle` was returned by this module and has not been
            // adopted elsewhere, so `Pipe` is its sole owner.
            handle: unsafe { OwnedHandle::from_raw_handle(handle) },
        })
    }

    /// A security descriptor built from SDDL, released with `LocalFree`.
    struct SecurityDescriptor {
        attributes: SECURITY_ATTRIBUTES,
        descriptor: PSECURITY_DESCRIPTOR,
    }

    impl SecurityDescriptor {
        fn new() -> io::Result<Self> {
            let sddl = wide(PIPE_SDDL);
            let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
            // SAFETY: `sddl` is NUL-terminated UTF-16 and `descriptor` is a
            // local this call fills in on success.
            let converted = unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    ptr::null_mut(),
                )
            };
            if converted == 0 {
                return Err(io::Error::last_os_error());
            }
            let attributes = SECURITY_ATTRIBUTES {
                nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor,
                bInheritHandle: 0,
            };
            Ok(Self {
                attributes,
                descriptor,
            })
        }
    }

    impl Drop for SecurityDescriptor {
        fn drop(&mut self) {
            // SAFETY: `descriptor` came from the SDDL conversion above and has
            // not been freed; `LocalFree` is that API's documented release.
            unsafe { LocalFree(self.descriptor) };
        }
    }

    /// The blocking half of a connected pipe.
    struct Pipe {
        handle: OwnedHandle,
    }

    impl Pipe {
        fn connect_path(path: &str) -> io::Result<Self> {
            let path = wide(path);
            // SAFETY: `path` is NUL-terminated UTF-16, no sharing is requested,
            // and a null template keeps the connection private to this client.
            let handle = unsafe {
                CreateFileW(
                    path.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    ptr::null(),
                    OPEN_EXISTING,
                    0,
                    ptr::null_mut(),
                )
            };
            owned(handle)
        }

        fn read_blocking(&self, buf: &mut [u8]) -> io::Result<usize> {
            if buf.is_empty() {
                return Ok(0);
            }
            let mut transferred = 0u32;
            // SAFETY: `buf` is writable for its whole length, the handle is
            // live, and a null `OVERLAPPED` asks for synchronous completion.
            let ok = unsafe {
                ReadFile(
                    self.handle.as_raw_handle(),
                    buf.as_mut_ptr(),
                    buf.len() as u32,
                    &mut transferred,
                    ptr::null_mut(),
                )
            };
            if ok != 0 {
                Ok(transferred as usize)
            } else {
                let error = io::Error::last_os_error();
                // A closed pipe is a clean end of stream for a reader.
                if disconnected(&error) {
                    Ok(0)
                } else {
                    Err(error)
                }
            }
        }

        fn write_blocking(&self, buf: &[u8]) -> io::Result<usize> {
            if buf.is_empty() {
                return Ok(0);
            }
            let mut transferred = 0u32;
            // SAFETY: `buf` is readable for its whole length, the handle is
            // live, and a null `OVERLAPPED` asks for synchronous completion.
            let ok = unsafe {
                WriteFile(
                    self.handle.as_raw_handle(),
                    buf.as_ptr(),
                    buf.len() as u32,
                    &mut transferred,
                    ptr::null_mut(),
                )
            };
            if ok != 0 {
                Ok(transferred as usize)
            } else {
                let error = io::Error::last_os_error();
                if disconnected(&error) {
                    Err(io::Error::new(io::ErrorKind::BrokenPipe, error))
                } else {
                    Err(error)
                }
            }
        }
    }

    impl Read for &Pipe {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.read_blocking(buf)
        }
    }

    impl Write for &Pipe {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.write_blocking(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn disconnected(error: &io::Error) -> bool {
        matches!(
            error.raw_os_error(),
            Some(code)
                if code == ERROR_BROKEN_PIPE as i32
                    || code == ERROR_NO_DATA as i32
                    || code == ERROR_PIPE_NOT_CONNECTED as i32
        )
    }

    /// One connection to the runtime socket.
    pub struct RuntimeStream {
        shared: Arc<Shared>,
    }

    struct Shared {
        pipe: Pipe,
        read_gate: Mutex<()>,
        write_gate: Mutex<()>,
    }

    impl RuntimeStream {
        /// Connects to the runtime socket for `session`.
        pub async fn connect(session: &str) -> io::Result<Self> {
            let path = super::pipe_path(session);
            // Opening the pipe blocks until the server has a listening instance
            // to offer, so it must not run on an async worker thread: a
            // runtime that hosts the server in the same process would
            // otherwise wait for a connection it cannot make.
            let pipe = tokio::task::spawn_blocking(move || Pipe::connect_path(&path))
                .await
                .map_err(io::Error::other)??;
            Ok(Self::from_pipe(pipe))
        }

        fn from_pipe(pipe: Pipe) -> Self {
            Self {
                shared: Arc::new(Shared {
                    pipe,
                    read_gate: Mutex::new(()),
                    write_gate: Mutex::new(()),
                }),
            }
        }
    }

    impl FrameConnection for RuntimeStream {
        fn read_frame(&mut self) -> io::Result<Option<Value>> {
            let _gate = self.shared.read_gate.lock();
            let mut reader = &self.shared.pipe;
            framing::read_frame(&mut reader)
        }

        fn write_frame(&mut self, frame: &Value) -> io::Result<()> {
            let _gate = self.shared.write_gate.lock();
            let mut writer = &self.shared.pipe;
            framing::write_frame(&mut writer, frame)
        }

        fn clone_box(&self) -> Box<dyn FrameConnection> {
            Box::new(Self {
                shared: Arc::clone(&self.shared),
            })
        }
    }
}

#[cfg(unix)]
mod imp {
    use std::io::{self, Read, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixStream as StdUnixStream;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use parking_lot::Mutex;
    use serde_json::Value;
    use tokio::net::{UnixListener, UnixStream};

    use crate::error::Result;
    use crate::runtime::client::FrameConnection;
    use crate::runtime::framing;

    /// A listener that hands out connections to the runtime socket.
    pub struct RuntimeListener {
        path: PathBuf,
        listener: UnixListener,
    }

    impl RuntimeListener {
        /// Starts listening on the runtime socket for `session`.
        pub fn bind(session: &str) -> Result<Self> {
            let dir = super::ensure_runtime_dir()?;
            let path = dir.join(format!("{}.sock", super::sanitize(session)));
            remove_stale_socket(&path)?;
            let listener = UnixListener::bind(&path)?;
            // The socket is for this user only, exactly like its directory.
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            Ok(Self { path, listener })
        }

        /// Waits for the next connection.
        pub async fn accept(&self) -> io::Result<RuntimeStream> {
            let (stream, _address) = self.listener.accept().await?;
            RuntimeStream::from_std(stream.into_std()?)
        }
    }

    impl Drop for RuntimeListener {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    /// A socket file left by a crashed runtime would make every later bind fail,
    /// so it is removed unless a live process still answers on it.
    fn remove_stale_socket(path: &Path) -> io::Result<()> {
        if !path.exists() {
            return Ok(());
        }
        match StdUnixStream::connect(path) {
            Ok(_live) => Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "another runtime listens on this socket",
            )),
            Err(_) => match std::fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            },
        }
    }

    /// One connection to the runtime socket.
    pub struct RuntimeStream {
        shared: Arc<Shared>,
    }

    struct Shared {
        pipe: Pipe,
        read_gate: Mutex<()>,
        write_gate: Mutex<()>,
    }

    impl RuntimeStream {
        fn from_std(stream: StdUnixStream) -> io::Result<Self> {
            Ok(Self {
                shared: Arc::new(Shared {
                    pipe: Pipe { stream },
                    read_gate: Mutex::new(()),
                    write_gate: Mutex::new(()),
                }),
            })
        }

        /// Connects to the runtime socket for `session`.
        pub async fn connect(session: &str) -> io::Result<Self> {
            let stream = UnixStream::connect(super::socket_path(session)).await?;
            Self::from_std(stream.into_std()?)
        }
    }

    struct Pipe {
        stream: StdUnixStream,
    }

    impl Read for &Pipe {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let mut stream = &self.stream;
            stream.read(buf)
        }
    }

    impl Write for &Pipe {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let mut stream = &self.stream;
            stream.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            let mut stream = &self.stream;
            stream.flush()
        }
    }

    impl FrameConnection for RuntimeStream {
        fn read_frame(&mut self) -> io::Result<Option<Value>> {
            let _gate = self.shared.read_gate.lock();
            let mut reader = &self.shared.pipe;
            framing::read_frame(&mut reader)
        }

        fn write_frame(&mut self, frame: &Value) -> io::Result<()> {
            let _gate = self.shared.write_gate.lock();
            let mut writer = &self.shared.pipe;
            framing::write_frame(&mut writer, frame)
        }

        fn clone_box(&self) -> Box<dyn FrameConnection> {
            Box::new(Self {
                shared: Arc::clone(&self.shared),
            })
        }
    }
}

pub use imp::{RuntimeListener, RuntimeStream};

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use super::*;
    use crate::runtime::client::FrameConnection;

    /// A session no other test on this machine uses.
    fn unique_session(tag: &str) -> String {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        format!(
            "{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )
    }

    #[test]
    fn two_sessions_do_not_share_a_socket() {
        let first = socket_path("a session/one");
        let second = socket_path("b session\\two");
        assert_ne!(first, second);

        for path in [first, second] {
            let name = path
                .file_name()
                .expect("the socket path has a name")
                .to_string_lossy()
                .into_owned();
            assert!(!name.contains(' '), "the session is sanitized: {name}");
            assert!(!name.contains('\\'), "the name has no separators: {name}");
            assert!(name.len() <= 64, "the name stays short: {name}");
        }
    }

    #[tokio::test]
    async fn a_runtime_client_can_reach_a_runtime_listener() {
        let session = unique_session("roundtrip");
        let listener = RuntimeListener::bind(&session).expect("the listener binds");

        let exchange = async { tokio::join!(listener.accept(), RuntimeStream::connect(&session)) };
        let (accepted, connected) = tokio::time::timeout(Duration::from_secs(10), exchange)
            .await
            .expect("the transport answers within the timeout");
        let mut server = accepted.expect("the listener accepts");
        let mut client = connected.expect("the client connects");

        let request = serde_json::json!({ "type": "ping", "id": 1 });
        client.write_frame(&request).expect("the client writes");
        let read = server.read_frame().expect("the listener reads");
        assert_eq!(read, Some(request));

        let answer = serde_json::json!({ "type": "pong", "id": 1 });
        server.write_frame(&answer).expect("the listener writes");
        let read = client.read_frame().expect("the client reads");
        assert_eq!(read, Some(answer));
    }

    #[tokio::test]
    async fn only_one_listener_holds_the_socket() {
        let session = unique_session("single");
        // Binding keeps the first instance open for the second bind to trip on.
        let _first = RuntimeListener::bind(&session).expect("the first listener binds");
        let second = RuntimeListener::bind(&session);
        assert!(
            second.is_err(),
            "a second listener must not take the same socket"
        );
    }

    #[tokio::test]
    async fn a_client_can_reach_the_runtime_after_another_client_leaves() {
        let session = unique_session("after");
        let listener = RuntimeListener::bind(&session).expect("the listener binds");

        {
            let exchange =
                async { tokio::join!(listener.accept(), RuntimeStream::connect(&session)) };
            let (accepted, connected) = tokio::time::timeout(Duration::from_secs(10), exchange)
                .await
                .expect("the first transport answers within the timeout");
            let mut server = accepted.expect("the listener accepts the first client");
            let mut client = connected.expect("the first client connects");
            let request = serde_json::json!({ "type": "ping", "id": 1 });
            client
                .write_frame(&request)
                .expect("the first client writes");
            assert_eq!(
                server.read_frame().expect("the listener reads"),
                Some(request)
            );
        }

        let exchange = async { tokio::join!(listener.accept(), RuntimeStream::connect(&session)) };
        let (accepted, connected) = tokio::time::timeout(Duration::from_secs(10), exchange)
            .await
            .expect("the second transport answers within the timeout");
        let mut server = accepted.expect("the listener accepts the second client");
        let mut client = connected.expect("the second client connects");
        let request = serde_json::json!({ "type": "ping", "id": 2 });
        client
            .write_frame(&request)
            .expect("the second client writes");
        assert_eq!(
            server.read_frame().expect("the listener reads"),
            Some(request)
        );
    }

    #[tokio::test]
    async fn a_client_can_reach_the_runtime_ten_times_in_a_row() {
        let session = unique_session("ten");
        let listener = RuntimeListener::bind(&session).expect("the listener binds");

        for i in 0..10 {
            let exchange =
                async { tokio::join!(listener.accept(), RuntimeStream::connect(&session)) };
            let (accepted, connected) = tokio::time::timeout(Duration::from_secs(10), exchange)
                .await
                .expect("the transport answers within the timeout");
            let mut server = accepted.expect("the listener accepts");
            let mut client = connected.expect("the client connects");
            let request = serde_json::json!({ "type": "ping", "id": i });
            client.write_frame(&request).expect("the client writes");
            assert_eq!(
                server.read_frame().expect("the listener reads"),
                Some(request)
            );
        }
    }
}
